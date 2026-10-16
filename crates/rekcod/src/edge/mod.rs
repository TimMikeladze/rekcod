//! `rekcod edge serve` (docs/edge-http.md): routes `Host: <app>.<domain>` to live instances.
//!
//! hyper parses every request, so each one is routed on its own Host, even on a keep-alive
//! connection a tunnel shares between hostnames. Upgrades (WebSockets) become a byte pipe
//! after the 101.

mod balance;
pub mod ban;
mod client_ip;
mod idle;
pub mod limit;
mod stats;

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::header::{self, HeaderMap, HeaderName, HeaderValue};
use hyper::http::request::Parts;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};

use crate::fleet::{self, QUICK, Remote, Route, call, call_within};
use crate::paths::Paths;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type Body = http_body_util::combinators::UnsyncBoxBody<Bytes, BoxError>;

const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_MAX_BODY: u64 = 100 << 20;
const MAX_CONNS: usize = 10_000;
/// Upgraded connections (WebSockets) open at once, and how long one may sit silent.
const MAX_TUNNELS: usize = 5_000;
const TUNNEL_IDLE: Duration = Duration::from_secs(3600);
static TUNNELS: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(MAX_TUNNELS)));
const AUTO_BAN: Duration = Duration::from_secs(600);
const IDLE_TIMEOUT: Duration = if cfg!(test) { Duration::from_millis(400) } else { Duration::from_secs(90) };

pub struct Options {
    pub listen: String,
    pub public_listen: Option<String>,
    pub proxy_protocol: bool,
    pub settings: Settings,
}

pub struct Settings {
    /// Time between tokens in each client IP's edge-wide bucket.
    pub rate: Duration,
    pub burst: u32,
    /// One line per request on stderr.
    pub access_log: bool,
    /// Peers whose `CF-Connecting-IP` / `X-Forwarded-For` name the client: the tunnel.
    pub trusted_proxies: Vec<ipnet::IpNet>,
}

impl Default for Settings {
    /// What `edge serve` starts with: cloudflared on this Mac connects from loopback.
    fn default() -> Settings {
        let trusted_proxies = ["127.0.0.0/8", "::1"].iter().map(|n| ban::parse_net(n).unwrap()).collect();
        Settings { rate: Duration::from_millis(20), burst: 100, access_log: false, trusted_proxies }
    }
}

/// How one listener treats its clients.
#[derive(Clone, Copy, Default)]
pub struct Listener {
    /// Only apps marked public; a private one looks like no app at all.
    pub public_only: bool,
    pub proxy_protocol: bool,
}

pub struct Edge {
    remote: Option<Remote>,
    pub table: RwLock<HashMap<String, Route>>,
    /// Hosts with requests since the last activity report.
    seen: Mutex<HashSet<String>>,
    pub bans: ban::Bans,
    limits: limit::Limits,
    pool: balance::Pool,
    request_ids: AtomicU64,
    id_prefix: String,
    access_log: bool,
    trusted_proxies: Vec<ipnet::IpNet>,
    stats: stats::Stats,
    /// Where the last good route table is kept, so a restart while the controller is down
    /// still routes (`~/.rekcod/edge-routes.json`).
    cache: Option<std::path::PathBuf>,
    /// One wake per host at a time; other requests for it wait on the same one.
    waking: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// When a host's last wake failed: it gets 503s, not another wake, for [`WAKE_BACKOFF`].
    wake_failed: Mutex<HashMap<String, std::time::Instant>>,
}

/// After a failed wake, how long the edge answers 503 instead of asking again. Public
/// traffic would otherwise turn into a rebuild loop.
const WAKE_BACKOFF: Duration = Duration::from_secs(30);

impl Edge {
    pub fn new(remote: Option<Remote>, s: Settings) -> Arc<Edge> {
        let id_prefix = crate::sandbox::new_id().unwrap_or_else(|_| format!("{:x}", std::process::id()));
        Arc::new(Edge {
            remote,
            table: Default::default(),
            seen: Default::default(),
            bans: Default::default(),
            limits: limit::Limits::new(s.rate, s.burst),
            pool: Default::default(),
            request_ids: AtomicU64::new(0),
            id_prefix,
            access_log: s.access_log,
            trusted_proxies: s.trusted_proxies,
            stats: Default::default(),
            cache: None,
            waking: Default::default(),
            wake_failed: Default::default(),
        })
    }

