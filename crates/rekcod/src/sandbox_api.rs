//! The sandbox API for agents (docs/agent-sandboxes.md): HTTP on a unix socket
//! (`rekcod sandbox serve`) and MCP over stdio (`rekcod mcp`). Both call `op`, which
//! calls `sandbox.rs`, so they offer what the CLI offers.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::paths::Paths;
use crate::sandbox::{self, CreateOptions, ExecOptions};

fn strs(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(|s| s.as_str().map(String::from)).collect()
}

/// A boolean argument: JSON `true`/`false`, or the strings `"true"`/`"false"` a query string
/// carries. Anything else that is present is an error, so `?check=1` can't apply for real.
fn flag(args: &Value, key: &str) -> Result<bool> {
    match &args[key] {
        Value::Null => Ok(false),
        Value::Bool(b) => Ok(*b),
        Value::String(s) if s == "true" => Ok(true),
        Value::String(s) if s == "false" => Ok(false),
        v => bail!("{key} takes true or false, not {v}"),
    }
}

/// A whole-number argument, as a JSON number or a query string's digits.
fn num(args: &Value, key: &str) -> Result<Option<u64>> {
    match &args[key] {
        Value::Null => Ok(None),
        Value::Number(n) if n.is_u64() => Ok(n.as_u64()),
        Value::String(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => Ok(Some(s.parse()?)),
        v => bail!("{key} takes a whole number, not {v}"),
    }
}

/// A string argument; another JSON type is an error rather than absent.
fn text<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match &args[key] {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s)),
        v => bail!("{key} takes a string, not {v}"),
    }
}

fn id_of(args: &Value) -> Result<String> {
    args["id"].as_str().map(String::from).ok_or_else(|| anyhow!("missing id"))
}

/// What callers may ask for, fixed when `serve`, `mcp` or a fleet node starts. The caller
/// is an agent or a remote user, so none of it comes from the request. It covers the
/// sandboxes callers reach as well as the ones they create: an existing sandbox is only
/// theirs to see and use if this server could have created it.
#[derive(Clone, Debug, Default)]
pub struct Policy {
    /// Directories a sandbox may be created for (they and anything under them). Empty:
    /// `dir` is refused and only `git` works, as on fleet nodes.
    pub roots: Vec<PathBuf>,
    /// Allows `mount`, a live read-write share of the directory instead of a copy.
    pub mount: bool,
    /// Hosts a caller may add to the allowlist; `*` allows any (and `*` itself).
    pub allow_hosts: Vec<String>,
    /// Environment variables a caller may pass in by name with `secrets`.
    pub secrets: Vec<String>,
    /// Allows macOS guests. Their network isn't filtered: they reach the internet, the Mac
    /// and the LAN.
    pub macos: bool,
    /// A fleet node, which only the controller calls (node secret). It takes git sources,
    /// serves [`NODE_ONLY`] and reaches only the sandboxes the fleet made, never the Mac
    /// owner's. Only [`Policy::node`] sets it.
    pub node: bool,
}

/// Operations for the controller, served only when [`Policy::node`] is set.
const NODE_ONLY: &[&str] = &["deploy_instance", "wake_instance", "run_job", "job_get", "job_cancel", "logs"];

/// A sandbox the server's policy doesn't cover; 403 over HTTP.
#[derive(Debug)]
pub struct Forbidden(pub String);
impl std::fmt::Display for Forbidden {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Forbidden {}

enum Refusal {
    /// Another project's, or the fleet's: answered like a sandbox that doesn't exist.
    Hidden,
    Forbidden(String),
}

impl Policy {
    /// Fleet nodes: only the controller calls them (node secret), and it filters what users send.
    pub fn node() -> Policy {
        Policy { roots: vec![], mount: false, allow_hosts: vec!["*".into()], secrets: vec![], macos: true, node: true }
    }

    fn host_allowed(&self, h: &str) -> bool {
        self.allow_hosts.iter().any(|a| a == "*" || a == h) || sandbox::DEV_HOSTS.contains(&h)
    }

    /// Why callers may not see or use the sandbox in `home`, if they may not.
    fn refusal(&self, home: &std::path::Path, m: &sandbox::Meta) -> Option<Refusal> {
        let fleet = home.join(crate::fleet::FLEET_MARKER).exists();
        if self.node {
            return (!fleet).then_some(Refusal::Hidden);
        }
        if fleet || !self.roots.iter().any(|r| m.project.starts_with(r)) {
            return Some(Refusal::Hidden);
        }
        let any = self.allow_hosts.iter().any(|h| h == "*");
        let why = if m.mode != "copy" && !self.mount {
            "it shares the live directory; start the server with --allow-mount".to_string()
        } else if m.os == "macos" && !self.macos {
            "it's a macOS guest, whose network isn't filtered; start the server with --allow-macos".into()
        } else if !m.os.is_empty() && m.os != "macos" {
            format!("its os {:?} isn't one this server knows", m.os)
        } else if !m.reach.is_empty() {
            "it may reach addresses on the Mac or the LAN".into()
        } else if let Some(s) = m.secrets.iter().find(|s| !self.secrets.contains(s)) {
            format!("it has secret {s}; start the server with --secret {s}")
        } else if !m.offline && m.allow.is_none() && !any {
            "its network is open; start the server with --allow-host '*'".into()
        } else {
            let h = m.allow.iter().flatten().find(|h| !m.offline && !self.host_allowed(h))?;
            format!("it may reach {h}; start the server with --allow-host {h}")
        };
        Some(Refusal::Forbidden(why))
    }

    /// The ID of the sandbox `args["id"]` names, once callers may use it.
    fn target(&self, paths: &Paths, args: &Value) -> Result<String> {
        let id = id_of(args)?;
        let (home, meta) = sandbox::lookup(paths, &id)?;
        match self.refusal(&home, &meta) {
            None => Ok(meta.id),
            Some(Refusal::Hidden) => Err(anyhow::Error::new(sandbox::NoSandbox(id))),
            Some(Refusal::Forbidden(why)) => Err(anyhow::Error::new(Forbidden(format!("sandbox {id} isn't allowed here: {why}")))),
        }
    }

    /// The project directory for a create, checked against `roots`.
    fn project(&self, dir: Option<&str>) -> Result<PathBuf> {
        let Some(first) = self.roots.first() else {
            match self.node {
                true => bail!("this server only creates sandboxes from git"),
                false => bail!("this server allows no directories; start it with --root"),
            }
        };
        let dir = match dir {
            Some(d) => PathBuf::from(d).canonicalize().with_context(|| format!("dir {d}"))?,
            None => first.clone(),
        };
        if !self.roots.iter().any(|r| dir.starts_with(r)) {
            let roots: Vec<String> = self.roots.iter().map(|r| r.display().to_string()).collect();
            bail!("{} is outside the directories this server allows ({}); start it with --root", dir.display(), roots.join(", "));
        }
        Ok(dir)
    }

