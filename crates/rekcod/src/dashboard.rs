//! `rekcod dashboard`: runs the web dashboard's standalone build (docs/dashboard.md) on
//! 127.0.0.1 and opens it. A dashboard that's already running is just opened.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::paths::Paths;

const FIRST_PORT: u16 = 4500;

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
    for c in candidates {
        if c.join("server.js").is_file() {
            return Ok(c.canonicalize().unwrap_or(c));
        }
        tried.push(c.display().to_string());
    }
    bail!("no dashboard build (looked in {}); in a checkout: `cd dashboard && npm ci && npm run bundle`", tried.join(", "))
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

/// The URL of a dashboard started earlier that still answers.
fn running(paths: &Paths) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(run_file(paths)).ok()?).ok()?;
    let (pid, port) = (v["pid"].as_i64()? as i32, v["port"].as_u64()? as u16);
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    (alive && answers(port)).then(|| format!("http://localhost:{port}"))
}

fn free_port() -> Result<u16> {
    (FIRST_PORT..FIRST_PORT + 200).find(|p| TcpListener::bind(("127.0.0.1", *p)).is_ok()).context("no free port from 4500 to 4699")
}

fn command(paths: &Paths, port: u16) -> Result<Command> {
    let dir = build_dir(paths)?;
    let mut c = Command::new(node()?);
    c.arg(dir.join("server.js"))
        .current_dir(&dir)
        // 127.0.0.1 only: the API runs rekcod commands as you.
        .env("HOSTNAME", "127.0.0.1")
        .env("PORT", port.to_string())
        .env("REKCOD_BIN", std::env::current_exe()?)
        .stdin(Stdio::null());
    Ok(c)
}

fn record(paths: &Paths, pid: u32, port: u16) -> Result<()> {
    let f = run_file(paths);
    std::fs::create_dir_all(f.parent().context("run dir")?)?;
    std::fs::write(f, json!({"pid": pid, "port": port}).to_string())?;
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
    let url = format!("http://localhost:{port}");
    if foreground {
        let mut child = command(paths, port)?.spawn().context("start node")?;
        record(paths, child.id(), port)?;
        eprintln!("rekcod dashboard: {url}");
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
    let child = command(paths, port)?.stdout(out.try_clone()?).stderr(out).spawn().context("start node")?;
    record(paths, child.id(), port)?;
    let t0 = Instant::now();
    while !answers(port) {
        if t0.elapsed() > Duration::from_secs(20) {
            bail!("the dashboard didn't answer on {url} within 20 s; see {}", log.display());
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
