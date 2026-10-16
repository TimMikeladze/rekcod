//! Self-hosted fleet and app hosting (docs/platform.md, tracks 2 and 3).
//!
//! - `rekcod controller serve`: the sandbox API for many Macs, plus apps. State in SQLite.
//! - `rekcod node join|serve`: runs on each Mac; serves `sandbox_api` on its address with a
//!   node secret, heartbeats capacity, and deploys app instances into sandboxes.
//! - `rekcod edge serve` lives in `edge/`.
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

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b)
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
    /// Namespace for app, sandbox and job commands; `REKCOD_NS` (and `--ns`) win.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ns: Option<String>,
}

impl Remote {
    pub fn ns(&self) -> String {
        std::env::var("REKCOD_NS").ok().filter(|n| !n.is_empty()).or(self.ns.clone()).unwrap_or_else(|| "default".into())
    }

    /// `path` inside this remote's namespace: `/apps` → `/ns/team/apps`.
    pub fn scoped(&self, path: impl AsRef<str>) -> String {
        format!("/ns/{}{}", self.ns(), path.as_ref())
    }
}

/// Paths the controller serves per namespace; everything else is fleet-wide.
pub fn is_scoped(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or(path);
    ["/apps", "/sandboxes", "/jobs", "/schedules"].iter().any(|p| path == *p || path.starts_with(&format!("{p}/")))
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

pub fn remote_add(paths: &Paths, name: &str, url: &str, token: &str, ns: Option<&str>) -> Result<()> {
    authority(url)?;
    let mut r = remotes(paths);
    r.remotes.insert(name.into(), Remote { url: url.trim_end_matches('/').into(), token: token.into(), ns: ns.map(String::from) });
    if r.default.is_none() {
        r.default = Some(name.into());
    }
    save_remotes(paths, &r)
}

fn save_remotes(paths: &Paths, r: &Remotes) -> Result<()> {
    std::fs::write(remotes_file(paths), serde_json::to_vec_pretty(r)?)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(remotes_file(paths), std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// `rekcod ns use NS`: the namespace a remote's commands go to from now on.
pub fn remote_use_ns(paths: &Paths, name: Option<&str>, ns: &str) -> Result<()> {
    let mut r = remotes(paths);
    let name = name.map(String::from).or(r.default.clone()).ok_or_else(|| anyhow!("no remote; `rekcod remote add NAME URL --token T`"))?;
    let rem = r.remotes.get_mut(&name).ok_or_else(|| anyhow!("no remote {name}"))?;
    rem.ns = Some(ns.into()).filter(|n| n != "default");
    save_remotes(paths, &r)?;
    eprintln!("rekcod: remote {name} now uses namespace {ns}");
    Ok(())
}

/// `rekcod remote use NAME`: the remote commands without `--remote` go to.
pub fn remote_use(paths: &Paths, name: &str) -> Result<()> {
    let mut r = remotes(paths);
    if !r.remotes.contains_key(name) {
        bail!("no remote {name}");
    }
    r.default = Some(name.into());
    save_remotes(paths, &r)?;
    eprintln!("rekcod: default remote is now {name}");
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
    save_remotes(paths, &r)
}

pub fn remote_list(paths: &Paths) {
    let r = remotes(paths);
    for (name, rem) in &r.remotes {
        let star = if r.default.as_deref() == Some(name) { "*" } else { " " };
        println!("{star} {name:<12} {}  ns={}", rem.url, rem.ns.as_deref().unwrap_or("default"));
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

/// Local paths and `file://` URLs are refused unless this is set: they'd let any token
/// holder clone the node owner's own repos. `scripts/e2e-fleet.sh` sets it.
const ALLOW_LOCAL_GIT: &str = "REKCOD_ALLOW_LOCAL_GIT";

/// Refuses a git URL or ref that git would read as an option (`--upload-pack=...` runs a
/// command) or that names a transport other than https, ssh and http.
pub fn check_git_source(url: &str, rev: &str) -> Result<()> {
    let ok_chars =
        |s: &str| !s.is_empty() && s.len() <= 1024 && !s.starts_with('-') && !s.chars().any(|c| c.is_control() || c.is_whitespace());
    if !ok_chars(url) {
        bail!("bad git URL {url:?}");
    }
    if !ok_chars(rev) || rev.contains("..") || rev.contains(':') {
        bail!("bad git ref {rev:?}");
    }
    let remote = url.starts_with("https://")
        || url.starts_with("http://")
        || url.starts_with("ssh://")
        || (!url.contains("://") && url.split_once(':').is_some_and(|(host, _)| host.contains('@') && !host.contains('/')));
    if !remote && std::env::var_os(ALLOW_LOCAL_GIT).is_none() {
        bail!("git URL {url} isn't https, ssh or http (local repos need {ALLOW_LOCAL_GIT}=1 on the controller and nodes)");
    }
    Ok(())
}

/// `git` with only network transports allowed (plus `file` under [`ALLOW_LOCAL_GIT`]),
/// no prompts and no ext:: helpers.
fn git_cmd() -> Command {
    let mut c = Command::new("git");
    let protocols = if std::env::var_os(ALLOW_LOCAL_GIT).is_some() { "https:ssh:http:file" } else { "https:ssh:http" };
    c.env("GIT_ALLOW_PROTOCOL", protocols).env("GIT_TERMINAL_PROMPT", "0").stdin(Stdio::null());
    c
}

/// Clones `url` at `rev` (branch, tag or commit) into `~/.rekcod/remote-ws/<id>`.
pub fn clone(paths: &Paths, url: &str, rev: &str) -> Result<PathBuf> {
    check_git_source(url, rev)?;
    let dir = paths.home.join("remote-ws").join(sandbox::new_id()?);
    std::fs::create_dir_all(&dir)?;
    let git = |args: &[&str]| -> Result<()> {
        let o = git_cmd().arg("-C").arg(&dir).args(args).output()?;
        if !o.status.success() {
            bail!("git {}: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&o.stderr).trim());
        }
        Ok(())
    };
    let r = (|| {
        git(&["init", "-q"])?;
        git(&["fetch", "-q", "--depth", "1", "--", url, rev])?;
        git(&["checkout", "-q", "FETCH_HEAD"])
    })();
    if let Err(e) = r {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e);
    }
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

/// Addresses besides this Mac allowed to reach instance relays: the controller and the
/// edges it has seen, refreshed from each heartbeat. Anyone else on the network would
/// otherwise skip the edge, and with it namespaces and private apps.
static RELAY_ALLOW: RwLock<Vec<IpAddr>> = RwLock::new(Vec::new());

fn relay_allowed(peer: IpAddr, node: IpAddr, allow: &[IpAddr]) -> bool {
    let peer = match peer {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(peer),
        v4 => v4,
    };
    peer.is_loopback() || peer == node || allow.contains(&peer)
}

/// Forwards `ip:ext` (0 picks one) to the Mac's `127.0.0.1:port`; returns the bound port.
fn start_relay(ip: IpAddr, ext: u16, port: u16) -> Result<u16> {
    let l = TcpListener::bind((ip, ext))?;
    let bound = l.local_addr()?.port();
    std::thread::spawn(move || {
        for c in l.incoming().flatten() {
            let peer = c.peer_addr().map(|a| a.ip()).unwrap_or(IpAddr::from([0, 0, 0, 0]));
            if !relay_allowed(peer, ip, &RELAY_ALLOW.read().unwrap()) {
                continue;
            }
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

/// Where a release's image is kept in RustFS (docs/rustfs.md): `<bucket>/<key>.tar.zst` is
/// `docker save | zstd`, `<key>.json` the commit's rekcod.toml. Sent by the controller.
#[derive(serde::Deserialize)]
struct ReleaseStore {
    s3: crate::s3::S3,
    bucket: String,
    key: String,
    /// Both objects are there: load them instead of cloning and building.
    cached: bool,
    /// BuildKit cache bucket.
    cache: Option<String>,
}

/// The stored rekcod.toml of a cached release, if it can be read.
fn stored_toml(st: &ReleaseStore) -> Option<HashMap<String, String>> {
    let mut b = Vec::new();
    st.s3.get(&st.bucket, &format!("{}.json", st.key), &mut b).map_err(|e| eprintln!("rekcod: {}: {e:#}", st.key)).ok()?;
    serde_json::from_slice(&b).ok()
}

/// `docker save IMAGE | zstd` into the store, then the toml (its presence marks it complete).
fn push_image(paths: &Paths, sb: &str, image: &str, st: &ReleaseStore, toml: &HashMap<String, String>) -> Result<()> {
    let tmp = paths.home.join("remote-ws").join(format!("{}.tar.zst", sandbox::new_id()?));
    let r = (|| -> Result<()> {
        let (mut d, _) = sandbox::docker_cmd(paths, sb)?;
        let mut child = d.args(["save", image]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let mut enc = zstd::Encoder::new(std::fs::File::create(&tmp)?, 3)?;
        std::io::copy(child.stdout.as_mut().context("no stdout")?, &mut enc)?;
        enc.finish()?;
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!("docker save: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        st.s3.put(&st.bucket, &format!("{}.tar.zst", st.key), crate::s3::Body::File(&tmp))?;
        st.s3.put(&st.bucket, &format!("{}.json", st.key), crate::s3::Body::Bytes(&serde_json::to_vec(toml)?))
    })();
    let _ = std::fs::remove_file(&tmp);
    r
}

/// Downloads a stored image into the sandbox's dockerd and tags it `image`.
fn load_image(paths: &Paths, sb: &str, image: &str, st: &ReleaseStore) -> Result<()> {
    let tmp = paths.home.join("remote-ws").join(format!("{}.tar.zst", sandbox::new_id()?));
    let r = (|| -> Result<()> {
        st.s3.get(&st.bucket, &format!("{}.tar.zst", st.key), &mut std::fs::File::create(&tmp)?)?;
        let (mut d, _) = sandbox::docker_cmd(paths, sb)?;
        let mut child = d.arg("load").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let mut stdin = child.stdin.take().context("no stdin")?;
        zstd::stream::copy_decode(std::fs::File::open(&tmp)?, &mut stdin)?;
        drop(stdin);
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!("docker load: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let loaded = text.lines().find_map(|l| l.strip_prefix("Loaded image: ")).context("docker load named no image")?.trim();
        let (mut d, _) = sandbox::docker_cmd(paths, sb)?;
        if !d.args(["tag", loaded, image]).stdin(Stdio::null()).status()?.success() {
            bail!("docker tag {loaded} {image} failed");
        }
        Ok(())
    })();
    let _ = std::fs::remove_file(&tmp);
    r
}

/// `tar -C dir -c . | zstd` into `bucket/key`.
fn push_dir(s3: &crate::s3::S3, bucket: &str, key: &str, dir: &Path, tmp: &Path) -> Result<()> {
    let mut child = Command::new("tar").arg("-C").arg(dir).args(["-cf", "-", "."]).stdout(Stdio::piped()).spawn()?;
    let mut enc = zstd::Encoder::new(std::fs::File::create(tmp)?, 3)?;
    std::io::copy(child.stdout.as_mut().context("no stdout")?, &mut enc)?;
    enc.finish()?;
    if !child.wait()?.success() {
        bail!("tar of {} failed", dir.display());
    }
    s3.put(bucket, key, crate::s3::Body::File(tmp))
}

/// The reverse of `push_dir`; false when there's nothing stored yet.
fn pull_dir(s3: &crate::s3::S3, bucket: &str, key: &str, dir: &Path, tmp: &Path) -> Result<bool> {
    if !s3.exists(bucket, key)? {
        return Ok(false);
    }
    s3.get(bucket, key, &mut std::fs::File::create(tmp)?)?;
    std::fs::create_dir_all(dir)?;
    let mut child = Command::new("tar").arg("-C").arg(dir).args(["-xf", "-"]).stdin(Stdio::piped()).spawn()?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    zstd::stream::copy_decode(std::fs::File::open(tmp)?, &mut stdin)?;
    drop(stdin);
    if !child.wait()?.success() {
        bail!("untar of {bucket}/{key} failed");
    }
    Ok(true)
}

/// `docker build`. With a store, BuildKit's cache goes to a local directory on this Mac
/// (`type=local`, which dockerd's builder supports and `s3` isn't), kept in the cache
/// bucket as `<app>.tar.zst` so a build on another node starts from it. A cache that
/// doesn't work (no buildx) falls back to a plain build.
fn build_image(paths: &Paths, sb: &str, image: &str, dir: &Path, st: Option<&ReleaseStore>) -> Result<()> {
    let tail = |b: &[u8]| {
        let err = String::from_utf8_lossy(b);
        err.lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
    };
    if let Some((st, bucket)) = st.and_then(|s| Some((s, s.cache.as_deref()?))) {
        let key = format!("{}.tar.zst", st.key.split('/').next().unwrap_or("app"));
        let work = paths.home.join("remote-ws").join(format!("cache-{}", sandbox::new_id()?));
        let (from, to, tmp) = (work.join("from"), work.join("to"), work.join("cache.tar.zst"));
        std::fs::create_dir_all(&work)?;
        let r = (|| -> Result<Option<String>> {
            let had = pull_dir(&st.s3, bucket, &key, &from, &tmp).unwrap_or_else(|e| {
                eprintln!("rekcod: build cache {bucket}/{key}: {e:#}");
                false
            });
            let (mut d, _) = sandbox::docker_cmd(paths, sb)?;
            d.args(["buildx", "build", "--load", "-q", "-t", image]);
            if had {
                d.args(["--cache-from", &format!("type=local,src={}", from.display())]);
            }
            let out =
                d.args(["--cache-to", &format!("type=local,dest={},mode=max", to.display())]).arg(dir).stdin(Stdio::null()).output()?;
            if !out.status.success() {
                let err = String::from_utf8_lossy(&out.stderr).to_lowercase();
                if ["cache", "buildx", "unknown flag"].iter().any(|w| err.contains(w)) {
                    return Ok(Some(tail(&out.stderr)));
                }
                bail!("build failed: {}", tail(&out.stderr));
            }
            if let Err(e) = push_dir(&st.s3, bucket, &key, &to, &tmp) {
                eprintln!("rekcod: build cache {bucket}/{key}: {e:#}");
            }
            Ok(None)
        })();
        let _ = std::fs::remove_dir_all(&work);
        match r? {
            None => return Ok(()),
            Some(err) => eprintln!("rekcod: build cache unavailable, building without it: {err}"),
        }
    }
    let (mut d, _) = sandbox::docker_cmd(paths, sb)?;
    let out = d.args(["build", "-q", "-t", image]).arg(dir).stdin(Stdio::null()).output()?;
    if !out.status.success() {
        bail!("build failed: {}", tail(&out.stderr));
    }
    Ok(())
}

/// An app with a bucket reaches RustFS through the sandbox's Mac/LAN block: its `IPv4:PORT`.
pub fn storage_reach(endpoint: Option<&str>) -> Vec<String> {
    let Some(auth) = endpoint.and_then(|e| e.strip_prefix("http://")) else { return vec![] };
    let addrs = auth.trim_end_matches('/').to_socket_addrs().into_iter().flatten();
    addrs.filter(|a| a.is_ipv4()).take(1).map(|a| a.to_string()).collect()
}

/// Node op: builds `git` in a new sandbox and runs it; returns `{sandbox, endpoint}`. With a
/// release store, a cached release is loaded instead of cloned and built, and a new build
/// is pushed (`pushed` says whether that worked).
pub fn deploy_instance(paths: &Paths, a: &Value) -> Result<Value> {
    let app = a["app"].as_str().context("missing app")?;
    let release = a["release"].as_i64().context("missing release")?;
    let store: Option<ReleaseStore> = a.get("store").filter(|s| s.is_object()).and_then(|s| serde_json::from_value(s.clone()).ok());
    let stored = store.as_ref().filter(|s| s.cached).and_then(stored_toml);
    let (dir, toml) = match &stored {
        Some(t) => {
            eprintln!(
                "rekcod: {app} r{release}: loading {}/{}",
                store.as_ref().map_or("", |s| s.bucket.as_str()),
                store.as_ref().map_or("", |s| s.key.as_str())
            );
            let dir = paths.home.join("remote-ws").join(sandbox::new_id()?);
            std::fs::create_dir_all(&dir)?;
            (dir.canonicalize()?, t.clone())
        }
        None => {
            let url = a["git"]["url"].as_str().context("missing git.url")?;
            eprintln!("rekcod: {app} r{release}: cloning {url}");
            let dir = clone(paths, url, a["git"]["ref"].as_str().unwrap_or("HEAD"))?;
            // The commit's own rekcod.toml fills in whatever the deploy didn't set.
            let toml = parse_toml(&std::fs::read_to_string(dir.join("rekcod.toml")).unwrap_or_default());
            (dir, toml)
        }
    };
    let port = a["port"].as_u64().filter(|p| *p > 0).or_else(|| toml.get("app.port").and_then(|p| p.parse().ok())).unwrap_or(3000) as u16;
    if stored.is_none() && !dir.join("Dockerfile").exists() {
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
        // Set by the controller only for apps with a bucket; never taken from the app's env,
        // which the app's owner controls.
        reach: storage_reach(a["storage_endpoint"].as_str()),
        ..Default::default()
    };
    let meta = sandbox::create(paths, o)?;
    let mut pushed = false;
    let result = (|| -> Result<Value> {
        let image = format!("rekcod-app-{app}:{release}");
        match (&stored, &store) {
            (Some(_), Some(st)) => load_image(paths, &meta.id, &image, st)?,
            _ => {
                build_image(paths, &meta.id, &image, &dir, store.as_ref())?;
                if let Some(st) = &store {
                    match push_image(paths, &meta.id, &image, st, &toml) {
                        Ok(()) => pushed = true,
                        Err(e) => eprintln!("rekcod: {app} r{release}: push to {}: {e:#}", st.bucket),
                    }
                }
            }
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
        Ok(json!({"sandbox": meta.id, "endpoint": format!("{ip}:{ext}"), "health": toml.get("app.health"), "pushed": pushed}))
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

/// Node op: boots a sleeping instance's VM and starts its app container again; returns
/// once the published port is back on the Mac.
pub fn wake_instance(paths: &Paths, id: &str) -> Result<Value> {
    let v: Value = serde_json::from_slice(&std::fs::read(sandbox::home(paths, id)?.join("instance.json"))?)?;
    let hp = v["hp"].as_u64().context("instance.json has no port")? as u16;
    let t0 = Instant::now();
    // dockerd may still be coming up right after the boot.
    loop {
        let (mut d, _) = sandbox::docker_cmd(paths, id)?;
        let out = d.args(["start", "app"]).stdin(Stdio::null()).output()?;
        if out.status.success() {
            break;
        }
        if t0.elapsed() > Duration::from_secs(30) {
            bail!("start failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    while TcpStream::connect(("127.0.0.1", hp)).is_err() {
        if t0.elapsed() > Duration::from_secs(20) {
            bail!("port {hp} never came back on the Mac");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(json!({"woken": id, "ms": t0.elapsed().as_millis() as u64}))
}

fn job_dir(paths: &Paths, id: &str) -> Result<PathBuf> {
    if !id.starts_with("j-") || !id[2..].bytes().all(|b| b.is_ascii_alphanumeric()) {
        bail!("bad job id {id}");
    }
    Ok(paths.home.join("jobs").join(id))
}

/// Node op: runs a job in a new sandbox in the background. Output goes to
/// `jobs/ID/log`, the outcome to `jobs/ID/result.json`; the sandbox is removed after.
pub fn run_job(paths: &Paths, a: &Value) -> Result<Value> {
    let id = a["id"].as_str().context("missing id")?.to_string();
    let dir = job_dir(paths, &id)?;
    std::fs::create_dir_all(&dir)?;
    let started = id.clone();
    let (paths, a) = (paths.clone(), a.clone());
    std::thread::spawn(move || {
        let t0 = Instant::now();
        let log = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("log"));
        let r = log.map_err(anyhow::Error::from).and_then(|log| job_body(&paths, &id, &a, &dir, log));
        let timeout = a["timeout_secs"].as_u64();
        let cancelled = dir.join("cancelled").exists();
        let result = match r {
            // `timeout -s KILL` in the container exits 137; report the conventional 124.
            Ok(code) if code != 0 && timeout.is_some_and(|t| t0.elapsed().as_secs() >= t) && !cancelled => {
                json!({"exit": 124, "error": format!("timed out after {}s", timeout.unwrap_or(0))})
            }
            Ok(code) => json!({"exit": code}),
            Err(e) => {
                let _ = std::fs::OpenOptions::new().append(true).open(dir.join("log")).and_then(|mut f| writeln!(f, "rekcod: {e:#}"));
                json!({"exit": -1, "error": format!("{e:#}")})
            }
        };
        let mut result = result;
        result["cancelled"] = json!(cancelled);
        result["ms"] = json!(t0.elapsed().as_millis() as u64);
        let _ = std::fs::write(dir.join("result.json"), result.to_string());
    });
    Ok(json!({"started": started}))
}

fn job_body(paths: &Paths, id: &str, a: &Value, dir: &Path, log: std::fs::File) -> Result<i32> {
    let cmd: Vec<String> = a["cmd"].as_array().into_iter().flatten().filter_map(|c| c.as_str().map(String::from)).collect();
    let src = clone(paths, a["git"]["url"].as_str().context("missing git.url")?, a["git"]["ref"].as_str().unwrap_or("HEAD"))?;
    let image = a["image"]
        .as_str()
        .map(String::from)
        .or_else(|| crate::xrun::plan(&src, &cmd).map(|p| p.image))
        .unwrap_or_else(|| "alpine:3.22".into());
    let o = sandbox::CreateOptions {
        dir: Some(src.clone()),
        name: Some(id.to_string()),
        mount: true,
        image: Some(image),
        cpus: a["cpus"].as_u64().unwrap_or(2) as usize,
        memory_gib: a["memory_gib"].as_u64().unwrap_or(2),
        allow_hosts: vec!["*".into()],
        ttl_secs: 0,
        ..Default::default()
    };
    let meta = sandbox::create(paths, o)?;
    std::fs::write(dir.join("sandbox"), &meta.id)?;
    let r = (|| -> Result<i32> {
        let env = a["env"].as_array().into_iter().flatten().filter_map(|e| e.as_str().map(String::from)).collect();
        let eo = sandbox::ExecOptions { workdir: None, env, timeout_secs: a["timeout_secs"].as_u64(), tty: false };
        let mut c = sandbox::exec_cmd(paths, &meta.id, &eo, &cmd)?;
        let status = c.stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log).status()?;
        Ok(status.code().unwrap_or(137))
    })();
    let _ = sandbox::rm(paths, std::slice::from_ref(&meta.id), false);
    let _ = std::fs::remove_dir_all(&src);
    r
}

/// Node op: `{running, exit, cancelled, error, log}` with the log's last 64 KiB.
pub fn job_get(paths: &Paths, id: &str) -> Result<Value> {
    let dir = job_dir(paths, id)?;
    if !dir.exists() {
        bail!("no job {id} on this node");
    }
    let log = std::fs::read(dir.join("log")).unwrap_or_default();
    let tail = String::from_utf8_lossy(&log[log.len().saturating_sub(JOB_LOG_BYTES)..]).to_string();
    let mut v = match std::fs::read(dir.join("result.json")) {
        Ok(b) => serde_json::from_slice::<Value>(&b)?,
        Err(_) => json!({"running": true}),
    };
    v["running"] = json!(v["exit"].is_null());
    v["log"] = json!(tail);
    Ok(v)
}

/// Node op: removes the job's sandbox, which ends the command; the job thread then records
/// it as cancelled.
pub fn job_cancel(paths: &Paths, id: &str) -> Result<Value> {
    let dir = job_dir(paths, id)?;
    std::fs::write(dir.join("cancelled"), b"")?;
    // The job thread removes it too once the command ends; whichever is second finds it gone.
    if let Ok(sb) = std::fs::read_to_string(dir.join("sandbox"))
        && let Err(e) = sandbox::rm(paths, &[sb.trim().to_string()], false)
    {
        eprintln!("rekcod: cancel {id}: {e:#}");
    }
    Ok(json!({"cancelled": id}))
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
    crate::paths::write_private(&paths.home.join("node.json"), &serde_json::to_vec_pretty(&cfg)?)?;
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
            let (disk_total, disk_free) = disk_gib(&hb_paths.home);
            let body = json!({"mem_free_mib": sandbox::free_mib(&hb_paths), "macos_free": macos_free, "sandboxes": sbs,
                              "cpus": crate::cli::sysctl_u64("hw.ncpu").unwrap_or(0), "load": load_average(),
                              "disk_total_gib": disk_total, "disk_free_gib": disk_free});
            match call(&controller, "POST", &format!("/nodes/{id}/heartbeat"), Some(&secret), &body) {
                Ok(v) => {
                    let mut ips: Vec<IpAddr> =
                        v["ingress"].as_array().into_iter().flatten().filter_map(|i| i.as_str()?.parse().ok()).collect();
                    // The controller's health checks come from wherever its URL points.
                    if let Ok(a) = authority(&controller) {
                        ips.extend(a.to_socket_addrs().into_iter().flatten().map(|s| s.ip()));
                    }
                    *RELAY_ALLOW.write().unwrap() = ips;
                }
                Err(e) => eprintln!("rekcod: heartbeat: {e:#}"),
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    });
    let l = TcpListener::bind(addr).with_context(|| format!("bind {addr}"))?;
    eprintln!("rekcod: node {} serving sandboxes on {} ({})", cfg.id, cfg.listen, cfg.advertise);
    for s in l.incoming().flatten() {
        let (p, t) = (paths.clone(), cfg.secret.clone());
        let _ = s.set_read_timeout(Some(crate::sandbox_api::READ_TIMEOUT));
        crate::sandbox_api::spawn_limited(move || crate::sandbox_api::handle(&p, &crate::sandbox_api::Policy::node(), s, Some(&t)));
    }
    Ok(())
}

/// The 1-minute load average.
fn load_average() -> f64 {
    let mut l = [0f64; 3];
    if unsafe { libc::getloadavg(l.as_mut_ptr(), 3) } < 1 { 0.0 } else { l[0] }
}

fn disk_gib(dir: &Path) -> (u64, u64) {
    crate::cli::disk_gib(dir).unwrap_or((0, 0))
}

// ------------------------------------------------------------- controller

const SCHEMA: &str = "
pragma journal_mode = wal;
create table if not exists config (key text primary key, value text not null);
create table if not exists nodes (id text primary key, url text not null, secret text not null,
  mem_total integer not null default 0, mem_free integer not null default 0, last_seen integer not null default 0,
  state text not null default 'ready', macos_free integer not null default 0);
create table if not exists tokens (token text primary key, name text not null, admin integer not null default 0,
  max_sandboxes integer not null default 10);
create table if not exists sandboxes (id text primary key, node text not null, owner text not null,
  state text not null default 'running', created integer not null);
create table if not exists apps (name text primary key, owner text not null, git_url text not null, git_ref text not null,
  port integer not null, instances integer not null default 1, cpus integer not null default 1, memory_gib integer not null default 1,
  health text not null default '/', start text, env text not null default '{}', secrets text not null default '{}',
  live_release integer, preview_of text, created integer not null, public integer not null default 0);
create table if not exists releases (app text not null, n integer not null, commit_sha text not null, state text not null,
  created integer not null, primary key (app, n));
create table if not exists instances (app text not null, release integer not null, sandbox text primary key, node text not null,
  endpoint text not null, state text not null);
create table if not exists audit (ts integer not null, who text not null, action text not null, target text not null, ok integer not null);
create table if not exists jobs (id text primary key, owner text not null, node text not null, name text, spec text not null,
  state text not null, exit integer, created integer not null, finished integer, log text not null default '', schedule text);
create table if not exists schedules (name text primary key, owner text not null, spec text not null, every integer not null,
  last_run integer not null default 0, last_job text);
create table if not exists namespaces (name text primary key, created integer not null, salt blob not null,
  max_sandboxes integer not null default 10, max_cpus integer not null default 16, max_memory_gib integer not null default 32,
  max_apps integer not null default 20);
create table if not exists members (ns text not null, who text not null, role text not null, primary key (ns, who));
create table if not exists node_pools (node text not null, ns text not null, primary key (node, ns));
create table if not exists bans (net text primary key, reason text not null default '', until integer, created integer not null);
insert or ignore into namespaces (name, created, salt) values ('default', 0, randomblob(16));
";

/// Columns added after the first release; `alter table` fails harmlessly once they exist.
const MIGRATIONS: &[&str] = &[
    "alter table apps add column idle_stop integer not null default 0",
    "alter table apps add column recycle integer not null default 0",
    "alter table apps add column last_request integer not null default 0",
    // Namespaces (docs/namespaces.md). Rows from before them belong to `default`.
    "alter table apps add column ns text not null default 'default'",
    "alter table sandboxes add column ns text not null default 'default'",
    "alter table sandboxes add column cpus integer not null default 0",
    "alter table sandboxes add column memory_gib integer not null default 0",
    "alter table jobs add column ns text not null default 'default'",
    "alter table schedules add column ns text not null default 'default'",
    "alter table audit add column ns text not null default 'default'",
    // Per-app edge settings (docs/edge-http.md).
    "alter table apps add column http text not null default '{}'",
    // Per-app buckets (docs/rustfs.md); the secret is sealed like app secrets.
    "alter table apps add column bucket integer not null default 0",
    "alter table apps add column storage_key text",
    "alter table apps add column storage_secret text",
    // Release detail (docs/dashboard.md): who deployed, when it finished, the deploy log.
    "alter table releases add column who text not null default ''",
    "alter table releases add column finished integer",
    "alter table releases add column log text not null default ''",
    // Node load and disk from heartbeats; load is the 1-minute average times 100.
    "alter table nodes add column cpus integer not null default 0",
    "alter table nodes add column load integer not null default 0",
    "alter table nodes add column disk_total integer not null default 0",
    "alter table nodes add column disk_free integer not null default 0",
    "alter table bans add column by text not null default ''",
    // Per-app requests per minute from the edges' activity reports.
    "create table if not exists traffic (app text not null, minute integer not null, requests integer not null,
       errors integer not null, p50 integer not null, p95 integer not null, primary key (app, minute))",
];

/// The controller's SQLite database. Queries use `?N` parameters; rows come back whole so
/// call sites read them with `row.get(i)` after the statement is done.
pub struct Db(rusqlite::Connection);

pub struct Row(Vec<rusqlite::types::Value>);

impl Row {
    /// Column `i`, converted; panics on a type mismatch, which is a bug in the query.
    pub fn get<T: rusqlite::types::FromSql>(&self, i: usize) -> T {
        T::column_result(rusqlite::types::ValueRef::from(&self.0[i])).unwrap_or_else(|e| panic!("column {i}: {e}"))
    }
}

type Params<'a> = &'a [&'a dyn rusqlite::ToSql];

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        let c = rusqlite::Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        c.busy_timeout(Duration::from_secs(5))?;
        c.execute_batch(SCHEMA)?;
        for m in MIGRATIONS {
            let _ = c.execute(m, []);
        }
        Ok(Db(c))
    }

    pub fn execute(&mut self, sql: &str, p: Params) -> Result<u64> {
        Ok(self.0.execute(sql, p)? as u64)
    }

    pub fn query(&mut self, sql: &str, p: Params) -> Result<Vec<Row>> {
        let mut st = self.0.prepare_cached(sql)?;
        let n = st.column_count();
        let rows =
            st.query_map(p, |r| (0..n).map(|i| r.get::<_, rusqlite::types::Value>(i)).collect::<rusqlite::Result<Vec<_>>>().map(Row))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn query_opt(&mut self, sql: &str, p: Params) -> Result<Option<Row>> {
        Ok(self.query(sql, p)?.into_iter().next())
    }

    pub fn query_one(&mut self, sql: &str, p: Params) -> Result<Row> {
        self.query_opt(sql, p)?.ok_or_else(|| anyhow!("query returned no row"))
    }
}

struct Controller {
    db: Mutex<Db>,
    domain: String,
    drain_secs: u64,
    /// One lock per app, held while it wakes or recycles, so those never overlap.
    busy: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// `controller/secret-key`, kept out of fleet.db; app secrets are sealed with a key
    /// derived from it and the namespace's salt.
    key: [u8; 32],
    /// Addresses that fetched `/routes` (edges) and when; nodes let them reach instances.
    edges: Mutex<HashMap<IpAddr, i64>>,
    /// RustFS with root keys (`--storage`): app buckets, release images, backups.
    storage: Option<crate::s3::S3>,
    /// Edge client reports by minute: (minute, ip) -> (requests, rate-limited). Ten minutes kept.
    clients: Mutex<HashMap<(i64, String), (i64, i64)>>,
}

struct Caller {
    name: String,
    admin: bool,
    max: i32,
}

impl Caller {
    /// The controller acting for itself (watchdog, webhooks).
    fn system(name: &str) -> Caller {
        Caller { name: name.into(), admin: true, max: i32::MAX }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Role {
    Viewer,
    Member,
    Owner,
}

impl Role {
    fn parse(s: &str) -> Result<Role> {
        match s {
            "viewer" => Ok(Role::Viewer),
            "member" => Ok(Role::Member),
            "owner" => Ok(Role::Owner),
            _ => bail!("roles are viewer, member and owner"),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Member => "member",
            Role::Owner => "owner",
        }
    }
}

/// Namespace names and app names: a DNS label without `--`, which separates them in hosts.
fn valid_label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 40
        && s.bytes().all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || x == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
        && !s.contains("--")
}

/// The app's row key and host label: `web` in `default`, `web--team` elsewhere.
pub fn app_key(ns: &str, name: &str) -> String {
    if ns == "default" { name.into() } else { format!("{name}--{ns}") }
}

/// `web--team` → `web`; a key without a namespace is its own name.
fn short_name(key: &str) -> &str {
    key.split_once("--").map_or(key, |(n, _)| n)
}

impl Controller {
    fn new(db: Db, domain: String, drain_secs: u64, key: [u8; 32]) -> Controller {
        Controller {
            db: Mutex::new(db),
            domain,
            drain_secs,
            busy: Default::default(),
            key,
            edges: Default::default(),
            storage: None,
            clients: Default::default(),
        }
    }

    fn config(&self, key: &str) -> Result<Option<String>> {
        Ok(self.db.lock().unwrap().query_opt("select value from config where key = ?1", &[&key])?.map(|r| r.get(0)))
    }

    fn caller(&self, token: &str) -> Result<Option<Caller>> {
        let row = self.db.lock().unwrap().query_opt("select name, admin, max_sandboxes from tokens where token = ?1", &[&token])?;
        Ok(row.map(|r| Caller { name: r.get(0), admin: r.get(1), max: r.get(2) }))
    }

    fn audit(&self, ns: &str, who: &str, action: &str, target: &str, ok: bool) {
        let _ = self.db.lock().unwrap().execute(
            "insert into audit (ts, who, action, target, ok, ns) values (?1, ?2, ?3, ?4, ?5, ?6)",
            &[&now(), &who, &action, &target, &ok, &ns],
        );
    }

    /// `who`'s role in `ns`. Everyone is a member of `default`, where each person only sees
    /// their own things (`can_touch`); fleet admins own every namespace.
    fn role(&self, who: &Caller, ns: &str) -> Result<Option<Role>> {
        let mut db = self.db.lock().unwrap();
        if db.query_opt("select 1 from namespaces where name = ?1", &[&ns])?.is_none() {
            return Ok(None);
        }
        if who.admin {
            return Ok(Some(Role::Owner));
        }
        if ns == "default" {
            return Ok(Some(Role::Member));
        }
        let r = db.query_opt("select role from members where ns = ?1 and who = ?2", &[&ns, &who.name])?;
        r.map(|r| Role::parse(&r.get::<String>(0))).transpose()
    }

    /// Fails unless `who` has at least `need` in `ns`. A namespace you aren't in doesn't exist.
    fn allow(&self, who: &Caller, ns: &str, need: Role) -> Result<()> {
        match self.role(who, ns)? {
            None => bail!("no namespace {ns}"),
            Some(r) if r < need => bail!("{ns}: needs the {} role (you're a {})", need.name(), r.name()),
            Some(_) => Ok(()),
        }
    }

    /// Ready nodes with at least `mib` free, most free first. A namespace with a node pool
    /// only gets its pool; other namespaces never get pooled nodes.
    fn place(&self, mib: i64, macos: bool, ns: &str) -> Result<Vec<(String, String, String)>> {
        let rows = self.db.lock().unwrap().query(
            "select id, url, secret from nodes where state = 'ready' and last_seen > ?1 and mem_free >= ?2
             and (not ?3 or macos_free > 0)
             and case when exists(select 1 from node_pools where ns = ?4) then id in (select node from node_pools where ns = ?4)
                      else id not in (select node from node_pools) end
             order by mem_free desc",
            &[&(now() - 15), &mib, &macos, &ns],
        )?;
        Ok(rows.iter().map(|r| (r.get(0), r.get(1), r.get(2))).collect())
    }

    fn app_lock(&self, app: &str) -> Arc<Mutex<()>> {
        self.busy.lock().unwrap().entry(app.to_string()).or_default().clone()
    }

    fn node(&self, id: &str) -> Result<(String, String, String)> {
        let r = self.db.lock().unwrap().query_opt("select url, secret, state from nodes where id = ?1", &[&id])?;
        r.map(|r| (r.get(0), r.get(1), r.get(2))).ok_or_else(|| anyhow!("no node {id}"))
    }

    /// The node holding sandbox `id` in `ns`, if `who` may use it.
    fn sandbox_node(&self, ns: &str, id: &str, who: &Caller, need: Role) -> Result<(String, String, String)> {
        self.allow(who, ns, need).map_err(|_| anyhow!(sandbox::NoSandbox(id.into())))?;
        let r = self.db.lock().unwrap().query_opt("select node, owner, state from sandboxes where id = ?1 and ns = ?2", &[&id, &ns])?;
        let Some(r) = r else { bail!(sandbox::NoSandbox(id.into())) };
        let (node, owner, state): (String, String, String) = (r.get(0), r.get(1), r.get(2));
        if !can_touch(who, ns, &owner) {
            bail!(sandbox::NoSandbox(id.into()));
        }
        if state == "lost" {
            bail!("sandbox {id} is on node {node}, which stopped answering");
        }
        self.node(&node)
    }

    /// CPUs, GiB and sandbox/job count in use in `ns`, leaving out app `skip_app`. Apps count
    /// while they have a live release (sleeping ones too, so a wake never hits the quota).
    fn usage(&self, ns: &str, skip_app: &str) -> Result<(i64, i64, i64)> {
        let r = self.db.lock().unwrap().query_one(
            "select
               coalesce((select sum(instances * cpus) from apps where ns = ?1 and name <> ?2 and live_release is not null), 0)
             + coalesce((select sum(cpus) from sandboxes s where ns = ?1 and state = 'running'
                           and not exists(select 1 from instances i where i.sandbox = s.id)), 0)
             + coalesce((select sum(coalesce(json_extract(spec, '$.cpus'), 2)) from jobs where ns = ?1 and state = 'running'), 0),
               coalesce((select sum(instances * memory_gib) from apps where ns = ?1 and name <> ?2 and live_release is not null), 0)
             + coalesce((select sum(memory_gib) from sandboxes s where ns = ?1 and state = 'running'
                           and not exists(select 1 from instances i where i.sandbox = s.id)), 0)
             + coalesce((select sum(coalesce(json_extract(spec, '$.memory_gib'), 2)) from jobs where ns = ?1 and state = 'running'), 0),
               (select count(*) from sandboxes s where ns = ?1 and state = 'running'
                  and not exists(select 1 from instances i where i.sandbox = s.id))
             + (select count(*) from jobs where ns = ?1 and state = 'running')",
            &[&ns, &skip_app],
        )?;
        Ok((r.get(0), r.get(1), r.get(2)))
    }

    /// Fails if adding `cpus`, `gib` and `sandboxes` to `ns` (with app `skip_app` replaced)
    /// goes over its quota; the error names the limit.
    fn check_quota(&self, ns: &str, skip_app: &str, cpus: i64, gib: i64, sandboxes: i64) -> Result<()> {
        let (uc, um, us) = self.usage(ns, skip_app)?;
        let q =
            self.db.lock().unwrap().query_one("select max_cpus, max_memory_gib, max_sandboxes from namespaces where name = ?1", &[&ns])?;
        let (mc, mm, ms): (i64, i64, i64) = (q.get(0), q.get(1), q.get(2));
        // A negative limit (`unlimited` on the CLI) is no limit.
        if mc >= 0 && uc + cpus > mc {
            bail!("quota: {ns} would use {} of {mc} CPUs", uc + cpus);
        }
        if mm >= 0 && um + gib > mm {
            bail!("quota: {ns} would use {} of {mm} GiB", um + gib);
        }
        if ms >= 0 && sandboxes > 0 && us + sandboxes > ms {
            bail!("quota: {us} of {ms} sandboxes and jobs in use in {ns}");
        }
        Ok(())
    }

    /// The AES-256-GCM key for `ns`'s secrets: HMAC(controller key, namespace salt). Deleting
    /// the namespace deletes the salt, so its secrets can't be read from an old backup.
    fn ns_key(&self, ns: &str) -> Result<[u8; 32]> {
        use hmac::Mac;
        let salt: Vec<u8> =
            self.db.lock().unwrap().query_opt("select salt from namespaces where name = ?1", &[&ns])?.context("no namespace")?.get(0);
        let mut m = hmac::Hmac::<sha2::Sha256>::new_from_slice(&self.key).expect("any key length");
        m.update(b"rekcod secrets ");
        m.update(&salt);
        Ok(m.finalize().into_bytes().into())
    }

    fn seal(&self, ns: &str, plain: &str) -> Result<String> {
        use aes_gcm::aead::{Aead, KeyInit};
        let cipher = aes_gcm::Aes256Gcm::new(&self.ns_key(ns)?.into());
        let nonce: [u8; 12] = random_bytes()?;
        let ct = cipher.encrypt(&nonce.into(), plain.as_bytes()).map_err(|_| anyhow!("encrypt failed"))?;
        Ok(format!("{SEALED}{}", hex(&[&nonce[..], &ct].concat())))
    }

    fn open_sealed(&self, ns: &str, sealed: &str) -> Result<String> {
        use aes_gcm::aead::{Aead, KeyInit};
        let Some(h) = sealed.strip_prefix(SEALED) else { return Ok(sealed.into()) };
        let b = unhex(h).context("bad sealed secret")?;
        if b.len() < 12 {
            bail!("bad sealed secret");
        }
        let cipher = aes_gcm::Aes256Gcm::new(&self.ns_key(ns)?.into());
        let plain = cipher
            .decrypt(b[..12].into(), &b[12..])
            .map_err(|_| anyhow!("can't open a secret in {ns}: controller/secret-key isn't the one it was sealed with"))?;
        Ok(String::from_utf8(plain)?)
    }

    /// Seals every secret still stored in the clear (from before secrets were encrypted).
    fn seal_plain_secrets(&self) -> Result<()> {
        let rows = self.db.lock().unwrap().query("select name, ns, secrets from apps", &[])?;
        for r in rows {
            let (name, ns, raw): (String, String, String) = (r.get(0), r.get(1), r.get(2));
            let mut m: serde_json::Map<String, Value> = serde_json::from_str(&raw).unwrap_or_default();
            let mut changed = false;
            for v in m.values_mut() {
                if let Some(s) = v.as_str().filter(|s| !s.starts_with(SEALED)) {
                    *v = json!(self.seal(&ns, s)?);
                    changed = true;
                }
            }
            if changed {
                self.db.lock().unwrap().execute("update apps set secrets = ?2 where name = ?1", &[&name, &Value::Object(m).to_string()])?;
            }
        }
        Ok(())
    }
}

const SEALED: &str = "sealed1:";

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len()).step_by(2).map(|i| s.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok())).collect()
}

/// In `default` each person only sees their own sandboxes, apps and jobs (how it worked
/// before namespaces); in any other namespace members share everything.
fn can_touch(who: &Caller, ns: &str, owner: &str) -> bool {
    ns != "default" || who.admin || owner == who.name
}

/// Every 5 s: marks nodes silent for 15 s as lost (with their sandboxes), deletes week-old
/// previews, puts idle apps to sleep, recycles old releases, and runs jobs and schedules.
fn watchdog(c: Arc<Controller>) {
    loop {
        std::thread::sleep(Duration::from_secs(5));
        for (what, r) in [
            ("idle", sleep_idle(&c)),
            ("recycle", recycle_due(&c)),
            ("jobs", poll_jobs(&c)),
            ("schedules", run_schedules(&c)),
            ("backup", backup_due(&c)),
        ] {
            if let Err(e) = r {
                eprintln!("rekcod: watchdog {what}: {e:#}");
            }
        }
        let lost = {
            let mut db = c.db.lock().unwrap();
            let lost = db
                .query("update nodes set state = 'lost' where state <> 'lost' and last_seen < ?1 returning id", &[&(now() - 15)])
                .unwrap_or_default();
            for r in &lost {
                let id: String = r.get(0);
                let _ = db.execute("update sandboxes set state = 'lost' where node = ?1 and state = 'running'", &[&id]);
                let _ = db.execute("update instances set state = 'lost' where node = ?1 and state in ('live', 'sleeping')", &[&id]);
                let _ = db.execute("update jobs set state = 'lost', finished = ?2 where node = ?1 and state = 'running'", &[&id, &now()]);
            }
            lost.len()
        };
        if lost > 0 {
            eprintln!("rekcod: {lost} node(s) lost");
        }
        let old: Vec<String> =
            c.db.lock()
                .unwrap()
                .query("select name from apps where preview_of is not null and created < ?1", &[&(now() - 7 * 86400)])
                .map(|rows| rows.iter().map(|r| r.get(0)).collect())
                .unwrap_or_default();
        for name in old {
            let _ = destroy_app(&c, &name);
        }
    }
}

/// Forwards the request to a node and streams its response back unchanged.
fn proxy(node_url: &str, secret: &str, req: &crate::sandbox_api::Request, path: &str, w: &mut impl Write) -> Result<()> {
    let q: Vec<String> = req.query.iter().map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v))).collect();
    let path = if q.is_empty() { path.to_string() } else { format!("{path}?{}", q.join("&")) };
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
    let peer = stream.peer_addr().ok().map(|a| a.ip());
    let mut r = BufReader::new(stream);
    let req = read_request(&mut r)?;
    let mut w = r.into_inner();
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(json!({}));
    let all: Vec<&str> = req.path.trim_matches('/').split('/').collect();
    // `/ns/NS/apps/...`; the same paths without the prefix are the `default` namespace.
    let (ns, seg) = match all.as_slice() {
        ["ns", ns, rest @ ..] if !rest.is_empty() => (ns.to_string(), rest.to_vec()),
        _ => ("default".to_string(), all.clone()),
    };
    let bearer =
        req.headers.iter().find(|(k, _)| k == "authorization").and_then(|(_, v)| v.strip_prefix("Bearer ")).unwrap_or("").to_string();

    // Unauthenticated by user token: node registration, heartbeats, GitHub webhooks.
    match (req.method.as_str(), seg.as_slice()) {
        ("POST", ["nodes", "register"]) => {
            let r = (|| {
                let want = c.config("join_token")?.unwrap_or_default();
                if want.is_empty() || !crate::sandbox_api::ct_eq(body["join_token"].as_str().unwrap_or("").as_bytes(), want.as_bytes()) {
                    bail!("bad join token");
                }
                let (id, sec) = (format!("n-{}", sandbox::new_id()?), secret()?);
                let url = body["url"].as_str().context("missing url")?;
                let total = body["mem_total_mib"].as_i64().unwrap_or(0);
                c.db.lock().unwrap().execute(
                    "insert into nodes (id, url, secret, mem_total, last_seen) values (?1, ?2, ?3, ?4, ?5)",
                    &[&id, &url, &sec, &total, &now()],
                )?;
                c.audit("-", "node", "register", &id, true);
                Ok(json!({"id": id, "secret": sec}))
            })();
            return json_reply(&mut w, r);
        }
        ("POST", ["nodes", id, "heartbeat"]) => {
            let r = (|| {
                let (_, sec, state) = c.node(id)?;
                if !crate::sandbox_api::ct_eq(sec.as_bytes(), bearer.as_bytes()) {
                    bail!("bad node secret");
                }
                let free = body["mem_free_mib"].as_i64().unwrap_or(0);
                let macos_free = body["macos_free"].as_i64().unwrap_or(0) as i32;
                let next = if state == "lost" { "ready".to_string() } else { state };
                let load = (body["load"].as_f64().unwrap_or(0.0) * 100.0) as i64;
                let (cpus, dt, df) = (
                    body["cpus"].as_i64().unwrap_or(0),
                    body["disk_total_gib"].as_i64().unwrap_or(0),
                    body["disk_free_gib"].as_i64().unwrap_or(0),
                );
                c.db.lock().unwrap().execute(
                    "update nodes set mem_free = ?2, last_seen = ?3, state = ?4, macos_free = ?5, cpus = ?6, load = ?7,
                       disk_total = ?8, disk_free = ?9 where id = ?1",
                    &[id, &free, &now(), &next, &macos_free, &cpus, &load, &dt, &df],
                )?;
                // Edges seen in the last 10 minutes may reach instances on this node.
                let ingress: Vec<String> =
                    c.edges.lock().unwrap().iter().filter(|(_, t)| **t > now() - 600).map(|(ip, _)| ip.to_string()).collect();
                Ok(json!({"ingress": ingress}))
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
            let mut db = c.db.lock().unwrap();
            let rows = db.query(
                "select id, url, mem_total, mem_free, last_seen, state, cpus, load, disk_total, disk_free,
                   (select count(*) from instances i where i.node = n.id and i.state in ('live', 'starting', 'sleeping', 'draining')),
                   (select count(*) from jobs j where j.node = n.id and j.state = 'running')
                 from nodes n order by id",
                &[],
            )?;
            let pools = db.query("select node, ns from node_pools order by ns", &[])?;
            Ok(json!(
                rows.iter()
                    .map(|r| {
                        let id: String = r.get(0);
                        let pool: Vec<String> = pools.iter().filter(|p| p.get::<String>(0) == id).map(|p| p.get(1)).collect();
                        json!({
                            "id": id, "url": r.get::<String>(1), "mem_total_mib": r.get::<i64>(2),
                            "mem_free_mib": r.get::<i64>(3), "last_seen": r.get::<i64>(4), "state": r.get::<String>(5), "pool": pool,
                            "cpus": r.get::<i64>(6), "load": r.get::<i64>(7) as f64 / 100.0,
                            "disk_total_gib": r.get::<i64>(8), "disk_free_gib": r.get::<i64>(9),
                            "instances": r.get::<i64>(10), "jobs_running": r.get::<i64>(11),
                        })
                    })
                    .collect::<Vec<_>>()
            ))
        }),
        ("POST", ["nodes", id, "undrain"]) => admin_only().and_then(|_| {
            let n = c.db.lock().unwrap().execute("update nodes set state = 'ready' where id = ?1 and state = 'drain'", &[id])?;
            if n == 0 {
                bail!("node {id} isn't drained");
            }
            c.audit("-", &who.name, "undrain", id, true);
            Ok(json!({"undrained": id}))
        }),
        ("POST", ["nodes", id, "drain"]) => admin_only().and_then(|_| {
            let n = c.db.lock().unwrap().execute("update nodes set state = 'drain' where id = ?1", &[id])?;
            if n == 0 {
                bail!("no node {id}");
            }
            c.audit("-", &who.name, "drain", id, true);
            Ok(json!({"drained": id}))
        }),
        // Dedicate a node to namespaces; an empty list gives it back to everyone.
        ("POST", ["nodes", id, "pool"]) => admin_only().and_then(|_| {
            c.node(id)?;
            let list: Vec<String> = body["ns"].as_array().into_iter().flatten().filter_map(|n| n.as_str().map(String::from)).collect();
            let mut db = c.db.lock().unwrap();
            for n in &list {
                if db.query_opt("select 1 from namespaces where name = ?1", &[n])?.is_none() {
                    bail!("no namespace {n}");
                }
            }
            db.execute("delete from node_pools where node = ?1", &[id])?;
            for n in &list {
                db.execute("insert into node_pools values (?1, ?2)", &[id, n])?;
            }
            Ok(json!({"node": id, "pool": list}))
        }),
        ("POST", ["tokens"]) => admin_only().and_then(|_| {
            let (t, name) = (secret()?, body["name"].as_str().context("missing name")?.to_string());
            let max = body["max_sandboxes"].as_i64().unwrap_or(10) as i32;
            let admin = body["admin"].as_bool().unwrap_or(false);
            let member_of = body["ns"].as_str().filter(|n| *n != "default");
            let role = Role::parse(body["role"].as_str().unwrap_or("member"))?;
            if let Some(n) = member_of {
                c.allow(&who, n, Role::Owner)?;
            }
            let mut db = c.db.lock().unwrap();
            db.execute("insert into tokens values (?1, ?2, ?3, ?4)", &[&t, &name, &admin, &max])?;
            if let Some(n) = member_of {
                db.execute("insert or replace into members values (?1, ?2, ?3)", &[&n, &name, &role.name()])?;
            }
            drop(db);
            c.audit("-", &who.name, "token", &name, true);
            Ok(json!({"token": t, "name": name}))
        }),
        // Every token with this name: they're one person's tokens.
        ("POST", ["tokens", name]) => admin_only().and_then(|_| {
            let max = body["max_sandboxes"].as_i64().context("missing max_sandboxes")?;
            let n = c.db.lock().unwrap().execute("update tokens set max_sandboxes = ?2 where name = ?1", &[name, &max])?;
            if n == 0 {
                bail!("no token named {name}");
            }
            c.audit("-", &who.name, &format!("max_sandboxes {max}"), name, true);
            Ok(json!({"name": name, "tokens": n, "max_sandboxes": limit(max)}))
        }),
        // `?peek=1` (the dashboard) reads the table without becoming an edge nodes let in.
        ("GET", ["routes"]) => admin_only().and_then(|_| {
            if let Some(ip) = peer.filter(|_| !req.query.iter().any(|(k, _)| k == "peek")) {
                c.edges.lock().unwrap().insert(ip, now());
            }
            routes(c)
        }),
        ("GET", ["edge", "clients"]) => admin_only().map(|_| edge_clients(c)),
        // The edge's hosts with requests since its last report; idle stop counts from these.
        ("POST", ["backups"]) => admin_only().and_then(|_| backup(c)),
        ("GET", ["backups"]) => admin_only().and_then(|_| {
            let s = c.storage_or_err()?;
            Ok(json!(s.list(BACKUPS, "")?))
        }),
        ("POST", ["routes", "activity"]) => admin_only().and_then(|_| {
            let suffix = format!(".{}", c.domain);
            let mut db = c.db.lock().unwrap();
            for h in body["hosts"].as_array().into_iter().flatten().filter_map(|h| h.as_str()) {
                if let Some(app) = h.strip_suffix(&suffix) {
                    db.execute("update apps set last_request = ?2 where name = ?1", &[&app, &now()])?;
                }
            }
            // Several edges report the same minute: counts add up, percentiles keep the worst.
            for t in body["traffic"].as_array().into_iter().flatten() {
                let Some(app) = t["host"].as_str().and_then(|h| h.strip_suffix(&suffix)) else { continue };
                let n = |k: &str| t[k].as_i64().unwrap_or(0);
                db.execute(
                    "insert into traffic values (?1, ?2, ?3, ?4, ?5, ?6) on conflict (app, minute) do update set
                       requests = requests + ?3, errors = errors + ?4, p50 = max(p50, ?5), p95 = max(p95, ?6)",
                    &[&app, &n("minute"), &n("requests"), &n("errors"), &n("p50_ms"), &n("p95_ms")],
                )?;
            }
            db.execute("delete from traffic where minute < ?1", &[&(now() / 60 - 24 * 60)])?;
            drop(db);
            let minute = now() / 60;
            let mut clients = c.clients.lock().unwrap();
            for cl in body["clients"].as_array().into_iter().flatten() {
                let Some(ip) = cl["ip"].as_str() else { continue };
                let e = clients.entry((minute, ip.to_string())).or_default();
                e.0 += cl["requests"].as_i64().unwrap_or(0);
                e.1 += cl["limited"].as_i64().unwrap_or(0);
            }
            clients.retain(|(m, _), _| *m > minute - 10);
            Ok(json!({}))
        }),
        ("GET", ["bans"]) => admin_only().and_then(|_| {
            let rows = c
                .db
                .lock()
                .unwrap()
                .query("select net, reason, until, created, by from bans where until is null or until > ?1 order by created", &[&now()])?;
            Ok(json!(
                rows.iter()
                    .map(|r| json!({"net": r.get::<String>(0), "reason": r.get::<String>(1), "until": r.get::<Option<i64>>(2),
                                    "created": r.get::<i64>(3), "by": r.get::<String>(4)}))
                    .collect::<Vec<_>>()
            ))
        }),
        // A ban for an address that's already banned replaces it (new reason, new expiry).
        ("POST", ["bans"]) => admin_only().and_then(|_| {
            let net = crate::edge::ban::normalize(body["net"].as_str().context("missing net")?)?;
            let until = body["secs"].as_i64().filter(|s| *s > 0).map(|s| now() + s);
            let reason = body["reason"].as_str().unwrap_or("");
            let mut db = c.db.lock().unwrap();
            db.execute("delete from bans where until is not null and until <= ?1", &[&now()])?;
            db.execute(
                "insert or replace into bans (net, reason, until, created, by) values (?1, ?2, ?3, ?4, ?5)",
                &[&net, &reason, &until, &now(), &who.name],
            )?;
            drop(db);
            c.audit("-", &who.name, "ban", &net, true);
            Ok(json!({"banned": net, "until": until}))
        }),
        ("DELETE", ["bans"]) => admin_only().and_then(|_| {
            let net = crate::edge::ban::normalize(body["net"].as_str().context("missing net")?)?;
            if c.db.lock().unwrap().execute("delete from bans where net = ?1", &[&net])? == 0 {
                bail!("{net} isn't banned");
            }
            c.audit("-", &who.name, "unban", &net, true);
            Ok(json!({"unbanned": net}))
        }),
        (_, ["namespaces", ..]) => namespaces_route(c, &who, &req.method, &seg, &body),
        (_, ["jobs", ..]) | (_, ["schedules", ..]) => jobs_route(c, &who, &ns, &req.method, &seg, &body, &req.query),
        ("POST", ["sandboxes"]) => create_sandbox(c, &who, &ns, &req, &mut w).map(|_| Value::Null),
        ("GET", ["sandboxes"]) => (|| {
            c.allow(&who, &ns, Role::Viewer)?;
            let rows = c.db.lock().unwrap().query(
                "select id, node, owner, state, created from sandboxes s where state <> 'removed' and ns = ?1
                   and (?1 <> 'default' or owner = ?2 or ?3) and not exists(select 1 from instances i where i.sandbox = s.id)
                 order by created",
                &[&ns, &who.name, &who.admin],
            )?;
            Ok(json!(
                rows.iter()
                    .map(|r| json!({
                        "id": r.get::<String>(0), "node": r.get::<String>(1), "owner": r.get::<String>(2),
                        "state": r.get::<String>(3), "created": r.get::<i64>(4), "ns": ns,
                    }))
                    .collect::<Vec<_>>()
            ))
        })(),
        ("POST", ["sandboxes", _, "apply"]) => Err(anyhow!("apply isn't offered remotely; `git apply` the patch from diff")),
        (m, ["sandboxes", id, rest @ ..]) => {
            let fork = m == "POST" && rest == ["fork"];
            let delete = m == "DELETE" && rest.is_empty();
            let need = if m == "GET" { Role::Viewer } else { Role::Member };
            let node_path = format!("/{}", seg.join("/"));
            match c.sandbox_node(&ns, id, &who, need) {
                Err(e) => Err(e),
                Ok((url, sec, _)) if fork => (|| {
                    let (cpus, gib): (i64, i64) = {
                        let r = c.db.lock().unwrap().query_one("select cpus, memory_gib from sandboxes where id = ?1", &[id])?;
                        (r.get(0), r.get(1))
                    };
                    c.check_quota(&ns, "", cpus, gib, 1)?;
                    let mut v = call(&url, "POST", &node_path, Some(&sec), &body)?;
                    let child = v["id"].as_str().context("node returned no id")?.to_string();
                    let node: String = c.db.lock().unwrap().query_one("select node from sandboxes where id = ?1", &[id])?.get(0);
                    c.db.lock().unwrap().execute(
                        "insert into sandboxes (id, node, owner, state, created, ns, cpus, memory_gib) values (?1, ?2, ?3, 'running', ?4, ?5, ?6, ?7)",
                        &[&child, &node, &who.name, &now(), &ns, &cpus, &gib],
                    )?;
                    c.audit(&ns, &who.name, "fork", &child, true);
                    v["node"] = json!(node);
                    Ok(v)
                })(),
                Ok((url, sec, _)) => {
                    proxy(&url, &sec, &req, &node_path, &mut w)?;
                    if delete {
                        c.db.lock().unwrap().execute("update sandboxes set state = 'removed' where id = ?1", &[id])?;
                    }
                    c.audit(&ns, &who.name, &format!("{m} {}", rest.join("/")), id, true);
                    return Ok(());
                }
            }
        }
        _ => apps_route(c, &who, &ns, &req.method, &seg, &body, &req.query),
    };
    if matches!(r, Ok(Value::Null)) {
        return Ok(());
    }
    json_reply(&mut w, r)
}

/// The fields a user may set on a remote sandbox. Everything else in the body (`dir`,
/// `mount`, `golden`, `secrets`, `reach`) names things on the node owner's Mac.
fn create_body(b: &Value) -> Value {
    let mut out = json!({"git": {"url": b["git"]["url"], "ref": b["git"]["ref"].as_str().unwrap_or("HEAD")}});
    for k in ["name", "image", "cpus", "memory_gib", "allow_hosts", "offline", "ttl_secs", "os"] {
        if let Some(v) = b.get(k).filter(|v| !v.is_null()) {
            out[k] = v.clone();
        }
    }
    out
}

fn create_sandbox(c: &Arc<Controller>, who: &Caller, ns: &str, req: &crate::sandbox_api::Request, w: &mut impl Write) -> Result<()> {
    let err = |w: &mut _, msg: String| respond(w, 400, "application/json", json!({"error": msg}).to_string().as_bytes());
    let body = create_body(&serde_json::from_slice(&req.body).unwrap_or(json!({})));
    let (Some(url), Some(rev)) = (body["git"]["url"].as_str(), body["git"]["ref"].as_str()) else {
        return err(w, "remote sandboxes need git.url (and git.ref)".into());
    };
    if let Err(e) = check_git_source(url, rev) {
        return err(w, format!("{e:#}"));
    }
    if let Err(e) = c.allow(who, ns, Role::Member) {
        return respond(w, 404, "application/json", json!({"error": format!("{e:#}")}).to_string().as_bytes());
    }
    let count: i64 =
        c.db.lock().unwrap().query_one("select count(*) from sandboxes where owner = ?1 and state = 'running'", &[&who.name])?.get(0);
    if who.max >= 0 && count >= who.max as i64 {
        return err(w, format!("quota: {count} of {} sandboxes in use", who.max));
    }
    let macos = body["os"].as_str() == Some("macos");
    let gib = body["memory_gib"].as_i64().unwrap_or(2).max(if macos { 8 } else { 0 });
    let cpus = body["cpus"].as_i64().unwrap_or(2);
    if let Err(e) = c.check_quota(ns, "", cpus, gib, 1) {
        return err(w, format!("{e:#}"));
    }
    let mib = gib * 1024;
    let Some((node, url, sec)) = c.place(mib, macos, ns)?.into_iter().next() else {
        let what = if macos { format!("a free macOS slot and {mib} MiB") } else { format!("{mib} MiB free") };
        return err(w, format!("no node has {what}"));
    };
    let mut v = match call(&url, "POST", "/sandboxes", Some(&sec), &body) {
        Ok(v) => v,
        Err(e) => return err(w, format!("node {node}: {e:#}")),
    };
    let id = v["id"].as_str().unwrap_or("").to_string();
    c.db.lock().unwrap().execute(
        "insert into sandboxes (id, node, owner, state, created, ns, cpus, memory_gib) values (?1, ?2, ?3, 'running', ?4, ?5, ?6, ?7)",
        &[&id, &node, &who.name, &now(), &ns, &cpus, &gib],
    )?;
    c.audit(ns, &who.name, "create", &id, true);
    v["node"] = json!(node);
    v["ns"] = json!(ns);
    respond(w, 201, "application/json", v.to_string().as_bytes())
}

// ------------------------------------------------------------- namespaces

/// A quota for display: a number, or "unlimited" for a negative one.
fn limit(v: i64) -> Value {
    if v < 0 { json!("unlimited") } else { json!(v) }
}

fn namespace_json(c: &Controller, name: &str, role: Option<Role>) -> Result<Value> {
    let q = c.db.lock().unwrap().query_one(
        "select max_sandboxes, max_cpus, max_memory_gib, max_apps, (select count(*) from apps where ns = ?1) from namespaces where name = ?1",
        &[&name],
    )?;
    let (cpus, gib, sbs) = c.usage(name, "")?;
    let members = c.db.lock().unwrap().query("select who, role from members where ns = ?1 order by who", &[&name])?;
    Ok(json!({
        "name": name, "role": role.map(Role::name),
        "quota": {"sandboxes": limit(q.get(0)), "cpus": limit(q.get(1)), "memory_gib": limit(q.get(2)), "apps": limit(q.get(3))},
        "used": {"sandboxes": sbs, "cpus": cpus, "memory_gib": gib, "apps": q.get::<i64>(4)},
        "members": members.iter().map(|r| json!({"who": r.get::<String>(0), "role": r.get::<String>(1)})).collect::<Vec<_>>(),
    }))
}

fn namespaces_route(c: &Arc<Controller>, who: &Caller, method: &str, seg: &[&str], b: &Value) -> Result<Value> {
    let admin_only = || -> Result<()> { if who.admin { Ok(()) } else { bail!("admin token required") } };
    let quota_cols =
        [("max_sandboxes", "max_sandboxes"), ("max_cpus", "max_cpus"), ("max_memory_gib", "max_memory_gib"), ("max_apps", "max_apps")];
    match (method, seg) {
        ("GET", ["namespaces"]) => {
            let names: Vec<String> =
                c.db.lock().unwrap().query("select name from namespaces order by name", &[])?.iter().map(|r| r.get(0)).collect();
            let mut out = vec![];
            for n in names {
                if let Some(role) = c.role(who, &n)? {
                    out.push(namespace_json(c, &n, Some(role))?);
                }
            }
            Ok(json!(out))
        }
        ("POST", ["namespaces"]) => {
            admin_only()?;
            let name = b["name"].as_str().context("missing name")?;
            if !valid_label(name) {
                bail!("namespace names are lowercase letters, digits and single dashes, at most 40");
            }
            let salt: [u8; 16] = random_bytes()?;
            c.db.lock()
                .unwrap()
                .execute("insert into namespaces (name, created, salt) values (?1, ?2, ?3)", &[&name, &now(), &salt.to_vec()])
                .map_err(|_| anyhow!("namespace {name} exists"))?;
            for (k, col) in quota_cols {
                if let Some(v) = b[k].as_i64() {
                    c.db.lock().unwrap().execute(&format!("update namespaces set {col} = ?2 where name = ?1"), &[&name, &v])?;
                }
            }
            c.audit(name, &who.name, "create namespace", name, true);
            namespace_json(c, name, Some(Role::Owner))
        }
        ("GET", ["namespaces", n]) => {
            let role = c.role(who, n)?.ok_or_else(|| anyhow!("no namespace {n}"))?;
            namespace_json(c, n, Some(role))
        }
        ("POST", ["namespaces", n, "quota"]) => {
            admin_only()?;
            c.allow(who, n, Role::Owner)?;
            for (k, col) in quota_cols {
                if let Some(v) = b[k].as_i64() {
                    c.db.lock().unwrap().execute(&format!("update namespaces set {col} = ?2 where name = ?1"), &[n, &v])?;
                }
            }
            namespace_json(c, n, Some(Role::Owner))
        }
        ("POST", ["namespaces", n, "members"]) => {
            c.allow(who, n, Role::Owner)?;
            if *n == "default" {
                bail!("everyone is in default");
            }
            let name = b["who"].as_str().context("missing who")?;
            let role = Role::parse(b["role"].as_str().unwrap_or("member"))?;
            let mut db = c.db.lock().unwrap();
            if db.query_opt("select 1 from tokens where name = ?1", &[&name])?.is_none() {
                bail!("no token named {name}; `rekcod controller token {name}` first");
            }
            db.execute("insert or replace into members values (?1, ?2, ?3)", &[n, &name, &role.name()])?;
            drop(db);
            c.audit(n, &who.name, &format!("member {}", role.name()), name, true);
            Ok(json!({"ns": n, "who": name, "role": role.name()}))
        }
        ("DELETE", ["namespaces", n, "members", name]) => {
            c.allow(who, n, Role::Owner)?;
            let k = c.db.lock().unwrap().execute("delete from members where ns = ?1 and who = ?2", &[n, name])?;
            if k == 0 {
                bail!("no member {name} in {n}");
            }
            c.audit(n, &who.name, "member removed", name, true);
            Ok(json!({"ns": n, "removed": name}))
        }
        ("DELETE", ["namespaces", n]) => {
            admin_only()?;
            if *n == "default" {
                bail!("default can't be deleted");
            }
            c.allow(who, n, Role::Owner)?;
            let force = b["force"].as_bool().unwrap_or(false);
            type IdNode = Vec<(String, String)>;
            let (apps, sbs, jobs): (Vec<String>, IdNode, IdNode) = {
                let mut db = c.db.lock().unwrap();
                (
                    db.query("select name from apps where ns = ?1", &[n])?.iter().map(|r| r.get(0)).collect(),
                    db.query(
                        "select id, node from sandboxes s where ns = ?1 and state = 'running' and not exists(select 1 from instances i where i.sandbox = s.id)",
                        &[n],
                    )?
                    .iter()
                    .map(|r| (r.get(0), r.get(1)))
                    .collect(),
                    db.query("select id, node from jobs where ns = ?1 and state = 'running'", &[n])?.iter().map(|r| (r.get(0), r.get(1))).collect(),
                )
            };
            if !force && (!apps.is_empty() || !sbs.is_empty() || !jobs.is_empty()) {
                bail!("{n} still has {} apps, {} sandboxes and {} running jobs; --force removes them", apps.len(), sbs.len(), jobs.len());
            }
            for a in &apps {
                destroy_app(c, a)?;
            }
            for (id, node) in sbs {
                delete_instance(c, &node, &id);
            }
            for (id, node) in jobs {
                if let Ok((url, sec, _)) = c.node(&node) {
                    let _ = call(&url, "DELETE", &format!("/jobs/{id}"), Some(&sec), &Value::Null);
                }
                c.db.lock().unwrap().execute("update jobs set state = 'cancelled', finished = ?2 where id = ?1", &[&id, &now()])?;
            }
            let mut db = c.db.lock().unwrap();
            for t in ["members", "node_pools", "schedules"] {
                db.execute(&format!("delete from {t} where ns = ?1"), &[n])?;
            }
            // The salt goes with the row, so any copy of its sealed secrets can't be opened.
            db.execute("delete from namespaces where name = ?1", &[n])?;
            drop(db);
            c.audit(n, &who.name, "delete namespace", n, true);
            Ok(json!({"deleted": n, "apps": apps}))
        }
        _ => Err(anyhow!("no such endpoint")),
    }
}

// ------------------------------------------------------------- apps

/// `{host: {"app", "public", "endpoints": [...], "sleeping"}}` for every app with live or
/// sleeping instances. A sleeping app has no endpoints; the edge wakes it.
fn routes(c: &Controller) -> Result<Value> {
    let rows = c.db.lock().unwrap().query(
        "select i.app, i.endpoint, a.public, i.state, a.ns, a.http from instances i join apps a on a.name = i.app
         where i.state in ('live', 'sleeping') order by i.app",
        &[],
    )?;
    let mut out: std::collections::BTreeMap<String, Route> = Default::default();
    for r in rows {
        let app: String = r.get(0);
        let route = out.entry(format!("{app}.{}", c.domain)).or_default();
        route.app = short_name(&app).to_string();
        route.ns = r.get(4);
        route.public = r.get(2);
        route.http = serde_json::from_str(&r.get::<String>(5)).unwrap_or_default();
        if r.get::<String>(3) == "live" {
            route.endpoints.push(r.get(1));
        }
    }
    for r in out.values_mut() {
        r.sleeping = r.endpoints.is_empty();
    }
    Ok(serde_json::to_value(out)?)
}

#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
pub struct Route {
    #[serde(default)]
    pub app: String,
    #[serde(default)]
    pub ns: String,
    pub public: bool,
    pub endpoints: Vec<String>,
    #[serde(default)]
    pub sleeping: bool,
    #[serde(default)]
    pub http: HttpSettings,
}

/// `[http]` in rekcod.toml: what the edge enforces for this app.
#[derive(serde::Serialize, serde::Deserialize, Default, Clone, PartialEq)]
pub struct HttpSettings {
    /// Per client IP, `20/s`, `600/m` or `1000/h`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burst: Option<u32>,
    /// Request body cap in bytes (100 MiB when unset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_body: Option<u64>,
}

/// Resolves a branch or tag to a commit with `git ls-remote`; a commit passes through.
fn resolve(url: &str, rev: &str) -> Result<String> {
    if rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(rev.into());
    }
    check_git_source(url, rev)?;
    let o = git_cmd().args(["ls-remote", "--", url, rev]).output()?;
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
    /// Row key and host label (`web--team`); `short` is what its owners call it.
    name: String,
    short: String,
    ns: String,
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
    idle_stop: i64,
}

/// App `name` in `ns`, if `who` has at least `need` there.
fn load_app(c: &Controller, ns: &str, name: &str, who: &Caller, need: Role) -> Result<App> {
    c.allow(who, ns, need).map_err(|e| if e.to_string().starts_with("no ") { anyhow!("no app {name}") } else { e })?;
    let app = load_app_key(c, &app_key(ns, name))?.ok_or_else(|| anyhow!("no app {name}"))?;
    let owner: String = c.db.lock().unwrap().query_one("select owner from apps where name = ?1", &[&app.name])?.get(0);
    if app.ns != ns || !can_touch(who, ns, &owner) {
        bail!("no app {name}");
    }
    Ok(app)
}

/// App by row key, no access check; secrets come back opened.
fn load_app_key(c: &Controller, key: &str) -> Result<Option<App>> {
    let r = c.db.lock().unwrap().query_opt(
        "select name, git_url, git_ref, port, instances, cpus, memory_gib, health, start, env, secrets, ns, idle_stop from apps where name = ?1",
        &[&key],
    )?;
    let Some(r) = r else { return Ok(None) };
    let ns: String = r.get(11);
    let mut secrets: Value = serde_json::from_str(&r.get::<String>(10)).unwrap_or(json!({}));
    for v in secrets.as_object_mut().into_iter().flat_map(|m| m.values_mut()) {
        if let Some(s) = v.as_str() {
            *v = json!(c.open_sealed(&ns, s)?);
        }
    }
    let name: String = r.get(0);
    Ok(Some(App {
        short: short_name(&name).to_string(),
        name,
        ns,
        git_url: r.get(1),
        git_ref: r.get(2),
        port: r.get(3),
        instances: r.get(4),
        cpus: r.get(5),
        memory_gib: r.get(6),
        health: r.get(7),
        start: r.get(8),
        env: serde_json::from_str(&r.get::<String>(9)).unwrap_or(json!({})),
        secrets,
        idle_stop: r.get(12),
    }))
}

fn delete_instance(c: &Controller, node: &str, sandbox_id: &str) {
    if let Ok((url, sec, _)) = c.node(node) {
        let _ = call(&url, "DELETE", &format!("/sandboxes/{sandbox_id}"), Some(&sec), &Value::Null);
    }
    let mut db = c.db.lock().unwrap();
    let _ = db.execute("update instances set state = 'removed' where sandbox = ?1", &[&sandbox_id]);
    let _ = db.execute("update sandboxes set state = 'removed' where id = ?1", &[&sandbox_id]);
}

/// New release from `commit` (or the app's ref): start instances on distinct nodes where
/// possible, wait for health, switch routes, drain the old ones.
fn deploy(c: &Arc<Controller>, app: &App, commit: Option<String>, who: &str) -> Result<Value> {
    let commit = match commit {
        Some(c) => c,
        None => resolve(&app.git_url, &app.git_ref)?,
    };
    let n: i32 = c.db.lock().unwrap().query_one("select coalesce(max(n), 0) + 1 from releases where app = ?1", &[&app.name])?.get(0);
    c.db.lock().unwrap().execute(
        "insert into releases (app, n, commit_sha, state, created, who) values (?1, ?2, ?3, 'building', ?4, ?5)",
        &[&app.name, &n, &commit, &now(), &who],
    )?;
    // The deploy log kept with the release (`app release NAME N`).
    let mut log = vec![format!("release {n} of {} at {commit}, by {who}", app.short)];
    let finish = |state: &str, log: &[String]| {
        let _ = c.db.lock().unwrap().execute(
            "update releases set state = ?3, finished = ?4, log = ?5 where app = ?1 and n = ?2",
            &[&app.name, &n, &state, &now(), &log.join("\n")],
        );
    };
    let nodes = c.place(app.memory_gib as i64 * 1024, false, &app.ns)?;
    if nodes.is_empty() {
        log.push(format!("no node has {} GiB free", app.memory_gib));
        finish("failed", &log);
        bail!("no node has {} GiB free", app.memory_gib);
    }
    log.push(format!(
        "placing {} instance(s) on {}",
        app.instances.max(1),
        nodes.iter().map(|n| n.0.as_str()).collect::<Vec<_>>().join(", ")
    ));
    let mut env = app.env.clone();
    for (k, v) in app.secrets.as_object().into_iter().flatten() {
        env[k] = v.clone();
    }
    let bucket = bucket_env(c, app).inspect_err(|e| {
        log.push(format!("bucket: {e:#}"));
        finish("failed", &log);
    })?;
    let storage_endpoint = bucket.iter().find(|(k, _)| *k == "S3_ENDPOINT").map(|(_, v)| v.clone());
    for (k, v) in bucket {
        env[k] = json!(v);
    }
    let mut spec = json!({
        "app": app.name, "release": n, "port": app.port, "git": {"url": app.git_url, "ref": commit},
        "cpus": app.cpus, "memory_gib": app.memory_gib, "env": env, "start": app.start, "store": release_store(c, &app.name, &commit),
        "storage_endpoint": storage_endpoint,
    });
    let launch = |i: usize, spec: &Value| {
        let (node, url, sec) = nodes[i % nodes.len()].clone();
        let spec = spec.clone();
        std::thread::spawn(move || (node.clone(), call(&url, "POST", "/instances", Some(&sec), &spec)))
    };
    let count = app.instances.max(1) as usize;
    let mut results = Vec::new();
    // Not stored yet: the first instance builds and pushes it, the rest load it.
    if spec["store"]["cached"] == json!(false) && count > 1 {
        let first = launch(0, &spec).join();
        if matches!(&first, Ok((_, Ok(v))) if v["pushed"] == json!(true)) {
            spec["store"]["cached"] = json!(true);
        }
        results.push(first);
    }
    let handles: Vec<_> = (results.len()..count).map(|i| launch(i, &spec)).collect();
    results.extend(handles.into_iter().map(|h| h.join()));
    let mut started = Vec::new();
    let mut errors = Vec::new();
    let mut health = app.health.clone();
    for h in results {
        match h {
            Ok((node, Ok(v))) => {
                log.push(format!(
                    "{node}: started {} at {}{}",
                    v["sandbox"].as_str().unwrap_or("?"),
                    v["endpoint"].as_str().unwrap_or("?"),
                    if v["pushed"] == json!(true) { " (built, image stored)" } else { "" }
                ));
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
            db.execute(
                "insert into sandboxes (id, node, owner, state, created, ns, cpus, memory_gib) values (?1, ?2, ?3, 'running', ?4, ?5, ?6, ?7)",
                &[sb, node, &who, &now(), &app.ns, &app.cpus, &app.memory_gib],
            )?;
            db.execute("insert into instances values (?1, ?2, ?3, ?4, ?5, 'starting')", &[&app.name, &n, sb, node, ep])?;
        }
    }
    if errors.is_empty() {
        let t0 = Instant::now();
        let mut pending: Vec<&(String, String, String)> = started.iter().collect();
        while !pending.is_empty() && t0.elapsed() < Duration::from_secs(30) {
            pending.retain(|(_, sb, ep)| {
                let ok = healthy(ep, &health);
                if ok {
                    log.push(format!("{sb}: {health} healthy after {:.1}s", t0.elapsed().as_secs_f64()));
                }
                !ok
            });
            if !pending.is_empty() {
                std::thread::sleep(Duration::from_millis(300));
            }
        }
        for (node, sb, _) in pending {
            let logs = c.node(node).and_then(|(url, sec, _)| call(&url, "GET", &format!("/sandboxes/{sb}/logs"), Some(&sec), &Value::Null));
            let tail = logs
                .map(|v| {
                    let lines: Vec<&str> = v["logs"].as_str().unwrap_or("").lines().collect();
                    lines[lines.len().saturating_sub(10)..].join("\n")
                })
                .unwrap_or_default();
            errors.push(format!("{sb}: health check {health} failed within 30s; last log lines:\n{tail}"));
        }
    }
    if !errors.is_empty() {
        for (node, sb, _) in &started {
            delete_instance(c, node, sb);
        }
        log.extend(errors.iter().cloned());
        log.push("failed; the previous release keeps serving".into());
        finish("failed", &log);
        c.audit(&app.ns, who, "deploy", &format!("{} r{n}", app.short), false);
        bail!("release {n} failed, the previous release keeps serving:\n{}", errors.join("\n"));
    }
    let old: Vec<(String, String)> = {
        let mut db = c.db.lock().unwrap();
        let old = db.query(
            "update instances set state = 'draining' where app = ?1 and release <> ?2 and state = 'live' returning node, sandbox",
            &[&app.name, &n],
        )?;
        db.execute("update instances set state = 'live' where app = ?1 and release = ?2", &[&app.name, &n])?;
        db.execute("update apps set live_release = ?2 where name = ?1", &[&app.name, &n])?;
        // One live release: the one it replaces is what `rollback` goes back to.
        db.execute("update releases set state = 'old' where app = ?1 and state = 'live' and n <> ?2", &[&app.name, &n])?;
        old.iter().map(|r| (r.get(0), r.get(1))).collect()
    };
    log.push(format!("live; draining {} old instance(s) for {}s", old.len(), c.drain_secs));
    finish("live", &log);
    let (c2, drain) = (c.clone(), c.drain_secs);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(drain));
        for (node, sb) in old {
            delete_instance(&c2, &node, &sb);
        }
    });
    if health != app.health {
        // rekcod.toml's health path, so waking checks the same one.
        c.db.lock().unwrap().execute("update apps set health = ?2 where name = ?1", &[&app.name, &health])?;
    }
    c.audit(&app.ns, who, "deploy", &format!("{} r{n}", app.short), true);
    Ok(json!({"app": app.short, "ns": app.ns, "release": n, "commit": commit, "url": format!("http://{}.{}", app.name, c.domain),
              "instances": started.iter().map(|(n, s, e)| json!({"node": n, "sandbox": s, "endpoint": e})).collect::<Vec<_>>()}))
}

fn destroy_app(c: &Arc<Controller>, name: &str) -> Result<Value> {
    if let Err(e) = drop_bucket(c, name) {
        eprintln!("rekcod: {name}: bucket: {e:#}");
    }
    let rows = c.db.lock().unwrap().query("select node, sandbox from instances where app = ?1 and state <> 'removed'", &[&name])?;
    for r in rows {
        delete_instance(c, &r.get::<String>(0), &r.get::<String>(1));
    }
    let mut db = c.db.lock().unwrap();
    db.execute("delete from releases where app = ?1", &[&name])?;
    db.execute("delete from instances where app = ?1", &[&name])?;
    db.execute("delete from apps where name = ?1", &[&name])?;
    Ok(json!({"destroyed": name}))
}

// ------------------------------------------------------------- scale to zero, recycling

fn live_commit(c: &Controller, app: &str) -> Result<Option<String>> {
    Ok(c.db
        .lock()
        .unwrap()
        .query_opt("select r.commit_sha from apps a join releases r on r.app = a.name and r.n = a.live_release where a.name = ?1", &[&app])?
        .map(|r| r.get(0)))
}

fn live_endpoints(c: &Controller, app: &str) -> Result<Vec<String>> {
    let rows = c.db.lock().unwrap().query("select endpoint from instances where app = ?1 and state = 'live'", &[&app])?;
    Ok(rows.iter().map(|r| r.get(0)).collect())
}

/// Starts a sleeping app's instances again and returns their endpoints once healthy. Falls
/// back to redeploying the live commit if none of them comes back.
fn wake(c: &Arc<Controller>, app: &App) -> Result<Vec<String>> {
    let lock = c.app_lock(&app.name);
    let _g = lock.lock().unwrap();
    let eps = live_endpoints(c, &app.name)?;
    if !eps.is_empty() {
        return Ok(eps);
    }
    let _ = c.db.lock().unwrap().execute("update apps set last_request = ?2 where name = ?1", &[&app.name, &now()]);
    let rows =
        c.db.lock().unwrap().query("select node, sandbox, endpoint from instances where app = ?1 and state = 'sleeping'", &[&app.name])?;
    let handles: Vec<_> = rows
        .iter()
        .map(|r| {
            let (node, sb, ep): (String, String, String) = (r.get(0), r.get(1), r.get(2));
            let target = c.node(&node);
            std::thread::spawn(move || {
                let (url, sec, _) = target?;
                call(&url, "POST", &format!("/instances/{sb}/wake"), Some(&sec), &Value::Null)?;
                Ok::<_, anyhow::Error>((node, sb, ep))
            })
        })
        .collect();
    let t0 = Instant::now();
    for h in handles {
        match h.join() {
            Ok(Ok((node, sb, ep))) => {
                while !healthy(&ep, &app.health) && t0.elapsed() < Duration::from_secs(30) {
                    std::thread::sleep(Duration::from_millis(200));
                }
                if healthy(&ep, &app.health) {
                    c.db.lock().unwrap().execute("update instances set state = 'live' where sandbox = ?1", &[&sb])?;
                } else {
                    eprintln!("rekcod: {}: {sb} didn't pass {} after waking", app.name, app.health);
                    delete_instance(c, &node, &sb);
                }
            }
            Ok(Err(e)) => eprintln!("rekcod: {}: wake: {e:#}", app.name),
            Err(_) => eprintln!("rekcod: {}: wake thread panicked", app.name),
        }
    }
    let eps = live_endpoints(c, &app.name)?;
    if !eps.is_empty() {
        c.audit(&app.ns, "edge", "wake", &app.short, true);
        return Ok(eps);
    }
    // Node lost or VM gone: a normal deploy of the same commit (a rebuild).
    let commit = live_commit(c, &app.name)?.ok_or_else(|| anyhow!("{} isn't deployed", app.name))?;
    let _ = c.db.lock().unwrap().execute("update instances set state = 'removed' where app = ?1 and state = 'sleeping'", &[&app.name]);
    deploy(c, app, Some(commit), "wake")?;
    live_endpoints(c, &app.name)
}

/// Apps whose last request and live release are older than `idle_stop`: route them away,
/// then stop their VMs (the disks stay, so waking doesn't rebuild).
fn sleep_idle(c: &Arc<Controller>) -> Result<()> {
    let rows = c.db.lock().unwrap().query(
        "select a.name from apps a join releases r on r.app = a.name and r.n = a.live_release
         where a.idle_stop > 0 and max(a.last_request, r.created) < ?1 - a.idle_stop
           and exists(select 1 from instances i where i.app = a.name and i.state = 'live')",
        &[&now()],
    )?;
    for r in rows {
        let name: String = r.get(0);
        let lock = c.app_lock(&name);
        // Waking or recycling right now: try again on the next tick.
        let Ok(_g) = lock.try_lock() else { continue };
        let stopped =
            c.db.lock()
                .unwrap()
                .query("update instances set state = 'sleeping' where app = ?1 and state = 'live' returning node, sandbox", &[&name])?;
        let targets: Vec<_> = stopped.iter().map(|s| (s.get::<String>(1), c.node(&s.get::<String>(0)))).collect();
        drop(_g);
        // Holds the app's lock until the VMs are down: a wake that comes in meanwhile waits,
        // instead of finding a VM that's still shutting down.
        std::thread::spawn(move || {
            let _g = lock.lock().unwrap();
            // After the edge's next route refresh (2 s), so requests in flight finish.
            std::thread::sleep(Duration::from_secs(3));
            for (sb, node) in targets {
                let r = node.and_then(|(url, sec, _)| call(&url, "POST", &format!("/sandboxes/{sb}/stop"), Some(&sec), &json!({})));
                if let Err(e) = r {
                    eprintln!("rekcod: stop {sb}: {e:#}");
                }
            }
        });
        c.audit("-", "watchdog", "sleep", &name, true);
    }
    Ok(())
}

/// Redeploys the live commit of apps whose live release is older than `recycle`, so
/// fresh VMs replace ones holding memory macOS can't get back.
fn recycle_due(c: &Arc<Controller>) -> Result<()> {
    let rows = c.db.lock().unwrap().query(
        "select a.name, r.commit_sha from apps a join releases r on r.app = a.name and r.n = a.live_release
         where a.recycle > 0 and r.created < ?1 - a.recycle
           and exists(select 1 from instances i where i.app = a.name and i.state = 'live')
           and not exists(select 1 from releases b where b.app = a.name and b.state = 'building')",
        &[&now()],
    )?;
    for r in rows {
        let (name, commit): (String, String) = (r.get(0), r.get(1));
        let lock = c.app_lock(&name);
        if lock.try_lock().is_err() {
            continue;
        }
        let c = c.clone();
        std::thread::spawn(move || {
            let Ok(_g) = lock.try_lock() else { return };
            let r = load_app_key(&c, &name)
                .and_then(|a| a.ok_or_else(|| anyhow!("gone")))
                .and_then(|app| deploy(&c, &app, Some(commit), "recycle"));
            if let Err(e) = r {
                eprintln!("rekcod: recycle {name}: {e:#}");
            }
        });
    }
    Ok(())
}

fn app_status(c: &Controller, app: &App) -> Result<Value> {
    app_details(c, std::slice::from_ref(&app.name))?.pop().context("app vanished")
}

/// `GET /apps/{name}` for each row key, from four queries however many apps there are.
/// Secrets come back as names only, so nothing is unsealed.
fn app_details(c: &Controller, keys: &[String]) -> Result<Vec<Value>> {
    if keys.is_empty() {
        return Ok(vec![]);
    }
    let marks = (1..=keys.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(", ");
    let params: Vec<&dyn rusqlite::ToSql> = keys.iter().map(|k| k as &dyn rusqlite::ToSql).collect();
    let mut db = c.db.lock().unwrap();
    let apps = db.query(
        &format!(
            "select name, ns, git_url, git_ref, port, instances, live_release, public, idle_stop, recycle, last_request, http, env, secrets
             from apps where name in ({marks}) order by name"
        ),
        &params,
    )?;
    let releases = db.query(
        &format!(
            "select app, n, commit_sha, state, created, who, finished from
               (select *, row_number() over (partition by app order by n desc) as k from releases where app in ({marks}))
             where k <= 20 order by app, n desc"
        ),
        &params,
    )?;
    let instances = db.query(
        &format!(
            "select i.app, i.release, i.sandbox, i.node, i.endpoint, i.state, coalesce(s.created, 0) from instances i
             left join sandboxes s on s.id = i.sandbox where i.app in ({marks}) and i.state <> 'removed'"
        ),
        &params,
    )?;
    let since = now() / 60 - 60;
    let mut tparams = params.clone();
    tparams.push(&since);
    let traffic = db.query(
        &format!(
            "select app, minute, requests, errors, p50, p95 from traffic where app in ({marks}) and minute >= ?{} order by app, minute",
            keys.len() + 1
        ),
        &tparams,
    )?;
    drop(db);
    Ok(apps
        .iter()
        .map(|r| {
            let name: String = r.get(0);
            let secrets: Value = serde_json::from_str(&r.get::<String>(13)).unwrap_or(json!({}));
            json!({
                "name": short_name(&name), "ns": r.get::<String>(1), "url": format!("http://{name}.{}", c.domain),
                "git": {"url": r.get::<String>(2), "ref": r.get::<String>(3)},
                "port": r.get::<i32>(4), "instances_wanted": r.get::<i32>(5), "live_release": r.get::<Option<i32>>(6),
                "public": r.get::<bool>(7), "idle_stop_secs": r.get::<i64>(8), "recycle_secs": r.get::<i64>(9),
                "last_request": r.get::<i64>(10), "http": serde_json::from_str::<Value>(&r.get::<String>(11)).unwrap_or(json!({})),
                "env": serde_json::from_str::<Value>(&r.get::<String>(12)).unwrap_or(json!({})),
                "secrets": secrets.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default(),
                "releases": releases.iter().filter(|x| x.get::<String>(0) == name).map(|x| json!({
                    "n": x.get::<i32>(1), "commit": x.get::<String>(2), "state": x.get::<String>(3), "created": x.get::<i64>(4),
                    "who": x.get::<String>(5), "finished": x.get::<Option<i64>>(6),
                })).collect::<Vec<_>>(),
                "instances": instances.iter().filter(|x| x.get::<String>(0) == name).map(|x| json!({
                    "release": x.get::<i32>(1), "sandbox": x.get::<String>(2), "node": x.get::<String>(3),
                    "endpoint": x.get::<String>(4), "state": x.get::<String>(5), "created": x.get::<i64>(6),
                })).collect::<Vec<_>>(),
                "traffic": traffic.iter().filter(|x| x.get::<String>(0) == name).map(|x| json!({
                    "minute": x.get::<i64>(1), "requests": x.get::<i64>(2), "errors": x.get::<i64>(3),
                    "p50_ms": x.get::<i64>(4), "p95_ms": x.get::<i64>(5),
                })).collect::<Vec<_>>(),
            })
        })
        .collect())
}

/// Top client IPs over the last 10 minutes of edge reports, busiest first.
fn edge_clients(c: &Controller) -> Value {
    let mut sum: HashMap<String, (i64, i64)> = HashMap::new();
    for ((_, ip), (n, l)) in c.clients.lock().unwrap().iter() {
        let e = sum.entry(ip.clone()).or_default();
        e.0 += n;
        e.1 += l;
    }
    let mut v: Vec<_> = sum.into_iter().collect();
    v.sort_by(|a, b| (b.1.0, &a.0).cmp(&(a.1.0, &b.0)));
    json!(v.iter().take(20).map(|(ip, (n, l))| json!({"ip": ip, "requests": n, "limited": l})).collect::<Vec<_>>())
}

/// Creates or updates app `b.name` in `ns`; returns its short name. The caller has checked
/// `who` may deploy in `ns`.
fn upsert_app(c: &Controller, ns: &str, who: &Caller, b: &Value, preview_of: Option<&str>) -> Result<String> {
    let short = b["name"].as_str().context("missing name")?.to_string();
    if !valid_label(&short) {
        bail!("app names are lowercase letters, digits and single dashes, at most 40");
    }
    let name = app_key(ns, &short);
    if name.len() > 63 {
        bail!("{name} is longer than a DNS label (63)");
    }
    let git_url = b["git"]["url"].as_str().context("missing git.url")?;
    let git_ref = b["git"]["ref"].as_str().unwrap_or("main");
    // 0: the commit's rekcod.toml decides (3000 if it doesn't say).
    let port = b["port"].as_i64().unwrap_or(0) as i32;
    // Left out (a deploy without --instances) keeps an existing app's count.
    let existing: Option<i32> = c.db.lock().unwrap().query_opt("select instances from apps where name = ?1", &[&name])?.map(|r| r.get(0));
    let instances = b["instances"].as_i64().map(|n| n as i32).or(existing).unwrap_or(1);
    let cpus = b["cpus"].as_i64().unwrap_or(1) as i32;
    let mem = b["memory_gib"].as_i64().unwrap_or(1) as i32;
    let health = b["health"].as_str().unwrap_or("/");
    let start = b["start"].as_str();
    let env = b.get("env").filter(|e| e.is_object()).map(|e| e.to_string());
    // None keeps an existing app's visibility; new apps are private.
    let public = b["public"].as_bool();
    // None keeps the current setting; 0 is off.
    let idle_stop = b["idle_stop_secs"].as_i64();
    let recycle = b["recycle_secs"].as_i64();
    let http = match b.get("http").filter(|h| h.is_object()) {
        Some(h) => {
            let s: HttpSettings = serde_json::from_value(h.clone()).context("http")?;
            if let Some(r) = &s.rate {
                crate::edge::limit::parse_rate(r)?;
            }
            Some(serde_json::to_string(&s)?)
        }
        None => None,
    };
    let exists = c.db.lock().unwrap().query_opt("select owner, ns from apps where name = ?1", &[&name])?;
    if exists.is_none() {
        let (n, max): (i64, i64) = {
            let r =
                c.db.lock()
                    .unwrap()
                    .query_one("select (select count(*) from apps where ns = ?1), max_apps from namespaces where name = ?1", &[&ns])?;
            (r.get(0), r.get(1))
        };
        if max >= 0 && n >= max {
            bail!("quota: {ns} has {n} of {max} apps");
        }
    }
    c.check_quota(ns, &name, (instances * cpus) as i64, (instances * mem) as i64, 0)?;
    let mut db = c.db.lock().unwrap();
    if let Some(r) = exists {
        let (owner, row_ns): (String, String) = (r.get(0), r.get(1));
        if row_ns != ns || !can_touch(who, ns, &owner) {
            bail!("app {short} belongs to someone else");
        }
        db.execute(
            "update apps set git_url = ?2, git_ref = ?3, port = ?4, instances = ?5, cpus = ?6, memory_gib = ?7, health = ?8,
             start = ?9, env = coalesce(?10, env), public = coalesce(?11, public), idle_stop = coalesce(?12, idle_stop),
             recycle = coalesce(?13, recycle), http = coalesce(?14, http) where name = ?1",
            &[&name, &git_url, &git_ref, &port, &instances, &cpus, &mem, &health, &start, &env, &public, &idle_stop, &recycle, &http],
        )?;
    } else {
        db.execute(
            "insert into apps (name, owner, git_url, git_ref, port, instances, cpus, memory_gib, health, start, env, preview_of, created, public,
               idle_stop, recycle, ns, http)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, coalesce(?11, '{}'), ?12, ?13, coalesce(?14, false), coalesce(?15, 0), coalesce(?16, 0), ?17,
               coalesce(?18, '{}'))",
            &[&name, &who.name, &git_url, &git_ref, &port, &instances, &cpus, &mem, &health, &start, &env, &preview_of, &now(), &public, &idle_stop, &recycle, &ns, &http],
        )?;
    }
    if let Some(on) = b["bucket"].as_bool() {
        db.execute("update apps set bucket = ?2 where name = ?1", &[&name, &on])?;
    }
    Ok(short)
}

fn apps_route(c: &Arc<Controller>, who: &Caller, ns: &str, method: &str, seg: &[&str], b: &Value, q: &[(String, String)]) -> Result<Value> {
    let load = |name: &str, need: Role| load_app(c, ns, name, who, need);
    match (method, seg) {
        ("GET", ["apps"]) => {
            c.allow(who, ns, Role::Viewer)?;
            let rows = c.db.lock().unwrap().query(
                "select name, git_url, live_release, instances, preview_of, public,
                   exists(select 1 from instances where app = name and state = 'sleeping')
                   and not exists(select 1 from instances where app = name and state = 'live')
                 from apps where ns = ?1 and (?1 <> 'default' or owner = ?2 or ?3) order by name",
                &[&ns, &who.name, &who.admin],
            )?;
            let mut list: Vec<Value> = rows
                .iter()
                .map(|r| json!({
                    "name": short_name(&r.get::<String>(0)), "ns": ns, "git": r.get::<String>(1), "live_release": r.get::<Option<i32>>(2),
                    "instances": r.get::<i32>(3), "preview_of": r.get::<Option<String>>(4).map(|p| short_name(&p).to_string()),
                    "public": r.get::<bool>(5), "url": format!("http://{}.{}", r.get::<String>(0), c.domain), "sleeping": r.get::<bool>(6),
                }))
                .collect();
            // `?detail=1`: each app's `GET /apps/{name}` too, for the dashboard's one call.
            if q.iter().any(|(k, v)| k == "detail" && v != "0") {
                let keys: Vec<String> = rows.iter().map(|r| r.get(0)).collect();
                for (a, d) in list.iter_mut().zip(app_details(c, &keys)?) {
                    a["detail"] = d;
                }
            }
            Ok(json!(list))
        }
        // Create or update the app's config, then deploy it.
        ("POST", ["apps"]) => {
            c.allow(who, ns, Role::Member)?;
            let name = upsert_app(c, ns, who, b, None)?;
            deploy(c, &load(&name, Role::Member)?, None, &who.name)
        }
        ("GET", ["apps", name]) => app_status(c, &load(name, Role::Viewer)?),
        ("POST", ["apps", name, "visibility"]) => {
            let app = load(name, Role::Member)?;
            let public = b["public"].as_bool().context("missing public")?;
            c.db.lock().unwrap().execute("update apps set public = ?2 where name = ?1", &[&app.name, &public])?;
            c.audit(ns, &who.name, if public { "make public" } else { "make private" }, name, true);
            Ok(json!({"app": name, "ns": ns, "public": public}))
        }
        ("POST", ["apps", name, "deploy"]) => deploy(c, &load(name, Role::Member)?, b["ref"].as_str().map(String::from), &who.name),
        ("POST", ["apps", name, "rollback"]) => {
            let app = load(name, Role::Member)?;
            let target: Option<i32> = b["release"].as_i64().map(|n| n as i32);
            let row = c.db.lock().unwrap().query_opt(
                "select commit_sha from releases where app = ?1 and state in ('live', 'old') and n = coalesce(?2,
                   (select max(n) from releases where app = ?1 and n < (select live_release from apps where name = ?1) and state in ('live', 'old')))",
                &[&app.name, &target],
            )?;
            let commit: String = row.ok_or_else(|| anyhow!("no earlier good release of {name}"))?.get(0);
            c.db.lock().unwrap().execute("update releases set state = 'old' where app = ?1 and state = 'live'", &[&app.name])?;
            deploy(c, &app, Some(commit), &who.name)
        }
        ("POST", ["apps", name, "scale"]) => {
            let n = b["instances"].as_i64().context("missing instances")? as i32;
            let app = load(name, Role::Member)?;
            c.check_quota(ns, &app.name, (n * app.cpus) as i64, (n * app.memory_gib) as i64, 0)?;
            c.db.lock().unwrap().execute("update apps set instances = ?2 where name = ?1", &[&app.name, &n])?;
            let commit = live_commit(c, &app.name)?;
            deploy(c, &load(name, Role::Member)?, commit, &who.name)
        }
        ("POST", ["apps", name, "env"]) | ("POST", ["apps", name, "secrets"]) => {
            let app = load(name, Role::Member)?;
            let secrets = seg[2] == "secrets";
            let mut map = if secrets { app.secrets.clone() } else { app.env.clone() };
            for (k, v) in b["set"].as_object().into_iter().flatten() {
                map[k] = v.clone();
            }
            for k in b["unset"].as_array().into_iter().flatten().filter_map(|k| k.as_str()) {
                map.as_object_mut().map(|m| m.remove(k));
            }
            if secrets {
                for v in map.as_object_mut().into_iter().flat_map(|m| m.values_mut()) {
                    let plain = v.as_str().map(String::from).unwrap_or_else(|| v.to_string());
                    *v = json!(c.seal(ns, &plain)?);
                }
            }
            let col = if secrets { "secrets" } else { "env" };
            c.db.lock().unwrap().execute(&format!("update apps set {col} = ?2 where name = ?1"), &[&app.name, &map.to_string()])?;
            let Some(commit) = live_commit(c, &app.name)? else { return Ok(json!({"updated": col})) };
            deploy(c, &load(name, Role::Member)?, Some(commit), &who.name)
        }
        ("GET", ["apps", name, "releases", n]) => {
            let app = load(name, Role::Viewer)?;
            let n: i32 = n.parse().map_err(|_| anyhow!("release numbers are integers"))?;
            let r = c.db.lock().unwrap().query_opt(
                "select n, commit_sha, state, created, who, finished, log from releases where app = ?1 and n = ?2",
                &[&app.name, &n],
            )?;
            let r = r.ok_or_else(|| anyhow!("{name} has no release {n}"))?;
            Ok(json!({
                "app": name, "n": r.get::<i32>(0), "commit": r.get::<String>(1), "state": r.get::<String>(2), "created": r.get::<i64>(3),
                "who": r.get::<String>(4), "finished": r.get::<Option<i64>>(5), "log": r.get::<String>(6),
            }))
        }
        ("GET", ["apps", name, "instances", sb, "logs"]) => {
            let app = load(name, Role::Viewer)?;
            let node: String =
                c.db.lock()
                    .unwrap()
                    .query_opt("select node from instances where app = ?1 and sandbox = ?2", &[&app.name, sb])?
                    .ok_or_else(|| anyhow!("{name} has no instance {sb}"))?
                    .get(0);
            let (url, sec, _) = c.node(&node)?;
            let v = call(&url, "GET", &format!("/sandboxes/{sb}/logs"), Some(&sec), &Value::Null)?;
            Ok(json!({"logs": v["logs"].as_str().unwrap_or("")}))
        }
        ("GET", ["apps", name, "logs"]) => {
            let app = load(name, Role::Viewer)?;
            let rows = c.db.lock().unwrap().query("select node, sandbox from instances where app = ?1 and state = 'live'", &[&app.name])?;
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
        ("POST", ["apps", name, "wake"]) => {
            let app = load(name, Role::Member)?;
            Ok(json!({"app": name, "ns": ns, "endpoints": wake(c, &app)?}))
        }
        ("POST", ["apps", name, "recycle"]) => {
            let app = load(name, Role::Member)?;
            let commit = live_commit(c, &app.name)?.ok_or_else(|| anyhow!("{name} has no live release"))?;
            let lock = c.app_lock(&app.name);
            let _g = lock.lock().unwrap();
            deploy(c, &app, Some(commit), &who.name)
        }
        ("GET", ["apps", name, "storage"]) => app_storage(c, &load(name, Role::Viewer)?),
        ("POST", ["apps", name, "storage", "url"]) => {
            let app = load(name, Role::Member)?;
            let key = b["key"].as_str().context("missing key")?;
            let secs = b["secs"].as_u64().unwrap_or(3600);
            let (s, bucket) = app_s3(c, &app)?;
            Ok(json!({"url": s.presign(&bucket, key, secs)?, "expires_secs": secs}))
        }
        ("POST", ["apps", name, "storage", "rotate"]) => {
            let app = load(name, Role::Member)?;
            app_s3(c, &app)?;
            c.db.lock().unwrap().execute("update apps set storage_secret = null where name = ?1", &[&app.name])?;
            c.audit(ns, &who.name, "rotate storage key", name, true);
            let Some(commit) = live_commit(c, &app.name)? else {
                bucket_env(c, &app)?;
                return Ok(json!({"rotated": name}));
            };
            deploy(c, &app, Some(commit), &who.name)
        }
        ("DELETE", ["apps", name]) => {
            let app = load(name, Role::Member)?;
            if !b["force"].as_bool().unwrap_or(false)
                && let Some(n) = bucket_objects(c, &app.name)?.filter(|n| *n > 0)
            {
                bail!("{name}'s bucket {} has {n} object(s); destroy --force deletes them too", app_bucket(&app.name));
            }
            c.audit(ns, &who.name, "destroy", name, true);
            destroy_app(c, &app.name)?;
            Ok(json!({"destroyed": name, "ns": ns}))
        }
        _ => Err(anyhow!("no such endpoint")),
    }
}

// ------------------------------------------------------------- jobs

/// How much of a job's output the controller keeps once it's finished.
const JOB_LOG_BYTES: usize = 64 << 10;

/// Places a job and starts it on a node; returns `{id, node}`.
fn start_job(c: &Controller, ns: &str, owner: &str, max: i32, spec: &Value, schedule: Option<&str>) -> Result<Value> {
    if spec["cmd"].as_array().is_none_or(|a| a.is_empty()) {
        bail!("missing cmd");
    }
    if spec["git"]["url"].as_str().is_none() {
        bail!("jobs need git.url (and git.ref)");
    }
    let used: i64 =
        c.db.lock()
            .unwrap()
            .query_one(
                "select (select count(*) from sandboxes where owner = ?1 and state = 'running')
                  + (select count(*) from jobs where owner = ?1 and state = 'running')",
                &[&owner],
            )?
            .get(0);
    if max >= 0 && used >= max as i64 {
        bail!("quota: {used} of {max} sandboxes and jobs in use");
    }
    let gib = spec["memory_gib"].as_i64().unwrap_or(2);
    c.check_quota(ns, "", spec["cpus"].as_i64().unwrap_or(2), gib, 1)?;
    let mib = gib * 1024;
    let (node, url, sec) = c.place(mib, false, ns)?.into_iter().next().ok_or_else(|| anyhow!("no node has {mib} MiB free"))?;
    let id = format!("j-{}", sandbox::new_id()?);
    let mut body = spec.clone();
    body["id"] = json!(id);
    call(&url, "POST", "/jobs", Some(&sec), &body).with_context(|| format!("node {node}"))?;
    let name = spec["name"].as_str();
    c.db.lock().unwrap().execute(
        "insert into jobs (id, owner, node, name, spec, state, created, schedule, ns) values (?1, ?2, ?3, ?4, ?5, 'running', ?6, ?7, ?8)",
        &[&id, &owner, &node, &name, &spec.to_string(), &now(), &schedule, &ns],
    )?;
    c.audit(ns, owner, "job", &id, true);
    Ok(json!({"id": id, "node": node}))
}

/// Asks each running job's node how it's doing; stores the result once it's finished.
fn poll_jobs(c: &Controller) -> Result<()> {
    let rows = c.db.lock().unwrap().query("select id, node from jobs where state = 'running'", &[])?;
    for r in rows {
        let (id, node): (String, String) = (r.get(0), r.get(1));
        let Ok((url, sec, _)) = c.node(&node) else { continue };
        let Ok(v) = call(&url, "GET", &format!("/jobs/{id}"), Some(&sec), &Value::Null) else { continue };
        if v["running"].as_bool() != Some(false) {
            continue;
        }
        let state = if v["cancelled"].as_bool() == Some(true) {
            "cancelled"
        } else if v["exit"].as_i64() == Some(0) {
            "done"
        } else {
            "failed"
        };
        let (exit, log) = (v["exit"].as_i64(), v["log"].as_str().unwrap_or("").to_string());
        c.db.lock().unwrap().execute(
            "update jobs set state = ?2, exit = ?3, finished = ?4, log = ?5 where id = ?1 and state = 'running'",
            &[&id, &state, &exit, &now(), &log],
        )?;
    }
    Ok(())
}

/// Starts a schedule's next run once its last one has finished and `every` has passed.
fn run_schedules(c: &Controller) -> Result<()> {
    let rows = c.db.lock().unwrap().query(
        "select s.name, s.owner, s.spec, coalesce(t.max_sandboxes, 10), s.ns from schedules s
           left join (select name, max(max_sandboxes) as max_sandboxes from tokens group by name) t on t.name = s.owner
         where s.last_run <= ?1 - s.every
           and not exists(select 1 from jobs j where j.id = s.last_job and j.state = 'running')",
        &[&now()],
    )?;
    for r in rows {
        let (name, owner, spec, max, ns): (String, String, String, i32, String) = (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4));
        let spec: Value = serde_json::from_str(&spec)?;
        // Marked as run even when it can't start, so a full fleet isn't retried every 5 s.
        let job = start_job(c, &ns, &owner, max, &spec, Some(&name));
        let id = job.as_ref().ok().and_then(|v| v["id"].as_str().map(String::from));
        c.db.lock().unwrap().execute("update schedules set last_run = ?2, last_job = ?3 where name = ?1", &[&name, &now(), &id])?;
        if let Err(e) = job {
            eprintln!("rekcod: schedule {name}: {e:#}");
        }
    }
    Ok(())
}

fn job_json(r: &Row) -> Value {
    json!({
        "id": r.get::<String>(0), "owner": r.get::<String>(1), "node": r.get::<String>(2), "name": r.get::<Option<String>>(3),
        "cmd": serde_json::from_str::<Value>(&r.get::<String>(4)).map(|s| s["cmd"].clone()).unwrap_or(Value::Null),
        "state": r.get::<String>(5), "exit": r.get::<Option<i64>>(6), "created": r.get::<i64>(7),
        "finished": r.get::<Option<i64>>(8), "schedule": r.get::<Option<String>>(9).map(|s| short_name(&s).to_string()),
        "ns": r.get::<String>(10),
    })
}

const JOB_COLS: &str = "id, owner, node, name, spec, state, exit, created, finished, schedule, ns";

fn load_job(c: &Controller, ns: &str, id: &str, who: &Caller, need: Role) -> Result<(Value, String)> {
    c.allow(who, ns, need).map_err(|e| if e.to_string().starts_with("no ") { anyhow!("no job {id}") } else { e })?;
    let r = c.db.lock().unwrap().query_opt(&format!("select {JOB_COLS}, log from jobs where id = ?1 and ns = ?2"), &[&id, &ns])?;
    let r = r.ok_or_else(|| anyhow!("no job {id}"))?;
    if !can_touch(who, ns, &r.get::<String>(1)) {
        bail!("no job {id}");
    }
    Ok((job_json(&r), r.get(11)))
}

fn jobs_route(c: &Arc<Controller>, who: &Caller, ns: &str, method: &str, seg: &[&str], b: &Value, q: &[(String, String)]) -> Result<Value> {
    match (method, seg) {
        ("POST", ["jobs"]) => {
            c.allow(who, ns, Role::Member)?;
            let every = b["every_secs"].as_i64().unwrap_or(0);
            if every <= 0 {
                return start_job(c, ns, &who.name, who.max, b, None);
            }
            let name = b["name"].as_str().context("a schedule needs --name")?;
            if !valid_label(name) {
                bail!("schedule names are lowercase letters, digits and single dashes");
            }
            let key = app_key(ns, name);
            let mut db = c.db.lock().unwrap();
            if let Some(r) = db.query_opt("select owner from schedules where name = ?1", &[&key])?
                && !can_touch(who, ns, &r.get::<String>(0))
            {
                bail!("schedule {name} belongs to someone else");
            }
            db.execute(
                "insert into schedules (name, owner, spec, every, ns) values (?1, ?2, ?3, ?4, ?5)
                 on conflict (name) do update set spec = ?3, every = ?4",
                &[&key, &who.name, &b.to_string(), &every, &ns],
            )?;
            Ok(json!({"scheduled": name, "ns": ns, "every_secs": every}))
        }
        ("GET", ["jobs"]) => {
            c.allow(who, ns, Role::Viewer)?;
            let mut db = c.db.lock().unwrap();
            // `?name=N`: the last 50 runs of one named or scheduled job.
            let name = q.iter().find(|(k, _)| k == "name").map(|(_, v)| v.as_str());
            let jobs = db.query(
                &format!(
                    "select {JOB_COLS} from jobs where ns = ?1 and (?1 <> 'default' or owner = ?2 or ?3)
                       and (?4 is null or name = ?4 or schedule = ?5) order by created desc limit 50"
                ),
                &[&ns, &who.name, &who.admin, &name, &name.map(|n| app_key(ns, n))],
            )?;
            let sch = db.query(
                "select name, owner, every, last_run, last_job from schedules where ns = ?1 and (?1 <> 'default' or owner = ?2 or ?3) order by name",
                &[&ns, &who.name, &who.admin],
            )?;
            Ok(json!({
                "jobs": jobs.iter().map(job_json).collect::<Vec<_>>(),
                // The watchdog starts it on the first tick after `next_run` once the last run has finished.
                "schedules": sch.iter().map(|r| json!({
                    "name": short_name(&r.get::<String>(0)), "owner": r.get::<String>(1), "every_secs": r.get::<i64>(2),
                    "last_run": r.get::<i64>(3), "last_job": r.get::<Option<String>>(4),
                    "next_run": (r.get::<i64>(3) + r.get::<i64>(2)).max(now()),
                })).collect::<Vec<_>>(),
            }))
        }
        ("GET", ["jobs", id]) => {
            let (mut v, log) = load_job(c, ns, id, who, Role::Viewer)?;
            v["log"] = json!(log);
            if v["state"] == "running" {
                let node = v["node"].as_str().unwrap_or("").to_string();
                if let Ok(live) =
                    c.node(&node).and_then(|(url, sec, _)| call(&url, "GET", &format!("/jobs/{id}"), Some(&sec), &Value::Null))
                {
                    v["log"] = live["log"].clone();
                }
            }
            Ok(v)
        }
        // The same spec again as a one-off job (a schedule's run doesn't renew the schedule).
        ("POST", ["jobs", id, "rerun"]) => {
            let (v, _) = load_job(c, ns, id, who, Role::Member)?;
            if v["state"] == "running" {
                bail!("job {id} is still running");
            }
            let spec: String = c.db.lock().unwrap().query_one("select spec from jobs where id = ?1", &[id])?.get(0);
            let mut spec: Value = serde_json::from_str(&spec)?;
            spec.as_object_mut().map(|o| o.remove("every_secs"));
            start_job(c, ns, &who.name, who.max, &spec, None)
        }
        ("POST", ["jobs", id, "cancel"]) => {
            let (v, _) = load_job(c, ns, id, who, Role::Member)?;
            if v["state"] != "running" {
                bail!("job {id} is {}", v["state"].as_str().unwrap_or(""));
            }
            let (url, sec, _) = c.node(v["node"].as_str().unwrap_or(""))?;
            call(&url, "DELETE", &format!("/jobs/{id}"), Some(&sec), &Value::Null)?;
            c.db.lock().unwrap().execute("update jobs set state = 'cancelled', finished = ?2 where id = ?1", &[id, &now()])?;
            c.audit(ns, &who.name, "cancel", id, true);
            Ok(json!({"cancelled": id}))
        }
        ("DELETE", ["schedules", name]) => {
            c.allow(who, ns, Role::Member).map_err(|_| anyhow!("no schedule {name}"))?;
            let n = c.db.lock().unwrap().execute(
                "delete from schedules where name = ?1 and ns = ?2 and (?2 <> 'default' or owner = ?3 or ?4)",
                &[&app_key(ns, name), &ns, &who.name, &who.admin],
            )?;
            if n == 0 {
                bail!("no schedule {name}");
            }
            Ok(json!({"unscheduled": name}))
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
    use crate::sandbox_api::ct_eq;
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
        "select name, owner, ns from apps where preview_of is null and (git_url = ?1 or git_url = ?2 or git_url = ?3) limit 1",
        &[&urls[0], &urls[1], &format!("{}.git", urls[1])],
    )?;
    let Some(base) = base else { return Ok(json!({"ignored": "no app for this repository"})) };
    let (base, owner, ns): (String, String, String) = (base.get(0), base.get(1), base.get(2));
    let preview = format!("{}-pr-{pr}", short_name(&base));
    // Previews belong to the base app's owner.
    let who = Caller { name: owner, ..Caller::system("github") };
    match b["action"].as_str().unwrap_or("") {
        "closed" => destroy_app(c, &app_key(&ns, &preview)),
        "opened" | "synchronize" | "reopened" => {
            let app = load_app_key(c, &base)?.context("base app gone")?;
            let head = b["pull_request"]["head"]["sha"].as_str().context("no head sha")?;
            let head_repo = b["pull_request"]["head"]["repo"]["clone_url"].as_str().unwrap_or(&app.git_url);
            let cfg = json!({
                "name": preview, "git": {"url": head_repo, "ref": head}, "port": app.port, "instances": 1,
                "cpus": app.cpus, "memory_gib": app.memory_gib, "health": app.health, "start": app.start, "env": app.env,
                // Previews sleep after 15 minutes unless the base app picks its own idle time.
                "idle_stop_secs": if app.idle_stop > 0 { app.idle_stop } else { 900 },
                // Its own empty bucket, deleted with the preview.
                "bucket": c.db.lock().unwrap().query_one("select bucket from apps where name = ?1", &[&base])?.get::<bool>(0),
            });
            upsert_app(c, &ns, &who, &cfg, Some(&base))?;
            let v = deploy(c, &load_app(c, &ns, &preview, &who, Role::Member)?, Some(head.into()), "github")?;
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

// ------------------------------------------------------------- storage (docs/rustfs.md)

const RELEASES: &str = "rekcod-releases";
const CACHE: &str = "rekcod-cache";
const BACKUPS: &str = "rekcod-backups";
const KEEP_BACKUPS: usize = 48;

/// `app-<app key>`; a hash of the key when that isn't a valid bucket name (too long).
pub fn app_bucket(app: &str) -> String {
    let b = format!("app-{app}");
    if crate::s3::valid_bucket(&b) {
        return b;
    }
    use sha2::Digest;
    format!("app-{}", &hex(&sha2::Sha256::digest(app.as_bytes()))[..32])
}

impl Controller {
    fn storage_or_err(&self) -> Result<&crate::s3::S3> {
        self.storage.as_ref().context("the controller has no storage; start it with --storage URL (see `rekcod storage serve`)")
    }

    /// `--storage URL` with the keys in `creds` (saved to fleet.db), else what fleet.db has.
    fn load_storage(&self, url: Option<&str>, creds: &Path) -> Result<Option<crate::s3::S3>> {
        let s = match url {
            Some(url) => {
                let mut s = crate::storage::read_credentials(creds)?;
                s.endpoint = url.trim_end_matches('/').to_string();
                let sealed = self.seal("default", &s.secret_key)?;
                let mut db = self.db.lock().unwrap();
                for (k, v) in [("storage_url", &s.endpoint), ("storage_access_key", &s.access_key), ("storage_secret_key", &sealed)] {
                    db.execute("insert or replace into config values (?1, ?2)", &[&k, v])?;
                }
                s
            }
            None => {
                let (Some(endpoint), Some(access_key), Some(sealed)) =
                    (self.config("storage_url")?, self.config("storage_access_key")?, self.config("storage_secret_key")?)
                else {
                    return Ok(None);
                };
                crate::s3::S3 { endpoint, access_key, secret_key: self.open_sealed("default", &sealed)? }
            }
        };
        // RustFS may come up after the controller at boot; deploys retry this.
        for b in [RELEASES, CACHE, BACKUPS] {
            if let Err(e) = s.create_bucket(b) {
                eprintln!("rekcod: storage {}: {e:#}", s.endpoint);
                break;
            }
        }
        eprintln!("rekcod: storage on {}", s.endpoint);
        Ok(Some(s))
    }
}

/// For a node: where the image of `app` at `commit` goes, and whether it's there already.
fn release_store(c: &Controller, app: &str, commit: &str) -> Value {
    let Some(s) = &c.storage else { return Value::Null };
    let key = format!("{app}/{commit}");
    let cached = s
        .create_bucket(RELEASES)
        .and_then(|_| s.create_bucket(CACHE))
        .and_then(|_| s.exists(RELEASES, &format!("{key}.json")))
        .unwrap_or(false);
    json!({"s3": s, "bucket": RELEASES, "key": key, "cached": cached, "cache": CACHE})
}

/// The bucket and its key of an app with `bucket = true`, created on first use; the env its
/// instances get. The user and policy are written on every deploy, so a RustFS that lost
/// them (reset, restored) gets them back. Nothing for apps without a bucket.
fn bucket_env(c: &Controller, app: &App) -> Result<Vec<(&'static str, String)>> {
    let r = c.db.lock().unwrap().query_one("select bucket, storage_key, storage_secret from apps where name = ?1", &[&app.name])?;
    let (on, key, sealed): (bool, Option<String>, Option<String>) = (r.get(0), r.get(1), r.get(2));
    if !on {
        return Ok(vec![]);
    }
    let s = c.storage_or_err().context("the app has [storage] bucket = true")?;
    let bucket = app_bucket(&app.name);
    s.create_bucket(&bucket)?;
    let key = match key {
        Some(k) => k,
        None => format!("app-{}", sandbox::new_id()?),
    };
    let secret_key = match sealed {
        Some(sec) => c.open_sealed(&app.ns, &sec)?,
        None => secret()?,
    };
    s.add_bucket_user(&key, &secret_key, &bucket)?;
    let sealed = c.seal(&app.ns, &secret_key)?;
    c.db.lock().unwrap().execute("update apps set storage_key = ?2, storage_secret = ?3 where name = ?1", &[&app.name, &key, &sealed])?;
    Ok(vec![
        ("S3_ENDPOINT", s.endpoint.clone()),
        ("S3_BUCKET", bucket),
        ("AWS_ACCESS_KEY_ID", key),
        ("AWS_SECRET_ACCESS_KEY", secret_key),
        ("AWS_REGION", crate::s3::REGION.into()),
    ])
}

/// The app's bucket with root keys, for the CLI's `app storage`.
fn app_s3(c: &Controller, app: &App) -> Result<(crate::s3::S3, String)> {
    let on: bool = c.db.lock().unwrap().query_one("select bucket from apps where name = ?1", &[&app.name])?.get(0);
    if !on {
        bail!("{} has no bucket; set `[storage] bucket = true` in rekcod.toml and deploy", app.short);
    }
    Ok((c.storage_or_err()?.clone(), app_bucket(&app.name)))
}

fn app_storage(c: &Controller, app: &App) -> Result<Value> {
    let (s, bucket) = app_s3(c, app)?;
    let key: Option<String> = c.db.lock().unwrap().query_one("select storage_key from apps where name = ?1", &[&app.name])?.get(0);
    let objects = if s.bucket_exists(&bucket)? { s.list(&bucket, "")? } else { vec![] };
    Ok(json!({
        "app": app.short, "endpoint": s.endpoint, "bucket": bucket, "access_key": key,
        "objects": objects.len(), "bytes": objects.iter().map(|o| o.size).sum::<u64>(), "list": objects,
    }))
}

/// How many objects the app's bucket holds; None when it has no bucket.
fn bucket_objects(c: &Controller, app: &str) -> Result<Option<usize>> {
    let on: bool = c.db.lock().unwrap().query_one("select bucket from apps where name = ?1", &[&app])?.get(0);
    let Some(s) = c.storage.as_ref().filter(|_| on) else { return Ok(None) };
    let bucket = app_bucket(app);
    if !s.bucket_exists(&bucket)? {
        return Ok(Some(0));
    }
    Ok(Some(s.list(&bucket, "")?.len()))
}

/// Deletes the app's bucket (objects too), its user and its policy.
fn drop_bucket(c: &Controller, app: &str) -> Result<()> {
    let r = c.db.lock().unwrap().query_opt("select bucket, storage_key from apps where name = ?1", &[&app])?;
    let Some(r) = r else { return Ok(()) };
    let (on, key): (bool, Option<String>) = (r.get(0), r.get(1));
    if !on && key.is_none() {
        return Ok(());
    }
    let s = c.storage_or_err()?;
    let bucket = app_bucket(app);
    s.delete_bucket(&bucket, true)?;
    if let Some(k) = key {
        let _ = s.remove_user(&k);
    }
    let _ = s.remove_policy(&bucket);
    Ok(())
}

/// Uploads `fleet.db` (a `VACUUM INTO` copy, so nothing stops) as `fleet-<UTC time>.db` and
/// keeps the newest 48. `controller/secret-key` isn't in it: keep that somewhere else.
fn backup(c: &Controller) -> Result<Value> {
    let s = c.storage_or_err()?;
    s.create_bucket(BACKUPS)?;
    let tmp = std::env::temp_dir().join(format!("rekcod-fleet-{}.db", sandbox::new_id()?));
    let path = tmp.display().to_string();
    let r = (|| -> Result<Value> {
        c.db.lock().unwrap().execute("vacuum into ?1", &[&path])?;
        let key = format!("fleet-{}.db", crate::s3::amz_date(now() as u64));
        s.put(BACKUPS, &key, crate::s3::Body::File(&tmp))?;
        let mut all: Vec<String> = s.list(BACKUPS, "fleet-")?.into_iter().map(|o| o.key).collect();
        all.sort();
        let old = all.len().saturating_sub(KEEP_BACKUPS);
        for k in &all[..old] {
            s.delete(BACKUPS, k)?;
        }
        Ok(json!({"backup": key, "bucket": BACKUPS, "kept": all.len() - old}))
    })();
    let _ = std::fs::remove_file(&tmp);
    r
}

/// Hourly. The attempt is recorded first, so a failing upload is retried in an hour, not
/// every watchdog tick.
fn backup_due(c: &Controller) -> Result<()> {
    if c.storage.is_none() {
        return Ok(());
    }
    let last: i64 = c.config("last_backup")?.and_then(|v| v.parse().ok()).unwrap_or(0);
    if now() - last < 3600 {
        return Ok(());
    }
    c.db.lock().unwrap().execute("insert or replace into config values ('last_backup', ?1)", &[&now().to_string()])?;
    backup(c).map(|_| ())
}

/// `rekcod controller restore`: puts a backup in place of fleet.db (the old file is kept
/// next to it). The controller must be stopped; it needs the same `controller/secret-key`.
pub fn controller_restore(paths: &Paths, db: Option<PathBuf>, key: Option<&str>, creds: Option<PathBuf>) -> Result<()> {
    let s = crate::storage::read_credentials(&creds.unwrap_or_else(|| crate::storage::credentials_file(paths)))?;
    let key = match key {
        Some(k) => k.to_string(),
        None => {
            let mut all: Vec<String> = s.list(BACKUPS, "fleet-")?.into_iter().map(|o| o.key).collect();
            all.sort();
            all.pop().context("no backups in rekcod-backups")?
        }
    };
    let db = db.unwrap_or_else(|| paths.home.join("controller").join("fleet.db"));
    std::fs::create_dir_all(db.parent().context("no parent")?)?;
    let tmp = db.with_extension("restoring");
    s.get(BACKUPS, &key, &mut std::fs::File::create(&tmp)?)?;
    Db::open(&tmp).context("the backup isn't a usable database")?;
    if db.exists() {
        let keep = db.with_extension(format!("db.before-restore-{}", now()));
        std::fs::rename(&db, &keep)?;
        eprintln!("rekcod: the old database is at {}", keep.display());
    }
    for ext in ["db-wal", "db-shm"] {
        let _ = std::fs::remove_file(db.with_extension(ext));
    }
    std::fs::rename(&tmp, &db)?;
    println!("restored {key} to {}; start the controller again", db.display());
    Ok(())
}

pub struct ControllerOptions {
    /// SQLite file (default `~/.rekcod/controller/fleet.db`).
    pub db: Option<PathBuf>,
    pub listen: String,
    pub domain: String,
    pub drain_secs: u64,
    /// RustFS URL (`rekcod storage serve`); remembered in fleet.db once given.
    pub storage: Option<String>,
    /// Its root keys (default `~/.rekcod/storage/credentials.json`, written by `storage serve`).
    pub storage_credentials: Option<PathBuf>,
}

/// `controller/secret-key`: 32 random bytes, created on first run. App secrets in fleet.db
/// can't be read without it, so back it up separately from the database.
fn secret_key(path: &Path) -> Result<[u8; 32]> {
    if let Ok(b) = std::fs::read(path) {
        return b.try_into().map_err(|_| anyhow!("{} isn't 32 bytes", path.display()));
    }
    let k: [u8; 32] = random_bytes()?;
    std::fs::write(path, k)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(k)
}

/// Starts the controller; on first run creates the schema, an admin token and a join token.
pub fn controller_serve(paths: &Paths, o: ControllerOptions) -> Result<()> {
    let dir = paths.home.join("controller");
    std::fs::create_dir_all(&dir)?;
    let mut db = Db::open(&o.db.unwrap_or_else(|| dir.join("fleet.db")))?;
    for (key, file) in [("join_token", "join-token"), ("webhook_secret", "webhook-secret")] {
        let v = match db.query_opt("select value from config where key = ?1", &[&key])? {
            Some(r) => r.get::<String>(0),
            None => {
                let v = secret()?;
                db.execute("insert into config values (?1, ?2)", &[&key, &v])?;
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
            db.execute("insert into tokens values (?1, 'admin', true, 100)", &[&t])?;
            t
        }
    };
    std::fs::write(dir.join("admin-token"), &admin)?;
    use std::os::unix::fs::PermissionsExt;
    for f in ["join-token", "webhook-secret", "admin-token"] {
        std::fs::set_permissions(dir.join(f), std::fs::Permissions::from_mode(0o600))?;
    }
    let key = secret_key(&dir.join("secret-key"))?;
    let mut ctl = Controller::new(db, o.domain, o.drain_secs, key);
    let creds = o.storage_credentials.unwrap_or_else(|| crate::storage::credentials_file(paths));
    ctl.storage = ctl.load_storage(o.storage.as_deref(), &creds)?;
    let c = Arc::new(ctl);
    c.seal_plain_secrets()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_sources_that_run_commands_are_refused() {
        for (url, rev) in [
            ("--upload-pack=touch /tmp/x", "main"),
            ("https://github.com/a/b", "--upload-pack=touch /tmp/x"),
            ("ext::sh -c touch% /tmp/x", "main"),
            ("https://github.com/a/b", "main:refs/heads/x"),
            ("https://github.com/a/b", "a..b"),
            ("https://github.com/a b", "main"),
        ] {
            assert!(check_git_source(url, rev).is_err(), "{url} {rev}");
        }
        for url in ["https://github.com/a/b.git", "ssh://git@host/a.git", "git@github.com:a/b.git", "http://git.tailnet:3000/a/b"] {
            assert!(check_git_source(url, "main").is_ok(), "{url}");
        }
        if std::env::var_os(ALLOW_LOCAL_GIT).is_none() {
            assert!(check_git_source("/Users/me/repo", "main").is_err());
            assert!(check_git_source("file:///Users/me/repo", "main").is_err());
        }
    }

    #[test]
    fn remote_creates_drop_node_local_fields() {
        let b = create_body(&json!({
            "git": {"url": "https://x/y", "ref": "main"}, "name": "n", "cpus": 2,
            "dir": "/Users/owner", "mount": true, "golden": "..", "secrets": ["GITHUB_TOKEN"], "reach": ["10.0.0.1:22"],
        }));
        assert_eq!(b, json!({"git": {"url": "https://x/y", "ref": "main"}, "name": "n", "cpus": 2}));
    }

    #[test]
    fn storage_names_and_reach() {
        assert_eq!(app_bucket("web--team"), "app-web--team");
        let long = app_bucket(&format!("{}--{}", "a".repeat(40), "b".repeat(20)));
        assert!(crate::s3::valid_bucket(&long) && long.len() == 36, "{long}");
        assert_eq!(storage_reach(Some("http://127.0.0.1:7711")), ["127.0.0.1:7711"]);
        assert!(storage_reach(None).is_empty());
        assert_eq!(crate::daemon::reach_arg("100.64.0.1:7710, 10.0.0.2:9000").unwrap(), "100.64.0.1:7710,10.0.0.2:9000");
        // Nothing but addresses reaches the kernel command line.
        assert!(crate::daemon::reach_arg("1.2.3.4:5 rekcod.sandbox=0").is_err());
    }

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
    fn urls() {
        assert_eq!(authority("http://10.0.0.2:7701/").unwrap(), "10.0.0.2:7701");
        assert!(authority("https://x").is_err());
        assert_eq!(percent_encode("/a b"), "%2Fa%20b");
    }

    #[test]
    fn sqlite_store_placement_and_returning() {
        let mut db = Db::open(Path::new(":memory:")).unwrap();
        for (id, free, mac) in [("n1", 4096i64, 0i32), ("n2", 16384, 2)] {
            db.execute(
                "insert into nodes (id, url, secret, mem_free, last_seen, macos_free) values (?1, ?2, 's', ?3, ?4, ?5)",
                &[&id, &format!("http://{id}"), &free, &now(), &mac],
            )
            .unwrap();
        }
        let c = Controller::new(db, "apps.test".into(), 0, [7; 32]);
        let ids = |v: Vec<(String, String, String)>| v.into_iter().map(|n| n.0).collect::<Vec<_>>();
        assert_eq!(ids(c.place(2048, false, "default").unwrap()), ["n2", "n1"]);
        assert_eq!(ids(c.place(8192, false, "default").unwrap()), ["n2"]);
        assert_eq!(ids(c.place(2048, true, "default").unwrap()), ["n2"]);
        // ?N binds by number, whatever order the SQL mentions them in.
        c.db.lock().unwrap().execute("update nodes set state = ?2 where id = ?1", &[&"n1", &"drain"]).unwrap();
        assert_eq!(ids(c.place(2048, false, "default").unwrap()), ["n2"]);
        let lost = c.db.lock().unwrap().query("update nodes set state = 'lost' where mem_free < ?1 returning id", &[&10000i64]).unwrap();
        assert_eq!(lost.len(), 1);
        assert_eq!(lost[0].get::<String>(0), "n1");
        let row = c.db.lock().unwrap().query_one("select count(*), max(state = 'lost') from nodes", &[]).unwrap();
        assert_eq!((row.get::<i64>(0), row.get::<bool>(1)), (2, true));
    }

    #[test]
    fn sleeping_apps_route_without_endpoints() {
        let mut db = Db::open(Path::new(":memory:")).unwrap();
        // A second open of the same schema reruns the migrations; they must not fail.
        for m in MIGRATIONS {
            let _ = db.execute(m, &[]);
        }
        for (app, sb, state) in [("web", "s1", "live"), ("web", "s2", "live"), ("cron", "s3", "sleeping"), ("old", "s4", "removed")] {
            db.execute(
                "insert or ignore into apps (name, owner, git_url, git_ref, port, created) values (?1, 'a', 'u', 'main', 80, 0)",
                &[&app],
            )
            .unwrap();
            db.execute("insert into instances values (?1, 1, ?2, 'n1', ?3, ?4)", &[&app, &sb, &format!("10.0.0.1:{}", sb.len()), &state])
                .unwrap();
        }
        let c = Controller::new(db, "apps.test".into(), 0, [7; 32]);
        let r: HashMap<String, Route> = serde_json::from_value(routes(&c).unwrap()).unwrap();
        assert_eq!(r.len(), 2, "removed instances don't route");
        assert_eq!((r["web.apps.test"].endpoints.len(), r["web.apps.test"].sleeping), (2, false));
        let cron = &r["cron.apps.test"];
        assert_eq!((cron.app.as_str(), cron.endpoints.len(), cron.sleeping), ("cron", 0, true));
        assert!(job_dir(&Paths { home: "/tmp/x".into() }, "j-abc123").is_ok());
        assert!(job_dir(&Paths { home: "/tmp/x".into() }, "j-../../etc").is_err());
    }

    #[test]
    fn namespaces_roles_quotas_pools_and_secrets() {
        let db = Db::open(Path::new(":memory:")).unwrap();
        let c = Controller::new(db, "apps.test".into(), 0, [7; 32]);
        let (alice, bob, admin) = (
            Caller { name: "alice".into(), admin: false, max: 10 },
            Caller { name: "bob".into(), admin: false, max: 10 },
            Caller::system("x"),
        );
        {
            let mut db = c.db.lock().unwrap();
            db.execute("insert into namespaces (name, created, salt, max_cpus, max_memory_gib) values ('team', 0, x'01', 4, 8)", &[])
                .unwrap();
            db.execute("insert into members values ('team', 'alice', 'member'), ('team', 'bob', 'viewer')", &[]).unwrap();
        }
        // Everyone is in default; named namespaces need a membership, and hide themselves otherwise.
        assert!(c.allow(&bob, "default", Role::Member).is_ok());
        assert!(c.allow(&alice, "team", Role::Member).is_ok());
        assert!(c.allow(&bob, "team", Role::Viewer).is_ok());
        assert!(c.allow(&bob, "team", Role::Member).unwrap_err().to_string().contains("member role"));
        let carol = Caller { name: "carol".into(), admin: false, max: 10 };
        assert!(c.allow(&carol, "team", Role::Viewer).unwrap_err().to_string().starts_with("no namespace"));
        assert!(c.allow(&admin, "team", Role::Owner).is_ok());
        assert!(c.allow(&admin, "nope", Role::Viewer).is_err());
        assert!(!can_touch(&bob, "default", "alice") && can_touch(&bob, "team", "alice"));

        assert_eq!((app_key("default", "web"), app_key("team", "web")), ("web".to_string(), "web--team".to_string()));
        assert_eq!((short_name("web--team"), short_name("web-pr-7")), ("web", "web-pr-7"));
        assert!(valid_label("web-2") && !valid_label("a--b") && !valid_label("-a") && !valid_label("Web") && !valid_label(""));

        // 4 CPUs / 8 GiB: a deployed 2x1 CPU app leaves room for a 2-CPU sandbox, not a 3-CPU one.
        c.db.lock()
            .unwrap()
            .execute(
                "insert into apps (name, owner, git_url, git_ref, port, instances, cpus, memory_gib, created, live_release, ns)
                 values ('web--team', 'alice', 'u', 'main', 80, 2, 1, 2, 0, 1, 'team')",
                &[],
            )
            .unwrap();
        assert!(c.check_quota("team", "", 2, 2, 1).is_ok());
        assert!(c.check_quota("team", "", 3, 1, 1).unwrap_err().to_string().contains("5 of 4 CPUs"));
        // Redeploying the same app replaces its own share.
        assert!(c.check_quota("team", "web--team", 4, 8, 0).is_ok());
        assert!(c.check_quota("default", "", 3, 2, 1).is_ok(), "other namespaces don't count");
        c.db.lock().unwrap().execute("update namespaces set max_cpus = -1, max_sandboxes = -1 where name = 'team'", &[]).unwrap();
        assert!(c.check_quota("team", "", 1000, 2, 1000).is_ok(), "unlimited CPUs and sandboxes");
        assert!(c.check_quota("team", "", 1, 9, 0).is_err(), "memory is still limited");
        assert_eq!(limit(-1), json!("unlimited"));

        // Pooled nodes only take their namespace; the rest only take unpooled nodes.
        {
            let mut db = c.db.lock().unwrap();
            for (id, free) in [("n1", 8192i64), ("n2", 16384)] {
                db.execute("insert into nodes (id, url, secret, mem_free, last_seen) values (?1, 'u', 's', ?2, ?3)", &[&id, &free, &now()])
                    .unwrap();
            }
            db.execute("insert into node_pools values ('n2', 'team')", &[]).unwrap();
        }
        let ids = |v: Vec<(String, String, String)>| v.into_iter().map(|n| n.0).collect::<Vec<_>>();
        assert_eq!(ids(c.place(1024, false, "team").unwrap()), ["n2"]);
        assert_eq!(ids(c.place(1024, false, "default").unwrap()), ["n1"]);

        // Secrets: sealed per namespace; deleting the namespace's salt makes them unreadable.
        let sealed = c.seal("team", "s3cr3t").unwrap();
        assert!(sealed.starts_with(SEALED) && !sealed.contains("s3cr3t"));
        assert_eq!(c.open_sealed("team", &sealed).unwrap(), "s3cr3t");
        assert!(c.open_sealed("default", &sealed).is_err(), "another namespace's key doesn't open it");
        assert_eq!(c.open_sealed("team", "plain").unwrap(), "plain");
        c.db.lock().unwrap().execute("delete from namespaces where name = 'team'", &[]).unwrap();
        assert!(c.open_sealed("team", &sealed).is_err());

        let (node, edge) = (IpAddr::from([100, 64, 0, 2]), IpAddr::from([100, 64, 0, 9]));
        assert!(relay_allowed(IpAddr::from([127, 0, 0, 1]), node, &[]));
        assert!(relay_allowed(node, node, &[]));
        assert!(relay_allowed(edge, node, &[edge]));
        assert!(!relay_allowed(IpAddr::from([100, 64, 0, 7]), node, &[edge]), "a tailnet peer that isn't an edge");
        assert!(relay_allowed("::ffff:100.64.0.9".parse().unwrap(), node, &[edge]));
    }

    #[test]
    fn github_signatures() {
        // From GitHub's docs ("Validating webhook deliveries").
        assert_eq!(
            github_signature("It's a Secret to Everybody", b"Hello, World!"),
            "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17"
        );
        assert!(
            crate::sandbox_api::ct_eq(b"abc", b"abc")
                && !crate::sandbox_api::ct_eq(b"abc", b"abd")
                && !crate::sandbox_api::ct_eq(b"ab", b"abc")
        );
    }
}