    fn check(&self, args: &Value) -> Result<()> {
        if flag(args, "mount")? && !self.mount {
            bail!("mount isn't allowed; start the server with --allow-mount");
        }
        if let Some(h) = strs(&args["allow_hosts"]).into_iter().find(|h| !self.host_allowed(h)) {
            bail!("allow_hosts {h} isn't allowed; start the server with --allow-host {h}");
        }
        if let Some(s) = strs(&args["secrets"]).into_iter().find(|s| !self.secrets.contains(s)) {
            bail!("secret {s} isn't allowed; start the server with --secret {s}");
        }
        if text(args, "os")? == Some("macos") && !self.macos {
            bail!("os macos isn't allowed: a macOS guest's network isn't filtered; start the server with --allow-macos");
        }
        Ok(())
    }
}

/// `env` entries must be `KEY=VALUE`: a bare `KEY` makes docker copy the value from this
/// process's environment, which would hand the caller any secret it can name.
fn exec_opts(args: &Value) -> Result<ExecOptions> {
    let env = strs(&args["env"]);
    if let Some(e) = env.iter().find(|e| !e.contains('=')) {
        bail!("env {e}: use KEY=VALUE");
    }
    Ok(ExecOptions { workdir: text(args, "workdir")?.map(String::from), env, timeout_secs: num(args, "timeout_secs")?, tty: false })
}

/// Most output kept per stream of a collected exec, and the largest file `read_file`
/// returns. Past it the command is killed: an endless printer can't grow the server.
const MAX_OUTPUT: usize = 8 << 20;

/// What a collected command left.
pub(crate) struct Collected {
    pub code: i32,
    pub out: Vec<u8>,
    pub err: Vec<u8>,
    /// A stream passed the cap and the command was killed.
    pub truncated: bool,
}

/// Runs `c` to completion. `stdin` is fed from a thread, and the child closing its end
/// (EPIPE) ends the feed, not this process. At most `cap` bytes of each stream are kept;
/// past that, or after `deadline`, the child is killed (exit 124 for the deadline, as
/// `timeout` does). The child is always waited for.
pub(crate) fn collect(
    c: &mut std::process::Command,
    stdin: Option<Vec<u8>>,
    cap: usize,
    deadline: Option<std::time::Duration>,
) -> Result<Collected> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut child = c
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("run docker")?;
    let feed = match (stdin, child.stdin.take()) {
        (Some(data), Some(mut w)) => Some(std::thread::spawn(move || match w.write_all(&data) {
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
            r => r,
        })),
        _ => None,
    };
    let over = Arc::new(AtomicBool::new(false));
    let read = |mut pipe: Box<dyn Read + Send>| {
        let over = over.clone();
        std::thread::spawn(move || {
            let (mut kept, mut buf) = (Vec::new(), vec![0u8; 1 << 16]);
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let room = cap - kept.len();
                kept.extend_from_slice(&buf[..n.min(room)]);
                if n > room {
                    over.store(true, Ordering::SeqCst);
                }
            }
            kept
        })
    };
    let out = read(Box::new(child.stdout.take().context("stdout")?));
    let err = read(Box::new(child.stderr.take().context("stderr")?));
    let t0 = Instant::now();
    let mut nap = std::time::Duration::from_millis(1);
    let (status, timed_out) = loop {
        if let Some(st) = child.try_wait()? {
            break (st, false);
        }
        let late = deadline.is_some_and(|d| t0.elapsed() >= d);
        if late || over.load(Ordering::SeqCst) {
            let _ = child.kill();
            break (child.wait()?, late);
        }
        std::thread::sleep(nap);
        nap = (nap * 2).min(std::time::Duration::from_millis(50));
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    if let Some(f) = feed {
        f.join().map_err(|_| anyhow!("stdin writer panicked"))?.context("write stdin")?;
    }
    use std::os::unix::process::ExitStatusExt;
    let code = if timed_out { 124 } else { status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0)) };
    Ok(Collected { code, out, err, truncated: over.load(Ordering::SeqCst) })
}

/// Runs `args["cmd"]` in the sandbox to completion.
fn exec_collect(paths: &Paths, id: &str, args: &Value, stdin: Option<Vec<u8>>) -> Result<Collected> {
    let cmd = strs(&args["cmd"]);
    if cmd.is_empty() {
        bail!("missing cmd");
    }
    let (mut c, deadline) = sandbox::exec_command(paths, id, &exec_opts(args)?, &cmd)?;
    let _busy = sandbox::busy(paths, id);
    let t0 = Instant::now();
    let r = collect(&mut c, stdin, MAX_OUTPUT, deadline.map(std::time::Duration::from_secs))?;
    sandbox::record(paths, id, &cmd, r.code, t0.elapsed().as_millis());
    Ok(r)
}

/// The bytes of the file `path` in the sandbox, up to [`MAX_OUTPUT`].
fn read_file(paths: &Paths, id: &str, path: &str) -> Result<Vec<u8>> {
    let r = exec_collect(paths, id, &json!({"cmd": ["cat", "--", path]}), None)?;
    if r.truncated {
        bail!("{path} is over {} MiB; copy it out with `rekcod sandbox cp`", MAX_OUTPUT >> 20);
    }
    if r.code != 0 {
        bail!("{}", String::from_utf8_lossy(&r.err).trim());
    }
    Ok(r.out)
}

/// Writes `body` to `path` in the sandbox, making its directory; returns the bytes written.
fn write_file(paths: &Paths, id: &str, path: &str, body: Vec<u8>) -> Result<usize> {
    let n = body.len();
    let a = json!({"cmd": ["sh", "-c", "mkdir -p \"$(dirname \"$1\")\" && cat > \"$1\"", "-", path]});
    let r = exec_collect(paths, id, &a, Some(body))?;
    if r.code != 0 {
        bail!("{}", String::from_utf8_lossy(&r.err).trim());
    }
    Ok(n)
}

