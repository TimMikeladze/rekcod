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

/// From the first byte of a request head until it's complete.
const HEADER_TIMEOUT: Duration = if cfg!(test) { Duration::from_millis(150) } else { Duration::from_secs(10) };
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_MAX_BODY: u64 = 100 << 20;
/// Client connections across both listeners, at most; fewer when descriptors are short.
const MAX_CONNS: usize = 10_000;
/// Upgraded connections (WebSockets) open at once, and how long one may sit silent.
const MAX_TUNNELS: usize = 5_000;
const TUNNEL_IDLE: Duration = Duration::from_secs(3600);
/// Per client address (IPv6 /64) that isn't a trusted proxy.
const CLIENT_CONNS: usize = 200;
const CLIENT_TUNNELS: usize = 100;
/// Concurrent requests on one HTTP/2 connection; each takes an instance connection.
const H2_STREAMS: u32 = 100;
const AUTO_BAN: Duration = Duration::from_secs(600);
/// A connection that moves no bytes for this long is closed, also mid-response.
const IDLE_TIMEOUT: Duration = if cfg!(test) { Duration::from_millis(400) } else { Duration::from_secs(90) };
/// How long a stopping edge waits for requests in flight. Under launchd's default exit
/// timeout (20 s), after which it sends SIGKILL.
const DRAIN: Duration = Duration::from_secs(15);

/// Connection and tunnel caps for a descriptor limit: each takes two descriptors (client
/// and instance), with some left for the process, idle instance connections and the log.
fn caps(fds: u64) -> (usize, usize) {
    let budget = (fds.saturating_sub(64) / 2).max(16) as usize;
    let tunnels = (budget / 3).min(MAX_TUNNELS);
    ((budget - tunnels).min(MAX_CONNS), tunnels)
}

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
    /// Peers whose `X-Forwarded-For` (or `client_ip_header`) names the client: the tunnel.
    pub trusted_proxies: Vec<ipnet::IpNet>,
    /// A header a trusted proxy overwrites with the client's address, read instead of
    /// `X-Forwarded-For` (`cf-connecting-ip` behind cloudflared).
    pub client_ip_header: Option<HeaderName>,
    /// Peers allowed to send a PROXY v2 header on a `proxy_protocol` listener; anyone else
    /// is disconnected.
    pub proxy_protocol_from: Vec<ipnet::IpNet>,
    /// How long a request for a sleeping app waits for it to wake.
    pub wake_wait: Duration,
    pub max_conns: usize,
    pub max_tunnels: usize,
}

