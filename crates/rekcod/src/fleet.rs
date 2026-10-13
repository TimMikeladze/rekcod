//! Self-hosted fleet and app hosting (docs/platform.md, tracks 2 and 3).
//!
//! - `rekcod controller serve`: the sandbox API for many Macs, plus apps. State in Postgres.
//! - `rekcod node join|serve`: runs on each Mac; serves `sandbox_api` on its address with a
//!   node secret, heartbeats capacity, and deploys app instances into sandboxes.
//! - `rekcod edge serve`: routes `Host: <app>.<domain>` to live instances.
//!
//! Everything speaks plain HTTP/1.1 with bearer tokens and `Connection: close`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::paths::Paths;
use crate::sandbox;
use crate::sandbox_api::{read_request, respond};

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn secret() -> Result<String> {
    Ok(format!("{}{}{}{}", sandbox::new_id()?, sandbox::new_id()?, sandbox::new_id()?, sandbox::new_id()?))
}

// ------------------------------------------------------------- HTTP client

/// `http://host:port` → `host:port`.
fn authority(base: &str) -> Result<String> {
    let rest = base.strip_prefix("http://").ok_or_else(|| anyhow!("{base}: only http:// URLs are supported"))?;
    Ok(rest.trim_end_matches('/').to_string())
}

pub fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Sends a request; returns the status and a reader at the body (read to EOF).
pub fn send(base: &str, method: &str, path: &str, token: Option<&str>, body: &[u8]) -> Result<(u16, BufReader<TcpStream>)> {
    let addr = authority(base)?;
    let sock = addr.to_socket_addrs()?.next().ok_or_else(|| anyhow!("can't resolve {addr}"))?;
    let mut s = TcpStream::connect_timeout(&sock, Duration::from_secs(5)).with_context(|| format!("connect {base}"))?;
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    s.write_all(body)?;
    let mut r = BufReader::new(s);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let code: u16 = line.split_whitespace().nth(1).and_then(|c| c.parse().ok()).ok_or_else(|| anyhow!("bad response from {base}"))?;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
    }
    Ok((code, r))
}

/// JSON request; an error status becomes an error carrying the server's message.
pub fn call(base: &str, method: &str, path: &str, token: Option<&str>, body: &Value) -> Result<Value> {
    let data = if body.is_null() { vec![] } else { serde_json::to_vec(body)? };
    let (code, mut r) = send(base, method, path, token, &data)?;
    let mut buf = Vec::new();
    r.read_to_end(&mut buf)?;
    let v: Value = serde_json::from_slice(&buf).unwrap_or_else(|_| json!({"text": String::from_utf8_lossy(&buf)}));
    if code >= 400 {
        bail!("{} ({code})", v["error"].as_str().map(String::from).unwrap_or_else(|| v.to_string()));
    }
    Ok(v)
}

// ------------------------------------------------------------- remotes

#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct Remotes {
    pub default: Option<String>,
    pub remotes: std::collections::BTreeMap<String, Remote>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct Remote {
    pub url: String,
    pub token: String,
}

fn remotes_file(paths: &Paths) -> PathBuf {
    paths.home.join("remotes.json")
}

