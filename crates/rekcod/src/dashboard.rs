//! `rekcod dashboard`: runs the web dashboard's standalone build (docs/dashboard.md) on
//! 127.0.0.1 and opens it. A dashboard that's already running is just opened.
//!
//! Each launch gets a new token. It goes to the server in `REKCOD_DASHBOARD_TOKEN` and to
//! `~/.rekcod/run/dashboard.json` (0600); the browser trades it for a cookie at `/auth`.
//! Containers reach the Mac's 127.0.0.1 through `host.docker.internal`, so loopback alone
//! doesn't keep them out.

use std::io::{IsTerminal, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::paths::Paths;

const FIRST_PORT: u16 = 4500;
/// `bundle.mjs` copies `scripts/start.cjs` here; it pins 127.0.0.1, then loads server.js.
const ENTRY: &str = "start.cjs";

fn run_file(paths: &Paths) -> PathBuf {
    paths.home.join("run").join("dashboard.json")
}

/// The standalone build: `REKCOD_DASHBOARD_DIR`, `~/.rekcod/dashboard`, the release
/// tarball's `share/rekcod/dashboard`, or this checkout's `dashboard/.next/standalone`.
fn build_dir(paths: &Paths) -> Result<PathBuf> {
    let exe = std::env::current_exe().ok().and_then(|e| e.canonicalize().ok());
    let mut tried = Vec::new();
    let mut candidates: Vec<PathBuf> = std::env::var_os("REKCOD_DASHBOARD_DIR").map(PathBuf::from).into_iter().collect();
    candidates.push(paths.home.join("dashboard"));
    if let Some(bin) = exe.as_deref().and_then(Path::parent) {
        candidates.push(bin.join("../share/rekcod/dashboard"));
        // target/{release,debug}/rekcod in a checkout.
        candidates.push(bin.join("../../dashboard/.next/standalone"));
    }
    // A build without start.cjs predates the token and would answer anyone; skip it.
    for c in candidates {
        if c.join("server.js").is_file() && c.join(ENTRY).is_file() {
            return Ok(c.canonicalize().unwrap_or(c));
        }
        tried.push(c.display().to_string());
    }
    bail!(
        "no current dashboard build (looked in {}); in a checkout: `cd dashboard && npm ci && npm run bundle -- --install`",
        tried.join(", ")
    )
}

/// `node` from `REKCOD_NODE`, `PATH`, or Homebrew's places (a LaunchAgent's PATH is short).
fn node() -> Result<PathBuf> {
    if let Some(n) = std::env::var_os("REKCOD_NODE") {
        return Ok(n.into());
    }
    let path = std::env::var("PATH").unwrap_or_default();
    path.split(':')
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join("node"))
        .chain(["/opt/homebrew/bin/node", "/usr/local/bin/node"].map(PathBuf::from))
        .find(|p| p.is_file())
        .context("the dashboard needs Node.js (`brew install node`), or set REKCOD_NODE")
}

fn answers(port: u16) -> bool {
    TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(300)).is_ok()
}

/// 32 random bytes as hex.
fn new_token() -> Result<String> {
    let mut b = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn auth_url(port: u16, token: &str) -> String {
    format!("http://localhost:{port}/auth?token={token}")
}

/// The pid, port and token `record` wrote. A file without a token was written for a build
/// that had none, so it doesn't count.
fn recorded(paths: &Paths) -> Option<(i32, u16, String)> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(run_file(paths)).ok()?).ok()?;
    Some((v["pid"].as_i64()? as i32, v["port"].as_u64()? as u16, v["token"].as_str()?.to_string()))
}

/// The sign-in URL of a dashboard started earlier that still answers.
fn running(paths: &Paths) -> Option<String> {
    let (pid, port, token) = recorded(paths)?;
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    (alive && answers(port)).then(|| auth_url(port, &token))
}

fn free_port() -> Result<u16> {
    (FIRST_PORT..FIRST_PORT + 200).find(|p| TcpListener::bind(("127.0.0.1", *p)).is_ok()).context("no free port from 4500 to 4699")
}