/// One API operation. `exec` here collects output; HTTP streams it separately. Every
/// operation on an existing sandbox authorizes it first ([`Policy::target`]).
pub fn op(paths: &Paths, policy: &Policy, name: &str, args: &Value) -> Result<Value> {
    if NODE_ONLY.contains(&name) && !policy.node {
        bail!("unknown operation {name}");
    }
    let target = || policy.target(paths, args);
    Ok(match name {
        "create" => {
            let d = CreateOptions::default();
            policy.check(args)?;
            // Fleet nodes: the workspace arrives as a git ref, not a Mac directory. Only there:
            // for `serve`/`mcp --root` a git URL would skip the root and have the Mac fetch
            // from any host before any sandbox exists.
            let dir = match args["git"]["url"].as_str() {
                Some(url) if policy.node => Some(crate::fleet::clone(paths, url, args["git"]["ref"].as_str().unwrap_or("HEAD"))?),
                Some(_) => bail!("git sources are only for fleet nodes; pass dir"),
                None => Some(policy.project(args["dir"].as_str())?),
            };
            let o = CreateOptions {
                dir,
                name: text(args, "name")?.map(String::from),
                mount: flag(args, "mount")?,
                image: text(args, "image")?.map(String::from),
                golden: text(args, "golden")?.map(String::from).unwrap_or(d.golden),
                cpus: num(args, "cpus")?.map(|c| c as usize).unwrap_or(d.cpus),
                memory_gib: num(args, "memory_gib")?.unwrap_or(d.memory_gib),
                allow_hosts: strs(&args["allow_hosts"]),
                offline: flag(args, "offline")?,
                secrets: strs(&args["secrets"]),
                ttl_secs: num(args, "ttl_secs")?.unwrap_or(d.ttl_secs),
                os: text(args, "os")?.map(String::from),
                // Never from the API: it opens a hole to the Mac and LAN.
                reach: vec![],
            };
            let meta = sandbox::create(paths, o)?;
            if policy.node {
                crate::fleet::mark_fleet(paths, &meta.id);
            }
            serde_json::to_value(meta)?
        }
        "list" => {
            let mine: Vec<sandbox::Listed> =
                sandbox::list(paths).into_iter().filter(|l| policy.refusal(&l.home, &l.meta).is_none()).collect();
            serde_json::to_value(mine)?
        }
        "get" => serde_json::to_value(sandbox::get(paths, &target()?)?)?,
        "exec" => {
            let r = exec_collect(paths, &target()?, args, None)?;
            let mut v = json!({"exit": r.code, "stdout": String::from_utf8_lossy(&r.out), "stderr": String::from_utf8_lossy(&r.err)});
            if r.truncated {
                v["truncated"] = json!(true);
            }
            v
        }
        // JSON carries text: a file that isn't UTF-8 comes back as base64. HTTP sends bytes.
        "read_file" => match String::from_utf8(read_file(paths, &target()?, text(args, "path")?.context("missing path")?)?) {
            Ok(text) => json!({"content": text}),
            Err(e) => json!({"content_base64": crate::daemon::base64(e.as_bytes())}),
        },
        "write_file" => {
            let path = text(args, "path")?.context("missing path")?;
            let body = match (text(args, "content")?, text(args, "content_base64")?) {
                (Some(t), None) => t.as_bytes().to_vec(),
                (None, Some(b)) => base64_decode(b)?,
                (None, None) => vec![],
                (Some(_), Some(_)) => bail!("pass content or content_base64, not both"),
            };
            json!({"written": write_file(paths, &target()?, path, body)?})
        }
        "fork" => {
            let meta = sandbox::fork(paths, &target()?, args["name"].as_str().map(String::from))?;
            if policy.node {
                crate::fleet::mark_fleet(paths, &meta.id);
            }
            serde_json::to_value(meta)?
        }
        "diff" => serde_json::to_value(sandbox::patch(paths, &target()?)?)?,
        "apply" => {
            let id = target()?;
            let reviewed = text(args, "reviewed").context("reviewed takes the review ID a diff returned")?;
            json!({"changes": sandbox::apply(paths, &id, flag(args, "check")?, reviewed)?})
        }
        "stop" => {
            let id = target()?;
            // An app instance stopped by the controller (idle) stays down across node restarts.
            if let Ok(h) = sandbox::home(paths, &id)
                && h.join("instance.json").exists()
            {
                let _ = std::fs::write(h.join(crate::fleet::ASLEEP_MARKER), b"");
            }
            sandbox::stop(paths, &id)?;
            json!({"stopped": true})
        }
        "remove" => {
            sandbox::rm(paths, &[target()?], false)?;
            json!({"removed": true})
        }
        "screenshot" => {
            let id = target()?;
            let out = std::env::temp_dir().join(format!("rekcod-shot-{}.png", sandbox::new_id()?));
            sandbox::screenshot(paths, &id, &out)?;
            let png = std::fs::read(&out)?;
            let _ = std::fs::remove_file(&out);
            json!({"png_base64": crate::daemon::base64(&png)})
        }
        "input" => {
            sandbox::input(paths, &target()?, &strs(&args["action"]))?;
            json!({"done": true})
        }
        "deploy_instance" => crate::fleet::deploy_instance(paths, args)?,
        "wake_instance" => crate::fleet::wake_instance(paths, &target()?)?,
        "run_job" => crate::fleet::run_job(paths, args)?,
        // Job IDs, not sandboxes: `fleet.rs` checks their form.
        "job_get" => crate::fleet::job_get(paths, &id_of(args)?)?,
        "job_cancel" => crate::fleet::job_cancel(paths, &id_of(args)?)?,
        "logs" => json!({"logs": crate::fleet::instance_logs(paths, &target()?)?}),
        _ => bail!("unknown operation {name}"),
    })
}

// ------------------------------------------------------------------- HTTP