    /// Starts from the cached route table, and keeps it up to date from now on.
    fn with_cache(self: Arc<Self>, path: std::path::PathBuf) -> Arc<Self> {
        if let Some(m) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<HashMap<String, Route>>(&b).ok()) {
            eprintln!("rekcod: edge: {} route(s) from {}", m.len(), path.display());
            *self.table.write().unwrap_or_else(std::sync::PoisonError::into_inner) = m;
        }
        let mut e = Arc::try_unwrap(self).unwrap_or_else(|_| unreachable!("not shared yet"));
        e.cache = Some(path);
        Arc::new(e)
    }

    fn trusts(&self, peer: IpAddr) -> bool {
        self.trusted_proxies.iter().any(|n| n.contains(&peer))
    }

    /// Reports activity, then pulls routes and bans from the controller. Blocking.
    fn refresh(&self) {
        let Some(r) = &self.remote else { return };
        let hosts: Vec<String> = self.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).drain().collect();
        let (traffic, clients) = self.stats.take(minute_now());
        if !(hosts.is_empty() && traffic.is_empty() && clients.is_empty())
            && let Err(e) = call_within(
                &r.url,
                "POST",
                "/routes/activity",
                Some(&r.token),
                &json!({"hosts": hosts, "traffic": traffic, "clients": clients}),
                QUICK,
            )
        {
            eprintln!("rekcod: edge: activity: {e:#}");
        }
        // A reply that doesn't parse keeps the table we have; emptying it would take every app down.
        match call_within(&r.url, "GET", "/routes", Some(&r.token), &Value::Null, QUICK)
            .and_then(|v| Ok(serde_json::from_value::<HashMap<String, Route>>(v)?))
        {
            Ok(m) => {
                let live: HashSet<String> = m.values().flat_map(|r| r.endpoints.iter().cloned()).collect();
                self.pool.retain(&live);
                if let Some(path) = &self.cache
                    && let Ok(b) = serde_json::to_vec(&m)
                {
                    let tmp = path.with_extension("tmp");
                    if std::fs::write(&tmp, b).is_ok() {
                        let _ = std::fs::rename(tmp, path);
                    }
                }
                *self.table.write().unwrap_or_else(std::sync::PoisonError::into_inner) = m;
            }
            Err(e) => eprintln!("rekcod: edge: routes: {e:#}"),
        }
        match call_within(&r.url, "GET", "/bans", Some(&r.token), &Value::Null, QUICK) {
            Ok(v) => self.bans.set_remote(
                v.as_array().into_iter().flatten().filter_map(|b| b["net"].as_str().and_then(|n| ban::parse_net(n).ok())).collect(),
            ),
            Err(e) => eprintln!("rekcod: edge: bans: {e:#}"),
        }
        self.limits.prune();
    }

    /// Asks the controller to wake the app and waits (up to its health timeout, or a rebuild).
    async fn wake(&self, host: &str, route: &Route) -> Vec<String> {
        let Some(r) = self.remote.clone() else { return vec![] };
        if self.wake_failed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(host).is_some_and(|t| t.elapsed() < WAKE_BACKOFF)
        {
            return vec![];
        }
        let lock = self.waking.lock().unwrap_or_else(std::sync::PoisonError::into_inner).entry(host.to_string()).or_default().clone();
        let _one = lock.lock().await;
        // Woken by whoever held the lock before us.
        if let Some(eps) = self.table.read().unwrap_or_else(std::sync::PoisonError::into_inner).get(host).map(|r| r.endpoints.clone())
            && !eps.is_empty()
        {
            return eps;
        }
        let ns = if route.ns.is_empty() { "default".to_string() } else { route.ns.clone() };
        let app = route.app.clone();
        let path = format!("/ns/{ns}/apps/{app}/wake");
        let v = tokio::task::spawn_blocking(move || call(&r.url, "POST", &path, Some(&r.token), &Value::Null)).await;
        match v.map_err(anyhow::Error::from).and_then(|v| v) {
            Ok(v) => {
                let eps: Vec<String> =
                    v["endpoints"].as_array().into_iter().flatten().filter_map(|e| e.as_str().map(String::from)).collect();
                if let Some(r) = self.table.write().unwrap_or_else(std::sync::PoisonError::into_inner).get_mut(host) {
                    r.endpoints = eps.clone();
                }
                if eps.is_empty() {
                    self.wake_failed
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(host.to_string(), std::time::Instant::now());
                }
                eps
            }
            Err(e) => {
                eprintln!("rekcod: edge: wake {app}: {e:#}");
                self.wake_failed
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(host.to_string(), std::time::Instant::now());
                vec![]
            }
        }
    }

    /// A 429 for `ip`; enough of them bans it here now and on every edge after the
    /// controller has it.
    fn strike(&self, ip: IpAddr) {
        // Banning the tunnel would ban everyone behind it.
        if !self.limits.strike(ip) || self.trusts(ip) {
            return;
        }
        eprintln!("rekcod: edge: banning {ip} for {}s ({} rate-limited requests in a minute)", AUTO_BAN.as_secs(), limit::STRIKES);
        self.bans.ban_for(ip, AUTO_BAN);
        if let Some(r) = self.remote.clone() {
            let body = json!({"net": ip.to_string(), "secs": AUTO_BAN.as_secs(), "reason": "auto: rate limited"});
            tokio::task::spawn_blocking(move || {
                if let Err(e) = call(&r.url, "POST", "/bans", Some(&r.token), &body) {
                    eprintln!("rekcod: edge: report ban: {e:#}");
                }
            });
        }
    }

    fn request_id(&self) -> String {
        format!("{}-{:x}", self.id_prefix, self.request_ids.fetch_add(1, Ordering::Relaxed))
    }
}

