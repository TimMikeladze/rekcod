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

fn id_of(args: &Value) -> Result<String> {
    args["id"].as_str().map(String::from).ok_or_else(|| anyhow!("missing id"))
}

/// What callers may ask for, fixed when `serve`, `mcp` or a fleet node starts. The caller
/// is an agent or a remote user, so none of it comes from the request.
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
}

impl Policy {
    /// Fleet nodes: only the controller calls them (node secret), and it filters what users send.
    pub fn node() -> Policy {
        Policy { roots: vec![], mount: false, allow_hosts: vec!["*".into()], secrets: vec![] }
    }

    /// The project directory for a create, checked against `roots`.
    fn project(&self, dir: Option<&str>) -> Result<PathBuf> {
        let Some(first) = self.roots.first() else { bail!("this server only creates sandboxes from git") };
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
        if args["mount"].as_bool() == Some(true) && !self.mount {
            bail!("mount isn't allowed; start the server with --allow-mount");
        }
        let any = self.allow_hosts.iter().any(|h| h == "*");
        if let Some(h) = strs(&args["allow_hosts"]).into_iter().find(|h| !any && !self.allow_hosts.contains(h)) {
            bail!("allow_hosts {h} isn't allowed; start the server with --allow-host {h}");
        }
        if let Some(s) = strs(&args["secrets"]).into_iter().find(|s| !self.secrets.contains(s)) {
            bail!("secret {s} isn't allowed; start the server with --secret {s}");
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
    Ok(ExecOptions { workdir: args["workdir"].as_str().map(String::from), env, timeout_secs: args["timeout_secs"].as_u64(), tty: false })
}

/// Runs a command to completion: (exit, stdout, stderr).
fn exec_collect(paths: &Paths, id: &str, args: &Value, stdin: Option<&[u8]>) -> Result<(i32, Vec<u8>, Vec<u8>)> {
    let cmd = strs(&args["cmd"]);
    if cmd.is_empty() {
        bail!("missing cmd");
    }
    let mut c = sandbox::exec_cmd(paths, id, &exec_opts(args)?, &cmd)?;
    let t0 = Instant::now();
    let mut child = c
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("run docker")?;
    if let Some(data) = stdin {
        let mut w = child.stdin.take().context("stdin")?;
        w.write_all(data)?;
    }
    let out = child.wait_with_output()?;
    let code = out.status.code().unwrap_or(1);
    sandbox::record(paths, id, &cmd, code, t0.elapsed().as_millis());
    Ok((code, out.stdout, out.stderr))
}

/// One API operation. `exec` here collects output; HTTP streams it separately.
pub fn op(paths: &Paths, policy: &Policy, name: &str, args: &Value) -> Result<Value> {
    Ok(match name {
        "create" => {
            let d = CreateOptions::default();
            policy.check(args)?;
            // Fleet nodes: the workspace arrives as a git ref, not a Mac directory.
            let dir = match args["git"]["url"].as_str() {
                Some(url) => Some(crate::fleet::clone(paths, url, args["git"]["ref"].as_str().unwrap_or("HEAD"))?),
                None => Some(policy.project(args["dir"].as_str())?),
            };
            let o = CreateOptions {
                dir,
                name: args["name"].as_str().map(String::from),
                mount: args["mount"].as_bool().unwrap_or(false),
                image: args["image"].as_str().map(String::from),
                golden: args["golden"].as_str().map(String::from).unwrap_or(d.golden),
                cpus: args["cpus"].as_u64().map(|c| c as usize).unwrap_or(d.cpus),
                memory_gib: args["memory_gib"].as_u64().unwrap_or(d.memory_gib),
                allow_hosts: strs(&args["allow_hosts"]),
                offline: args["offline"].as_bool().unwrap_or(false),
                secrets: strs(&args["secrets"]),
                ttl_secs: args["ttl_secs"].as_u64().unwrap_or(d.ttl_secs),
                os: args["os"].as_str().map(String::from),
                // Never from the API: it opens a hole to the Mac and LAN.
                reach: vec![],
            };
            serde_json::to_value(sandbox::create(paths, o)?)?
        }
        "list" => serde_json::to_value(sandbox::list(paths))?,
        "get" => serde_json::to_value(sandbox::get(paths, &id_of(args)?)?)?,
        "exec" => {
            let (exit, out, err) = exec_collect(paths, &id_of(args)?, args, None)?;
            json!({"exit": exit, "stdout": String::from_utf8_lossy(&out), "stderr": String::from_utf8_lossy(&err)})
        }
        "read_file" => {
            let a = json!({"cmd": ["cat", "--", args["path"].as_str().context("missing path")?]});
            let (exit, out, err) = exec_collect(paths, &id_of(args)?, &a, None)?;
            if exit != 0 {
                bail!("{}", String::from_utf8_lossy(&err).trim());
            }
            json!({"content": String::from_utf8_lossy(&out)})
        }
        "write_file" => {
            let path = args["path"].as_str().context("missing path")?;
            let a = json!({"cmd": ["sh", "-c", "mkdir -p \"$(dirname \"$1\")\" && cat > \"$1\"", "-", path]});
            let body = args["content"].as_str().unwrap_or_default();
            let (exit, _, err) = exec_collect(paths, &id_of(args)?, &a, Some(body.as_bytes()))?;
            if exit != 0 {
                bail!("{}", String::from_utf8_lossy(&err).trim());
            }
            json!({"written": body.len()})
        }
        "fork" => serde_json::to_value(sandbox::fork(paths, &id_of(args)?, args["name"].as_str().map(String::from))?)?,
        "diff" => {
            let (changes, patch) = sandbox::patch(paths, &id_of(args)?)?;
            json!({"changes": changes, "patch": patch})
        }
        "apply" => json!({"changes": sandbox::apply(
            paths,
            &id_of(args)?,
            args["check"].as_bool().unwrap_or(false),
            args["reviewed"].as_bool().unwrap_or(false)
        )?}),
        "stop" => {
            sandbox::stop(paths, &id_of(args)?)?;
            json!({"stopped": true})
        }
        "remove" => {
            sandbox::rm(paths, &[id_of(args)?], false)?;
            json!({"removed": true})
        }
        "screenshot" => {
            let out = std::env::temp_dir().join(format!("rekcod-shot-{}.png", sandbox::new_id()?));
            sandbox::screenshot(paths, &id_of(args)?, &out)?;
            let png = std::fs::read(&out)?;
            let _ = std::fs::remove_file(&out);
            json!({"png_base64": crate::daemon::base64(&png)})
        }
        "input" => {
            sandbox::input(paths, &id_of(args)?, &strs(&args["action"]))?;
            json!({"done": true})
        }
        "deploy_instance" => crate::fleet::deploy_instance(paths, args)?,
        "wake_instance" => crate::fleet::wake_instance(paths, &id_of(args)?)?,
        "run_job" => crate::fleet::run_job(paths, args)?,
        "job_get" => crate::fleet::job_get(paths, &id_of(args)?)?,
        "job_cancel" => crate::fleet::job_cancel(paths, &id_of(args)?)?,
        "logs" => json!({"logs": crate::fleet::instance_logs(paths, &id_of(args)?)?}),
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
    if e.downcast_ref::<sandbox::NoSandbox>().is_some() { 404 } else { 400 }
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

/// `exec` over HTTP: NDJSON events as the command prints, then `{"exit": n}`.
fn stream_exec(paths: &Paths, args: &Value, w: &mut (impl Write + Send)) -> Result<()> {
    let id = id_of(args)?;
    let cmd = strs(&args["cmd"]);
    if cmd.is_empty() {
        bail!("missing cmd");
    }
    let mut c = sandbox::exec_cmd(paths, &id, &exec_opts(args)?, &cmd)?;
    let t0 = Instant::now();
    let mut child = c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    write!(w, "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nConnection: close\r\n\r\n")?;
    let (tx, rx) = std::sync::mpsc::channel::<(&'static str, Vec<u8>)>();
    let mut readers = Vec::new();
    for (name, mut pipe) in [
        ("stdout", Box::new(child.stdout.take().context("stdout")?) as Box<dyn Read + Send>),
        ("stderr", Box::new(child.stderr.take().context("stderr")?)),
    ] {
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 || tx.send((name, buf[..n].to_vec())).is_err() {
                    break;
                }
            }
        }));
    }
    drop(tx);
    for (name, data) in rx {
        writeln!(w, "{}", json!({ name: String::from_utf8_lossy(&data) }))?;
        w.flush()?;
    }
    let code = child.wait()?.code().unwrap_or(1);
    sandbox::record(paths, &id, &cmd, code, t0.elapsed().as_millis());
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
    if let Err(e) = read_body(&mut r, &mut req, MAX_BODY) {
        return respond(r.get_mut(), 413, "application/json", json!({"error": format!("{e:#}")}).to_string().as_bytes());
    }
    let mut w = r.into_inner();
    let body: Value = if req.body.is_empty() { json!({}) } else { serde_json::from_slice(&req.body).unwrap_or(json!({})) };
    let Some((name, mut args)) = route(&req.method, &req.path, &req.query, &body) else {
        return respond(&mut w, 404, "application/json", br#"{"error":"no such endpoint"}"#);
    };
    if name == "write_file" {
        args["content"] = json!(String::from_utf8_lossy(&req.body));
    }
    if name == "exec" {
        if let Err(e) = stream_exec(paths, &args, &mut w) {
            let _ = respond(&mut w, error_code(&e), "application/json", json!({"error": format!("{e:#}")}).to_string().as_bytes());
        }
        return Ok(());
    }
    match op(paths, policy, name, &args) {
        Ok(v) if name == "screenshot" => {
            let png = base64_decode(v["png_base64"].as_str().unwrap_or(""));
            respond(&mut w, 200, "image/png", &png)
        }
        Ok(v) if name == "read_file" => respond(&mut w, 200, "application/octet-stream", v["content"].as_str().unwrap_or("").as_bytes()),
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

fn base64_decode(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes().filter(|c| *c != b'=') {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            _ => 63,
        } as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
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
            "Read a file in the sandbox.",
            false,
            json!({"id": id, "path": {"type": "string"}}),
            vec!["id", "path"],
        ),
        (
            "sandbox_write_file",
            "write_file",
            "Write a file in the sandbox.",
            false,
            json!({"id": id, "path": {"type": "string"}, "content": {"type": "string"}}),
            vec!["id", "path", "content"],
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
            "Changes in the sandbox's workspace compared with the real project, as a patch.",
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
                "reviewed": {"type": "boolean", "description": "Write the copy the last sandbox_diff showed, not the workspace as it is now"}
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
            "annotations": {"destructiveHint": destructive, "readOnlyHint": matches!(name, "sandbox_list" | "sandbox_read_file" | "sandbox_diff")}
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

/// `rekcod mcp`: newline-delimited JSON-RPC on stdin/stdout.
pub fn mcp(paths: Paths, policy: Policy) -> Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
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
        let p = Policy { roots: vec![root.clone()], mount: false, allow_hosts: vec!["example.com".into()], secrets: vec!["TOKEN".into()] };
        assert_eq!(p.project(None).unwrap(), root);
        assert_eq!(p.project(Some(root.join("app").to_str().unwrap())).unwrap(), root.join("app"));
        assert!(p.project(Some("/")).is_err());
        assert!(p.project(Some(root.join("app/../..").to_str().unwrap())).is_err());
        assert!(p.check(&json!({"mount": true})).is_err());
        assert!(p.check(&json!({"allow_hosts": ["example.com"], "secrets": ["TOKEN"]})).is_ok());
        assert!(p.check(&json!({"allow_hosts": ["*"]})).is_err());
        assert!(p.check(&json!({"secrets": ["AWS_SECRET_ACCESS_KEY"]})).is_err());
        assert!(Policy::node().project(Some("/Users")).is_err());
        assert!(exec_opts(&json!({"env": ["AWS_SECRET_ACCESS_KEY"]})).is_err());
        assert!(exec_opts(&json!({"env": ["A=1"]})).is_ok());
        std::fs::remove_dir_all(root).unwrap();
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
        assert!(mcp_reply(&paths, &Policy::default(), &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none());
        let bad =
            mcp_reply(&paths, &Policy::default(), &json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "x"}}))
                .unwrap();
        assert_eq!(bad["error"]["code"], -32602);
    }
}