pub(crate) struct Request {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match (b[i], hex) {
            (b'%', Some(v)) => {
                out.push(v);
                i += 2;
            }
            (b'+', _) => out.push(b' '),
            (c, _) => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into()
}

/// Longest request line or header line accepted.
const MAX_LINE: u64 = 16 << 10;
const MAX_HEADERS: usize = 100;
/// Largest body: `write_file` and deploy specs.
pub(crate) const MAX_BODY: usize = 64 << 20;
/// How long a peer may take to send its request.
pub(crate) const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn read_line_capped(r: &mut impl BufRead, out: &mut String) -> Result<usize> {
    let n = r.by_ref().take(MAX_LINE).read_line(out)?;
    if n as u64 == MAX_LINE && !out.ends_with('\n') {
        bail!("header line too long");
    }
    Ok(n)
}

/// The request line and headers; the body is left unread for [`read_body`].
pub(crate) fn read_head(r: &mut impl BufRead) -> Result<Request> {
    let mut line = String::new();
    read_line_capped(r, &mut line)?;
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("/").to_string());
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if read_line_capped(r, &mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        if headers.len() == MAX_HEADERS {
            bail!("too many headers");
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let (path, q) = target.split_once('?').unwrap_or((&target, ""));
    let query = q
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect();
    Ok(Request { method, path: path.to_string(), query, headers, body: vec![] })
}

pub(crate) fn read_body(r: &mut impl BufRead, req: &mut Request, max: usize) -> Result<()> {
    let len: usize = req.header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    if len > max {
        bail!("body too large");
    }
    req.body = vec![0; len];
    r.read_exact(&mut req.body)?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn read_request(r: &mut impl BufRead) -> Result<Request> {
    let mut req = read_head(r)?;
    read_body(r, &mut req, MAX_BODY)?;
    Ok(req)
}

impl Request {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

/// Constant-time comparison, so token and signature checks don't leak timing.
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub(crate) fn respond(w: &mut impl Write, code: u16, ctype: &str, body: &[u8]) -> Result<()> {
    let reason = match code {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        _ => "Error",
    };
    write!(w, "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())?;
    w.write_all(body)?;
    Ok(())
}

fn error_code(e: &anyhow::Error) -> u16 {
    match () {
        _ if e.downcast_ref::<sandbox::NoSandbox>().is_some() => 404,
        _ if e.downcast_ref::<Forbidden>().is_some() => 403,
        _ => 400,
    }
}

/// Maps a request to an operation name and its arguments.
pub fn route(method: &str, path: &str, query: &[(String, String)], body: &Value) -> Option<(&'static str, Value)> {
    let seg: Vec<&str> = path.trim_matches('/').split('/').collect();
    let mut args = if body.is_object() { body.clone() } else { json!({}) };
    for (k, v) in query {
        args[k.as_str()] = json!(v);
    }
    // Last: the controller authorizes the ID in the path, so nothing else may replace it.
    if let Some(id) = seg.get(1) {
        args["id"] = json!(id);
    }
    let op = match (method, seg.as_slice()) {
        ("POST", ["sandboxes"]) => "create",
        ("GET", ["sandboxes"]) => "list",
        ("GET", ["sandboxes", _]) => "get",
        ("DELETE", ["sandboxes", _]) => "remove",
        ("POST", ["sandboxes", _, "exec"]) => "exec",
        ("GET", ["sandboxes", _, "files"]) => "read_file",
        ("PUT", ["sandboxes", _, "files"]) => "write_file",
        ("POST", ["sandboxes", _, "fork"]) => "fork",
        ("GET", ["sandboxes", _, "diff"]) => "diff",
        ("POST", ["sandboxes", _, "apply"]) => "apply",
        ("POST", ["sandboxes", _, "stop"]) => "stop",
        ("GET", ["sandboxes", _, "logs"]) => "logs",
        ("GET", ["sandboxes", _, "screenshot"]) => "screenshot",
        ("POST", ["sandboxes", _, "input"]) => "input",
        ("POST", ["instances"]) => "deploy_instance",
        ("POST", ["instances", _, "wake"]) => "wake_instance",
        ("POST", ["jobs"]) => "run_job",
        ("GET", ["jobs", _]) => "job_get",
        ("DELETE", ["jobs", _]) => "job_cancel",
        _ => return None,
    };
    Some((op, args))
}

/// `exec` over HTTP: NDJSON events as the command prints, then `{"exit": n}`. A client that
/// goes away gets the command killed; a macOS guest's command is killed after
/// `timeout_secs` (exit 124). The child is always waited for.
fn stream_exec(paths: &Paths, args: &Value, w: &mut (impl Write + Send)) -> Result<()> {
    let id = id_of(args)?;
    let cmd = strs(&args["cmd"]);
    if cmd.is_empty() {
        bail!("missing cmd");
    }
    let (mut c, deadline) = sandbox::exec_command(paths, &id, &exec_opts(args)?, &cmd)?;
    let _busy = sandbox::busy(paths, &id);
    let t0 = Instant::now();
    let mut child = c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let (tx, rx) = std::sync::mpsc::channel::<(&'static str, Vec<u8>)>();
    for (name, mut pipe) in [
        ("stdout", Box::new(child.stdout.take().context("stdout")?) as Box<dyn Read + Send>),
        ("stderr", Box::new(child.stderr.take().context("stderr")?)),
    ] {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 || tx.send((name, buf[..n].to_vec())).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);
    let mut timed_out = false;
    // A character split across two reads is held back until the rest arrives.
    let mut pending: std::collections::HashMap<&str, Vec<u8>> = Default::default();
    let sent = (|| -> Result<()> {
        write!(w, "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nConnection: close\r\n\r\n")?;
        loop {
            let wait = deadline.map(|d| std::time::Duration::from_secs(d).saturating_sub(t0.elapsed()));
            let got = match wait {
                Some(left) => rx.recv_timeout(left),
                None => rx.recv().map_err(|_| std::sync::mpsc::RecvTimeoutError::Disconnected),
            };
            match got {
                Ok((name, data)) => {
                    let buf = pending.entry(name).or_default();
                    buf.extend_from_slice(&data);
                    let rest = buf.split_off(buf.len() - incomplete_tail(buf));
                    let text = String::from_utf8_lossy(&std::mem::replace(buf, rest)).into_owned();
                    if !text.is_empty() {
                        writeln!(w, "{}", json!({ name: text }))?;
                        w.flush()?;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    timed_out = true;
                    let _ = child.kill();
                    return Ok(());
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    for (name, rest) in pending.drain().filter(|(_, b)| !b.is_empty()) {
                        writeln!(w, "{}", json!({ name: String::from_utf8_lossy(&rest) }))?;
                    }
                    return Ok(());
                }
            }
        }
    })();
    if sent.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    let code = if timed_out { 124 } else { status.code().unwrap_or(1) };
    sandbox::record(paths, &id, &cmd, code, t0.elapsed().as_millis());
    sent?;
    writeln!(w, "{}", json!({"exit": code}))?;
    Ok(())
}

pub(crate) fn handle(paths: &Paths, policy: &Policy, stream: impl Read + Write + Send, token: Option<&str>) -> Result<()> {
    let mut r = BufReader::new(stream);
    let mut req = read_head(&mut r)?;
    // The token is checked before the body is read, so nobody without it can make us buffer 64 MiB.
    if let Some(t) = token {
        let auth = req.header("authorization").unwrap_or("");
        if !ct_eq(auth.as_bytes(), format!("Bearer {t}").as_bytes()) {
            return respond(r.get_mut(), 401, "application/json", br#"{"error":"bad token"}"#);
        }
    }
    // Only Content-Length bodies are read; a chunked one would arrive empty and its
    // arguments be taken as absent.
    if req.header("transfer-encoding").is_some() {
        return respond(r.get_mut(), 400, "application/json", br#"{"error":"send the body with Content-Length, not Transfer-Encoding"}"#);
    }
    if let Err(e) = read_body(&mut r, &mut req, MAX_BODY) {
        return respond(r.get_mut(), 413, "application/json", json!({"error": format!("{e:#}")}).to_string().as_bytes());
    }
    let mut w = r.into_inner();
    let body: Option<Value> =
        if req.body.is_empty() { Some(json!({})) } else { serde_json::from_slice(&req.body).ok().filter(Value::is_object) };
    // Node-only routes don't exist on `sandbox serve`.
    let found = route(&req.method, &req.path, &req.query, body.as_ref().unwrap_or(&json!({})))
        .filter(|(name, _)| policy.node || !NODE_ONLY.contains(name));
    let Some((name, args)) = found else {
        return respond(&mut w, 404, "application/json", br#"{"error":"no such endpoint"}"#);
    };
    // A body that doesn't parse is refused, not read as no arguments: `{"check": true,}`
    // mustn't apply for real. `write_file`'s body is the file.
    if body.is_none() && name != "write_file" {
        return respond(&mut w, 400, "application/json", br#"{"error":"the body isn't a JSON object"}"#);
    }
    if name == "read_file" || name == "write_file" {
        // Bytes both ways, not through `op`'s JSON.
        let r = policy.target(paths, &args).and_then(|id| {
            let path = text(&args, "path")?.context("missing path")?;
            match name {
                "read_file" => read_file(paths, &id, path),
                _ => Ok(json!({"written": write_file(paths, &id, path, std::mem::take(&mut req.body))?}).to_string().into_bytes()),
            }
        });
        let ctype = if name == "read_file" { "application/octet-stream" } else { "application/json" };
        return match r {
            Ok(body) => respond(&mut w, 200, ctype, &body),
            Err(e) => respond(&mut w, error_code(&e), "application/json", json!({"error": format!("{e:#}")}).to_string().as_bytes()),
        };
    }
    let mut args = args;
    if name == "exec" {
        let r = policy.target(paths, &args).and_then(|id| {
            args["id"] = json!(id);
            stream_exec(paths, &args, &mut w)
        });
        if let Err(e) = r {
            let _ = respond(&mut w, error_code(&e), "application/json", json!({"error": format!("{e:#}")}).to_string().as_bytes());
        }
        return Ok(());
    }
    match op(paths, policy, name, &args) {
        Ok(v) if name == "screenshot" => {
            let png = base64_decode(v["png_base64"].as_str().unwrap_or(""))?;
            respond(&mut w, 200, "image/png", &png)
        }
        Ok(v) => respond(&mut w, if name == "create" { 201 } else { 200 }, "application/json", v.to_string().as_bytes()),
        Err(e) => respond(&mut w, error_code(&e), "application/json", json!({"error": format!("{e:#}")}).to_string().as_bytes()),
    }
}

/// `rekcod sandbox serve`: the unix socket always; TCP on 127.0.0.1 only, with a token.
pub fn serve(paths: Paths, policy: Policy, listen: Option<String>) -> Result<()> {
    let sock = paths.home.join("run/sandbox.sock");
    let _ = std::fs::remove_file(&sock);
    // Bound under a 077 umask, so the socket is never reachable by others, even briefly.
    let unix = crate::paths::with_umask(0o077, || std::os::unix::net::UnixListener::bind(&sock))
        .with_context(|| format!("bind {}", sock.display()))?;
    std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600))?;
    eprintln!("rekcod: sandbox API on {}", sock.display());
    let roots: Vec<String> = policy.roots.iter().map(|r| r.display().to_string()).collect();
    eprintln!("rekcod: sandboxes allowed for {}", roots.join(", "));
    if let Some(addr) = listen {
        let addr: std::net::SocketAddr = addr.parse().context("--listen takes 127.0.0.1:PORT")?;
        if !addr.ip().is_loopback() {
            bail!("--listen must be a loopback address");
        }
        let token_file = paths.home.join("sandbox-token");
        if !token_file.exists() {
            let token = sandbox::new_id()? + &sandbox::new_id()? + &sandbox::new_id()? + &sandbox::new_id()?;
            crate::paths::write_private(&token_file, token.as_bytes())?;
        }
        let token = std::fs::read_to_string(&token_file)?.trim().to_string();
        let tcp = std::net::TcpListener::bind(addr)?;
        eprintln!("rekcod: sandbox API on http://{addr} (token in {})", token_file.display());
        let (p, pol) = (paths.clone(), policy.clone());
        std::thread::spawn(move || {
            for s in tcp.incoming().flatten() {
                let _ = s.set_read_timeout(Some(READ_TIMEOUT));
                let (p, pol, t) = (p.clone(), pol.clone(), token.clone());
                spawn_limited(move || handle(&p, &pol, s, Some(&t)));
            }
        });
    }
    for s in unix.incoming().flatten() {
        let _ = s.set_read_timeout(Some(READ_TIMEOUT));
        let (p, pol) = (paths.clone(), policy.clone());
        spawn_limited(move || handle(&p, &pol, s, None));
    }
    Ok(())
}

/// Connections served at once by the threaded HTTP servers (sandbox API, node, controller).
const MAX_CONNECTIONS: usize = 256;
static CONNECTIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Runs `f` on its own thread unless [`MAX_CONNECTIONS`] are already being served; then the
/// connection (owned by `f`) is dropped, which closes it.
pub(crate) fn spawn_limited<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) {
    use std::sync::atomic::Ordering;
    if CONNECTIONS.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
        CONNECTIONS.fetch_sub(1, Ordering::SeqCst);
        eprintln!("rekcod: {MAX_CONNECTIONS} connections open; refusing another");
        return;
    }
    std::thread::spawn(move || {
        struct Done;
        impl Drop for Done {
            fn drop(&mut self) {
                CONNECTIONS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let _done = Done;
        f()
    });
}

/// Standard base64 with padding; anything else is an error, not bytes.
fn base64_decode(s: &str) -> Result<Vec<u8>> {
    let s = s.trim_end_matches('=');
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => bail!("content_base64 isn't base64"),
        } as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    if bits >= 6 {
        bail!("content_base64 isn't base64");
    }
    Ok(out)
}

/// How many bytes at the end of `b` start a UTF-8 character that isn't complete yet.
fn incomplete_tail(b: &[u8]) -> usize {
    for back in 1..=b.len().min(3) {
        let c = b[b.len() - back];
        if c & 0xc0 == 0x80 {
            continue;
        }
        let need = match c {
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => 1,
        };
        return if need > back { back } else { 0 };
    }
    0
}

// -------------------------------------------------------------------- MCP

const PROTOCOL: &str = "2025-06-18";

/// (tool, operation, description, destructive, input schema properties, required)
type Tool = (&'static str, &'static str, &'static str, bool, Value, Vec<&'static str>);

fn tools() -> Vec<Tool> {
    let id = json!({"type": "string", "description": "Sandbox ID or name"});
    vec![
        (
            "sandbox_create",
            "create",
            "Create a sandbox: a throwaway Linux VM with docker that sees a copy of one project directory. Returns its ID.",
            false,
            json!({
                "dir": {"type": "string", "description": "Absolute project directory on the Mac"},
                "name": {"type": "string"},
                "image": {"type": "string", "description": "Workspace image (default: picked from the project's version files)"},
                "allow_hosts": {"type": "array", "items": {"type": "string"}, "description": "Extra hosts the sandbox may reach; '*' opens the internet"},
                "offline": {"type": "boolean"},
                "mount": {"type": "boolean", "description": "Share the live directory instead of a copy"},
                "os": {"type": "string", "enum": ["linux", "macos"], "description": "macos: a macOS guest from `rekcod macos install` (two per Mac)"}
            }),
            vec!["dir"],
        ),
        ("sandbox_list", "list", "List sandboxes.", false, json!({}), vec![]),
        (
            "sandbox_exec",
            "exec",
            "Run a command in the sandbox's workspace container. Returns exit code, stdout and stderr.",
            false,
            json!({
                "id": id,
                "cmd": {"type": "array", "items": {"type": "string"}, "description": "Command and arguments, e.g. [\"npm\", \"test\"]"},
                "workdir": {"type": "string"},
                "timeout_secs": {"type": "integer"}
            }),
            vec!["id", "cmd"],
        ),
        (
            "sandbox_read_file",
            "read_file",
            "Read a file in the sandbox (up to 8 MiB). Returns content, or content_base64 when the file isn't UTF-8.",
            false,
            json!({"id": id, "path": {"type": "string"}}),
            vec!["id", "path"],
        ),
        (
            "sandbox_write_file",
            "write_file",
            "Write a file in the sandbox, making its directory. Pass content as text or content_base64 for bytes.",
            false,
            json!({"id": id, "path": {"type": "string"}, "content": {"type": "string"}, "content_base64": {"type": "string"}}),
            vec!["id", "path"],
        ),
        (
            "sandbox_fork",
            "fork",
            "Copy a sandbox's disk and workspace into a new sandbox. Running processes don't carry over.",
            false,
            json!({"id": id, "name": {"type": "string"}}),
            vec!["id"],
        ),
        (
            "sandbox_diff",
            "diff",
            "Changes in the sandbox's workspace compared with the real project, as a patch, and the review ID that \
             sandbox_apply takes to write exactly what this diff showed. Replaces the copy any earlier diff froze.",
            false,
            json!({"id": id}),
            vec!["id"],
        ),
        (
            "sandbox_apply",
            "apply",
            "Write the sandbox's workspace changes into the real project on the Mac.",
            true,
            json!({
                "id": id,
                "check": {"type": "boolean", "description": "Only report what would change"},
                "reviewed": {"type": "string", "description": "The review ID sandbox_diff returned: write the copy that diff showed, not the workspace as it is now; refused if a later diff replaced it"}
            }),
            vec!["id"],
        ),
        ("sandbox_screenshot", "screenshot", "PNG of a macOS sandbox's screen.", false, json!({"id": id}), vec!["id"]),
        (
            "sandbox_input",
            "input",
            "Mouse and keyboard for a macOS sandbox: [\"click\", X, Y], [\"type\", TEXT] or [\"key\", \"cmd+return\"].",
            false,
            json!({"id": id, "action": {"type": "array", "items": {"type": "string"}}}),
            vec!["id", "action"],
        ),
        ("sandbox_stop", "stop", "Power off a sandbox, keeping its state.", false, json!({"id": id}), vec!["id"]),
        ("sandbox_remove", "remove", "Delete a sandbox and everything in it.", true, json!({"id": id}), vec!["id"]),
    ]
}

pub fn mcp_reply(paths: &Paths, policy: &Policy, msg: &Value) -> Option<Value> {
    let id = msg.get("id")?.clone();
    let result = match msg["method"].as_str().unwrap_or("") {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "rekcod", "version": env!("CARGO_PKG_VERSION")}
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools().into_iter().map(|(name, _, desc, destructive, props, req)| json!({
            "name": name,
            "description": desc,
            "inputSchema": {"type": "object", "properties": props, "required": req},
            "annotations": {"destructiveHint": destructive, "readOnlyHint": matches!(name, "sandbox_list" | "sandbox_read_file")}
        })).collect::<Vec<_>>()})),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("");
            let args = msg["params"]["arguments"].clone();
            match tools().into_iter().find(|t| t.0 == name) {
                None => Err(json!({"code": -32602, "message": format!("unknown tool {name}")})),
                Some(t) => Ok(match op(paths, policy, t.1, &args) {
                    Ok(v) if t.1 == "screenshot" => {
                        json!({"content": [{"type": "image", "mimeType": "image/png", "data": v["png_base64"]}], "isError": false})
                    }
                    Ok(v) => {
                        json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default()}], "isError": false})
                    }
                    Err(e) => json!({"content": [{"type": "text", "text": format!("{e:#}")}], "isError": true}),
                }),
            }
        }
        m => Err(json!({"code": -32601, "message": format!("method not found: {m}")})),
    };
    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
    })
}