fn minute_now() -> i64 {
    (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() / 60) as i64
}

/// `rekcod edge serve`. `listen` routes every app (for the tailnet); `public_listen` only
/// apps marked public (point cloudflared or another tunnel at it).
pub fn serve(paths: &Paths, remote_name: Option<&str>, o: Options) -> Result<()> {
    crate::lifecycle::trim_log_hourly(paths.home.join("logs/edge.log"));
    let edge = Edge::new(Some(fleet::remote(paths, remote_name)?), o.settings).with_cache(paths.home.join("edge-routes.json"));
    let e = edge.clone();
    std::thread::spawn(move || {
        loop {
            e.refresh();
            std::thread::sleep(Duration::from_secs(2));
        }
    });
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(async move {
        if let Some(p) = &o.public_listen {
            let l = TcpListener::bind(p).await.with_context(|| format!("bind {p}"))?;
            eprintln!("rekcod: edge (public apps only) on http://{p}");
            let opts = Listener { public_only: true, proxy_protocol: o.proxy_protocol };
            tokio::spawn(run(l, edge.clone(), opts));
        }
        let l = TcpListener::bind(&o.listen).await.with_context(|| format!("bind {}", o.listen))?;
        eprintln!("rekcod: edge (all apps) on http://{}", o.listen);
        run(l, edge, Listener::default()).await;
        Ok(())
    })
}

