//! User-facing commands: start, stop, status, import, shell, and docker passthrough.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::docker::{self, CONTEXT};
use crate::http;
use crate::paths::Paths;
use crate::vm;

const HELPER_IMAGE: &str = "alpine:3.22";

fn block_on<F: Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime").block_on(f)
}

/// GET on the rekcod docker socket.
pub(crate) fn api_get(paths: &Paths, path: &str) -> Result<Vec<u8>> {
    block_on(async {
        let stream = tokio::net::UnixStream::connect(paths.socket()).await?;
        tokio::time::timeout(Duration::from_secs(5), http::get(stream, path)).await?
    })
}

/// The daemon's pid while one runs. The daemon holds its pid file locked
/// ([`Paths::hold_pid_file`]); a file nobody holds is left over from a crash.
pub(crate) fn running_pid(paths: &Paths) -> Option<i32> {
    let pid: i32 = std::fs::read_to_string(paths.pid_file()).ok()?.trim().parse().ok()?;
    if crate::paths::locked(&paths.pid_file()) {
        return Some(pid);
    }
    // Daemons from before the lock still run after an upgrade: trust the pid only if it's
    // alive and is a rekcod binary.
    (unsafe { libc::kill(pid, 0) } == 0 && pid_is_rekcod(pid)).then_some(pid)
}

fn pid_is_rekcod(pid: i32) -> bool {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    n > 0 && std::str::from_utf8(&buf[..n as usize]).is_ok_and(|p| p.ends_with("/rekcod"))
}

#[derive(serde::Serialize, serde::Deserialize, Default, Clone, Copy, PartialEq, Debug)]
pub struct Saved {
    pub cpus: Option<usize>,
    pub memory_gib: Option<u64>,
    pub disk_gib: Option<u64>,
}