impl Default for Settings {
    /// What `edge serve` starts with: cloudflared on this Mac connects from loopback.
    fn default() -> Settings {
        let trusted_proxies = ["127.0.0.0/8", "::1"].iter().map(|n| ban::parse_net(n).unwrap()).collect();
        Settings {
            rate: Duration::from_millis(20),
            burst: 100,
            access_log: false,
            trusted_proxies,
            client_ip_header: None,
            proxy_protocol_from: vec![],
            wake_wait: Duration::from_secs(120),
            max_conns: MAX_CONNS,
            max_tunnels: MAX_TUNNELS,
        }
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
    client_ip_header: Option<HeaderName>,
    proxy_protocol_from: Vec<ipnet::IpNet>,
    stats: stats::Stats,
    /// Where the last good route table is kept, so a restart while the controller is down
    /// still routes (`~/.rekcod/edge-routes.json`).
    cache: Option<std::path::PathBuf>,
    /// One wake per host at a time; other requests for it wait on the same one.
    waking: Mutex<HashMap<String, Waking>>,
    /// How long a request waits for a wake before it gets a 503.
    wake_wait: Duration,
    max_conns: usize,
    conns: Arc<tokio::sync::Semaphore>,
    tunnels: Arc<tokio::sync::Semaphore>,
    client_conns: Arc<limit::PerClient>,
    client_tunnels: Arc<limit::PerClient>,
    /// Set once on SIGTERM: listeners stop accepting and connections finish what they have.
    stop: tokio::sync::watch::Sender<bool>,
    /// When a host's last wake failed: it gets 503s, not another wake, for [`WAKE_BACKOFF`].
    wake_failed: Mutex<HashMap<String, std::time::Instant>>,
}

/// After a failed wake, how long the edge answers 503 instead of asking again. Public
/// traffic would otherwise turn into a rebuild loop.
const WAKE_BACKOFF: Duration = Duration::from_secs(30);
/// Requests waiting on one host's wake; more get a 503 at once.
const WAKE_WAITERS: usize = 256;

/// A wake in flight. It runs in its own task, so it finishes (and records a failure for
/// the backoff) even when every request that waited for it has gone.
struct Waking {
    /// `Some(endpoints)` once it's over; empty when it failed.
    done: tokio::sync::watch::Receiver<Option<Vec<String>>>,
    waiters: Arc<tokio::sync::Semaphore>,
}

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
            client_ip_header: s.client_ip_header,
            proxy_protocol_from: s.proxy_protocol_from,
            stats: Default::default(),
            cache: None,
            waking: Default::default(),
            wake_wait: s.wake_wait,
            max_conns: s.max_conns,
            conns: Arc::new(tokio::sync::Semaphore::new(s.max_conns)),
            tunnels: Arc::new(tokio::sync::Semaphore::new(s.max_tunnels)),
            client_conns: limit::PerClient::new(CLIENT_CONNS.min(s.max_conns)),
            client_tunnels: limit::PerClient::new(CLIENT_TUNNELS.min(s.max_tunnels)),
            stop: tokio::sync::watch::channel(false).0,
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
        // Only a list replaces the bans: anything else (an error page, a cut-off body, which
        // never parses as an array) keeps the ones we have.
        match call_within(&r.url, "GET", "/bans", Some(&r.token), &Value::Null, QUICK) {
            Ok(Value::Array(list)) => {
                self.bans.set_remote(list.iter().filter_map(|b| b["net"].as_str().and_then(|n| ban::parse_net(n).ok())).collect())
            }
            Ok(v) => eprintln!("rekcod: edge: bans: not a list: {:.200}", v.to_string()),
            Err(e) => eprintln!("rekcod: edge: bans: {e:#}"),
        }
        self.limits.prune();
    }

    fn backing_off(&self, host: &str) -> bool {
        self.wake_failed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(host).is_some_and(|t| t.elapsed() < WAKE_BACKOFF)
    }

    /// Asks the controller to wake the app and waits for it, at most `wake_wait`. `failed`
    /// are endpoints this request just couldn't reach: the table listing them again doesn't
    /// mean the app is up.
    async fn wake(self: &Arc<Self>, host: &str, route: &Route, failed: &[String]) -> Vec<String> {
        let Some(r) = self.remote.clone() else { return vec![] };
        let (mut done, waiters) = {
            let mut waking = self.waking.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match waking.get(host) {
                Some(w) => (w.done.clone(), w.waiters.clone()),
                None => {
                    if self.backing_off(host) {
                        return vec![];
                    }
                    // Woken since this request read the table, by a wake that's over now.
                    if let Some(eps) =
                        self.table.read().unwrap_or_else(std::sync::PoisonError::into_inner).get(host).map(|r| r.endpoints.clone())
                        && !eps.is_empty()
                        && eps != failed
                    {
                        return eps;
                    }
                    let (tx, done) = tokio::sync::watch::channel(None);
                    let waiters = Arc::new(tokio::sync::Semaphore::new(WAKE_WAITERS));
                    waking.insert(host.to_string(), Waking { done: done.clone(), waiters: waiters.clone() });
                    tokio::spawn(self.clone().run_wake(r, host.to_string(), route.clone(), tx));
                    (done, waiters)
                }
            }
        };
        let Ok(_waiter) = waiters.try_acquire_owned() else { return vec![] };
        match tokio::time::timeout(self.wake_wait, done.wait_for(Option::is_some)).await {
            Ok(Ok(eps)) => eps.clone().unwrap_or_default(),
            _ => vec![],
        }
    }

    /// The controller call behind [`Edge::wake`]; it may take minutes when the app is rebuilt.
    async fn run_wake(self: Arc<Self>, r: Remote, host: String, route: Route, tx: tokio::sync::watch::Sender<Option<Vec<String>>>) {
        let ns = if route.ns.is_empty() { "default".to_string() } else { route.ns.clone() };
        let app = route.app.clone();
        let path = format!("/ns/{ns}/apps/{app}/wake");
        let v = tokio::task::spawn_blocking(move || call(&r.url, "POST", &path, Some(&r.token), &Value::Null)).await;
        let eps = match v.map_err(anyhow::Error::from).and_then(|v| v) {
            Ok(v) => v["endpoints"].as_array().into_iter().flatten().filter_map(|e| e.as_str().map(String::from)).collect(),
            Err(e) => {
                eprintln!("rekcod: edge: wake {app}: {e:#}");
                vec![]
            }
        };
        if !eps.is_empty()
            && let Some(r) = self.table.write().unwrap_or_else(std::sync::PoisonError::into_inner).get_mut(&host)
        {
            r.endpoints = eps.clone();
        }
        if eps.is_empty() {
            self.wake_failed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(host.clone(), Instant::now());
        }
        self.waking.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&host);
        let _ = tx.send(Some(eps));
    }

    /// For a 503 while `host` is asleep: the backoff left, or a few seconds.
    fn retry_after(&self, host: &str) -> u64 {
        let failed = self.wake_failed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(host).copied();
        failed.map_or(5, |t| WAKE_BACKOFF.saturating_sub(t.elapsed()).as_secs().max(1))
    }

    /// An edge-wide 429 for `ip`; enough of them ban its address (its /64 for IPv6) here now
    /// and on every edge after the controller has it.
    fn strike(&self, ip: IpAddr) {
        // Banning the tunnel would ban everyone behind it.
        if !self.limits.strike(ip) || self.trusts(ip) {
            return;
        }
        let net = limit::ban_net(ip);
        eprintln!("rekcod: edge: banning {net} for {}s ({} rate-limited requests in a minute)", AUTO_BAN.as_secs(), limit::STRIKES);
        self.bans.ban_for(net, AUTO_BAN);
        if let Some(r) = self.remote.clone() {
            let body = json!({"net": net.to_string(), "secs": AUTO_BAN.as_secs(), "reason": "auto: rate limited"});
            tokio::task::spawn_blocking(move || {
                if let Err(e) = call(&r.url, "POST", "/bans", Some(&r.token), &body) {
                    eprintln!("rekcod: edge: report ban: {e:#}");
                }
            });
        }
    }

    /// Stops the listeners and asks every connection to finish; then waits up to `limit`
    /// for them. True when they all did.
    pub async fn shutdown(&self, limit: Duration) -> bool {
        self.stop.send_replace(true);
        let t = Instant::now();
        while self.conns.available_permits() < self.max_conns {
            if t.elapsed() >= limit {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        true
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
pub fn serve(paths: &Paths, remote_name: Option<&str>, mut o: Options) -> Result<()> {
    crate::lifecycle::trim_log_hourly(paths.home.join("logs/edge.log"));
    let fds = crate::lifecycle::raise_fd_limit();
    (o.settings.max_conns, o.settings.max_tunnels) = caps(fds);
    eprintln!("rekcod: edge: {fds} descriptors: up to {} connections, {} WebSockets", o.settings.max_conns, o.settings.max_tunnels);
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
        tokio::spawn(run(l, edge.clone(), Listener::default()));
        let (mut term, mut int) = (
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?,
        );
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
        eprintln!("rekcod: edge: stopping; waiting up to {}s for requests in flight", DRAIN.as_secs());
        if !edge.shutdown(DRAIN).await {
            eprintln!("rekcod: edge: requests still in flight; exiting anyway");
        }
        // Not a runtime shutdown: that would wait on a wake's blocking controller call.
        std::process::exit(0)
    })
}

/// Accepts until [`Edge::shutdown`].
pub async fn run(l: TcpListener, edge: Arc<Edge>, opts: Listener) {
    let mut stop = edge.stop.subscribe();
    loop {
        let accepted = tokio::select! {
            a = l.accept() => a,
            _ = stop.wait_for(|s| *s) => return,
        };
        let (s, peer) = match accepted {
            Ok(c) => c,
            Err(e) => {
                // Out of file descriptors, usually; don't spin.
                eprintln!("rekcod: edge: accept: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let Ok(slot) = edge.conns.clone().try_acquire_owned() else { continue };
        let edge = edge.clone();
        tokio::spawn(async move {
            let _ = conn(s, peer, edge, opts).await;
            drop(slot);
        });
    }
}

async fn conn(mut s: TcpStream, peer: SocketAddr, edge: Arc<Edge>, opts: Listener) -> Result<()> {
    let _ = s.set_nodelay(true);
    // A dual-stack `[::]` listener sees IPv4 clients as `::ffff:a.b.c.d`.
    let mut peer = peer.ip().to_canonical();
    if opts.proxy_protocol {
        // Whoever may send the header picks the client address, so only the balancer may.
        if !edge.proxy_protocol_from.iter().any(|n| n.contains(&peer)) {
            return Ok(());
        }
        if let Some(ip) = tokio::time::timeout(HEADER_TIMEOUT, client_ip::read_proxy_v2(&mut s)).await?? {
            peer = ip;
        }
    }
    // Before any parsing: a banned client gets a closed connection and nothing else. Not
    // for the tunnel, which carries everyone; its clients are checked per request.
    if !edge.trusts(peer) && edge.bans.is_banned(peer) {
        return Ok(());
    }
    // One address can't hold every slot; the tunnel carries everyone, so it isn't counted.
    let _held = if edge.trusts(peer) {
        None
    } else {
        match edge.client_conns.take(peer) {
            Some(h) => Some(h),
            None => return Ok(()),
        }
    };
    let mut stop = edge.stop.subscribe();
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
    // hyper starts this timer when a keep-alive connection goes idle, so it's the idle
    // timeout; the head's own limit is checked below.
    b.http1().timer(TokioTimer::new()).header_read_timeout(IDLE_TIMEOUT);
    b.http2().max_concurrent_streams(H2_STREAMS);
    let conn = b.serve_connection_with_upgrades(TokioIo::new(idle::Tracked { io: s, activity: activity.clone() }), svc);
    tokio::pin!(conn);
    let mut tick = tokio::time::interval(IDLE_TIMEOUT.min(HEADER_TIMEOUT) / 4);
    let mut closing = false;
    loop {
        tokio::select! {
            r = conn.as_mut() => return r.map_err(|e| anyhow::anyhow!(e)),
            _ = tick.tick() => {
                if activity.head_overdue(HEADER_TIMEOUT) {
                    return Ok(());
                }
                if activity.idle() >= IDLE_TIMEOUT {
                    // graceful_shutdown only ends keep-alive; a response nobody reads (or an
                    // instance that stopped sending) would hold the connection for good.
                    if closing {
                        return Ok(());
                    }
                    closing = true;
                    conn.as_mut().graceful_shutdown();
                }
            }
            _ = stop.wait_for(|s| *s), if !closing => {
                closing = true;
                conn.as_mut().graceful_shutdown();
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
fn prepare(edge: &Edge, p: &mut Parts, client: IpAddr, peer: IpAddr, upgrade: bool, routed: &str) {
    let trusted = edge.trusts(peer);
    let host = p.headers.get(header::HOST).cloned().or_else(|| p.uri.authority().and_then(|a| HeaderValue::from_str(a.as_str()).ok()));
    strip_hop(&mut p.headers, upgrade);
    // Client-address and origin headers an app may believe. Whatever a client or a tunnel
    // passed through is dropped; X-Real-IP and X-Forwarded-Host are set from what the edge
    // decided below.
    for n in ["forwarded", "x-real-ip", "true-client-ip", "x-client-ip", "x-forwarded-host", "x-forwarded-port", "x-forwarded-server"] {
        p.headers.remove(n);
    }
    if let Some(h) = host {
        p.headers.insert(header::HOST, h);
    }
    p.uri = p.uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/").parse().unwrap_or_default();
    p.version = hyper::Version::HTTP_11;
    let xff = HeaderName::from_static("x-forwarded-for");
    // Every line, in order: a proxy that appends a line of its own keeps it.
    let prior: Vec<&str> = p.headers.get_all(&xff).iter().filter_map(|v| v.to_str().ok()).collect();
    let chain = if trusted && !prior.is_empty() { format!("{}, {peer}", prior.join(", ")) } else { client.to_string() };
    if let Ok(v) = HeaderValue::from_str(&chain) {
        p.headers.insert(xff, v);
    }
    if let Ok(v) = HeaderValue::from_str(&client.to_string()) {
        p.headers.insert(HeaderName::from_static("x-real-ip"), v);
    }
    if let Ok(v) = HeaderValue::from_str(routed) {
        p.headers.insert(HeaderName::from_static("x-forwarded-host"), v);
    }
    let proto = HeaderName::from_static("x-forwarded-proto");
    if !trusted || !p.headers.contains_key(&proto) {
        p.headers.insert(proto, HeaderValue::from_static("http"));
    }
    // An app that trusts this header would trust whatever the client wrote. Only a trusted
    // peer that the edge was told overwrites it (cloudflared) passes it on.
    let cf = HeaderName::from_static("cf-connecting-ip");
    if !trusted || edge.client_ip_header.as_ref() != Some(&cf) {
        p.headers.remove(&cf);
    }
    if let Some(n) = edge.client_ip_header.as_ref().filter(|_| !trusted) {
        p.headers.remove(n);
    }
    let id = HeaderName::from_static("x-request-id");
    if !p.headers.contains_key(&id)
        && let Ok(v) = HeaderValue::from_str(&edge.request_id())
    {
        p.headers.insert(id, v);
    }
}

async fn handle(edge: Arc<Edge>, req: Request<Incoming>, peer: IpAddr, opts: Listener) -> Response<Body> {
    let client = if edge.trusts(peer) {
        client_ip::forwarded(req.headers(), &edge.trusted_proxies, edge.client_ip_header.as_ref()).unwrap_or(peer)
    } else {
        peer
    };
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

async fn forward(edge: &Arc<Edge>, mut req: Request<Incoming>, host: &str, client: IpAddr, peer: IpAddr, opts: Listener) -> Response<Body> {
    let route =
        edge.table.read().unwrap_or_else(std::sync::PoisonError::into_inner).get(host).filter(|r| r.public || !opts.public_only).cloned();
    let Some(route) = route else {
        return text(StatusCode::NOT_FOUND, format!("no app at {host}\n"));
    };
    if let Err(refused) = edge.limits.check(client, host, &route.http) {
        edge.stats.limited(client);
        if refused.edge_wide {
            edge.strike(client);
        }
        let mut r = text(StatusCode::TOO_MANY_REQUESTS, "too many requests\n");
        r.headers_mut().insert(header::RETRY_AFTER, refused.wait.as_secs().max(1).into());
        return r;
    }
    edge.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(host.to_string());
    let max_body = route.http.max_body.unwrap_or(DEFAULT_MAX_BODY);
    let declared = req.headers().get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()?.parse::<u64>().ok());
    if declared.is_some_and(|n| n > max_body) {
        return text(StatusCode::PAYLOAD_TOO_LARGE, format!("request body over {max_body} bytes\n"));
    }
    let upgrade = is_upgrade(req.headers());
    // A tunnel's slots are taken before the instance sees the request, so a client is never
    // told 101 and then dropped.
    let tunnel_slots = if upgrade {
        match (edge.tunnels.clone().try_acquire_owned(), edge.client_tunnels.take(client)) {
            (Ok(all), Some(mine)) => Some((all, mine)),
            _ => {
                let mut r = text(StatusCode::SERVICE_UNAVAILABLE, "too many open WebSockets\n");
                r.headers_mut().insert(header::RETRY_AFTER, 5.into());
                return r;
            }
        }
    } else {
        None
    };
    let client_upgrade = upgrade.then(|| hyper::upgrade::on(&mut req));
    let (mut parts, body) = req.into_parts();
    prepare(edge, &mut parts, client, peer, upgrade, host);
    // A chunked body can't be checked up front: it's cut off at the limit and the client gets a 413.
    let mut req = Some((parts, Limited::new(body, max_body as usize).boxed_unsync()));

    let mut eps = route.endpoints.clone();
    if eps.is_empty() && !route.app.is_empty() {
        // Asleep: hold this request while the controller starts it.
        eps = edge.wake(host, &route, &[]).await;
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
                        && let Some(slots) = tunnel_slots
                    {
                        let up = hyper::upgrade::on(&mut resp);
                        // The connection's slot is freed once it's upgraded, so tunnels have
                        // their own cap. They close after an hour with no bytes either way, and
                        // within a minute of the client being banned.
                        let edge = edge.clone();
                        tokio::spawn(async move {
                            if let (Ok(a), Ok(b)) = tokio::join!(down, up) {
                                let act = Arc::new(idle::Activity::new());
                                let mut a = idle::Tracked { io: TokioIo::new(a), activity: act.clone() };
                                let mut b = idle::Tracked { io: TokioIo::new(b), activity: act.clone() };
                                let watch = async {
                                    while act.idle() < TUNNEL_IDLE && !edge.bans.is_banned(client) {
                                        tokio::time::sleep(Duration::from_secs(60)).await;
                                    }
                                };
                                tokio::select! {
                                    _ = tokio::io::copy_bidirectional(&mut a, &mut b) => {}
                                    _ = watch => {}
                                }
                            }
                            drop(slots);
                        });
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
            eps = edge.wake(host, &route, &eps).await;
        } else {
            break;
        }
    }
    let mut r = text(StatusCode::SERVICE_UNAVAILABLE, format!("{host} didn't come up\n"));
    r.headers_mut().insert(header::RETRY_AFTER, edge.retry_after(host).into());
    r
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
    async fn an_apps_own_rate_limit_never_bans() {
        let a = instance("a").await;
        let edge = Edge::new(None, direct());
        let http = HttpSettings { rate: Some("1/m".into()), burst: Some(1), ..Default::default() };
        edge.table
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend([("a.test".to_string(), Route { http, ..route(&[&a], false) }), ("b.test".to_string(), route(&[&a], false))]);
        let addr = edge_on(&edge, Listener::default()).await;
        let mut s = dial(&addr).await;
        assert_eq!(get(&mut s, "a.test", "").await.0, 200);
        for _ in 0..limit::STRIKES + 5 {
            assert_eq!(get(&mut s, "a.test", "").await.0, 429);
        }
        assert_eq!(get(&mut dial(&addr).await, "b.test", "").await.0, 200);
        assert_eq!(edge.bans.local_len(), 0);
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
        let edge = Edge::new(None, Settings { rate: Duration::from_secs(60), burst: 1, ..Default::default() });
        edge.table.write().unwrap_or_else(std::sync::PoisonError::into_inner).insert("a.test".into(), route(&[&a], true));
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

        // A client's own X-Forwarded-For line ahead of the proxy's can't take the strikes.
        let two = "X-Forwarded-For: 198.51.100.77\r\nX-Forwarded-For: 203.0.113.20\r\n";
        for _ in 0..=limit::STRIKES {
            get(&mut s, "a.test", two).await;
        }
        assert!(edge.bans.is_banned("203.0.113.20".parse().unwrap()));
        assert_eq!(get(&mut s, "a.test", "X-Forwarded-For: 198.51.100.77\r\n").await.0, 200);
    }

    #[tokio::test]
    async fn an_ipv6_ban_covers_the_64_that_was_counted() {
        let a = instance("a").await;
        let edge = Edge::new(None, Settings { rate: Duration::from_secs(60), burst: 1, ..Default::default() });
        edge.table.write().unwrap_or_else(std::sync::PoisonError::into_inner).insert("a.test".into(), route(&[&a], false));
        let addr = edge_on(&edge, Listener::default()).await;
        let mut s = dial(&addr).await;
        let from = |i: u32| format!("X-Forwarded-For: 2001:db8:0:1::{i:x}\r\n");
        assert_eq!(get(&mut s, "a.test", &from(1000)).await.0, 200);
        for i in 0..limit::STRIKES {
            assert_eq!(get(&mut s, "a.test", &from(i)).await.0, 429);
        }
        assert_eq!(get(&mut s, "a.test", &from(0xbeef)).await.0, 403);
        assert_eq!(edge.bans.local_len(), 1);
    }

    #[test]
    fn bans_survive_a_reply_that_is_not_a_list() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            use std::io::{BufRead, Write};
            for (n, s) in l.incoming().enumerate() {
                let mut r = std::io::BufReader::new(s.unwrap());
                let mut first = String::new();
                r.read_line(&mut first).unwrap();
                let mut line = String::new();
                while line != "\r\n" {
                    line.clear();
                    r.read_line(&mut line).unwrap();
                }
                // Each refresh asks for /routes, then /bans: an object, then a list cut short.
                let (body, len) = match (first.contains("/bans"), n) {
                    (false, _) => ("{}", 2),
                    (true, n) if n < 2 => ("{}", 2),
                    (true, _) => ("[]", 40),
                };
                let _ = write!(r.get_mut(), "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}");
            }
        });
        let edge = Edge::new(Some(Remote { url, token: "t".into(), ns: None, ca: None }), fast());
        edge.bans.set_remote(vec![ban::parse_net("198.51.100.9").unwrap()]);
        edge.refresh();
        edge.refresh();
        assert!(edge.bans.is_banned("198.51.100.9".parse().unwrap()));
    }

    /// An instance that answers with the request head it received, lowercased.
    async fn echo_head() -> String {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            loop {
                let (s, _) = l.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut r = BufReader::new(s);
                    loop {
                        let mut head = String::new();
                        loop {
                            let mut line = String::new();
                            if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                                return;
                            }
                            if line == "\r\n" {
                                break;
                            }
                            head.push_str(&line.to_ascii_lowercase());
                        }
                        let resp = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{head}", head.len());
                        r.get_mut().write_all(resp.as_bytes()).await.unwrap();
                    }
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn client_address_headers_come_from_the_edge() {
        let a = echo_head().await;
        let spoof = "X-Real-IP: 10.0.0.1\r\nForwarded: for=10.0.0.1\r\nX-Forwarded-Host: admin.internal\r\n\
                     X-Forwarded-Port: 443\r\nTrue-Client-IP: 10.0.0.1\r\nCF-Connecting-IP: 10.0.0.1\r\n";
        for settings in [direct(), fast()] {
            let edge = Edge::new(None, settings);
            edge.table.write().unwrap_or_else(std::sync::PoisonError::into_inner).insert("a.test".into(), route(&[&a], false));
            let addr = edge_on(&edge, Listener::default()).await;
            let (code, head) = get(&mut dial(&addr).await, "a.test:8080", spoof).await;
            assert_eq!(code, 200);
            assert!(!head.contains("10.0.0.1") && !head.contains("admin.internal") && !head.contains("forwarded:"), "{head}");
            assert!(!head.contains("x-forwarded-port") && !head.contains("cf-connecting-ip"), "{head}");
            assert!(head.contains("x-real-ip: 127.0.0.1\r\n") && head.contains("x-forwarded-host: a.test\r\n"), "{head}");
        }
        // Behind a proxy that appends its own X-Forwarded-For line, the client's line can't
        // choose the address, and the app gets both lines in order.
        let edge = Edge::new(None, fast());
        edge.table.write().unwrap_or_else(std::sync::PoisonError::into_inner).insert("a.test".into(), route(&[&a], false));
        let addr = edge_on(&edge, Listener::default()).await;
        let two = "X-Forwarded-For: 6.6.6.6\r\nX-Forwarded-For: 203.0.113.50\r\n";
        let head = get(&mut dial(&addr).await, "a.test", two).await.1;
        assert!(head.contains("x-forwarded-for: 6.6.6.6, 203.0.113.50, 127.0.0.1\r\n"), "{head}");
        assert!(head.contains("x-real-ip: 203.0.113.50\r\n"), "{head}");
    }

    #[tokio::test]
    async fn a_dual_stack_listener_applies_ipv4_bans() {
        let a = instance("a").await;
        let edge = Edge::new(None, direct());
        edge.table.write().unwrap_or_else(std::sync::PoisonError::into_inner).insert("a.test".into(), route(&[&a], false));
        let l = TcpListener::bind("[::]:0").await.unwrap();
        let addr = format!("127.0.0.1:{}", l.local_addr().unwrap().port());
        tokio::spawn(run(l, edge.clone(), Listener::default()));
        assert_eq!(get(&mut dial(&addr).await, "a.test", "").await.1, "a|127.0.0.1");
        edge.bans.set_remote(vec![ban::parse_net("127.0.0.1").unwrap()]);
        let mut s = dial(&addr).await;
        s.get_mut().write_all(b"GET / HTTP/1.1\r\nHost: a.test\r\n\r\n").await.unwrap();
        let mut buf = Vec::new();
        assert_eq!(s.read_to_end(&mut buf).await.unwrap_or(0), 0);
    }

    #[tokio::test]
    async fn proxy_protocol_only_from_the_balancer() {
        let a = instance("a").await;
        let header = |ip: [u8; 4]| {
            let mut h = b"\r\n\r\n\0\r\nQUIT\n\x21\x11\x00\x0c".to_vec();
            h.extend(ip);
            h.extend([127, 0, 0, 1, 0x1f, 0x90, 0, 80]);
            h
        };
        let opts = Listener { public_only: true, proxy_protocol: true };
        for (from, served) in [("127.0.0.1", true), ("192.0.2.0/24", false)] {
            let edge = Edge::new(None, Settings { proxy_protocol_from: vec![ban::parse_net(from).unwrap()], ..direct() });
            edge.table.write().unwrap_or_else(std::sync::PoisonError::into_inner).insert("a.test".into(), route(&[&a], true));
            let addr = edge_on(&edge, opts).await;
            let mut s = dial(&addr).await;
            s.get_mut().write_all(&header([198, 51, 100, 7])).await.unwrap();
            s.get_mut().write_all(b"GET / HTTP/1.1\r\nHost: a.test\r\nConnection: close\r\n\r\n").await.unwrap();
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf).await;
            let body = String::from_utf8_lossy(&buf);
            assert_eq!(body.ends_with("a|198.51.100.7"), served, "{from}: {body}");
            if !served {
                assert!(buf.is_empty(), "{body}");
            }
        }
    }

    /// An instance that waits `delay`, then answers a `size`-byte body; reports when the
    /// edge closes the connection (a write fails, or a read ends) instead of keeping it.
    async fn slow_instance(delay: Duration, size: usize) -> (String, tokio::sync::mpsc::UnboundedReceiver<()>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                let (s, _) = l.accept().await.unwrap();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let mut r = BufReader::new(s);
                    let mut line = String::new();
                    while line != "\r\n" {
                        line.clear();
                        if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                            return;
                        }
                    }
                    tokio::time::sleep(delay).await;
                    let s = r.get_mut();
                    let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {size}\r\n\r\n");
                    let chunk = vec![b'x'; 64 << 10];
                    let mut res = s.write_all(head.as_bytes()).await;
                    let mut left = size;
                    while res.is_ok() && left > 0 {
                        let n = left.min(chunk.len());
                        res = s.write_all(&chunk[..n]).await;
                        left -= n;
                    }
                    let mut b = [0u8; 1];
                    if res.is_err() || matches!(s.read(&mut b).await, Ok(0) | Err(_)) {
                        let _ = tx.send(());
                    }
                });
            }
        });
        (addr, rx)
    }

    #[tokio::test]
    async fn a_response_nobody_reads_is_closed() {
        let (a, mut failed) = slow_instance(Duration::ZERO, 8 << 20).await;
        let edge = Edge::new(None, fast());
        edge.table.write().unwrap_or_else(std::sync::PoisonError::into_inner).insert("a.test".into(), route(&[&a], false));
        let addr = edge_on(&edge, Listener::default()).await;
        let mut s = dial(&addr).await;
        s.get_mut().write_all(b"GET / HTTP/1.1\r\nHost: a.test\r\n\r\n").await.unwrap();
        // The client reads nothing; the edge must let go of both sides.
        let closed = tokio::time::timeout(IDLE_TIMEOUT * 5, failed.recv()).await;
        assert!(matches!(closed, Ok(Some(()))), "the instance kept writing");
        drop(s);
    }

    #[tokio::test]
    async fn keep_alive_outlives_the_header_timeout_but_a_slow_head_does_not() {
        let a = instance("a").await;
        let edge = Edge::new(None, fast());
        edge.table.write().unwrap_or_else(std::sync::PoisonError::into_inner).insert("a.test".into(), route(&[&a], false));
        let addr = edge_on(&edge, Listener::default()).await;
        let mut s = dial(&addr).await;
        assert_eq!(get(&mut s, "a.test", "").await.0, 200);
        tokio::time::sleep(HEADER_TIMEOUT * 2).await;
        assert_eq!(get(&mut s, "a.test", "").await.0, 200);

        let mut s = dial(&addr).await;
        s.get_mut().write_all(b"GET / HTTP/1.1\r\nHost: a.te").await.unwrap();
        let mut buf = Vec::new();
        let closed = tokio::time::timeout(HEADER_TIMEOUT * 2, s.read_to_end(&mut buf)).await;
        assert!(matches!(closed, Ok(Ok(_))), "{closed:?}");
    }

    #[tokio::test]
    async fn no_101_without_a_tunnel_slot() {
        let a = instance("a").await;
        let edge = Edge::new(None, Settings { max_tunnels: 1, ..fast() });
        edge.table.write().unwrap_or_else(std::sync::PoisonError::into_inner).insert("a.test".into(), route(&[&a], false));
        let addr = edge_on(&edge, Listener::default()).await;
        let upgrade = "GET /ws HTTP/1.1\r\nHost: a.test\r\nConnection: Upgrade\r\nUpgrade: echo\r\n\r\n";
        let mut first = dial(&addr).await;
        first.get_mut().write_all(upgrade.as_bytes()).await.unwrap();
        let mut line = String::new();
        first.read_line(&mut line).await.unwrap();
        assert!(line.starts_with("HTTP/1.1 101"), "{line}");
        let mut second = dial(&addr).await;
        second.get_mut().write_all(upgrade.as_bytes()).await.unwrap();
        line.clear();
        second.read_line(&mut line).await.unwrap();
        assert!(line.starts_with("HTTP/1.1 503"), "{line}");
    }

    #[tokio::test]
    async fn shutdown_finishes_requests_in_flight() {
        let (a, _) = slow_instance(Duration::from_millis(300), 2).await;
        let edge = Edge::new(None, fast());
        edge.table.write().unwrap_or_else(std::sync::PoisonError::into_inner).insert("a.test".into(), route(&[&a], false));
        let addr = edge_on(&edge, Listener::default()).await;
        let mut s = dial(&addr).await;
        let req = tokio::spawn(async move { get(&mut s, "a.test", "").await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(edge.shutdown(Duration::from_secs(5)).await);
        assert_eq!(req.await.unwrap(), (200, "xx".to_string()));
        assert!(TcpStream::connect(&addr).await.is_err(), "still accepting");
    }

    #[test]
    fn caps_follow_the_descriptor_limit() {
        assert_eq!(caps(256), (64, 32));
        assert_eq!(caps(1 << 20), (MAX_CONNS, MAX_TUNNELS));
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

    /// A controller whose `/wake` answers `(status, body)` after `delay`; counts the wakes.
    async fn controller(delay: Duration, status: u16, body: String) -> (Remote, Arc<AtomicU64>) {
        let controller = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", controller.local_addr().unwrap());
        let wakes = Arc::new(AtomicU64::new(0));
        let w = wakes.clone();
        tokio::spawn(async move {
            loop {
                let (s, _) = controller.accept().await.unwrap();
                let (w, body) = (w.clone(), body.clone());
                tokio::spawn(async move {
                    let mut r = BufReader::new(s);
                    let mut first = String::new();
                    r.read_line(&mut first).await.unwrap();
                    let mut line = String::new();
                    while line != "\r\n" {
                        line.clear();
                        r.read_line(&mut line).await.unwrap();
                    }
                    if first.contains("/wake") {
                        w.fetch_add(1, Ordering::SeqCst);
                    }
                    tokio::time::sleep(delay).await;
                    let resp = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                    let _ = r.get_mut().write_all(resp.as_bytes()).await;
                });
            }
        });
        (Remote { url, token: "t".into(), ns: None, ca: None }, wakes)
    }

    fn sleeping(edge: &Arc<Edge>, eps: &[&str]) {
        edge.table
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert("a.test".into(), Route { app: "a".into(), sleeping: true, ..route(eps, false) });
    }

    #[tokio::test]
    async fn a_burst_during_a_failed_wake_asks_the_controller_once() {
        // A slow failure, so the burst queues behind it.
        let (remote, wakes) = controller(Duration::from_millis(300), 500, r#"{"error":"build failed"}"#.into()).await;
        let edge = Edge::new(Some(remote), fast());
        sleeping(&edge, &[]);
        let addr = edge_on(&edge, Listener::default()).await;
        // The first client gives up while the wake runs; the wake and its backoff go on.
        let mut quitter = dial(&addr).await;
        quitter.get_mut().write_all(b"GET / HTTP/1.1\r\nHost: a.test\r\n\r\n").await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(quitter);
        let burst: Vec<_> = (0..8)
            .map(|_| {
                let addr = addr.clone();
                tokio::spawn(async move { get(&mut dial(&addr).await, "a.test", "").await.0 })
            })
            .collect();
        for r in burst {
            assert_eq!(r.await.unwrap(), 503);
        }
        assert_eq!(get(&mut dial(&addr).await, "a.test", "").await.0, 503);
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_slow_wake_answers_503_at_the_bound() {
        let (remote, wakes) = controller(Duration::from_secs(3), 200, r#"{"endpoints":[]}"#.into()).await;
        let edge = Edge::new(Some(remote), Settings { wake_wait: Duration::from_millis(300), ..fast() });
        sleeping(&edge, &[]);
        let addr = edge_on(&edge, Listener::default()).await;
        let mut s = dial(&addr).await;
        let t = Instant::now();
        s.get_mut().write_all(b"GET / HTTP/1.1\r\nHost: a.test\r\n\r\n").await.unwrap();
        let mut head = String::new();
        while !head.ends_with("\r\n\r\n") {
            let mut line = String::new();
            s.read_line(&mut line).await.unwrap();
            head.push_str(&line.to_ascii_lowercase());
        }
        assert!(head.starts_with("http/1.1 503") && head.contains("retry-after: 5"), "{head}");
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
        // The wake itself goes on: a second request joins it rather than starting another.
        assert_eq!(get(&mut dial(&addr).await, "a.test", "").await.0, 503);
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_app_that_slept_since_the_last_refresh_is_woken() {
        let live = instance("live").await;
        let dead = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().to_string();
        let (remote, wakes) = controller(Duration::ZERO, 200, format!(r#"{{"endpoints":["{live}"]}}"#)).await;
        let edge = Edge::new(Some(remote), fast());
        sleeping(&edge, &[&dead]);
        let addr = edge_on(&edge, Listener::default()).await;
        let (code, body) = get(&mut dial(&addr).await, "a.test", "").await;
        assert_eq!((code, app(&body)), (200, "live"));
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
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