pub async fn run(l: TcpListener, edge: Arc<Edge>, opts: Listener) {
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNS));
    loop {
        let (s, peer) = match l.accept().await {
            Ok(c) => c,
            Err(e) => {
                // Out of file descriptors, usually; don't spin.
                eprintln!("rekcod: edge: accept: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let Ok(slot) = slots.clone().try_acquire_owned() else { continue };
        let edge = edge.clone();
        tokio::spawn(async move {
            let _ = conn(s, peer, edge, opts).await;
            drop(slot);
        });
    }
}

async fn conn(mut s: TcpStream, peer: SocketAddr, edge: Arc<Edge>, opts: Listener) -> Result<()> {
    let _ = s.set_nodelay(true);
    let mut peer = peer.ip();
    if opts.proxy_protocol
        && let Some(ip) = tokio::time::timeout(HEADER_TIMEOUT, client_ip::read_proxy_v2(&mut s)).await??
    {
        peer = ip;
    }
    // Before any parsing: a banned client gets a closed connection and nothing else. Not
    // for the tunnel, which carries everyone; its clients are checked per request.
    if !edge.trusts(peer) && edge.bans.is_banned(peer) {
        return Ok(());
    }
    let activity = Arc::new(idle::Activity::new());
    let a = activity.clone();
    let svc = hyper::service::service_fn(move |req| {
        let (edge, a) = (edge.clone(), a.clone());
        async move {
            let _busy = a.busy();
            Ok::<_, Infallible>(handle(edge, req, peer, opts).await)
        }
    });
    let mut b = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
    b.http1().timer(TokioTimer::new()).header_read_timeout(HEADER_TIMEOUT);
    let conn = b.serve_connection_with_upgrades(TokioIo::new(idle::Tracked { io: s, activity: activity.clone() }), svc);
    tokio::pin!(conn);
    let mut tick = tokio::time::interval(IDLE_TIMEOUT / 4);
    let mut closing = false;
    loop {
        tokio::select! {
            r = conn.as_mut() => return r.map_err(|e| anyhow::anyhow!(e)),
            _ = tick.tick(), if !closing => {
                if activity.idle() >= IDLE_TIMEOUT {
                    closing = true;
                    conn.as_mut().graceful_shutdown();
                }
            }
        }
    }
}

fn text(code: StatusCode, msg: impl Into<String>) -> Response<Body> {
    let mut r = Response::new(Full::new(Bytes::from(msg.into())).map_err(|e| match e {}).boxed_unsync());
    *r.status_mut() = code;
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    r
}

/// The Host header (h1) or `:authority` (h2), without the port, lowercased.
fn req_host(req: &Request<Incoming>) -> String {
    let h = req.headers().get(header::HOST).and_then(|v| v.to_str().ok()).or(req.uri().authority().map(|a| a.as_str())).unwrap_or("");
    let h = h.rsplit_once(':').filter(|(_, p)| p.bytes().all(|b| b.is_ascii_digit())).map_or(h, |(h, _)| h);
    h.to_ascii_lowercase()
}

/// The header names listed in `Connection`, lowercased.
fn connection_tokens(h: &HeaderMap) -> Vec<HeaderName> {
    h.get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|t| HeaderName::from_bytes(t.trim().to_ascii_lowercase().as_bytes()).ok())
        .collect()
}

fn is_upgrade(h: &HeaderMap) -> bool {
    h.contains_key(header::UPGRADE) && connection_tokens(h).contains(&header::UPGRADE)
}

/// Hop-by-hop headers belong to one connection, not the request. An upgrade keeps
/// `Upgrade` and `Connection: upgrade`.
fn strip_hop(h: &mut HeaderMap, upgrade: bool) {
    for n in connection_tokens(h).into_iter().filter(|n| !(upgrade && n == header::UPGRADE)) {
        h.remove(n);
    }
    for n in ["keep-alive", "proxy-connection", "te", "trailer", "transfer-encoding"] {
        h.remove(n);
    }
    h.remove(header::CONNECTION);
    if upgrade {
        h.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
    }
}

/// Turns the client's request into the one an instance gets: HTTP/1.1, origin-form path,
/// a Host header, and the forwarding headers.
fn prepare(edge: &Edge, p: &mut Parts, client: IpAddr, peer: IpAddr, upgrade: bool) {
    let trusted = edge.trusts(peer);
    let host = p.headers.get(header::HOST).cloned().or_else(|| p.uri.authority().and_then(|a| HeaderValue::from_str(a.as_str()).ok()));
    strip_hop(&mut p.headers, upgrade);
    if let Some(h) = host {
        p.headers.insert(header::HOST, h);
    }
    p.uri = p.uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/").parse().unwrap_or_default();
    p.version = hyper::Version::HTTP_11;
    let xff = HeaderName::from_static("x-forwarded-for");
    let chain = match p.headers.get(&xff).and_then(|v| v.to_str().ok()) {
        Some(prior) if trusted => format!("{prior}, {peer}"),
        _ => client.to_string(),
    };
    if let Ok(v) = HeaderValue::from_str(&chain) {
        p.headers.insert(xff, v);
    }
    let proto = HeaderName::from_static("x-forwarded-proto");
    if !trusted || !p.headers.contains_key(&proto) {
        p.headers.insert(proto, HeaderValue::from_static("http"));
    }
    if !trusted {
        // An app that trusts this header would trust whatever the client wrote.
        p.headers.remove("cf-connecting-ip");
    }
    let id = HeaderName::from_static("x-request-id");
    if !p.headers.contains_key(&id)
        && let Ok(v) = HeaderValue::from_str(&edge.request_id())
    {
        p.headers.insert(id, v);
    }
}

async fn handle(edge: Arc<Edge>, req: Request<Incoming>, peer: IpAddr, opts: Listener) -> Response<Body> {
    let client = if edge.trusts(peer) { client_ip::forwarded(req.headers(), &edge.trusted_proxies).unwrap_or(peer) } else { peer };
    let host = req_host(&req);
    let t = Instant::now();
    let log = edge.access_log.then(|| (req.method().clone(), req.uri().path().to_string()));
    // Also catches a ban made while this connection was kept alive.
    let resp = if edge.bans.is_banned(client) {
        text(StatusCode::FORBIDDEN, "forbidden\n")
    } else {
        forward(&edge, req, &host, client, peer, opts).await
    };
    let ms = t.elapsed().as_millis().min(u32::MAX as u128) as u32;
    let known = edge.table.read().unwrap_or_else(std::sync::PoisonError::into_inner).contains_key(&host);
    edge.stats.request(minute_now(), known.then_some(host.as_str()), (!edge.trusts(client)).then_some(client), resp.status().as_u16(), ms);
    if let Some((method, path)) = log {
        eprintln!("{client} {method} {host}{path} {} {ms}ms", resp.status().as_u16());
    }
    resp
}

/// Whether the request failed because its body went over `max_body`.
fn over_limit(e: &hyper::Error) -> bool {
    let mut cause = std::error::Error::source(e);
    while let Some(c) = cause {
        if c.is::<http_body_util::LengthLimitError>() {
            return true;
        }
        cause = c.source();
    }
    false
}

async fn forward(edge: &Edge, mut req: Request<Incoming>, host: &str, client: IpAddr, peer: IpAddr, opts: Listener) -> Response<Body> {
    let route =
        edge.table.read().unwrap_or_else(std::sync::PoisonError::into_inner).get(host).filter(|r| r.public || !opts.public_only).cloned();
    let Some(route) = route else {
        return text(StatusCode::NOT_FOUND, format!("no app at {host}\n"));
    };
    if let Err(wait) = edge.limits.check(client, host, &route.http) {
        edge.stats.limited(client);
        edge.strike(client);
        let mut r = text(StatusCode::TOO_MANY_REQUESTS, "too many requests\n");
        r.headers_mut().insert(header::RETRY_AFTER, wait.as_secs().max(1).into());
        return r;
    }
    edge.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(host.to_string());
    let max_body = route.http.max_body.unwrap_or(DEFAULT_MAX_BODY);
    let declared = req.headers().get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()?.parse::<u64>().ok());
    if declared.is_some_and(|n| n > max_body) {
        return text(StatusCode::PAYLOAD_TOO_LARGE, format!("request body over {max_body} bytes\n"));
    }
    let upgrade = is_upgrade(req.headers());
    let client_upgrade = upgrade.then(|| hyper::upgrade::on(&mut req));
    let (mut parts, body) = req.into_parts();
    prepare(edge, &mut parts, client, peer, upgrade);
    // A chunked body can't be checked up front: it's cut off at the limit and the client gets a 413.
    let mut req = Some((parts, Limited::new(body, max_body as usize).boxed_unsync()));

    let mut eps = route.endpoints.clone();
    if eps.is_empty() && !route.app.is_empty() {
        // Asleep: hold this request while the controller starts it.
        eps = edge.wake(host, &route).await;
    }
    // Twice: the second pass after a wake, for an app that went to sleep since the last
    // route refresh.
    for pass in 0..2 {
        if let Some(mut lease) = edge.pool.lease(&eps).await {
            let (parts, body) = req.take().expect("sent once");
            let sent = tokio::time::timeout(RESPONSE_TIMEOUT, lease.sender().send_request(Request::from_parts(parts, body))).await;
            return match sent {
                Err(_) => text(StatusCode::GATEWAY_TIMEOUT, format!("{host} took over {}s to answer\n", RESPONSE_TIMEOUT.as_secs())),
                Ok(Err(e)) if over_limit(&e) => text(StatusCode::PAYLOAD_TOO_LARGE, format!("request body over {max_body} bytes\n")),
                Ok(Err(e)) => text(StatusCode::BAD_GATEWAY, format!("{host} ({}): {e}\n", lease.endpoint)),
                Ok(Ok(mut resp)) => {
                    if resp.status() == StatusCode::SWITCHING_PROTOCOLS
                        && let Some(down) = client_upgrade
                    {
                        let up = hyper::upgrade::on(&mut resp);
                        // The connection's slot is freed once it's upgraded, so tunnels have
                        // their own cap, and close after an hour with no bytes either way.
                        if let Ok(slot) = TUNNELS.clone().try_acquire_owned() {
                            tokio::spawn(async move {
                                if let (Ok(a), Ok(b)) = tokio::join!(down, up) {
                                    let act = Arc::new(idle::Activity::new());
                                    let mut a = idle::Tracked { io: TokioIo::new(a), activity: act.clone() };
                                    let mut b = idle::Tracked { io: TokioIo::new(b), activity: act.clone() };
                                    let watch = async {
                                        while act.idle() < TUNNEL_IDLE {
                                            tokio::time::sleep(Duration::from_secs(60)).await;
                                        }
                                    };
                                    tokio::select! {
                                        _ = tokio::io::copy_bidirectional(&mut a, &mut b) => {}
                                        _ = watch => {}
                                    }
                                }
                                drop(slot);
                            });
                        }
                        lease.done(false);
                    } else {
                        strip_hop(resp.headers_mut(), false);
                        lease.done(true);
                    }
                    resp.map(|b| b.map_err(Into::into).boxed_unsync())
                }
            };
        }
        if pass == 0 && !route.app.is_empty() {
            eps = edge.wake(host, &route).await;
        } else {
            break;
        }
    }
    text(StatusCode::SERVICE_UNAVAILABLE, format!("{host} didn't come up\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::HttpSettings;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    /// An instance that answers `<name>|<X-Forwarded-For>` on keep-alive, and echoes bytes
    /// after a 101 for an upgrade request.
    async fn instance(name: &'static str) -> String {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            loop {
                let (s, _) = l.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut r = BufReader::new(s);
                    loop {
                        let mut head = Vec::new();
                        loop {
                            let mut line = String::new();
                            if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                                return;
                            }
                            if line == "\r\n" {
                                break;
                            }
                            head.push(line.to_ascii_lowercase());
                        }
                        let xff = head.iter().find_map(|l| l.strip_prefix("x-forwarded-for:")).unwrap_or("").trim().to_string();
                        if head.iter().any(|l| l.starts_with("upgrade:")) {
                            r.get_mut()
                                .write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: upgrade\r\nUpgrade: echo\r\n\r\n")
                                .await
                                .unwrap();
                            let mut s = r.into_inner();
                            let mut buf = [0u8; 64];
                            while let Ok(n) = s.read(&mut buf).await {
                                if n == 0 || s.write_all(&buf[..n]).await.is_err() {
                                    return;
                                }
                            }
                            return;
                        }
                        if head.iter().any(|l| l.starts_with("transfer-encoding: chunked")) {
                            // Read the body before answering, as an app would.
                            let mut line = String::new();
                            while line != "0\r\n" {
                                line.clear();
                                if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                                    return;
                                }
                            }
                            r.read_line(&mut line).await.unwrap_or(0);
                        }
                        let body = format!("{name}|{xff}");
                        let resp = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len());
                        r.get_mut().write_all(resp.as_bytes()).await.unwrap();
                    }
                });
            }
        });
        addr
    }

    /// Generous edge-wide limits; loopback (the test client) is a trusted proxy.
    fn fast() -> Settings {
        Settings { rate: Duration::from_millis(1), burst: 1000, ..Default::default() }
    }

    fn direct() -> Settings {
        Settings { trusted_proxies: vec![], ..fast() }
    }

    fn route(eps: &[&str], public: bool) -> Route {
        Route { public, endpoints: eps.iter().map(|e| e.to_string()).collect(), ..Default::default() }
    }

    async fn edge_on(edge: &Arc<Edge>, opts: Listener) -> String {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        tokio::spawn(run(l, edge.clone(), opts));
        addr
    }

    /// Sends one request on `s` and reads one response: (status, body).
    async fn get(s: &mut BufReader<TcpStream>, host: &str, extra: &str) -> (u16, String) {
        s.get_mut().write_all(format!("GET / HTTP/1.1\r\nHost: {host}\r\n{extra}\r\n").as_bytes()).await.unwrap();
        let mut status = String::new();
        s.read_line(&mut status).await.unwrap();
        let code = status.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
        let mut len = 0;
        loop {
            let mut line = String::new();
            s.read_line(&mut line).await.unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap();
            }
        }
        let mut body = vec![0u8; len];
        s.read_exact(&mut body).await.unwrap();
        (code, String::from_utf8(body).unwrap())
    }

    async fn dial(addr: &str) -> BufReader<TcpStream> {
        BufReader::new(TcpStream::connect(addr).await.unwrap())
    }

    fn app(body: &str) -> &str {
        body.split('|').next().unwrap()
    }

    #[tokio::test]
    async fn routes_every_request_on_a_kept_alive_connection() {
        let (a, b) = (instance("a").await, instance("b").await);
        let edge = Edge::new(None, fast());
        edge.table
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend([("a.test".to_string(), route(&[&a], true)), ("b.test".to_string(), route(&[&b], false))]);
        let private = edge_on(&edge, Listener::default()).await;
        let mut s = dial(&private).await;
        assert_eq!(app(&get(&mut s, "a.test", "").await.1), "a");
        assert_eq!(app(&get(&mut s, "B.test:8080", "").await.1), "b");
        assert_eq!(get(&mut s, "nope.test", "").await.0, 404);

        // The public listener hides private apps, also on a connection opened for a public one.
        let public = edge_on(&edge, Listener { public_only: true, ..Default::default() }).await;
        let mut s = dial(&public).await;
        assert_eq!(app(&get(&mut s, "a.test", "").await.1), "a");
        assert_eq!(get(&mut s, "b.test", "").await.0, 404);
    }

    #[tokio::test]
    async fn skips_a_dead_instance() {
        let dead = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().to_string();
        let live = instance("live").await;
        let edge = Edge::new(None, fast());
        edge.table.write().unwrap_or_else(std::sync::PoisonError::into_inner).insert("a.test".into(), route(&[&dead, &live], false));
        let addr = edge_on(&edge, Listener::default()).await;
        for _ in 0..4 {
            let (code, body) = get(&mut dial(&addr).await, "a.test", "").await;
            assert_eq!((code, app(&body)), (200, "live"));
        }
        edge.table.write().unwrap_or_else(std::sync::PoisonError::into_inner).insert("a.test".into(), route(&[&dead], false));
        assert_eq!(get(&mut dial(&addr).await, "a.test", "").await.0, 503);
    }

    #[tokio::test]
    async fn rate_limits_then_bans() {
        let a = instance("a").await;
        let edge = Edge::new(None, Settings { rate: Duration::from_secs(60), burst: 2, ..direct() });
        edge.table.write().unwrap_or_else(std::sync::PoisonError::into_inner).insert("a.test".into(), route(&[&a], false));
        let addr = edge_on(&edge, Listener::default()).await;
        let mut s = dial(&addr).await;
        assert_eq!(get(&mut s, "a.test", "").await.0, 200);
        assert_eq!(get(&mut s, "a.test", "").await.0, 200);
        assert_eq!(get(&mut s, "a.test", "").await.0, 429);

        // A per-app rate applies on top of the edge's.
        let edge = Edge::new(None, direct());
        let http = HttpSettings { rate: Some("1/m".into()), burst: Some(1), max_body: None };
        edge.table
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert("a.test".into(), Route { http, ..route(&[&a], false) });
        let addr = edge_on(&edge, Listener::default()).await;
        let mut s = dial(&addr).await;
        assert_eq!(get(&mut s, "a.test", "").await.0, 200);
        for _ in 1..limit::STRIKES {
            assert_eq!(get(&mut s, "a.test", "").await.0, 429);
        }
        // The last strike bans 127.0.0.1: new connections are closed without a reply.
        assert_eq!(get(&mut s, "a.test", "").await.0, 429);
        let mut s = dial(&addr).await;
        s.get_mut().write_all(b"GET / HTTP/1.1\r\nHost: a.test\r\n\r\n").await.unwrap();
        let mut buf = Vec::new();
        assert_eq!(s.read_to_end(&mut buf).await.unwrap_or(0), 0);
    }

    #[tokio::test]
    async fn forwarded_for_only_from_trusted_proxies() {
        let a = instance("a").await;
        let spoof = "X-Forwarded-For: 203.0.113.9\r\n";
        let edge = Edge::new(None, direct());
        edge.table.write().unwrap_or_else(std::sync::PoisonError::into_inner).insert("a.test".into(), route(&[&a], true));
        let addr = edge_on(&edge, Listener::default()).await;
        assert_eq!(get(&mut dial(&addr).await, "a.test", spoof).await.1, "a|127.0.0.1");

        // Loopback is trusted by default, as cloudflared on the same Mac.
        let edge = Edge::new(None, fast());
        let http = HttpSettings { rate: Some("1/m".into()), burst: Some(1), max_body: None };
        edge.table.write().unwrap_or_else(std::sync::PoisonError::into_inner).insert("a.test".into(), Route { http, ..route(&[&a], true) });
        let tunnel = edge_on(&edge, Listener { public_only: true, ..Default::default() }).await;
        let mut s = dial(&tunnel).await;
        assert_eq!(get(&mut s, "a.test", spoof).await.1, "a|203.0.113.9, 127.0.0.1");
        // Each visitor behind the tunnel has their own bucket.
        assert_eq!(get(&mut s, "a.test", spoof).await.0, 429);
        assert_eq!(get(&mut s, "a.test", "X-Forwarded-For: 203.0.113.10\r\n").await.0, 200);
        // Strikes ban the visitor, never the tunnel.
        for _ in 0..limit::STRIKES {
            get(&mut s, "a.test", spoof).await;
        }
        assert_eq!(get(&mut s, "a.test", spoof).await.0, 403);
        assert!(!edge.bans.is_banned("127.0.0.1".parse().unwrap()));
        assert_eq!(get(&mut dial(&tunnel).await, "a.test", "X-Forwarded-For: 203.0.113.11\r\n").await.0, 200);
    }

    #[tokio::test]
    async fn closes_idle_connections_and_caps_chunked_bodies() {
        let a = instance("a").await;
        let edge = Edge::new(None, fast());
        let http = HttpSettings { max_body: Some(8), ..Default::default() };
        edge.table
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert("a.test".into(), Route { http, ..route(&[&a], false) });
        let addr = edge_on(&edge, Listener::default()).await;
        let mut s = dial(&addr).await;
        assert_eq!(get(&mut s, "a.test", "").await.0, 200);
        let mut buf = Vec::new();
        let closed = tokio::time::timeout(IDLE_TIMEOUT * 3, s.read_to_end(&mut buf)).await;
        assert!(matches!(closed, Ok(Ok(0))), "{closed:?}");

        let mut s = dial(&addr).await;
        let post = "POST / HTTP/1.1\r\nHost: a.test\r\nTransfer-Encoding: chunked\r\n\r\n10\r\n0123456789abcdef\r\n0\r\n\r\n";
        s.get_mut().write_all(post.as_bytes()).await.unwrap();
        let mut status = String::new();
        s.read_line(&mut status).await.unwrap();
        assert!(status.starts_with("HTTP/1.1 413"), "{status}");
    }

    #[tokio::test]
    async fn websocket_upgrade_pipes_bytes() {
        let a = instance("a").await;
        let edge = Edge::new(None, fast());
        edge.table.write().unwrap_or_else(std::sync::PoisonError::into_inner).insert("a.test".into(), route(&[&a], false));
        let addr = edge_on(&edge, Listener::default()).await;
        let mut s = dial(&addr).await;
        s.get_mut().write_all(b"GET /ws HTTP/1.1\r\nHost: a.test\r\nConnection: Upgrade\r\nUpgrade: echo\r\n\r\n").await.unwrap();
        let mut line = String::new();
        s.read_line(&mut line).await.unwrap();
        assert!(line.starts_with("HTTP/1.1 101"), "{line}");
        while line != "\r\n" {
            line.clear();
            s.read_line(&mut line).await.unwrap();
        }
        s.get_mut().write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        s.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
    }
}