pub fn remotes(paths: &Paths) -> Remotes {
    std::fs::read(remotes_file(paths)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn remote(paths: &Paths, name: Option<&str>) -> Result<Remote> {
    let r = remotes(paths);
    let name = name.map(String::from).or(r.default.clone()).ok_or_else(|| anyhow!("no remote; `rekcod remote add NAME URL --token T`"))?;
    r.remotes.get(&name).cloned().ok_or_else(|| anyhow!("no remote {name}"))
}

pub fn remote_add(paths: &Paths, name: &str, url: &str, token: &str) -> Result<()> {
    authority(url)?;
    let mut r = remotes(paths);
    r.remotes.insert(name.into(), Remote { url: url.trim_end_matches('/').into(), token: token.into() });
    if r.default.is_none() {
        r.default = Some(name.into());
    }
    std::fs::write(remotes_file(paths), serde_json::to_vec_pretty(&r)?)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(remotes_file(paths), std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

pub fn remote_rm(paths: &Paths, name: &str) -> Result<()> {
    let mut r = remotes(paths);
    if r.remotes.remove(name).is_none() {
        bail!("no remote {name}");
    }
    if r.default.as_deref() == Some(name) {
        r.default = r.remotes.keys().next().cloned();
    }
    std::fs::write(remotes_file(paths), serde_json::to_vec_pretty(&r)?)?;
    Ok(())
}

pub fn remote_list(paths: &Paths) {
    let r = remotes(paths);
    for (name, rem) in &r.remotes {
        let star = if r.default.as_deref() == Some(name) { "*" } else { " " };
        println!("{star} {name:<12} {}", rem.url);
    }
}

/// The git remote URL and HEAD commit of `dir`, for remote creates and deploys.
pub fn git_origin(dir: &Path) -> Result<(String, String)> {
    let run = |args: &[&str]| -> Result<String> {
        let o = Command::new("git").arg("-C").arg(dir).args(args).output()?;
        if !o.status.success() {
            bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    Ok((run(&["remote", "get-url", "origin"])?, run(&["rev-parse", "HEAD"])?))
}

// ------------------------------------------------------------- node side

/// Clones `url` at `rev` (branch, tag or commit) into `~/.rekcod/remote-ws/<id>`.
pub fn clone(paths: &Paths, url: &str, rev: &str) -> Result<PathBuf> {
    let dir = paths.home.join("remote-ws").join(sandbox::new_id()?);
    std::fs::create_dir_all(&dir)?;
    let git = |args: &[&str]| -> Result<()> {
        let o = Command::new("git").arg("-C").arg(&dir).args(args).stdin(Stdio::null()).output()?;
        if !o.status.success() {
            bail!("git {}: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&o.stderr).trim());
        }
        Ok(())
    };
    git(&["init", "-q"])?;
    git(&["fetch", "-q", "--depth", "1", url, rev])?;
    git(&["checkout", "-q", "FETCH_HEAD"])?;
    Ok(dir.canonicalize()?)
}

/// The address this node's relays listen on.
static NODE_IP: OnceLock<IpAddr> = OnceLock::new();

fn free_port() -> Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn pipe(mut a: TcpStream, mut b: TcpStream) {
    let (Ok(mut a2), Ok(mut b2)) = (a.try_clone(), b.try_clone()) else { return };
    let t = std::thread::spawn(move || {
        let _ = std::io::copy(&mut a2, &mut b2);
        let _ = b2.shutdown(std::net::Shutdown::Write);
    });
    let _ = std::io::copy(&mut b, &mut a);
    let _ = a.shutdown(std::net::Shutdown::Write);
    let _ = t.join();
}

/// Forwards `ip:ext` (0 picks one) to the Mac's `127.0.0.1:port`; returns the bound port.
fn start_relay(ip: IpAddr, ext: u16, port: u16) -> Result<u16> {
    let l = TcpListener::bind((ip, ext))?;
    let bound = l.local_addr()?.port();
    std::thread::spawn(move || {
        for c in l.incoming().flatten() {
            std::thread::spawn(move || {
                if let Ok(up) = TcpStream::connect(("127.0.0.1", port)) {
                    pipe(c, up);
                }
            });
        }
    });
    Ok(bound)
}

/// `rekcod.toml`: `[app]`, `[resources]` and `[env]` with `key = value` lines.
pub fn parse_toml(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.split(" #").next().unwrap_or("").trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = s.trim().to_string();
        } else if let Some((k, v)) = line.split_once('=') {
            out.insert(format!("{section}.{}", k.trim()), v.trim().trim_matches('"').to_string());
        }
    }
    out
}

/// A Dockerfile for a repo without one: the `rekcod x` image plus the start command.
pub fn generated_dockerfile(dir: &Path, start: &str) -> Result<String> {
    let cmd: Vec<String> = start.split_whitespace().map(String::from).collect();
    let plan = crate::xrun::plan(dir, &cmd).ok_or_else(|| anyhow!("no Dockerfile, and can't tell which runtime `{start}` needs"))?;
    let install = if dir.join("package-lock.json").exists() {
        "RUN npm ci\n"
    } else if dir.join("package.json").exists() {
        "RUN npm install\n"
    } else if dir.join("requirements.txt").exists() {
        "RUN pip install --no-cache-dir -r requirements.txt\n"
    } else {
        ""
    };
    Ok(format!("FROM {}\nWORKDIR /app\nCOPY . /app\n{install}CMD {}\n", plan.image, serde_json::to_string(&cmd)?))
}

/// Node op: builds `git` in a new sandbox and runs it; returns `{sandbox, endpoint}`.
pub fn deploy_instance(paths: &Paths, a: &Value) -> Result<Value> {
    let app = a["app"].as_str().context("missing app")?;
    let release = a["release"].as_i64().context("missing release")?;
    let dir = clone(paths, a["git"]["url"].as_str().context("missing git.url")?, a["git"]["ref"].as_str().unwrap_or("HEAD"))?;
    // The commit's own rekcod.toml fills in whatever the deploy didn't set.
    let toml = parse_toml(&std::fs::read_to_string(dir.join("rekcod.toml")).unwrap_or_default());
    let port = a["port"].as_u64().filter(|p| *p > 0).or_else(|| toml.get("app.port").and_then(|p| p.parse().ok())).unwrap_or(3000) as u16;
    if !dir.join("Dockerfile").exists() {
        let start = a["start"].as_str().or(toml.get("app.start").map(String::as_str));
        let start = start.ok_or_else(|| anyhow!("no Dockerfile and no start command in rekcod.toml"))?;
        std::fs::write(dir.join("Dockerfile"), generated_dockerfile(&dir, start)?)?;
    }
    let mut env: serde_json::Map<String, Value> =
        toml.iter().filter_map(|(k, v)| k.strip_prefix("env.").map(|k| (k.to_string(), json!(v)))).collect();
    for (k, v) in a["env"].as_object().into_iter().flatten() {
        env.insert(k.clone(), v.clone());
    }
    let o = sandbox::CreateOptions {
        dir: Some(dir.clone()),
        name: Some(format!("{app}-r{release}-{}", &sandbox::new_id()?[..4])),
        mount: true,
        image: Some("alpine:3.22".into()),
        cpus: a["cpus"].as_u64().unwrap_or(1) as usize,
        memory_gib: a["memory_gib"].as_u64().unwrap_or(1),
        allow_hosts: vec!["*".into()],
        ttl_secs: 0,
        ..Default::default()
    };
    let meta = sandbox::create(paths, o)?;
    let result = (|| -> Result<Value> {
        let image = format!("rekcod-app-{app}:{release}");
        let (mut d, _) = sandbox::docker_cmd(paths, &meta.id)?;
        let out = d.args(["build", "-q", "-t", &image]).arg(&dir).stdin(Stdio::null()).output()?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            bail!("build failed: {}", err.lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"));
        }
        let hp = free_port()?;
        let (mut d, _) = sandbox::docker_cmd(paths, &meta.id)?;
        d.args(["run", "-d", "--name", "app", "--restart", "unless-stopped", "-p", &format!("127.0.0.1:{hp}:{port}")]);
        for (k, v) in &env {
            d.args(["-e", &format!("{k}={}", v.as_str().unwrap_or(""))]);
        }
        let out = d.arg(&image).stdin(Stdio::null()).output()?;
        if !out.status.success() {
            bail!("run failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        // The sandbox daemon mirrors the published port onto the Mac's loopback.
        let t0 = Instant::now();
        while TcpStream::connect(("127.0.0.1", hp)).is_err() {
            if t0.elapsed() > Duration::from_secs(20) {
                bail!("port {hp} never appeared on the Mac");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let ip = *NODE_IP.get().unwrap_or(&IpAddr::from([127, 0, 0, 1]));
        let ext = start_relay(ip, 0, hp)?;
        std::fs::write(sandbox::home(paths, &meta.id)?.join("instance.json"), json!({"hp": hp, "ext": ext}).to_string())?;
        Ok(json!({"sandbox": meta.id, "endpoint": format!("{ip}:{ext}"), "health": toml.get("app.health")}))
    })();
    if result.is_err() {
        let _ = sandbox::rm(paths, std::slice::from_ref(&meta.id), false);
    }
    result
}

pub fn instance_logs(paths: &Paths, id: &str) -> Result<String> {
    let (mut d, _) = sandbox::docker_cmd(paths, id)?;
    let o = d.args(["logs", "--tail", "200", "app"]).output()?;
    Ok(format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
}

#[derive(serde::Serialize, serde::Deserialize)]
struct NodeConfig {
    controller: String,
    id: String,
    secret: String,
    listen: String,
    advertise: String,
}

pub fn node_join(paths: &Paths, controller: &str, join_token: &str, listen: &str, advertise: Option<&str>, install: bool) -> Result<()> {
    let addr: SocketAddr = listen.parse().context("--listen takes IP:PORT")?;
    let advertise = match advertise {
        Some(a) => a.trim_end_matches('/').to_string(),
        None if addr.ip().is_unspecified() => bail!("--listen {listen} needs --advertise http://<this Mac's address>:{}", addr.port()),
        None => format!("http://{addr}"),
    };
    let host = crate::cli::sysctl_u64("hw.memsize").unwrap_or(0) >> 20;
    let r = call(controller, "POST", "/nodes/register", None, &json!({"join_token": join_token, "url": advertise, "mem_total_mib": host}))?;
    let cfg = NodeConfig {
        controller: controller.trim_end_matches('/').into(),
        id: r["id"].as_str().context("no node id")?.into(),
        secret: r["secret"].as_str().context("no node secret")?.into(),
        listen: listen.into(),
        advertise,
    };
    std::fs::write(paths.home.join("node.json"), serde_json::to_vec_pretty(&cfg)?)?;
    eprintln!("rekcod: joined {} as node {}", cfg.controller, cfg.id);
    if install {
        return crate::service::install(paths, "node", &["node".into(), "serve".into()]);
    }
    node_serve(paths)
}

pub fn node_serve(paths: &Paths) -> Result<()> {
    let cfg: NodeConfig = serde_json::from_slice(&std::fs::read(paths.home.join("node.json")).context("not joined; `rekcod node join`")?)?;
    let addr: SocketAddr = cfg.listen.parse()?;
    let ip = if addr.ip().is_unspecified() { IpAddr::from([0, 0, 0, 0]) } else { addr.ip() };
    let _ = NODE_IP.set(ip);
    // Relays don't survive a restart; bring back each instance's on its old port.
    for l in sandbox::list(paths) {
        let f = sandbox::home(paths, &l.meta.id).map(|h| h.join("instance.json")).ok();
        if let Some(v) = f.and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
            let (hp, ext) = (v["hp"].as_u64().unwrap_or(0) as u16, v["ext"].as_u64().unwrap_or(0) as u16);
            if let Err(e) = start_relay(ip, ext, hp) {
                eprintln!("rekcod: relay for {}: {e:#}", l.meta.id);
            }
        }
    }
    let hb_paths = paths.clone();
    let (controller, id, secret) = (cfg.controller.clone(), cfg.id.clone(), cfg.secret.clone());
    std::thread::spawn(move || {
        loop {
            let sbs: Vec<String> = sandbox::list(&hb_paths).into_iter().map(|l| l.meta.id).collect();
            let macos_free = if crate::macos::ready(&hb_paths, "default") {
                crate::macos::MAX_RUNNING.saturating_sub(sandbox::macos_running(&hb_paths))
            } else {
                0
            };
            let body = json!({"mem_free_mib": sandbox::free_mib(&hb_paths), "macos_free": macos_free, "sandboxes": sbs});
            if let Err(e) = call(&controller, "POST", &format!("/nodes/{id}/heartbeat"), Some(&secret), &body) {
                eprintln!("rekcod: heartbeat: {e:#}");
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    });
    let l = TcpListener::bind(addr).with_context(|| format!("bind {addr}"))?;
    eprintln!("rekcod: node {} serving sandboxes on {} ({})", cfg.id, cfg.listen, cfg.advertise);
    for s in l.incoming().flatten() {
        let (p, t) = (paths.clone(), cfg.secret.clone());
        std::thread::spawn(move || crate::sandbox_api::handle(&p, s, Some(&t)));
    }
    Ok(())
}

// ------------------------------------------------------------- controller

const SCHEMA: &str = "
create table if not exists config (key text primary key, value text not null);
create table if not exists nodes (id text primary key, url text not null, secret text not null,
  mem_total bigint not null default 0, mem_free bigint not null default 0, last_seen bigint not null default 0,
  state text not null default 'ready');
create table if not exists tokens (token text primary key, name text not null, admin boolean not null default false,
  max_sandboxes int not null default 10);
create table if not exists sandboxes (id text primary key, node text not null, owner text not null,
  state text not null default 'running', created bigint not null);
create table if not exists apps (name text primary key, owner text not null, git_url text not null, git_ref text not null,
  port int not null, instances int not null default 1, cpus int not null default 1, memory_gib int not null default 1,
  health text not null default '/', start text, env text not null default '{}', secrets text not null default '{}',
  live_release int, preview_of text, created bigint not null);
create table if not exists releases (app text not null, n int not null, commit text not null, state text not null,
  created bigint not null, primary key (app, n));
create table if not exists instances (app text not null, release int not null, sandbox text primary key, node text not null,
  endpoint text not null, state text not null);
alter table nodes add column if not exists macos_free int not null default 0;
alter table apps add column if not exists public boolean not null default false;
create table if not exists audit (ts bigint not null, who text not null, action text not null, target text not null, ok boolean not null);
";

struct Controller {
    db: Mutex<postgres::Client>,
    domain: String,
    drain_secs: u64,
}

struct Caller {
    name: String,
    admin: bool,
    max: i32,
}

impl Controller {
    fn config(&self, key: &str) -> Result<Option<String>> {
        Ok(self.db.lock().unwrap().query_opt("select value from config where key = $1", &[&key])?.map(|r| r.get(0)))
    }

    fn caller(&self, token: &str) -> Result<Option<Caller>> {
        let row = self.db.lock().unwrap().query_opt("select name, admin, max_sandboxes from tokens where token = $1", &[&token])?;
        Ok(row.map(|r| Caller { name: r.get(0), admin: r.get(1), max: r.get(2) }))
    }

    fn audit(&self, who: &str, action: &str, target: &str, ok: bool) {
        let _ = self.db.lock().unwrap().execute("insert into audit values ($1, $2, $3, $4, $5)", &[&now(), &who, &action, &target, &ok]);
    }

    /// Ready nodes with at least `mib` free, most free first.
    fn place(&self, mib: i64, macos: bool) -> Result<Vec<(String, String, String)>> {
        let rows = self.db.lock().unwrap().query(
            "select id, url, secret from nodes where state = 'ready' and last_seen > $1 and mem_free >= $2
             and (not $3 or macos_free > 0) order by mem_free desc",
            &[&(now() - 15), &mib, &macos],
        )?;
        Ok(rows.iter().map(|r| (r.get(0), r.get(1), r.get(2))).collect())
    }

    fn node(&self, id: &str) -> Result<(String, String, String)> {
        let r = self.db.lock().unwrap().query_opt("select url, secret, state from nodes where id = $1", &[&id])?;
        r.map(|r| (r.get(0), r.get(1), r.get(2))).ok_or_else(|| anyhow!("no node {id}"))
    }

    /// The node holding sandbox `id`, if `who` may use it.
    fn sandbox_node(&self, id: &str, who: &Caller) -> Result<(String, String, String)> {
        let r = self.db.lock().unwrap().query_opt("select node, owner, state from sandboxes where id = $1", &[&id])?;
        let Some(r) = r else { bail!(sandbox::NoSandbox(id.into())) };
        let (node, owner, state): (String, String, String) = (r.get(0), r.get(1), r.get(2));
        if owner != who.name && !who.admin {
            bail!(sandbox::NoSandbox(id.into()));
        }
        if state == "lost" {
            bail!("sandbox {id} is on node {node}, which stopped answering");
        }
        self.node(&node)
    }
}

/// Marks nodes silent for 15 s as lost (with their sandboxes) and deletes week-old previews.
fn watchdog(c: Arc<Controller>) {
    loop {
        std::thread::sleep(Duration::from_secs(5));
        let lost = {
            let mut db = c.db.lock().unwrap();
            let lost = db
                .query("update nodes set state = 'lost' where state <> 'lost' and last_seen < $1 returning id", &[&(now() - 15)])
                .unwrap_or_default();
            for r in &lost {
                let id: String = r.get(0);
                let _ = db.execute("update sandboxes set state = 'lost' where node = $1 and state = 'running'", &[&id]);
                let _ = db.execute("update instances set state = 'lost' where node = $1 and state = 'live'", &[&id]);
            }
            lost.len()
        };
        if lost > 0 {
            eprintln!("rekcod: {lost} node(s) lost");
        }
        let old: Vec<String> =
            c.db.lock()
                .unwrap()
                .query("select name from apps where preview_of is not null and created < $1", &[&(now() - 7 * 86400)])
                .map(|rows| rows.iter().map(|r| r.get(0)).collect())
                .unwrap_or_default();
        for name in old {
            let _ = destroy_app(&c, &name);
        }
    }
}

/// Forwards the request to a node and streams its response back unchanged.
fn proxy(node_url: &str, secret: &str, req: &crate::sandbox_api::Request, w: &mut impl Write) -> Result<()> {
    let q: Vec<String> = req.query.iter().map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v))).collect();
    let path = if q.is_empty() { req.path.clone() } else { format!("{}?{}", req.path, q.join("&")) };
    let addr = authority(node_url)?;
    let mut s = TcpStream::connect(&addr).with_context(|| format!("connect node {node_url}"))?;
    write!(
        s,
        "{} {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {secret}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        req.method,
        req.body.len()
    )?;
    s.write_all(&req.body)?;
    std::io::copy(&mut s, w)?;
    Ok(())
}

fn json_reply(w: &mut impl Write, r: Result<Value>) -> Result<()> {
    match r {
        Ok(v) => respond(w, 200, "application/json", v.to_string().as_bytes()),
        Err(e) => {
            let code = if e.downcast_ref::<sandbox::NoSandbox>().is_some() || e.to_string().starts_with("no ") { 404 } else { 400 };
            respond(w, code, "application/json", json!({"error": format!("{e:#}")}).to_string().as_bytes())
        }
    }
}

fn controller_handle(c: &Arc<Controller>, stream: TcpStream) -> Result<()> {
    let mut r = BufReader::new(stream);
    let req = read_request(&mut r)?;
    let mut w = r.into_inner();
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(json!({}));
    let seg: Vec<&str> = req.path.trim_matches('/').split('/').collect();
    let bearer =
        req.headers.iter().find(|(k, _)| k == "authorization").and_then(|(_, v)| v.strip_prefix("Bearer ")).unwrap_or("").to_string();

    // Unauthenticated by user token: node registration, heartbeats, GitHub webhooks.
    match (req.method.as_str(), seg.as_slice()) {
        ("POST", ["nodes", "register"]) => {
            let r = (|| {
                if Some(body["join_token"].as_str().unwrap_or("")) != c.config("join_token")?.as_deref() {
                    bail!("bad join token");
                }
                let (id, sec) = (format!("n-{}", sandbox::new_id()?), secret()?);
                let url = body["url"].as_str().context("missing url")?;
                let total = body["mem_total_mib"].as_i64().unwrap_or(0);
                c.db.lock().unwrap().execute(
                    "insert into nodes (id, url, secret, mem_total, last_seen) values ($1, $2, $3, $4, $5)",
                    &[&id, &url, &sec, &total, &now()],
                )?;
                c.audit("node", "register", &id, true);
                Ok(json!({"id": id, "secret": sec}))
            })();
            return json_reply(&mut w, r);
        }
        ("POST", ["nodes", id, "heartbeat"]) => {
            let r = (|| {
                let (_, sec, state) = c.node(id)?;
                if sec != bearer {
                    bail!("bad node secret");
                }
                let free = body["mem_free_mib"].as_i64().unwrap_or(0);
                let macos_free = body["macos_free"].as_i64().unwrap_or(0) as i32;
                let next = if state == "lost" { "ready".to_string() } else { state };
                c.db.lock().unwrap().execute(
                    "update nodes set mem_free = $2, last_seen = $3, state = $4, macos_free = $5 where id = $1",
                    &[id, &free, &now(), &next, &macos_free],
                )?;
                Ok(json!({}))
            })();
            return json_reply(&mut w, r);
        }
        ("POST", ["webhooks", "github"]) => return json_reply(&mut w, github_webhook(c, &req)),
        _ => {}
    }

    let Some(who) = c.caller(&bearer)? else {
        return respond(&mut w, 401, "application/json", br#"{"error":"bad token"}"#);
    };
    let admin_only = || -> Result<()> { if who.admin { Ok(()) } else { bail!("admin token required") } };
    let r: Result<Value> = match (req.method.as_str(), seg.as_slice()) {
        ("GET", ["nodes"]) => admin_only().and_then(|_| {
            let rows = c.db.lock().unwrap().query("select id, url, mem_total, mem_free, last_seen, state from nodes order by id", &[])?;
            Ok(json!(
                rows.iter()
                    .map(|r| json!({
                        "id": r.get::<_, String>(0), "url": r.get::<_, String>(1), "mem_total_mib": r.get::<_, i64>(2),
                        "mem_free_mib": r.get::<_, i64>(3), "last_seen": r.get::<_, i64>(4), "state": r.get::<_, String>(5)
                    }))
                    .collect::<Vec<_>>()
            ))
        }),
        ("POST", ["nodes", id, "drain"]) => admin_only().and_then(|_| {
            let n = c.db.lock().unwrap().execute("update nodes set state = 'drain' where id = $1", &[id])?;
            if n == 0 {
                bail!("no node {id}");
            }
            c.audit(&who.name, "drain", id, true);
            Ok(json!({"drained": id}))
        }),
        ("POST", ["tokens"]) => admin_only().and_then(|_| {
            let (t, name) = (secret()?, body["name"].as_str().context("missing name")?.to_string());
            let max = body["max_sandboxes"].as_i64().unwrap_or(10) as i32;
            let admin = body["admin"].as_bool().unwrap_or(false);
            c.db.lock().unwrap().execute("insert into tokens values ($1, $2, $3, $4)", &[&t, &name, &admin, &max])?;
            c.audit(&who.name, "token", &name, true);
            Ok(json!({"token": t, "name": name}))
        }),
        ("GET", ["routes"]) => admin_only().and_then(|_| routes(c)),
        ("POST", ["sandboxes"]) => create_sandbox(c, &who, &req, &mut w).map(|_| Value::Null),
        ("GET", ["sandboxes"]) => {
            let rows = c.db.lock().unwrap().query(
                "select id, node, owner, state, created from sandboxes where state <> 'removed' and (owner = $1 or $2) order by created",
                &[&who.name, &who.admin],
            )?;
            Ok(json!(
                rows.iter()
                    .map(|r| json!({
                        "id": r.get::<_, String>(0), "node": r.get::<_, String>(1), "owner": r.get::<_, String>(2),
                        "state": r.get::<_, String>(3), "created": r.get::<_, i64>(4)
                    }))
                    .collect::<Vec<_>>()
            ))
        }
        ("POST", ["sandboxes", _, "apply"]) => Err(anyhow!("apply isn't offered remotely; `git apply` the patch from diff")),
        (m, ["sandboxes", id, rest @ ..]) => {
            let fork = m == "POST" && rest == ["fork"];
            let delete = m == "DELETE" && rest.is_empty();
            match c.sandbox_node(id, &who) {
                Err(e) => Err(e),
                Ok((url, sec, _)) if fork => (|| {
                    let mut v = call(&url, "POST", &req.path, Some(&sec), &body)?;
                    let child = v["id"].as_str().context("node returned no id")?.to_string();
                    let node: String = c.db.lock().unwrap().query_one("select node from sandboxes where id = $1", &[id])?.get(0);
                    c.db.lock()
                        .unwrap()
                        .execute("insert into sandboxes values ($1, $2, $3, 'running', $4)", &[&child, &node, &who.name, &now()])?;
                    c.audit(&who.name, "fork", &child, true);
                    v["node"] = json!(node);
                    Ok(v)
                })(),
                Ok((url, sec, _)) => {
                    proxy(&url, &sec, &req, &mut w)?;
                    if delete {
                        c.db.lock().unwrap().execute("update sandboxes set state = 'removed' where id = $1", &[id])?;
                    }
                    c.audit(&who.name, &format!("{m} {}", rest.join("/")), id, true);
                    return Ok(());
                }
            }
        }
        _ => apps_route(c, &who, &req.method, &seg, &body),
    };
    if matches!(r, Ok(Value::Null)) {
        return Ok(());
    }
    json_reply(&mut w, r)
}

fn create_sandbox(c: &Arc<Controller>, who: &Caller, req: &crate::sandbox_api::Request, w: &mut impl Write) -> Result<()> {
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(json!({}));
    if body["git"]["url"].as_str().is_none() {
        return respond(w, 400, "application/json", br#"{"error":"remote sandboxes need git.url (and git.ref)"}"#);
    }
    let count: i64 =
        c.db.lock().unwrap().query_one("select count(*) from sandboxes where owner = $1 and state = 'running'", &[&who.name])?.get(0);
    if count >= who.max as i64 {
        return respond(
            w,
            400,
            "application/json",
            json!({"error": format!("quota: {count} of {} sandboxes in use", who.max)}).to_string().as_bytes(),
        );
    }
    let macos = body["os"].as_str() == Some("macos");
    let mib = body["memory_gib"].as_i64().unwrap_or(2).max(if macos { 8 } else { 0 }) * 1024;
    let Some((node, url, sec)) = c.place(mib, macos)?.into_iter().next() else {
        let what = if macos { format!("a free macOS slot and {mib} MiB") } else { format!("{mib} MiB free") };
        return respond(w, 400, "application/json", json!({"error": format!("no node has {what}")}).to_string().as_bytes());
    };
    let mut v = match call(&url, "POST", "/sandboxes", Some(&sec), &body) {
        Ok(v) => v,
        Err(e) => return respond(w, 400, "application/json", json!({"error": format!("node {node}: {e:#}")}).to_string().as_bytes()),
    };
    let id = v["id"].as_str().unwrap_or("").to_string();
    c.db.lock().unwrap().execute("insert into sandboxes values ($1, $2, $3, 'running', $4)", &[&id, &node, &who.name, &now()])?;
    c.audit(&who.name, "create", &id, true);
    v["node"] = json!(node);
    respond(w, 201, "application/json", v.to_string().as_bytes())
}

// ------------------------------------------------------------- apps

/// `{host: {"public": bool, "endpoints": [...]}}` for every app with live instances.
fn routes(c: &Controller) -> Result<Value> {
    let rows = c.db.lock().unwrap().query(
        "select i.app, i.endpoint, a.public from instances i join apps a on a.name = i.app where i.state = 'live' order by i.app",
        &[],
    )?;
    let mut out: std::collections::BTreeMap<String, Route> = Default::default();
    for r in rows {
        let app: String = r.get(0);
        let route = out.entry(format!("{app}.{}", c.domain)).or_default();
        route.public = r.get(2);
        route.endpoints.push(r.get(1));
    }
    Ok(serde_json::to_value(out)?)
}

#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
pub struct Route {
    pub public: bool,
    pub endpoints: Vec<String>,
}

/// Resolves a branch or tag to a commit with `git ls-remote`; a commit passes through.
fn resolve(url: &str, rev: &str) -> Result<String> {
    if rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(rev.into());
    }
    let o = Command::new("git").args(["ls-remote", url, rev]).output()?;
    let text = String::from_utf8_lossy(&o.stdout);
    text.split_whitespace().next().map(String::from).ok_or_else(|| anyhow!("{url} has no ref {rev}"))
}

/// GET with a 2 s timeout; true on 2xx/3xx.
fn healthy(endpoint: &str, path: &str) -> bool {
    let Ok(addr) = endpoint.parse::<SocketAddr>() else { return false };
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_secs(2)) else { return false };
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    if write!(s, "GET {path} HTTP/1.1\r\nHost: health\r\nConnection: close\r\n\r\n").is_err() {
        return false;
    }
    let mut line = String::new();
    let _ = BufReader::new(s).read_line(&mut line);
    line.split_whitespace().nth(1).and_then(|c| c.parse::<u16>().ok()).is_some_and(|c| (200..400).contains(&c))
}

struct App {
    name: String,
    git_url: String,
    git_ref: String,
    port: i32,
    instances: i32,
    cpus: i32,
    memory_gib: i32,
    health: String,
    start: Option<String>,
    env: Value,
    secrets: Value,
}

fn load_app(c: &Controller, name: &str, who: &Caller) -> Result<App> {
    let r = c.db.lock().unwrap().query_opt(
        "select name, git_url, git_ref, port, instances, cpus, memory_gib, health, start, env, secrets, owner from apps where name = $1",
        &[&name],
    )?;
    let r = r.ok_or_else(|| anyhow!("no app {name}"))?;
    let owner: String = r.get(11);
    if owner != who.name && !who.admin {
        bail!("no app {name}");
    }
    Ok(App {
        name: r.get(0),
        git_url: r.get(1),
        git_ref: r.get(2),
        port: r.get(3),
        instances: r.get(4),
        cpus: r.get(5),
        memory_gib: r.get(6),
        health: r.get(7),
        start: r.get(8),
        env: serde_json::from_str(&r.get::<_, String>(9)).unwrap_or(json!({})),
        secrets: serde_json::from_str(&r.get::<_, String>(10)).unwrap_or(json!({})),
    })
}

fn delete_instance(c: &Controller, node: &str, sandbox_id: &str) {
    if let Ok((url, sec, _)) = c.node(node) {
        let _ = call(&url, "DELETE", &format!("/sandboxes/{sandbox_id}"), Some(&sec), &Value::Null);
    }
    let mut db = c.db.lock().unwrap();
    let _ = db.execute("update instances set state = 'removed' where sandbox = $1", &[&sandbox_id]);
    let _ = db.execute("update sandboxes set state = 'removed' where id = $1", &[&sandbox_id]);
}

/// New release from `commit` (or the app's ref): start instances on distinct nodes where
/// possible, wait for health, switch routes, drain the old ones.
fn deploy(c: &Arc<Controller>, app: &App, commit: Option<String>, who: &str) -> Result<Value> {
    let commit = match commit {
        Some(c) => c,
        None => resolve(&app.git_url, &app.git_ref)?,
    };
    let n: i32 = c.db.lock().unwrap().query_one("select coalesce(max(n), 0) + 1 from releases where app = $1", &[&app.name])?.get(0);
    c.db.lock().unwrap().execute("insert into releases values ($1, $2, $3, 'building', $4)", &[&app.name, &n, &commit, &now()])?;
    let nodes = c.place(app.memory_gib as i64 * 1024, false)?;
    if nodes.is_empty() {
        c.db.lock().unwrap().execute("update releases set state = 'failed' where app = $1 and n = $2", &[&app.name, &n])?;
        bail!("no node has {} GiB free", app.memory_gib);
    }
    let mut env = app.env.clone();
    for (k, v) in app.secrets.as_object().into_iter().flatten() {
        env[k] = v.clone();
    }
    let spec = json!({
        "app": app.name, "release": n, "port": app.port, "git": {"url": app.git_url, "ref": commit},
        "cpus": app.cpus, "memory_gib": app.memory_gib, "env": env, "start": app.start,
    });
    let handles: Vec<_> = (0..app.instances.max(1) as usize)
        .map(|i| {
            let (node, url, sec) = nodes[i % nodes.len()].clone();
            let spec = spec.clone();
            std::thread::spawn(move || (node.clone(), call(&url, "POST", "/instances", Some(&sec), &spec)))
        })
        .collect();
    let mut started = Vec::new();
    let mut errors = Vec::new();
    let mut health = app.health.clone();
    for h in handles {
        match h.join() {
            Ok((node, Ok(v))) => {
                if let Some(h) = v["health"].as_str().filter(|_| app.health == "/") {
                    health = h.to_string();
                }
                started.push((node, v["sandbox"].as_str().unwrap_or("").to_string(), v["endpoint"].as_str().unwrap_or("").to_string()))
            }
            Ok((node, Err(e))) => errors.push(format!("{node}: {e:#}")),
            Err(_) => errors.push("deploy thread panicked".into()),
        }
    }
    {
        let mut db = c.db.lock().unwrap();
        for (node, sb, ep) in &started {
            db.execute("insert into sandboxes values ($1, $2, $3, 'running', $4)", &[sb, node, &who, &now()])?;
            db.execute("insert into instances values ($1, $2, $3, $4, $5, 'starting')", &[&app.name, &n, sb, node, ep])?;
        }
    }
    if errors.is_empty() {
        let t0 = Instant::now();
        let mut pending: Vec<&(String, String, String)> = started.iter().collect();
        while !pending.is_empty() && t0.elapsed() < Duration::from_secs(30) {
            pending.retain(|(_, _, ep)| !healthy(ep, &health));
            if !pending.is_empty() {
                std::thread::sleep(Duration::from_millis(300));
            }
        }
        for (node, sb, _) in pending {
            let logs = c.node(node).and_then(|(url, sec, _)| call(&url, "GET", &format!("/sandboxes/{sb}/logs"), Some(&sec), &Value::Null));
            let tail =
                logs.map(|v| v["logs"].as_str().unwrap_or("").lines().rev().take(10).collect::<Vec<_>>().join("\n")).unwrap_or_default();
            errors.push(format!("{sb}: health check {health} failed within 30s; last log lines:\n{tail}"));
        }
    }
    if !errors.is_empty() {
        for (node, sb, _) in &started {
            delete_instance(c, node, sb);
        }
        c.db.lock().unwrap().execute("update releases set state = 'failed' where app = $1 and n = $2", &[&app.name, &n])?;
        c.audit(who, "deploy", &format!("{} r{n}", app.name), false);
        bail!("release {n} failed, the previous release keeps serving:\n{}", errors.join("\n"));
    }
    let old: Vec<(String, String)> = {
        let mut db = c.db.lock().unwrap();
        let old = db.query(
            "update instances set state = 'draining' where app = $1 and release <> $2 and state = 'live' returning node, sandbox",
            &[&app.name, &n],
        )?;
        db.execute("update instances set state = 'live' where app = $1 and release = $2", &[&app.name, &n])?;
        db.execute("update releases set state = 'live' where app = $1 and n = $2", &[&app.name, &n])?;
        db.execute("update apps set live_release = $2 where name = $1", &[&app.name, &n])?;
        old.iter().map(|r| (r.get(0), r.get(1))).collect()
    };
    let (c2, drain) = (c.clone(), c.drain_secs);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(drain));
        for (node, sb) in old {
            delete_instance(&c2, &node, &sb);
        }
    });
    c.audit(who, "deploy", &format!("{} r{n}", app.name), true);
    Ok(json!({"app": app.name, "release": n, "commit": commit, "url": format!("http://{}.{}", app.name, c.domain),
              "instances": started.iter().map(|(n, s, e)| json!({"node": n, "sandbox": s, "endpoint": e})).collect::<Vec<_>>()}))
}

fn destroy_app(c: &Arc<Controller>, name: &str) -> Result<Value> {
    let rows = c.db.lock().unwrap().query("select node, sandbox from instances where app = $1 and state <> 'removed'", &[&name])?;
    for r in rows {
        delete_instance(c, &r.get::<_, String>(0), &r.get::<_, String>(1));
    }
    let mut db = c.db.lock().unwrap();
    db.execute("delete from releases where app = $1", &[&name])?;
    db.execute("delete from instances where app = $1", &[&name])?;
    db.execute("delete from apps where name = $1", &[&name])?;
    Ok(json!({"destroyed": name}))
}

fn app_status(c: &Controller, app: &App) -> Result<Value> {
    let mut db = c.db.lock().unwrap();
    let row = db.query_one("select live_release, public from apps where name = $1", &[&app.name])?;
    let (live, public): (Option<i32>, bool) = (row.get(0), row.get(1));
    let rel = db.query("select n, commit, state, created from releases where app = $1 order by n desc limit 20", &[&app.name])?;
    let inst =
        db.query("select release, sandbox, node, endpoint, state from instances where app = $1 and state <> 'removed'", &[&app.name])?;
    Ok(json!({
        "name": app.name, "url": format!("http://{}.{}", app.name, c.domain), "git": {"url": app.git_url, "ref": app.git_ref},
        "port": app.port, "instances_wanted": app.instances, "live_release": live, "public": public,
        "env": app.env, "secrets": app.secrets.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default(),
        "releases": rel.iter().map(|r| json!({"n": r.get::<_, i32>(0), "commit": r.get::<_, String>(1), "state": r.get::<_, String>(2), "created": r.get::<_, i64>(3)})).collect::<Vec<_>>(),
        "instances": inst.iter().map(|r| json!({"release": r.get::<_, i32>(0), "sandbox": r.get::<_, String>(1), "node": r.get::<_, String>(2), "endpoint": r.get::<_, String>(3), "state": r.get::<_, String>(4)})).collect::<Vec<_>>(),
    }))
}

fn upsert_app(c: &Controller, who: &Caller, b: &Value, preview_of: Option<&str>) -> Result<String> {
    let name = b["name"].as_str().context("missing name")?.to_string();
    if !name.bytes().all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || x == b'-') || name.is_empty() {
        bail!("app names are lowercase letters, digits and -");
    }
    let git_url = b["git"]["url"].as_str().context("missing git.url")?;
    let git_ref = b["git"]["ref"].as_str().unwrap_or("main");
    // 0: the commit's rekcod.toml decides (3000 if it doesn't say).
    let port = b["port"].as_i64().unwrap_or(0) as i32;
    let instances = b["instances"].as_i64().unwrap_or(1) as i32;
    let cpus = b["cpus"].as_i64().unwrap_or(1) as i32;
    let mem = b["memory_gib"].as_i64().unwrap_or(1) as i32;
    let health = b["health"].as_str().unwrap_or("/");
    let start = b["start"].as_str();
    let env = b.get("env").filter(|e| e.is_object()).map(|e| e.to_string());
    // None keeps an existing app's visibility; new apps are private.
    let public = b["public"].as_bool();
    let mut db = c.db.lock().unwrap();
    if let Some(r) = db.query_opt("select owner from apps where name = $1", &[&name])? {
        let owner: String = r.get(0);
        if owner != who.name && !who.admin {
            bail!("app {name} belongs to someone else");
        }
        db.execute(
            "update apps set git_url = $2, git_ref = $3, port = $4, instances = $5, cpus = $6, memory_gib = $7, health = $8,
             start = $9, env = coalesce($10, env), public = coalesce($11, public) where name = $1",
            &[&name, &git_url, &git_ref, &port, &instances, &cpus, &mem, &health, &start, &env, &public],
        )?;
    } else {
        db.execute(
            "insert into apps (name, owner, git_url, git_ref, port, instances, cpus, memory_gib, health, start, env, preview_of, created, public)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, coalesce($11, '{}'), $12, $13, coalesce($14, false))",
            &[&name, &who.name, &git_url, &git_ref, &port, &instances, &cpus, &mem, &health, &start, &env, &preview_of, &now(), &public],
        )?;
    }
    Ok(name)
}

fn apps_route(c: &Arc<Controller>, who: &Caller, method: &str, seg: &[&str], b: &Value) -> Result<Value> {
    match (method, seg) {
        ("GET", ["apps"]) => {
            let rows = c.db.lock().unwrap().query(
                "select name, git_url, live_release, instances, preview_of, public from apps where owner = $1 or $2 order by name",
                &[&who.name, &who.admin],
            )?;
            Ok(json!(
                rows.iter()
                    .map(|r| json!({
                        "name": r.get::<_, String>(0), "git": r.get::<_, String>(1), "live_release": r.get::<_, Option<i32>>(2),
                        "instances": r.get::<_, i32>(3), "preview_of": r.get::<_, Option<String>>(4), "public": r.get::<_, bool>(5),
                        "url": format!("http://{}.{}", r.get::<_, String>(0), c.domain),
                    }))
                    .collect::<Vec<_>>()
            ))
        }
        // Create or update the app's config, then deploy it.
        ("POST", ["apps"]) => {
            let name = upsert_app(c, who, b, None)?;
            deploy(c, &load_app(c, &name, who)?, None, &who.name)
        }
        ("GET", ["apps", name]) => app_status(c, &load_app(c, name, who)?),
        ("POST", ["apps", name, "visibility"]) => {
            load_app(c, name, who)?;
            let public = b["public"].as_bool().context("missing public")?;
            c.db.lock().unwrap().execute("update apps set public = $2 where name = $1", &[name, &public])?;
            c.audit(&who.name, if public { "make public" } else { "make private" }, name, true);
            Ok(json!({"app": name, "public": public}))
        }
        ("POST", ["apps", name, "deploy"]) => deploy(c, &load_app(c, name, who)?, b["ref"].as_str().map(String::from), &who.name),
        ("POST", ["apps", name, "rollback"]) => {
            let app = load_app(c, name, who)?;
            let target: Option<i32> = b["release"].as_i64().map(|n| n as i32);
            let row = c.db.lock().unwrap().query_opt(
                "select commit from releases where app = $1 and state in ('live', 'old') and n = coalesce($2,
                   (select max(n) from releases where app = $1 and n < (select live_release from apps where name = $1) and state in ('live', 'old')))",
                &[&app.name, &target],
            )?;
            let commit: String = row.ok_or_else(|| anyhow!("no earlier good release of {name}"))?.get(0);
            c.db.lock().unwrap().execute("update releases set state = 'old' where app = $1 and state = 'live'", &[&app.name])?;
            deploy(c, &app, Some(commit), &who.name)
        }
        ("POST", ["apps", name, "scale"]) => {
            let n = b["instances"].as_i64().context("missing instances")? as i32;
            load_app(c, name, who)?;
            c.db.lock().unwrap().execute("update apps set instances = $2 where name = $1", &[name, &n])?;
            let live: Option<i32> = c.db.lock().unwrap().query_one("select live_release from apps where name = $1", &[name])?.get(0);
            let commit: Option<String> = match live {
                Some(l) => {
                    c.db.lock().unwrap().query_opt("select commit from releases where app = $1 and n = $2", &[name, &l])?.map(|r| r.get(0))
                }
                None => None,
            };
            deploy(c, &load_app(c, name, who)?, commit, &who.name)
        }
        ("POST", ["apps", name, "env"]) | ("POST", ["apps", name, "secrets"]) => {
            let app = load_app(c, name, who)?;
            let secrets = seg[2] == "secrets";
            let mut map = if secrets { app.secrets.clone() } else { app.env.clone() };
            for (k, v) in b["set"].as_object().into_iter().flatten() {
                map[k] = v.clone();
            }
            for k in b["unset"].as_array().into_iter().flatten().filter_map(|k| k.as_str()) {
                map.as_object_mut().map(|m| m.remove(k));
            }
            let col = if secrets { "secrets" } else { "env" };
            c.db.lock().unwrap().execute(&format!("update apps set {col} = $2 where name = $1"), &[name, &map.to_string()])?;
            let live: Option<i32> = c.db.lock().unwrap().query_one("select live_release from apps where name = $1", &[name])?.get(0);
            if live.is_none() {
                return Ok(json!({"updated": col}));
            }
            let commit: String =
                c.db.lock().unwrap().query_one("select commit from releases where app = $1 and n = $2", &[name, &live])?.get(0);
            deploy(c, &load_app(c, name, who)?, Some(commit), &who.name)
        }
        ("GET", ["apps", name, "logs"]) => {
            load_app(c, name, who)?;
            let rows = c.db.lock().unwrap().query("select node, sandbox from instances where app = $1 and state = 'live'", &[name])?;
            let mut out = String::new();
            for r in rows {
                let (node, sb): (String, String) = (r.get(0), r.get(1));
                let logs = c
                    .node(&node)
                    .and_then(|(url, sec, _)| call(&url, "GET", &format!("/sandboxes/{sb}/logs"), Some(&sec), &Value::Null))
                    .map(|v| v["logs"].as_str().unwrap_or("").to_string())
                    .unwrap_or_else(|e| format!("({e:#})\n"));
                for l in logs.lines() {
                    out.push_str(&format!("{sb} | {l}\n"));
                }
            }
            Ok(json!({"logs": out}))
        }
        ("DELETE", ["apps", name]) => {
            load_app(c, name, who)?;
            c.audit(&who.name, "destroy", name, true);
            destroy_app(c, name)
        }
        _ => Err(anyhow!("no such endpoint")),
    }
}

/// HMAC-SHA256 of the body, as GitHub sends it in `X-Hub-Signature-256`.
pub fn github_signature(secret: &str, body: &[u8]) -> String {
    use hmac::Mac;
    let mut m = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).expect("any key length");
    m.update(body);
    format!("sha256={}", m.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// Pull request events: deploy `<app>-pr-<n>` on push, destroy it on close.
fn github_webhook(c: &Arc<Controller>, req: &crate::sandbox_api::Request) -> Result<Value> {
    let secret = c.config("webhook_secret")?.context("no webhook secret")?;
    let sig = req.headers.iter().find(|(k, _)| k == "x-hub-signature-256").map(|(_, v)| v.as_str()).unwrap_or("");
    use subtle_eq::ct_eq;
    if !ct_eq(sig.as_bytes(), github_signature(&secret, &req.body).as_bytes()) {
        bail!("bad signature");
    }
    let event = req.headers.iter().find(|(k, _)| k == "x-github-event").map(|(_, v)| v.as_str()).unwrap_or("");
    if event != "pull_request" {
        return Ok(json!({"ignored": event}));
    }
    let b: Value = serde_json::from_slice(&req.body)?;
    let pr = b["number"].as_i64().context("no PR number")?;
    let urls = [b["repository"]["clone_url"].as_str().unwrap_or(""), b["repository"]["html_url"].as_str().unwrap_or("")];
    let base = c.db.lock().unwrap().query_opt(
        "select name, owner from apps where preview_of is null and (git_url = $1 or git_url = $2 or git_url = $3) limit 1",
        &[&urls[0], &urls[1], &format!("{}.git", urls[1])],
    )?;
    let Some(base) = base else { return Ok(json!({"ignored": "no app for this repository"})) };
    let (base, owner): (String, String) = (base.get(0), base.get(1));
    let preview = format!("{base}-pr-{pr}");
    let who = Caller { name: owner, admin: false, max: 0 };
    match b["action"].as_str().unwrap_or("") {
        "closed" => destroy_app(c, &preview),
        "opened" | "synchronize" | "reopened" => {
            let app = load_app(c, &base, &who)?;
            let head = b["pull_request"]["head"]["sha"].as_str().context("no head sha")?;
            let head_repo = b["pull_request"]["head"]["repo"]["clone_url"].as_str().unwrap_or(&app.git_url);
            let cfg = json!({
                "name": preview, "git": {"url": head_repo, "ref": head}, "port": app.port, "instances": 1,
                "cpus": app.cpus, "memory_gib": app.memory_gib, "health": app.health, "start": app.start, "env": app.env,
            });
            upsert_app(c, &who, &cfg, Some(&base))?;
            let v = deploy(c, &load_app(c, &preview, &who)?, Some(head.into()), "github")?;
            if let (Ok(token), Some(api)) = (std::env::var("GITHUB_TOKEN"), b["pull_request"]["comments_url"].as_str()) {
                let body = json!({"body": format!("Preview: {}", v["url"].as_str().unwrap_or(""))}).to_string();
                let _ = Command::new("curl")
                    .args(["-sf", "-X", "POST", "-H", &format!("Authorization: Bearer {token}"), "-d", &body, api])
                    .stdout(Stdio::null())
                    .status();
            }
            Ok(v)
        }
        a => Ok(json!({"ignored": a})),
    }
}

mod subtle_eq {
    /// Constant-time comparison, so signature checks don't leak timing.
    pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }
}

pub struct ControllerOptions {
    pub db: String,
    pub listen: String,
    pub domain: String,
    pub drain_secs: u64,
}

/// Starts the controller; on first run creates the schema, an admin token and a join token.
pub fn controller_serve(paths: &Paths, o: ControllerOptions) -> Result<()> {
    let mut db = postgres::Client::connect(&o.db, postgres::NoTls).context("connect to Postgres")?;
    db.batch_execute(SCHEMA)?;
    let dir = paths.home.join("controller");
    std::fs::create_dir_all(&dir)?;
    for (key, file) in [("join_token", "join-token"), ("webhook_secret", "webhook-secret")] {
        let v = match db.query_opt("select value from config where key = $1", &[&key])? {
            Some(r) => r.get::<_, String>(0),
            None => {
                let v = secret()?;
                db.execute("insert into config values ($1, $2)", &[&key, &v])?;
                v
            }
        };
        std::fs::write(dir.join(file), &v)?;
    }
    let admin: Option<String> = db.query_opt("select token from tokens where admin and name = 'admin'", &[])?.map(|r| r.get(0));
    let admin = match admin {
        Some(t) => t,
        None => {
            let t = secret()?;
            db.execute("insert into tokens values ($1, 'admin', true, 100)", &[&t])?;
            t
        }
    };
    std::fs::write(dir.join("admin-token"), &admin)?;
    use std::os::unix::fs::PermissionsExt;
    for f in ["join-token", "webhook-secret", "admin-token"] {
        std::fs::set_permissions(dir.join(f), std::fs::Permissions::from_mode(0o600))?;
    }
    let c = Arc::new(Controller { db: Mutex::new(db), domain: o.domain, drain_secs: o.drain_secs });
    let l = TcpListener::bind(&o.listen).with_context(|| format!("bind {}", o.listen))?;
    eprintln!("rekcod: controller on http://{} (tokens in {})", o.listen, dir.display());
    let w = c.clone();
    std::thread::spawn(move || watchdog(w));
    for s in l.incoming().flatten() {
        let c = c.clone();
        std::thread::spawn(move || {
            if let Err(e) = controller_handle(&c, s) {
                eprintln!("rekcod: controller: {e:#}");
            }
        });
    }
    Ok(())
}

// ------------------------------------------------------------- edge

/// The host part of a Host header, lowercased.
pub fn host_of(head: &str) -> Option<String> {
    head.lines()
        .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case("host")).map(|(_, v)| v.trim().to_string()))
        .map(|h| h.rsplit_once(':').filter(|(_, p)| p.bytes().all(|b| b.is_ascii_digit())).map_or(h.clone(), |(h, _)| h.to_string()))
        .map(|h| h.to_ascii_lowercase())
}

/// `rekcod edge serve`: Host-based HTTP routing to live instances, refreshed every 2 s.
/// `listen` routes every app (for the tailnet); `public_listen` only apps marked public
/// (point cloudflared or another tunnel at it).
pub fn edge_serve(paths: &Paths, remote_name: Option<&str>, listen: &str, public_listen: Option<&str>) -> Result<()> {
    let r = remote(paths, remote_name)?;
    let table: Arc<RwLock<HashMap<String, Route>>> = Arc::default();
    let t = table.clone();
    std::thread::spawn(move || {
        loop {
            match call(&r.url, "GET", "/routes", Some(&r.token), &Value::Null) {
                Ok(v) => {
                    let m: HashMap<String, Route> = serde_json::from_value(v).unwrap_or_default();
                    *t.write().unwrap() = m;
                }
                Err(e) => eprintln!("rekcod: edge: routes: {e:#}"),
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    });
    let rr = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let accept =
        |l: TcpListener, public_only: bool, table: Arc<RwLock<HashMap<String, Route>>>, rr: Arc<std::sync::atomic::AtomicUsize>| {
            for s in l.incoming().flatten() {
                let (table, rr) = (table.clone(), rr.clone());
                std::thread::spawn(move || {
                    let _ = edge_conn(s, &table, &rr, public_only);
                });
            }
        };
    if let Some(p) = public_listen {
        let l = TcpListener::bind(p).with_context(|| format!("bind {p}"))?;
        eprintln!("rekcod: edge (public apps only) on http://{p}");
        let (t, rr) = (table.clone(), rr.clone());
        std::thread::spawn(move || accept(l, true, t, rr));
    }
    let l = TcpListener::bind(listen).with_context(|| format!("bind {listen}"))?;
    eprintln!("rekcod: edge (all apps) on http://{listen}");
    accept(l, false, table, rr);
    Ok(())
}

fn edge_conn(
    mut s: TcpStream,
    table: &RwLock<HashMap<String, Route>>,
    rr: &std::sync::atomic::AtomicUsize,
    public_only: bool,
) -> Result<()> {
    let mut head = Vec::new();
    let mut buf = [0u8; 4096];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = s.read(&mut buf)?;
        if n == 0 || head.len() > 64 << 10 {
            return Ok(());
        }
        head.extend_from_slice(&buf[..n]);
    }
    let host = host_of(&String::from_utf8_lossy(&head)).unwrap_or_default();
    // A private app looks like no app at all on the public listener.
    let eps = table.read().unwrap().get(&host).filter(|r| r.public || !public_only).map(|r| r.endpoints.clone()).unwrap_or_default();
    if eps.is_empty() {
        return respond(&mut s, 404, "text/plain", format!("no app at {host}\n").as_bytes());
    }
    let start = rr.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    for i in 0..eps.len() {
        if let Ok(mut up) = TcpStream::connect(&eps[(start + i) % eps.len()]) {
            up.write_all(&head)?;
            pipe(s, up);
            return Ok(());
        }
    }
    respond(&mut s, 400, "text/plain", b"every instance refused the connection\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_and_dockerfile() {
        let t = parse_toml("[app]\nname = \"dash\"\nport = 3000 # comment\nstart = \"python3 app.py\"\n[env]\nA = \"1\"\n");
        assert_eq!(t["app.name"], "dash");
        assert_eq!(t["app.port"], "3000");
        assert_eq!(t["env.A"], "1");
        let dir = std::env::temp_dir().join(format!("rekcod-fleet-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("requirements.txt"), "").unwrap();
        let d = generated_dockerfile(&dir, "python3 app.py").unwrap();
        assert!(
            d.starts_with("FROM python:3.13-slim\n") && d.contains("pip install") && d.ends_with("CMD [\"python3\",\"app.py\"]\n"),
            "{d}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn host_header_and_urls() {
        assert_eq!(host_of("GET / HTTP/1.1\r\nHOST: Dash.Apps.Test:8080\r\n\r\n").as_deref(), Some("dash.apps.test"));
        assert_eq!(host_of("GET / HTTP/1.1\r\n\r\n"), None);
        assert_eq!(authority("http://10.0.0.2:7701/").unwrap(), "10.0.0.2:7701");
        assert!(authority("https://x").is_err());
        assert_eq!(percent_encode("/a b"), "%2Fa%20b");
    }

    #[test]
    fn github_signatures() {
        // From GitHub's docs ("Validating webhook deliveries").
        assert_eq!(
            github_signature("It's a Secret to Everybody", b"Hello, World!"),
            "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17"
        );
        assert!(subtle_eq::ct_eq(b"abc", b"abc") && !subtle_eq::ct_eq(b"abc", b"abd") && !subtle_eq::ct_eq(b"ab", b"abc"));
    }
}