fn command(paths: &Paths, port: u16, token: &str) -> Result<Command> {
    let dir = build_dir(paths)?;
    let mut c = Command::new(node()?);
    c.arg(dir.join(ENTRY))
        .current_dir(&dir)
        // start.cjs forces 127.0.0.1 anyway; this covers server.js run by hand from here.
        .env("HOSTNAME", "127.0.0.1")
        .env("PORT", port.to_string())
        .env("REKCOD_BIN", std::env::current_exe()?)
        .env("REKCOD_DASHBOARD_TOKEN", token)
        .stdin(Stdio::null());
    Ok(c)
}

/// Writes dashboard.json, created as 0600. An old file is removed first: opening it would
/// keep whatever mode it had.
fn record(paths: &Paths, pid: u32, port: u16, token: &str) -> Result<()> {
    let f = run_file(paths);
    std::fs::create_dir_all(f.parent().context("run dir")?)?;
    match std::fs::remove_file(&f) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    let mut out = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&f)?;
    out.write_all(json!({"pid": pid, "port": port, "token": token}).to_string().as_bytes())?;
    Ok(())
}

fn open(url: &str) {
    let _ = Command::new("/usr/bin/open").arg(url).status();
}

pub fn run(paths: &Paths, foreground: bool, open_it: bool) -> Result<()> {
    if let Some(url) = running(paths) {
        println!("{url}");
        if open_it {
            open(&url);
        }
        return Ok(());
    }
    let port = free_port()?;
    let token = new_token()?;
    let url = auth_url(port, &token);
    if foreground {
        let mut child = command(paths, port, &token)?.spawn().context("start node")?;
        record(paths, child.id(), port, &token)?;
        // Under the LaunchAgent stderr is dashboard.log, which isn't 0600; keep the token out.
        if std::io::stderr().is_terminal() {
            eprintln!("rekcod dashboard: {url}");
        } else {
            eprintln!("rekcod dashboard: http://localhost:{port} (`rekcod dashboard` opens it signed in)");
        }
        let status = child.wait()?;
        let _ = std::fs::remove_file(run_file(paths));
        if !status.success() {
            bail!("dashboard exited: {status}");
        }
        return Ok(());
    }
    let log = paths.home.join("logs").join("dashboard.log");
    std::fs::create_dir_all(log.parent().context("log dir")?)?;
    let out = std::fs::OpenOptions::new().create(true).append(true).open(&log)?;
    let child = command(paths, port, &token)?.stdout(out.try_clone()?).stderr(out).spawn().context("start node")?;
    record(paths, child.id(), port, &token)?;
    let t0 = Instant::now();
    while !answers(port) {
        if t0.elapsed() > Duration::from_secs(20) {
            bail!("the dashboard didn't answer on port {port} within 20 s; see {}", log.display());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    println!("{url}");
    if open_it {
        open(&url);
    }
    Ok(())
}

/// A LaunchAgent running `rekcod dashboard --foreground`. Checks the build and Node first,
/// so a broken setup fails here rather than in a restart loop.
pub fn install(paths: &Paths) -> Result<()> {
    build_dir(paths)?;
    node()?;
    crate::service::install(paths, "dashboard", &["dashboard".into(), "--foreground".into(), "--no-open".into()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn home(name: &str) -> Paths {
        let dir = std::env::temp_dir().join(format!("rekcod-dashboard-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Paths::at(dir).unwrap()
    }

    #[test]
    fn tokens_are_64_hex_and_differ() {
        let (a, b) = (new_token().unwrap(), new_token().unwrap());
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn record_writes_the_token_0600_over_an_old_file() {
        let paths = home("record");
        let f = run_file(&paths);
        std::fs::write(&f, r#"{"pid":1,"port":4500}"#).unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        // The pre-token format isn't a dashboard we can sign in to.
        assert_eq!(recorded(&paths), None);

        record(&paths, 42, 4501, "ab12").unwrap();
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(recorded(&paths), Some((42, 4501, "ab12".to_string())));
        assert_eq!(auth_url(4501, "ab12"), "http://localhost:4501/auth?token=ab12");
        std::fs::remove_dir_all(&paths.home).unwrap();
    }
}