/// `rekcod start --cpus/--memory/--disk`, kept so the menu bar, passthrough commands and
/// the login agent boot the same VM.
pub(crate) fn saved(paths: &Paths) -> Saved {
    std::fs::read(paths.config()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_settings(paths: &Paths, cpus: Option<usize>, memory_gib: Option<u64>, disk_gib: u64) -> Result<()> {
    let old = saved(paths);
    let new = Saved {
        cpus: cpus.or(old.cpus),
        memory_gib: memory_gib.or(old.memory_gib),
        disk_gib: Some(disk_gib.max(old.disk_gib.unwrap_or(0))),
    };
    if new != old {
        std::fs::write(paths.config(), serde_json::to_vec_pretty(&new)?)?;
    }
    Ok(())
}

/// (total, free) GiB on the volume holding `dir`.
pub(crate) fn disk_gib(dir: &std::path::Path) -> Option<(u64, u64)> {
    let c = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).ok()?;
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let bs = s.f_bsize as u64;
    Some(((s.f_blocks * bs) >> 30, (s.f_bavail * bs) >> 30))
}

pub(crate) fn free_disk_gib(dir: &std::path::Path) -> Option<u64> {
    disk_gib(dir).map(|(_, free)| free)
}

pub(crate) fn sysctl_u64(name: &str) -> Option<u64> {
    let name = std::ffi::CString::new(name).ok()?;
    let mut value: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    let rc = unsafe { libc::sysctlbyname(name.as_ptr(), (&mut value as *mut u64).cast(), &mut len, std::ptr::null_mut(), 0) };
    (rc == 0).then_some(value)
}

/// Performance cores only: vCPUs that land on efficiency cores, and the cost of
/// coordinating many vCPUs, made pgbench ~1.7x slower with all 18 cores of an
/// M5 Pro than with its 6 performance cores.
fn default_cpus() -> usize {
    let host = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    match sysctl_u64("hw.perflevel0.physicalcpu") {
        Some(p) if p > 0 => (p as usize).clamp(4.min(host), host),
        _ => host.div_ceil(2).max(1),
    }
}

fn random_mac() -> Result<String> {
    let mut b = [0u8; 6];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    b[0] = (b[0] & 0xfe) | 0x02; // unicast, locally administered
    Ok(b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(":"))
}

/// Root disk follows the installed guest version; data disk and MAC persist.
fn prepare_vm(paths: &Paths, disk_gib: u64) -> Result<()> {
    let guest = paths.guest_dir()?;
    let version = std::fs::read_to_string(guest.join("VERSION")).unwrap_or_else(|_| "dev".into());
    let installed = std::fs::read_to_string(paths.guest_version()).unwrap_or_default();
    if !paths.root_disk().exists() || installed.trim() != version.trim() {
        // std::fs::copy clones on APFS, so this is instant and takes no space.
        std::fs::copy(guest.join("root.img"), paths.root_disk()).context("install root disk")?;
        std::fs::write(paths.guest_version(), &version)?;
    }
    // Sparse; the guest grows the filesystem to match at boot.
    if !paths.data_disk().exists() {
        // Only a disk we just made may be formatted (boot.sh); removed once dockerd answers.
        std::fs::write(paths.data_fresh(), b"")?;
    }
    let f = OpenOptions::new().create(true).truncate(false).write(true).open(paths.data_disk())?;
    if f.metadata()?.len() < disk_gib << 30 {
        f.set_len(disk_gib << 30)?;
    }
    if !paths.mac_address().exists() {
        std::fs::write(paths.mac_address(), random_mac()?)?;
    }
    Ok(())
}

fn tail(path: &std::path::Path, lines: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

pub struct BootOptions {
    pub cpus: Option<usize>,
    pub memory_gib: Option<u64>,
    pub disk_gib: u64,
    /// Sandbox VM: share only this directory (docs/sandbox.md).
    pub project: Option<PathBuf>,
    /// Guest image to boot (the sandbox uses the main install's).
    pub guest_dir: Option<PathBuf>,
    /// Agent sandbox: share this copy as the project (docs/agent-sandboxes.md).
    pub workspace: Option<PathBuf>,
    /// Agent sandbox: egress allowlist, comma-separated.
    pub allow: Option<String>,
    /// Sandbox: `IP:PORT`s on the Mac or LAN it may still reach (RustFS for app instances).
    pub reach: Vec<String>,
    /// Agent sandbox: power off after this many idle seconds.
    pub idle_ttl: Option<u64>,
    /// `e2fsck -fy` the data disk and reinstall the root disk before booting.
    pub repair: bool,
}

impl Default for BootOptions {
    fn default() -> Self {
        BootOptions {
            cpus: None,
            memory_gib: None,
            disk_gib: crate::paths::DEFAULT_DISK_GIB,
            project: None,
            guest_dir: None,
            workspace: None,
            allow: None,
            reach: vec![],
            idle_ttl: None,
            repair: false,
        }
    }
}

/// Boots the VM unless it is running; returns the pid when it already was.
pub(crate) fn boot(paths: &Paths, opts: &BootOptions) -> Result<Option<i32>> {
    if let Some(pid) = running_pid(paths) {
        return Ok(Some(pid));
    }
    let _lock = crate::paths::lock(&paths.boot_lock())?;
    // Another command may have booted it while we waited for the lock.
    if let Some(pid) = running_pid(paths) {
        return Ok(Some(pid));
    }
    {
        if let Some(dir) = &opts.guest_dir {
            // SAFETY: single-threaded CLI, before spawning anything.
            unsafe { std::env::set_var("REKCOD_GUEST_DIR", dir) };
        }
        // The main VM boots with what `rekcod start` was last given; sandboxes say their own.
        let saved = if opts.project.is_none() { saved(paths) } else { Saved::default() };
        if opts.repair {
            // A fresh copy of the release's root disk; it holds nothing of yours.
            let _ = std::fs::remove_file(paths.root_disk());
        }
        prepare_vm(paths, opts.disk_gib.max(saved.disk_gib.unwrap_or(0)))?;
        let cpus = opts.cpus.or(saved.cpus).unwrap_or_else(default_cpus).clamp(1, vm::max_cpus());
        let host_mib = sysctl_u64("hw.memsize").unwrap_or(16 << 30) >> 20;
        let memory_mib = opts.memory_gib.or(saved.memory_gib).map(|g| g << 10).unwrap_or((host_mib / 2).min(8 << 10));
        if let Some(free) = free_disk_gib(&paths.home).filter(|f| *f < 5) {
            eprintln!("rekcod: warning: only {free} GiB free on the Mac's disk; the VM's disk fails when it runs out");
        }
        let console_start = std::fs::metadata(paths.console_log()).map(|m| m.len()).unwrap_or(0);

        let log = OpenOptions::new().create(true).append(true).open(paths.daemon_log())?;
        let mut cmd = Command::new(std::env::current_exe()?);
        cmd.args(["daemon", "--cpus", &cpus.to_string(), "--memory-mib", &memory_mib.to_string()])
            .env("REKCOD_HOME", &paths.home)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        if let Some(project) = &opts.project {
            cmd.arg("--project").arg(project);
        }
        if let Some(ws) = &opts.workspace {
            cmd.arg("--workspace").arg(ws);
        }
        if let Some(allow) = &opts.allow {
            cmd.args(["--allow", allow]);
        }
        if !opts.reach.is_empty() {
            cmd.args(["--reach", &opts.reach.join(",")]);
        }
        if let Some(ttl) = opts.idle_ttl {
            cmd.args(["--idle-ttl", &ttl.to_string()]);
        }
        if opts.repair {
            cmd.arg("--repair");
        }
        // Detach from the terminal so closing it doesn't kill the VM.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut child = cmd.spawn().context("spawn rekcod daemon")?;

        let deadline = Instant::now() + Duration::from_secs(if opts.repair { 1800 } else { 120 });
        let mut checked = Instant::now();
        loop {
            if api_get(paths, "/_ping").is_ok_and(|b| b == b"OK") {
                let _ = std::fs::remove_file(paths.data_fresh());
                break;
            }
            if checked.elapsed() > Duration::from_millis(500) {
                checked = Instant::now();
                if let Some(err) = data_disk_error(paths, console_start) {
                    bail!(
                        "the VM's data disk can't be used: {err}\nNothing was formatted. `rekcod stop`, then `rekcod start --repair` runs e2fsck -fy on it.\nsee {}",
                        paths.console_log().display()
                    );
                }
            }
            if let Some(status) = child.try_wait()? {
                bail!(
                    "daemon exited ({status}).\n--- {} ---\n{}\n--- {} ---\n{}",
                    paths.daemon_log().display(),
                    tail(&paths.daemon_log(), 15),
                    paths.console_log().display(),
                    tail(&paths.console_log(), 25),
                );
            }
            if Instant::now() > deadline {
                bail!("dockerd did not answer within 120s; see {}", paths.console_log().display());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    Ok(None)
}

/// The guest's "data disk error" line from this boot's part of the console log.
fn data_disk_error(paths: &Paths, from: u64) -> Option<String> {
    use std::io::{Seek, SeekFrom};
    let mut f = std::fs::File::open(paths.console_log()).ok()?;
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut text = String::new();
    f.take(1 << 20).read_to_string(&mut text).ok()?;
    text.lines().find_map(|l| l.split_once("data disk error: ").map(|(_, e)| e.trim().to_string()))
}

/// SIGTERM the daemon and wait for the VM to power off; false if it wasn't running.
pub(crate) fn stop_daemon(paths: &Paths) -> Result<bool> {
    signal_daemon(paths, libc::SIGTERM)
}

/// Sends `sig` to the daemon and waits for it to exit (SIGUSR1: force stop the VM).
pub(crate) fn signal_daemon(paths: &Paths, sig: libc::c_int) -> Result<bool> {
    let Some(pid) = running_pid(paths) else { return Ok(false) };
    unsafe { libc::kill(pid, sig) };
    let deadline = Instant::now() + Duration::from_secs(45);
    while unsafe { libc::kill(pid, 0) } == 0 {
        // `rekcod x --sandbox` spawned this daemon itself: reap it, or its zombie answers kill(0).
        if unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) } == pid {
            break;
        }
        if Instant::now() > deadline {
            bail!("daemon (pid {pid}) did not exit; see {}", paths.daemon_log().display());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(true)
}

pub fn start(paths: &Paths, cpus: Option<usize>, memory_gib: Option<u64>, disk_gib: u64, no_switch: bool, repair: bool) -> Result<()> {
    let t0 = Instant::now();
    if repair && let Some(pid) = running_pid(paths) {
        bail!("rekcod is running (pid {pid}); `rekcod stop` first, then `rekcod start --repair`");
    }
    save_settings(paths, cpus, memory_gib, disk_gib)?;
    if autostart_installed() && !repair {
        // Let the login agent own the VM, so a crash brings it back. Whoever boots first
        // wins the boot lock; the agent supervises either way.
        crate::service::kickstart(AUTOSTART);
    }
    let opts = BootOptions { cpus, memory_gib, disk_gib, repair, ..Default::default() };
    if repair {
        println!("checking the data disk with e2fsck -fy and reinstalling the root disk; this can take a while");
    }
    if let Some(pid) = boot(paths, &opts)? {
        println!("rekcod is already running (pid {pid})");
    }
    let version = api_get(paths, "/version")
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v["Version"].as_str().map(String::from))
        .unwrap_or_else(|| "?".into());
    println!("rekcod ready in {:.1}s · dockerd {version}", t0.elapsed().as_secs_f64());

    if !docker::available() {
        println!("no docker CLI installed: use `rekcod <docker command>` (e.g. `rekcod ps`, `rekcod compose up`),");
        println!("or point other clients at it: export DOCKER_HOST={}", paths.docker_host());
        // Default socket still works for API clients without the CLI.
        default_socket_on_start(paths, no_switch)?;
        return Ok(());
    }
    if no_switch && !paths.is_default_home() {
        println!("docker context `{CONTEXT}` left alone (--no-switch, REKCOD_HOME); export DOCKER_HOST={}", paths.docker_host());
        return Ok(());
    }
    docker::ensure_context(&paths.docker_host())?;
    if no_switch {
        println!("docker context `{CONTEXT}` is available (not switched: --no-switch)");
    } else {
        let previous = docker::current_context().unwrap_or_else(|_| "default".into());
        if previous != CONTEXT {
            std::fs::write(paths.prev_context(), &previous)?;
            docker::use_context(CONTEXT)?;
            println!("docker context: {CONTEXT} (was {previous}; `rekcod stop` switches back)");
        } else {
            println!("docker context: {CONTEXT}");
        }
    }
    default_socket_on_start(paths, no_switch)?;
    if std::env::var_os("DOCKER_HOST").is_some() {
        println!("warning: DOCKER_HOST is set and overrides docker contexts; `unset DOCKER_HOST` to use rekcod");
    }
    Ok(())
}

fn default_socket_on_start(paths: &Paths, no_switch: bool) -> Result<()> {
    if no_switch {
        return Ok(());
    }
    if crate::syssock::on_start(paths)? {
        println!("default socket: rekcod (/var/run/docker.sock, for clients that ignore contexts)");
        return Ok(());
    }
    // First start from a terminal: set it up (sudo asks once). Never again after that,
    // whether it worked or not; the menu bar (no terminal) only shows the menu item.
    let asked = paths.home.join("default.sock.asked");
    let tty = unsafe { libc::isatty(0) } == 1;
    if tty && !asked.exists() {
        std::fs::write(&asked, "")?;
        println!("routing /var/run/docker.sock to rekcod too, for clients that ignore docker contexts");
        if let Err(e) = crate::syssock::install(paths, true) {
            println!("skipped ({e:#}); run `rekcod socket install` later");
        }
    } else {
        println!("tip: `rekcod socket install` routes /var/run/docker.sock here too (oxker, SDKs, IDE plugins)");
    }
    Ok(())
}

/// The login agent that `rekcod start --install` writes (`dev.rekcod.vm`).
pub const AUTOSTART: &str = "vm";

pub fn autostart_install(paths: &Paths) -> Result<()> {
    let args: Vec<String> = ["start", "--no-switch", "--foreground"].map(String::from).into();
    crate::service::install_with(paths, AUTOSTART, &args, true)
}

pub fn autostart_installed() -> bool {
    crate::service::installed(AUTOSTART)
}

/// `rekcod start --foreground`, the login agent's process: waits while the VM runs. Exits 0
/// when it stopped on purpose (so launchd leaves it stopped) and 1 when it crashed (so
/// launchd boots it again). A boot that fails also exits 0: retrying a broken disk every
/// few seconds helps nobody.
pub fn supervise(paths: &Paths, started: Result<()>) -> Result<()> {
    if let Err(e) = started {
        eprintln!("rekcod: boot failed, not retrying: {e:#}");
        return Ok(());
    }
    while running_pid(paths).is_some() {
        std::thread::sleep(Duration::from_secs(2));
    }
    let how = std::fs::read_to_string(paths.last_exit()).unwrap_or_default();
    if how.trim() == "clean" {
        return Ok(());
    }
    eprintln!("rekcod: the VM stopped without `rekcod stop`; launchd will boot it again");
    std::process::exit(1);
}

pub fn stop(paths: &Paths, no_switch: bool, force: bool) -> Result<()> {
    let stopped = if force { signal_daemon(paths, libc::SIGUSR1)? } else { stop_daemon(paths)? };
    if stopped {
        println!("rekcod stopped");
    } else {
        println!("rekcod is not running");
    }
    if paths.home.join("sandbox").is_dir()
        && let Ok(sandbox) = crate::xrun::sandbox_paths(paths)
        && stop_daemon(&sandbox)?
    {
        println!("sandbox VM stopped");
    }
    if !no_switch && docker::available() && docker::current_context().is_ok_and(|c| c == CONTEXT) {
        let mut previous = std::fs::read_to_string(paths.prev_context()).unwrap_or_default().trim().to_string();
        if previous.is_empty() || !docker::context_exists(&previous) {
            previous = "default".into();
        }
        docker::use_context(&previous)?;
        println!("docker context: {previous}");
    }
    if !no_switch {
        crate::syssock::on_stop(paths)?;
    }
    Ok(())
}

/// FUSE opcodes worth naming in `rekcod status` (linux/fuse.h).
fn fuse_op(op: &str) -> &str {
    match op {
        "1" => "lookup",
        "3" => "getattr",
        "14" => "open",
        "15" => "read",
        "16" => "write",
        "26" => "init",
        "27" => "opendir",
        "28" => "readdir",
        "35" => "create",
        "44" => "readdirplus",
        other => other,
    }
}

/// Everything `rekcod status` knows, as one JSON value (`status --json`).
pub(crate) fn status_value(paths: &Paths) -> serde_json::Value {
    use serde_json::json;
    use std::os::unix::fs::MetadataExt;
    let Some(pid) = running_pid(paths) else { return json!({"running": false}) };
    let file: serde_json::Value = std::fs::read(paths.status_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let version: serde_json::Value = api_get(paths, "/version").ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    // Older guests answer `error: ...`; their stats are just missing.
    let stats: serde_json::Value = crate::daemon::send_control(paths, &rekcod_proto::Control::Stats)
        .ok()
        .and_then(|r| serde_json::from_str(&r).ok())
        .unwrap_or_default();
    let on_mac = std::fs::metadata(paths.data_disk()).map(|m| m.blocks() * 512).unwrap_or(0);
    let mut v = json!({
        "running": true,
        "pid": pid,
        "dockerd": version["Version"],
        "api": version["ApiVersion"],
        "kernel": version["KernelVersion"],
        "cpus": file["cpus"],
        "memory_mib": file["memory_mib"],
        "fs_cache": file["fs_cache"],
        "socket": paths.docker_host(),
        "ports": file["ports"],
        "disk": {"used_in_vm": stats["disk_used"], "size": stats["disk_size"], "on_mac": on_mac},
        "warming": stats["warming"],
        "fs_requests": stats["fs_requests"],
    });
    if let Some(project) = file.get("project") {
        v["project"] = project.clone();
    }
    v["default_socket"] = crate::syssock::state(paths).into();
    if docker::available() {
        v["context"] = docker::current_context().ok().into();
    }
    v
}

/// Port lines for `rekcod status`; a 127.0.0.1 + ::1 pair (autoports) is `localhost`.
fn port_lines(ports: &[serde_json::Value]) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for p in ports {
        let addr = p["address"].as_str().unwrap_or("?");
        let addr = match addr {
            "::" => "*",
            "127.0.0.1" | "::1" => "localhost",
            a => a,
        };
        let note = p["error"].as_str().map(|e| format!("  ! not forwarded: {e}")).unwrap_or_default();
        let udp = if p["proto"].as_str() == Some("udp") { "/udp" } else { "" };
        let line = format!("{addr}:{}{udp} -> {}{note}", p["port"], p["container"].as_str().unwrap_or("?"));
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    lines
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1u64 << 30) as f64)
}

/// The sandbox VM's status, when it runs.
pub(crate) fn sandbox_status(paths: &Paths) -> Option<serde_json::Value> {
    paths
        .home
        .join("sandbox")
        .is_dir()
        .then(|| crate::xrun::sandbox_paths(paths).ok())
        .flatten()
        .map(|sb| status_value(&sb))
        .filter(|s| s["running"] == true)
}

pub fn status(paths: &Paths, json: bool) -> Result<()> {
    let v = status_value(paths);
    let sandbox = sandbox_status(paths);
    if json {
        let mut out = v;
        out["sandbox"] = sandbox.unwrap_or_else(|| serde_json::json!({"running": false}));
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    if v["running"] != true {
        println!("rekcod is not running (`rekcod start`)");
        return Ok(());
    }
    let s = |k: &str| v[k].as_str().unwrap_or("?").to_string();
    println!("rekcod running (pid {})", v["pid"]);
    println!("  dockerd   {} (API {})", s("dockerd"), s("api"));
    println!("  kernel    {}", s("kernel"));
    println!("  vm        {} CPUs, {} MiB", v["cpus"], v["memory_mib"]);
    let sharing = if v["fs_cache"] == true { "cached, invalidated by FSEvents" } else { "uncached (stock virtio-fs)" };
    println!("  files     {sharing}");
    if let Some(ops) = v["fs_requests"].as_object().filter(|o| !o.is_empty()) {
        let total: u64 = ops.values().filter_map(|n| n.as_u64()).sum();
        let mut top: Vec<(&str, u64)> = ops.iter().map(|(k, n)| (fuse_op(k), n.as_u64().unwrap_or(0))).collect();
        top.sort_by_key(|t| std::cmp::Reverse(t.1));
        let top: Vec<String> = top.iter().take(4).map(|(k, n)| format!("{k} {n}")).collect();
        println!("  cache     {total} trips to macOS since boot ({})", top.join(", "));
    }
    for w in v["warming"].as_array().into_iter().flatten() {
        println!("  warming   {}", w.as_str().unwrap_or("?"));
    }
    if let (Some(used), Some(size)) = (v["disk"]["used_in_vm"].as_u64(), v["disk"]["size"].as_u64()) {
        let on_mac = v["disk"]["on_mac"].as_u64().unwrap_or(0);
        println!("  disk      {} used of {}, {} on the Mac (`rekcod compact` returns the gap)", gib(used), gib(size), gib(on_mac));
    }
    println!("  socket    {}", s("socket"));
    if let Some(c) = v["context"].as_str() {
        println!("  context   {c}");
    }
    println!("  default   /var/run/docker.sock: {}", s("default_socket"));
    let ports = port_lines(v["ports"].as_array().map(Vec::as_slice).unwrap_or_default());
    if ports.is_empty() {
        println!("  ports     none published");
    }
    for p in ports {
        println!("  port      {p}");
    }
    if let Some(sb) = sandbox {
        println!("  sandbox   running for {} ({} MiB)", sb["project"].as_str().unwrap_or("?"), sb["memory_mib"]);
        for p in port_lines(sb["ports"].as_array().map(Vec::as_slice).unwrap_or_default()) {
            println!("  port      {p} (sandbox)");
        }
    }
    println!("  logs      {} , {}", paths.daemon_log().display(), paths.console_log().display());
    Ok(())
}

pub fn compact(paths: &Paths) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let used = || std::fs::metadata(paths.data_disk()).map(|m| m.blocks() * 512).unwrap_or(0);
    let before = used();
    let reply = crate::daemon::send_control(paths, &rekcod_proto::Control::Trim)?;
    if !reply.starts_with("ok") {
        bail!("compact failed: {reply}");
    }
    let after = used();
    let gib = |b: u64| b as f64 / (1u64 << 30) as f64;
    println!(
        "data disk: {:.2} GiB -> {:.2} GiB on the Mac ({:.2} GiB returned)",
        gib(before),
        gib(after),
        gib(before.saturating_sub(after))
    );
    Ok(())
}

/// `docker <args>` against rekcod's socket, booting the VM first if needed.
pub fn passthrough(paths: &Paths, args: &[String]) -> Result<std::convert::Infallible> {
    if running_pid(paths).is_none() {
        let t0 = Instant::now();
        eprintln!("rekcod: starting the VM");
        boot(paths, &BootOptions::default())?;
        eprintln!("rekcod: ready in {:.1}s", t0.elapsed().as_secs_f64());
    }
    let mut cmd = docker::any_cli(paths)?;
    cmd.env("DOCKER_HOST", paths.docker_host()).args(args);
    Err(cmd.exec()).context("exec docker")
}

pub fn shell(paths: &Paths) -> Result<()> {
    let interactive = unsafe { libc::isatty(0) } == 1;
    let host = paths.docker_host();
    let mut cmd = docker::any_cli(paths)?;
    cmd.args(["-H", &host, "run", "--rm", if interactive { "-it" } else { "-i" }]);
    cmd.args(["--privileged", "--pid=host", "--net=host", HELPER_IMAGE]);
    cmd.args(["nsenter", "-t", "1", "-m", "-u", "-i", "-n", "-p", "--", "sh"]);
    Err(cmd.exec()).context("exec docker")
}

// ---------------------------------------------------------------- import

fn is_anonymous_volume(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Copies volume `name` with `docker cp` (the archive API) between two idle
/// helper containers, then checks that size and file count match. Streaming
/// through `docker run` stdout wedges Docker Desktop's API after a few hundred
/// MB, and a tar file on a shared Mac directory can be read stale by the guest.
fn copy_volume(src: &[&str], dst: &[&str], name: &str) -> Result<()> {
    let helper =
        |ctx: &[&str], mount: &str| -> Result<String> { docker::run_ok(&[ctx, &["create", "-v", mount, HELPER_IMAGE, "true"]].concat()) };
    let from = helper(src, &format!("{name}:/v:ro"))?;
    let result = helper(dst, &format!("{name}:/v")).and_then(|to| {
        // The target may hold a previous partial copy (or data under --force).
        let r = docker::run_ok(&[dst, &["run", "--rm", "-v", &format!("{name}:/v"), HELPER_IMAGE, "find", "/v", "-mindepth", "1", "-delete"]].concat())
            .and_then(|_| pipe(&[src, &["cp", "-a", &format!("{from}:/v/."), "-"]].concat(), &[dst, &["cp", "-a", "-", &format!("{to}:/v")]].concat()))
            .and_then(|()| {
                let measure = |ctx: &[&str]| {
                    docker::run_ok(&[ctx, &["run", "--rm", "-v", &format!("{name}:/v:ro"), HELPER_IMAGE, "sh", "-c", "echo $(find /v | wc -l) entries, $(find /v -type f -exec stat -c %s {} + | awk '{s+=$1} END {print s+0}') bytes"]].concat())
                };
                let (a, b) = (measure(src)?, measure(dst)?);
                if a != b {
                    bail!("copy does not match: {a} at the source, {b} here");
                }
                Ok(())
            });
        let _ = docker::run(&[dst, &["rm", "-f", &to]].concat());
        r
    });
    let _ = docker::run(&[src, &["rm", "-f", &from]].concat());
    result
}

/// `docker <src...> | docker <dst...>`; returns stderr of whichever side failed.
fn pipe(src: &[&str], dst: &[&str]) -> Result<()> {
    let mut producer = docker::cli().args(src).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let stdout = producer.stdout.take().context("producer stdout")?;
    let consumer = docker::cli().args(dst).stdin(stdout).stdout(Stdio::null()).stderr(Stdio::piped()).output()?;
    let produced = producer.wait_with_output()?;
    if !produced.status.success() {
        bail!("{}", String::from_utf8_lossy(&produced.stderr).trim());
    }
    if !consumer.status.success() {
        bail!("{}", String::from_utf8_lossy(&consumer.stderr).trim());
    }
    Ok(())
}

fn ensure_image(target: &[&str], image: &str) -> Result<()> {
    let mut inspect = target.to_vec();
    inspect.extend(["image", "inspect", image]);
    if !docker::run(&inspect)?.status.success() {
        let mut pull = target.to_vec();
        pull.extend(["pull", "-q", image]);
        docker::run_ok(&pull)?;
    }
    Ok(())
}

pub fn import(paths: &Paths, from: &str, only: &[String], images: bool, volumes: bool, force: bool) -> Result<()> {
    let wanted = |name: &str| only.is_empty() || only.iter().any(|o| o == name);
    if from == CONTEXT {
        bail!("--from must be another docker context, not `{CONTEXT}`");
    }
    if !api_get(paths, "/_ping").is_ok_and(|b| b == b"OK") {
        bail!("rekcod is not running; `rekcod start` first");
    }
    let host = paths.docker_host();
    let dst: [&str; 2] = ["-H", &host];
    let src: [&str; 2] = ["--context", from];
    docker::run_ok(&[&src[..], &["version", "--format", "{{.Server.Version}}"]].concat())
        .with_context(|| format!("source context `{from}` is not reachable"))?;

    let (mut imported, mut skipped) = (0, 0);
    if images {
        let list = docker::run_ok(&[&src[..], &["image", "ls", "--format", "{{.Repository}}:{{.Tag}}"]].concat())?;
        let mut refs: Vec<&str> = list.lines().filter(|r| !r.contains("<none>") && wanted(r)).collect();
        refs.sort();
        refs.dedup();
        println!("images: {} in {from}", refs.len());
        for (i, r) in refs.iter().enumerate() {
            let t = Instant::now();
            if docker::run(&[&dst[..], &["image", "inspect", r]].concat())?.status.success() {
                println!("  [{}/{}] {r}: already here", i + 1, refs.len());
                skipped += 1;
                continue;
            }
            let load = [&dst[..], &["load", "-q"]].concat();
            let mut result = pipe(&[&src[..], &["save", r]].concat(), &load);
            if result.is_err() {
                // containerd image stores may hold partial multi-platform indexes; export just the local platform.
                let platform = docker::run_ok(&[&src[..], &["image", "inspect", "--format", "{{.Os}}/{{.Architecture}}", r]].concat())?;
                result = pipe(&[&src[..], &["save", "--platform", &platform, r]].concat(), &load);
            }
            match result {
                Ok(()) => {
                    imported += 1;
                    println!("  [{}/{}] {r}: ok ({:.1}s)", i + 1, refs.len(), t.elapsed().as_secs_f64());
                }
                Err(e) => println!("  [{}/{}] {r}: FAILED: {e:#}", i + 1, refs.len()),
            }
        }
    }

    if volumes {
        let list = docker::run_ok(&[&src[..], &["volume", "ls", "--format", "{{.Name}}"]].concat())?;
        let names: Vec<&str> = list.lines().filter(|n| !n.is_empty()).collect();
        let anonymous = names.iter().filter(|n| is_anonymous_volume(n)).count();
        let named: Vec<&str> = names.into_iter().filter(|n| !is_anonymous_volume(n) && wanted(n)).collect();
        println!("volumes: {} named in {from} ({anonymous} anonymous skipped)", named.len());
        if !named.is_empty() {
            ensure_image(&src, HELPER_IMAGE)?;
            ensure_image(&dst, HELPER_IMAGE)?;
        }
        for name in named {
            let users = docker::run_ok(&[&src[..], &["ps", "--filter", &format!("volume={name}"), "--format", "{{.Names}}"]].concat())?;
            if !users.is_empty() && !force {
                println!("  {name}: skipped, in use by {} (stop it at the source first, or --force)", users.replace('\n', ", "));
                skipped += 1;
                continue;
            }
            if docker::run(&[&dst[..], &["volume", "inspect", name]].concat())?.status.success() {
                let mount = format!("{name}:/v:ro");
                let probe = [&dst[..], &["run", "--rm", "-v", &mount, HELPER_IMAGE, "ls", "-A", "/v"]].concat();
                if !docker::run_ok(&probe)?.is_empty() && !force {
                    println!("  {name}: skipped, already has data here (--force to overwrite)");
                    skipped += 1;
                    continue;
                }
            } else {
                let fmt = "{{.Driver}}\t{{json .Labels}}";
                let info = docker::run_ok(&[&src[..], &["volume", "inspect", "--format", fmt, name]].concat())?;
                let (driver, labels) = info.split_once('\t').unwrap_or(("local", "null"));
                if driver != "local" {
                    println!("  {name}: skipped, driver `{driver}` is not supported");
                    skipped += 1;
                    continue;
                }
                let labels: BTreeMap<String, String> = serde_json::from_str(labels).ok().flatten().unwrap_or_default();
                let mut create: Vec<String> = dst.iter().map(|s| s.to_string()).collect();
                create.extend(["volume".into(), "create".into()]);
                // Compose checks its labels before reusing a volume.
                for (k, v) in &labels {
                    create.extend(["--label".into(), format!("{k}={v}")]);
                }
                create.push(name.into());
                docker::run_ok(&create.iter().map(String::as_str).collect::<Vec<_>>())?;
            }
            let t = Instant::now();
            let result = copy_volume(&src, &dst, name);
            match result {
                Ok(()) => {
                    imported += 1;
                    println!("  {name}: ok ({:.1}s)", t.elapsed().as_secs_f64());
                }
                Err(e) => println!("  {name}: FAILED: {e:#}"),
            }
        }
    }
    println!("imported {imported}, skipped {skipped}. Containers are not copied: run `docker compose up -d` in your projects.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_lines_collapse_localhost_pairs() {
        let ports: Vec<serde_json::Value> = serde_json::from_str(
            r#"[{"proto":"tcp","address":"127.0.0.1","port":3000,"container":"x"},
                {"proto":"tcp","address":"::1","port":3000,"container":"x"},
                {"proto":"udp","address":"::","port":53,"container":"dns","error":"in use"}]"#,
        )
        .unwrap();
        assert_eq!(port_lines(&ports), vec!["localhost:3000 -> x", "*:53/udp -> dns  ! not forwarded: in use"]);
    }

    #[test]
    fn anonymous_volume_names() {
        assert!(is_anonymous_volume(&"a".repeat(64)));
        assert!(!is_anonymous_volume("myapp_pgdata"));
        assert!(!is_anonymous_volume(&"g".repeat(64)));
    }
}