/// A private, close-on-exec copy of `fd`.
fn private_dup(fd: i32) -> Result<std::fs::File> {
    use std::os::fd::FromRawFd;
    let d = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if d < 0 {
        bail!("dup fd {fd}: {}", std::io::Error::last_os_error());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(d) })
}

/// `rekcod mcp`: newline-delimited JSON-RPC on stdin/stdout. Requests are read from and
/// replies written to private copies of fds 0 and 1, which then point at /dev/null and
/// stderr: nothing else in this process (a `println!`) or its children (ssh, docker)
/// can read a queued request or write a line into the stream.
pub fn mcp(paths: Paths, policy: Policy) -> Result<()> {
    use std::os::fd::AsRawFd;
    let (input, mut out) = (private_dup(0)?, private_dup(1)?);
    let null = std::fs::OpenOptions::new().read(true).write(true).open("/dev/null")?;
    if unsafe { libc::dup2(null.as_raw_fd(), 0) } < 0 || unsafe { libc::dup2(2, 1) } < 0 {
        bail!("redirect stdio: {}", std::io::Error::last_os_error());
    }
    for line in BufReader::new(input).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => mcp_reply(&paths, &policy, &msg),
            Err(e) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}})),
        };
        if let Some(r) = reply {
            writeln!(out, "{r}")?;
            out.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_map_to_operations() {
        let q = vec![("path".to_string(), "/a b".to_string())];
        let (op, a) = route("GET", "/sandboxes/abc/files", &q, &json!(null)).unwrap();
        assert_eq!((op, a["id"].as_str(), a["path"].as_str()), ("read_file", Some("abc"), Some("/a b")));
        assert_eq!(route("POST", "/sandboxes", &[], &json!({"dir": "/x"})).unwrap().0, "create");
        assert_eq!(route("DELETE", "/sandboxes/abc", &[], &json!(null)).unwrap().0, "remove");
        assert!(route("POST", "/nope", &[], &json!(null)).is_none());
        assert_eq!(percent_decode("%2FUsers%2Fme+x"), "/Users/me x");
    }

    #[test]
    fn path_id_wins_over_query_and_body() {
        let q = vec![("id".to_string(), "victim".to_string())];
        let (_, a) = route("POST", "/sandboxes/mine/exec", &q, &json!({"id": "victim2"})).unwrap();
        assert_eq!(a["id"], "mine");
    }

    #[test]
    fn policy_limits_what_callers_ask_for() {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!("rekcod-pol-{}", std::process::id()));
        std::fs::create_dir_all(root.join("app")).unwrap();
        let p = Policy {
            roots: vec![root.clone()],
            allow_hosts: vec!["example.com".into()],
            secrets: vec!["TOKEN".into()],
            ..Default::default()
        };
        assert_eq!(p.project(None).unwrap(), root);
        assert_eq!(p.project(Some(root.join("app").to_str().unwrap())).unwrap(), root.join("app"));
        assert!(p.project(Some("/")).is_err());
        assert!(p.project(Some(root.join("app/../..").to_str().unwrap())).is_err());
        assert!(p.check(&json!({"mount": true})).is_err());
        assert!(p.check(&json!({"allow_hosts": ["example.com", "github.com"], "secrets": ["TOKEN"]})).is_ok());
        assert!(p.check(&json!({"allow_hosts": ["*"]})).is_err());
        assert!(p.check(&json!({"secrets": ["AWS_SECRET_ACCESS_KEY"]})).is_err());
        assert!(p.check(&json!({"os": "macos"})).unwrap_err().to_string().contains("--allow-macos"));
        assert!(Policy { macos: true, ..p.clone() }.check(&json!({"os": "macos"})).is_ok());
        assert!(Policy::node().project(Some("/Users")).is_err());
        let refused = op(&Paths { home: std::env::temp_dir() }, &p, "create", &json!({"git": {"url": "https://127.0.0.1:1/x.git"}}));
        assert!(refused.unwrap_err().to_string().contains("only for fleet nodes"));
        assert!(exec_opts(&json!({"env": ["AWS_SECRET_ACCESS_KEY"]})).is_err());
        assert!(exec_opts(&json!({"env": ["A=1"]})).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn collected_commands_are_bounded() {
        let sh = |script: &str| {
            let mut c = std::process::Command::new("sh");
            c.args(["-c", script]);
            c
        };
        // A child that exits without reading its stdin: the feed ends, the exit code comes back.
        let r = collect(&mut sh("echo no >&2; exit 3"), Some(vec![b'x'; 4 << 20]), MAX_OUTPUT, None).unwrap();
        assert_eq!((r.code, r.err.as_slice(), r.truncated), (3, &b"no\n"[..], false));
        let r = collect(&mut sh("cat"), Some(b"round trip".to_vec()), MAX_OUTPUT, None).unwrap();
        assert_eq!((r.code, r.out.as_slice()), (0, &b"round trip"[..]));
        // An endless printer is cut at the cap and killed.
        let r = collect(&mut sh("yes"), None, 1 << 20, None).unwrap();
        assert!(r.truncated && r.out.len() == 1 << 20 && r.code != 0, "{}", r.code);
        let t0 = Instant::now();
        let r = collect(&mut sh("sleep 30"), None, MAX_OUTPUT, Some(std::time::Duration::from_secs(1))).unwrap();
        assert_eq!(r.code, 124);
        assert!(t0.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn bytes_survive_base64_and_split_reads() {
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(base64_decode(&crate::daemon::base64(&all)).unwrap(), all);
        assert_eq!(base64_decode("aGk=").unwrap(), b"hi");
        assert!(base64_decode("a$==").is_err() && base64_decode("a").is_err());
        let e = "\u{e9}\u{20ac}\u{1f600}".as_bytes();
        assert_eq!(incomplete_tail(&e[..1]), 1);
        assert_eq!(incomplete_tail(&e[..2]), 0);
        assert_eq!(incomplete_tail(&e[..4]), 2);
        assert_eq!(incomplete_tail(&e[..7]), 2);
        assert_eq!(incomplete_tail(b"ok\xff"), 0);
    }

    #[test]
    fn requests_are_capped() {
        let long = format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(20 << 10));
        assert!(read_request(&mut std::io::Cursor::new(long)).is_err());
        let many = format!("GET / HTTP/1.1\r\n{}\r\n", "X: y\r\n".repeat(200));
        assert!(read_request(&mut std::io::Cursor::new(many)).is_err());
        let big = format!("POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n", MAX_BODY + 1);
        assert!(read_request(&mut std::io::Cursor::new(big)).is_err());
        let ok = read_request(&mut std::io::Cursor::new("POST /x?a=1 HTTP/1.1\r\nContent-Length: 2\r\n\r\nhi")).unwrap();
        assert_eq!((ok.path.as_str(), ok.body.as_slice(), ok.header("content-length")), ("/x", &b"hi"[..], Some("2")));
        assert!(ct_eq(b"abc", b"abc") && !ct_eq(b"abc", b"abd") && !ct_eq(b"ab", b"abc"));
    }

    #[test]
    fn mcp_lists_every_tool_and_rejects_unknown() {
        let paths = Paths { home: std::env::temp_dir() };
        let init = mcp_reply(&paths, &Policy::default(), &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"})).unwrap();
        assert_eq!(init["result"]["protocolVersion"], PROTOCOL);
        let list = mcp_reply(&paths, &Policy::default(), &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).unwrap();
        let tools = list["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 12);
        let apply = tools.iter().find(|t| t["name"] == "sandbox_apply").unwrap();
        assert_eq!(apply["annotations"]["destructiveHint"], true);
        // A diff replaces the reviewed copy, so a client mustn't run it without asking.
        let diff = tools.iter().find(|t| t["name"] == "sandbox_diff").unwrap();
        assert_eq!(diff["annotations"]["readOnlyHint"], false);
        assert!(mcp_reply(&paths, &Policy::default(), &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none());
        let bad =
            mcp_reply(&paths, &Policy::default(), &json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "x"}}))
                .unwrap();
        assert_eq!(bad["error"]["code"], -32602);
    }

    use crate::sandbox::testing::fake;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rekcod-api-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    /// Sends one request through `handle`; returns the status and body.
    fn http(paths: &Paths, policy: &Policy, method: &str, path: &str, body: &str) -> (u16, String) {
        struct Pipe(std::io::Cursor<Vec<u8>>, Vec<u8>);
        impl Read for Pipe {
            fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
                self.0.read(b)
            }
        }
        impl Write for Pipe {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.1.write(b)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let req = format!("{method} {path} HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        let mut p = Pipe(std::io::Cursor::new(req.into_bytes()), vec![]);
        handle(paths, policy, &mut p, None).unwrap();
        let out = String::from_utf8_lossy(&p.1).to_string();
        let (head, body) = out.split_once("\r\n\r\n").unwrap();
        (head[9..12].parse().unwrap(), body.to_string())
    }

    fn mcp_call(paths: &Paths, policy: &Policy, tool: &str, args: Value) -> (bool, String) {
        let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}});
        let r = mcp_reply(paths, policy, &msg).unwrap();
        (r["result"]["isError"] == true, r["result"]["content"][0]["text"].as_str().unwrap_or("").to_string())
    }

    #[test]
    fn servers_only_reach_sandboxes_their_policy_covers() {
        let base = tmp("existing");
        let (a, b) = (base.join("a"), base.join("b"));
        for d in [&a, &b] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("f.txt"), "host").unwrap();
        }
        let paths = Paths { home: base.join("home") };
        let ws = |id: &str| paths.home.join("sb").join(id).join("ws");
        // Project B's sandbox, with a change reviewed from the CLI.
        let sb = fake(&paths, &b, |m| m.name = Some("bee".into()));
        std::fs::write(ws(&sb.id).join("f.txt"), "sandbox").unwrap();
        sandbox::patch(&paths, &sb.id).unwrap();
        let pol = Policy { roots: vec![a.clone()], ..Default::default() };
        let (_, list) = mcp_call(&paths, &pol, "sandbox_list", json!({}));
        assert!(!list.contains(&sb.id), "{list}");
        for (tool, args) in [
            ("sandbox_apply", json!({"id": sb.id, "reviewed": "0123"})),
            ("sandbox_apply", json!({"id": "bee"})),
            ("sandbox_diff", json!({"id": sb.id})),
            ("sandbox_exec", json!({"id": sb.id, "cmd": ["true"]})),
            ("sandbox_read_file", json!({"id": sb.id, "path": "/etc/hostname"})),
            ("sandbox_fork", json!({"id": sb.id})),
            ("sandbox_stop", json!({"id": sb.id})),
            ("sandbox_remove", json!({"id": sb.id})),
        ] {
            let (err, text) = mcp_call(&paths, &pol, tool, args);
            assert!(err && text.starts_with("no sandbox"), "{tool}: {text}");
        }
        assert_eq!(std::fs::read_to_string(b.join("f.txt")).unwrap(), "host");
        assert!(paths.home.join("sb").join(&sb.id).exists());
        assert_eq!(http(&paths, &pol, "GET", "/sandboxes", ""), (200, "[]".into()));
        for (method, path, body) in [
            ("POST", format!("/sandboxes/{}/apply", sb.id), r#"{"reviewed": "0123"}"#),
            ("POST", format!("/sandboxes/{}/exec", sb.id), r#"{"cmd": ["true"]}"#),
            ("GET", format!("/sandboxes/{}", sb.id), ""),
            ("DELETE", "/sandboxes/bee".into(), ""),
        ] {
            assert_eq!(http(&paths, &pol, method, &path, body).0, 404, "{method} {path}");
        }
        assert_eq!(std::fs::read_to_string(b.join("f.txt")).unwrap(), "host");

        // Under the root but broader than the policy: not listed, and refused with the flag
        // the server would need.
        let mine = fake(&paths, &a, |m| m.secrets = vec!["TOKEN".into()]);
        std::fs::write(ws(&mine.id).join("f.txt"), "sandbox").unwrap();
        let (code, body) = http(&paths, &pol, "POST", &format!("/sandboxes/{}/apply", mine.id), "{}");
        assert!(code == 403 && body.contains("--secret TOKEN"), "{code} {body}");
        assert_eq!(http(&paths, &pol, "GET", "/sandboxes", "").1, "[]");
        for (edit, flag) in [
            (Box::new(|m: &mut sandbox::Meta| m.mode = "mount".into()) as Box<dyn FnOnce(&mut sandbox::Meta)>, "--allow-mount"),
            (Box::new(|m: &mut sandbox::Meta| m.allow = None), "--allow-host '*'"),
            (Box::new(|m: &mut sandbox::Meta| m.allow = sandbox::allowlist(&["api.example.com".into()])), "--allow-host api.example.com"),
            (Box::new(|m: &mut sandbox::Meta| m.os = "macos".into()), "--allow-macos"),
            (Box::new(|m: &mut sandbox::Meta| m.reach = vec!["192.168.1.2:9000".into()]), "the Mac or the LAN"),
        ] {
            let m = fake(&paths, &a, edit);
            let (err, text) = mcp_call(&paths, &pol, "sandbox_diff", json!({"id": m.id}));
            assert!(err && text.contains(flag), "{flag}: {text}");
        }
        let pol = Policy { secrets: vec!["TOKEN".into()], ..pol };
        let (_, list) = mcp_call(&paths, &pol, "sandbox_list", json!({}));
        assert!(list.contains(&mine.id) && !list.contains(&sb.id), "{list}");
        let (code, body) = http(&paths, &pol, "POST", &format!("/sandboxes/{}/apply", mine.id), "{}");
        assert_eq!((code, body.as_str()), (200, r#"{"changes":[{"kind":"M","path":"f.txt"}]}"#));
        assert_eq!(std::fs::read_to_string(a.join("f.txt")).unwrap(), "sandbox");

        // A fleet node reaches only the fleet's sandboxes, and a user's server never does.
        let node = Policy::node();
        assert_eq!(http(&paths, &node, "GET", &format!("/sandboxes/{}", sb.id), "").0, 404);
        std::fs::write(paths.home.join("sb").join(&sb.id).join(crate::fleet::FLEET_MARKER), b"").unwrap();
        assert_eq!(http(&paths, &node, "GET", &format!("/sandboxes/{}", sb.id), "").0, 200);
        let everything = Policy { roots: vec![base.clone()], ..Default::default() };
        assert_eq!(http(&paths, &everything, "GET", &format!("/sandboxes/{}", sb.id), "").0, 404);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn apply_arguments_never_fail_open() {
        let base = tmp("failopen");
        let project = base.join("p");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("f"), "old").unwrap();
        let paths = Paths { home: base.join("home") };
        let pol = Policy { roots: vec![base.clone()], ..Default::default() };
        let sb = fake(&paths, &project, |_| {});
        std::fs::write(paths.home.join("sb").join(&sb.id).join("ws/f"), "from-sandbox").unwrap();
        let apply = format!("/sandboxes/{}/apply", sb.id);
        assert_eq!(
            http(&paths, &pol, "POST", &format!("{apply}?check=true"), ""),
            (200, r#"{"changes":[{"kind":"M","path":"f"}]}"#.into())
        );
        for (path, body) in [
            (apply.clone(), r#"{"check": true,}"#),
            (apply.clone(), "[]"),
            (apply.clone(), r#"{"check": "yes"}"#),
            (format!("{apply}?check=1"), ""),
            (format!("{apply}?reviewed=true"), ""),
            (apply.clone(), r#"{"reviewed": true}"#),
        ] {
            let (code, text) = http(&paths, &pol, "POST", &path, body);
            assert_eq!(code, 400, "{path} {body}: {text}");
        }
        assert_eq!(std::fs::read_to_string(project.join("f")).unwrap(), "old");
        assert!(num(&json!({"t": "5"}), "t").unwrap() == Some(5) && num(&json!({"t": "-5"}), "t").is_err());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn node_routes_exist_only_on_nodes() {
        let base = tmp("noderoutes");
        let paths = Paths { home: base.join("home") };
        std::fs::create_dir_all(&paths.home).unwrap();
        let user = Policy { roots: vec![base.clone()], allow_hosts: vec!["*".into()], ..Default::default() };
        let git = r#"{"app": "x", "release": 1, "git": {"url": "https://127.0.0.1:1/x.git"}, "id": "j-abc", "cmd": ["true"]}"#;
        for (method, path, body) in [
            ("POST", "/instances", git),
            ("POST", "/instances/abc/wake", ""),
            ("POST", "/jobs", git),
            ("GET", "/jobs/j-abc", ""),
            ("DELETE", "/jobs/j-abc", ""),
            ("GET", "/sandboxes/abc/logs", ""),
        ] {
            assert_eq!(http(&paths, &user, method, path, body), (404, r#"{"error":"no such endpoint"}"#.into()), "{method} {path}");
        }
        for name in NODE_ONLY {
            let e = op(&paths, &user, name, &serde_json::from_str(git).unwrap()).unwrap_err();
            assert!(e.to_string().starts_with("unknown operation"), "{name}: {e}");
        }
        assert!(!paths.home.join("remote-ws").exists() && !paths.home.join("jobs").exists());
        let (code, body) = http(&paths, &Policy::node(), "GET", "/jobs/j-abc", "");
        assert!(code == 400 && body.contains("no job j-abc"), "{code} {body}");
        std::fs::remove_dir_all(base).unwrap();
    }
}
