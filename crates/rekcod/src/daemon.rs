//! `rekcod daemon`: owns the VM and everything that talks to it.
//!
//! - `~/.rekcod/run/docker.sock` -> vsock DOCKER_PORT (raw byte pipe)
//! - published ports: watches `/events`, mirrors each port on the Mac,
//!   tunnels connections to vsock FORWARD_PORT
//! - published ports: TCP and UDP, mirrored on the Mac
//! - `host.docker.internal`: keeps connections parked in the guest that the
//!   agent hands back to be spliced to the Mac's `127.0.0.1:<port>`
//! - pushes the Mac clock to the guest every 30 s
//! - `~/.rekcod/run/control.sock`: relays `rekcod compact` (and research
//!   commands) to the guest agent
//! - sandbox VMs: serves their network card and checks every connection (`netstack.rs`),
//!   and refuses DNS for names off the allowlist
//! - SIGTERM: ask the guest to power off, force it after a timeout

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::{SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rekcod_proto::{
    CAP_FS_NOTIFY, CONTROL_PORT, Control, DNS_PORT, DOCKER_PORT, FORWARD_PORT, META_PORT, Proto, REVERSE_PORT, forward_header, frame,
    parse_reverse_header,
};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, copy_bidirectional};
use tokio::net::{TcpListener, TcpStream, UdpSocket, UnixListener};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::dns;
use crate::http;
use crate::memory;
use crate::netstack;
use crate::paths::Paths;
use crate::ports::{self, Binding};
use crate::vm::{Net, State, Vm, VmConfig};
use crate::watch::{self, Watch};

// Stock-kernel guests: virtio-rng must load from the initramfs, or boot stalls ~5s
// waiting for entropy. The custom kernel has it built in and ignores `modules=`.
const KERNEL_CMDLINE: &str = "console=hvc0 root=/dev/vda rootfstype=ext4 modules=virtio_blk,ext4,virtio-rng rw quiet";

macro_rules! log {
    ($($arg:tt)*) => {{
        let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        eprintln!("[{}.{:03}] {}", t.as_secs(), t.subsec_millis(), format!($($arg)*));
    }};
}

pub struct Options {
    pub cpus: usize,
    pub memory_mib: u64,
    /// Sandbox VM (docs/sandbox.md): share only this directory, and nothing
    /// on the Mac is reachable from the guest.
    pub project: Option<PathBuf>,
    /// Agent sandbox (docs/agent-sandboxes.md): the Mac directory shared as `project`,
    /// when it's a copy of the project rather than the project itself.
    pub workspace: Option<PathBuf>,
    /// Egress allowlist (comma-separated names); none means the internet is open.
    pub allow: Option<String>,
    /// `IP:PORT,...` the sandbox may reach although it's on the Mac or LAN.
    pub reach: Option<String>,
    /// Power off after this many seconds without an exec (`run/last-used`).
    pub idle_ttl: Option<u64>,
    /// `e2fsck -fy` the data disk at boot (`rekcod start --repair`).
    pub repair: bool,
}

/// `IP:PORT,...` checked, so nothing else lands on the kernel command line (boot.sh reads it).
pub fn reach_arg(s: &str) -> Result<String> {
    Ok(reach_addrs(s)?.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(","))
}

fn reach_addrs(s: &str) -> Result<Vec<SocketAddrV4>> {
    s.split(',')
        .filter(|a| !a.trim().is_empty())
        .map(|a| a.trim().parse::<SocketAddrV4>().map_err(|_| anyhow::anyhow!("--reach {a}: not IPv4:PORT")))
        .collect()
}

/// Standard base64, for passing a path on the kernel command line.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn run(paths: Paths, opts: Options) -> Result<()> {
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(serve(paths, opts))
}

async fn serve(paths: Paths, opts: Options) -> Result<()> {
    let t0 = Instant::now();
    // Held until we exit: a second daemon on the same disk fails here.
    let _pid_lock = paths.hold_pid_file()?;
    let _ = std::fs::remove_file(paths.last_exit());
    raise_fd_limit();
    crate::lifecycle::trim_log_hourly(paths.daemon_log());
    let guest = paths.guest_dir()?;
    let mut cmdline = std::env::var("REKCOD_KERNEL_CMDLINE").unwrap_or_else(|_| KERNEL_CMDLINE.into());
    if paths.data_fresh().exists() {
        cmdline.push_str(" rekcod.fresh=1");
    }
    if opts.repair {
        cmdline.push_str(" rekcod.fsck=yes");
    }
    let shares: Vec<(String, PathBuf)> = match &opts.project {
        Some(dir) => {
            let dir = dir.to_str().context("project path is not UTF-8")?;
            cmdline.push_str(&format!(" rekcod.sandbox=1 rekcod.project={}", base64(dir.as_bytes())));
            if let Some(allow) = &opts.allow {
                cmdline.push_str(&format!(" rekcod.allow={}", base64(allow.as_bytes())));
            }
            if let Some(reach) = opts.reach.as_deref().filter(|r| !r.is_empty()) {
                cmdline.push_str(&format!(" rekcod.reach={}", reach_arg(reach)?));
            }
            vec![("project".into(), opts.workspace.clone().unwrap_or_else(|| PathBuf::from(dir)))]
        }
        None => ["Users", "Volumes", "private"]
            .iter()
            .filter(|d| std::path::Path::new("/").join(d).exists())
            .map(|d| (d.to_string(), std::path::Path::new("/").join(d)))
            .collect(),
    };
    // Sandbox VMs: the network card is a socket this daemon serves, and every connection is
    // checked here (docs/sandbox-network.md). The guest's own firewall stays as a second layer.
    let allow: Option<Vec<String>> = opts
        .allow
        .as_deref()
        .map(|a| a.split(',').map(str::trim).filter(|h| !h.is_empty()).map(String::from).collect())
        .filter(|a: &Vec<String>| !a.iter().any(|h| h == "*"));
    let reach = reach_addrs(opts.reach.as_deref().unwrap_or(""))?;
    let resolved = Arc::new(netstack::Resolved::default());
    let mut host_net = None;
    let net = match &opts.project {
        None => Net::Nat,
        // Offline: an empty allowlist and nothing to reach.
        Some(_) if allow.as_ref().is_some_and(|a| a.is_empty()) && reach.is_empty() => Net::None,
        Some(_) => {
            let (vm_end, ours) = netstack::socketpair().context("create the sandbox network socket")?;
            cmdline.push(' ');
            cmdline.push_str(netstack::CMDLINE);
            host_net = Some(ours);
            Net::Socket(vm_end)
        }
    };
    let mut cfg = VmConfig {
        cpus: opts.cpus,
        memory_mib: opts.memory_mib,
        kernel: guest.join("Image"),
        initrd: Some(guest.join("initramfs")).filter(|p| p.exists()),
        cmdline,
        root_disk: paths.root_disk(),
        data_disk: paths.data_disk(),
        mac_address: std::fs::read_to_string(paths.mac_address())?.trim().to_string(),
        console_log: paths.console_log(),
        shares,
        net,
    };
    let vm = Vm::create(&cfg)?;
    // The VM holds its own copy of its end.
    cfg.net = Net::None;
    let dns_filter = match (&opts.project, &allow) {
        (Some(_), Some(a)) => Some(Arc::new(DnsFilter { allow: a.clone(), resolved: resolved.clone() })),
        _ => None,
    };
    let _net = match host_net {
        Some(fd) => {
            let policy = Arc::new(netstack::Policy::new(allow, reach, resolved));
            Some(netstack::start(fd, policy, Arc::new(|line| log!("{line}"))).context("start the sandbox network")?)
        }
        None => None,
    };
    log!("kernel command line: {}", cfg.cmdline);
    vm.start().await?;
    log!("VM started ({} CPUs, {} MiB, rosetta={})", cfg.cpus, cfg.memory_mib, vm.rosetta);

    // Docker socket first so `rekcod start` can poll it while the guest boots.
    let sock = paths.socket();
    let _ = std::fs::remove_file(&sock);
    let listener = crate::paths::with_umask(0o077, || UnixListener::bind(&sock)).with_context(|| format!("bind {}", sock.display()))?;
    tokio::spawn(docker_proxy(vm.clone(), listener));

    let caps = wait_for_agent(&vm).await?;
    let fs_cache = caps.split_whitespace().any(|c| c == CAP_FS_NOTIFY);
    log!("guest agent up after {:.2}s (cached file sharing: {fs_cache})", t0.elapsed().as_secs_f64());

    let notify_vm = vm.clone();
    let notify_conn: Arc<NotifyConn> = Arc::default();
    let roots = match &opts.project {
        // A copied workspace changes only from inside the guest; events would name the copy's paths.
        Some(_) if opts.workspace.is_some() => vec![],
        Some(dir) => vec![dir.clone()],
        None => watch::GLOBAL_ROOTS.iter().map(PathBuf::from).collect(),
    };
    // Batches the guest didn't take, as directory trees; resent with the next batch and by
    // `retry_notify`. Dropping them would leave containers reading stale files until the
    // (day-long) cache TTL ran out.
    let failed: Arc<std::sync::Mutex<BTreeSet<String>>> = Arc::default();
    tokio::spawn(retry_notify(vm.clone(), notify_conn.clone(), failed.clone()));
    let watch = Watch::start(fs_cache, roots, move |changed: Vec<String>| {
        let vm = notify_vm.clone();
        let conn = notify_conn.clone();
        let failed = failed.clone();
        async move {
            if std::env::var_os("REKCOD_DEBUG").is_some() {
                log!("file events: {changed:?}");
            }
            for tree in changed.iter().filter(|l| l.ends_with('/')) {
                log!("file events: invalidating everything under {tree}");
            }
            let mut batch: Vec<String> =
                std::mem::take(&mut *failed.lock().unwrap_or_else(std::sync::PoisonError::into_inner)).into_iter().collect();
            batch.extend(changed);
            if let Err(e) = notify_guest(&vm, &conn, &batch).await {
                log!("file events: {e:#}; will resend {} path(s) as directories", batch.len());
                failed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).extend(as_trees(&batch));
            }
        }
    })?;
    for line in watch.set_roots(Default::default()) {
        log!("{line}");
    }
    let status = Arc::new(Status::new(paths.clone(), cfg.cpus, cfg.memory_mib, fs_cache, watch, opts.project.clone()));
    status.write();
    let ctl = paths.control_socket();
    let _ = std::fs::remove_file(&ctl);
    let control = crate::paths::with_umask(0o077, || UnixListener::bind(&ctl)).with_context(|| format!("bind {}", ctl.display()))?;
    tokio::spawn(control_server(vm.clone(), control));
    tokio::spawn(clock_sync(vm.clone()));
    if opts.project.is_none() {
        // A sandbox must not reach the Mac's loopback services.
        tokio::spawn(reverse_pool(vm.clone(), paths.clone()));
    }
    if std::env::var_os("REKCOD_RESEARCH").is_some() {
        tokio::spawn(meta_server(vm.clone()));
    }
    if std::env::var_os("REKCOD_BALLOON").is_some() {
        tokio::spawn(memory_manager(vm.clone(), cfg.memory_mib * 1024 * 1024, status.clone()));
    }
    if fs_cache {
        tokio::spawn(warm_recent(vm.clone(), paths.warm_paths()));
    }
    tokio::spawn(dns_bridge(vm.clone(), dns_filter));
    tokio::spawn(port_forwarder(vm.clone(), status.clone()));

    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    // `rekcod sandbox rm`: nothing on the disk is kept, so skip the graceful shutdown.
    let mut kill = signal(SignalKind::user_defined1())?;
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            _ = term.recv() => break,
            _ = int.recv() => break,
            _ = kill.recv() => {
                log!("force stop");
                let _ = vm.force_stop().await;
                cleanup(&paths);
                clean_exit(&paths);
                return Ok(());
            }
            _ = tick.tick() => {
                if let Some(ttl) = opts.idle_ttl
                    && let Ok(used) = std::fs::metadata(paths.home.join("run/last-used")).and_then(|m| m.modified())
                    && used.elapsed().is_ok_and(|e| e.as_secs() > ttl)
                {
                    log!("idle for {ttl}s, stopping");
                    break;
                }
                // vm.state() waits on the VM's dispatch queue, which a stuck VM can hold.
                let probe = vm.clone();
                let Ok(Ok(s)) = tokio::time::timeout(Duration::from_secs(2), tokio::task::spawn_blocking(move || probe.state())).await else {
                    continue;
                };
                if matches!(s, State::Stopped | State::Error) {
                    log!("guest stopped on its own ({s:?})");
                    cleanup(&paths);
                    // Not clean: the login agent (`start --install`) boots it again.
                    bail!("guest stopped on its own ({s:?})");
                }
            }
        }
    }
    shutdown(&vm).await;
    cleanup(&paths);
    clean_exit(&paths);
    Ok(())
}

/// Tells `rekcod start --foreground` (the login agent) that this stop was on purpose.
fn clean_exit(paths: &Paths) {
    let _ = std::fs::write(paths.last_exit(), b"clean");
}

/// launchd starts agents with a soft limit of 256 descriptors; every forwarded connection
/// and docker client takes two.
fn raise_fd_limit() {
    let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        return;
    }
    let per_proc = crate::cli::sysctl_u64("kern.maxfilesperproc").unwrap_or(10240) as libc::rlim_t;
    let want = lim.rlim_max.min(per_proc);
    if want > lim.rlim_cur {
        lim.rlim_cur = want;
        unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) };
    }
}

/// After a failed accept (EMFILE, ENFILE), wait before the next one instead of spinning.
async fn accept_failed(what: &str, e: std::io::Error) {
    log!("{what}: accept: {e}");
    tokio::time::sleep(Duration::from_millis(100)).await;
}

fn cleanup(paths: &Paths) {
    for f in [paths.socket(), paths.control_socket(), paths.pid_file(), paths.status_file()] {
        let _ = std::fs::remove_file(f);
    }
}

async fn shutdown(vm: &Vm) {
    log!("shutting down guest");
    let asked = control(vm, Control::Shutdown).await;
    if let Err(e) = &asked {
        log!("graceful shutdown request failed: {e:#}");
    }
    // The guest waits up to 18 s for dockerd (which gives containers 10 s), then unmounts.
    let deadline = Instant::now() + Duration::from_secs(30);
    while asked.is_ok() && Instant::now() < deadline {
        if matches!(vm.state(), State::Stopped | State::Error) {
            log!("guest powered off");
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    log!("forcing VM stop");
    if let Err(e) = vm.force_stop().await {
        log!("force stop failed: {e:#}");
    }
}

/// Waits for the agent; returns its capabilities (the rest of the ping reply).
async fn wait_for_agent(vm: &Vm) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        match control(vm, Control::Ping).await {
            Ok(reply) if reply.starts_with("pong") => return Ok(reply.trim_start_matches("pong").trim().to_string()),
            Ok(reply) => bail!("unexpected ping reply {reply:?}"),
            Err(_) if Instant::now() < deadline => tokio::time::sleep(Duration::from_millis(50)).await,
            Err(e) => return Err(e.context("guest agent did not come up (see ~/.rekcod/logs/console.log)")),
        }
        if matches!(vm.state(), State::Stopped | State::Error) {
            bail!("guest stopped during boot (see ~/.rekcod/logs/console.log)");
        }
    }
}

async fn control(vm: &Vm, cmd: Control) -> Result<String> {
    control_within(vm, cmd, Duration::from_secs(5)).await
}

async fn control_within(vm: &Vm, cmd: Control, timeout: Duration) -> Result<String> {
    let stream = vm.connect(CONTROL_PORT).await?;
    let mut stream = BufReader::new(stream);
    stream.get_mut().write_all(cmd.encode().as_bytes()).await?;
    let mut reply = String::new();
    tokio::time::timeout(timeout, stream.read_line(&mut reply)).await??;
    Ok(reply.trim_end().to_string())
}

/// `~/.rekcod/run/control.sock`: one command line in, the agent's reply out.
/// Only commands that are safe to expose to the CLI are relayed.
async fn control_server(vm: Vm, listener: UnixListener) {
    loop {
        let conn = match listener.accept().await {
            Ok((c, _)) => c,
            Err(e) => {
                accept_failed("control", e).await;
                continue;
            }
        };
        let vm = vm.clone();
        tokio::spawn(async move {
            let mut conn = BufReader::new(conn);
            let mut line = String::new();
            if conn.read_line(&mut line).await.is_err() {
                return;
            }
            let reply = match Control::parse(&line) {
                Some(cmd @ (Control::Trim | Control::BenchMeta(_) | Control::Stats)) => {
                    control_within(&vm, cmd, Duration::from_secs(600)).await.unwrap_or_else(|e| format!("error: {e:#}"))
                }
                _ => format!("error: unsupported command {:?}", line.trim_end()),
            };
            let _ = conn.get_mut().write_all(format!("{reply}\n").as_bytes()).await;
        });
    }
}

/// Sends one command through the daemon's control socket (used by the CLI).
pub fn send_control(paths: &Paths, cmd: &Control) -> Result<String> {
    use std::io::{BufRead, Write};
    let mut conn =
        std::os::unix::net::UnixStream::connect(paths.control_socket()).context("connect to the rekcod daemon (is rekcod running?)")?;
    // `rekcod status` and the menu bar must not hang on a wedged agent; trim really is slow.
    let wait = if matches!(cmd, Control::Trim | Control::BenchMeta(_)) { 600 } else { 5 };
    conn.set_read_timeout(Some(Duration::from_secs(wait)))?;
    conn.set_write_timeout(Some(Duration::from_secs(5)))?;
    conn.write_all(cmd.encode().as_bytes())?;
    let mut reply = String::new();
    std::io::BufReader::new(conn).read_line(&mut reply)?;
    Ok(reply.trim_end().to_string())
}

/// Balloon policy (see memory.rs); reclaims page cache only under macOS memory pressure.
async fn memory_manager(vm: Vm, max: u64, status: Arc<Status>) {
    let headroom = memory::headroom(max);
    let min = (1u64 << 30).min(max);
    let mut target = max;
    let mut idle_ticks = 0;
    let mut tick = 0u32;
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        tick = tick.wrapping_add(1);
        // kern.memorystatus_vm_pressure_level: 1 normal, 2 warning, 4 critical.
        let pressure = tick.is_multiple_of(10) && crate::cli::sysctl_u64("kern.memorystatus_vm_pressure_level").unwrap_or(1) & 0xff >= 2;
        if pressure && let Some((_, cached)) = control(&vm, Control::Meminfo).await.ok().as_deref().and_then(memory::parse_meminfo) {
            let _ = control(&vm, Control::Reclaim(cached / 2)).await;
            log!("macOS memory pressure: reclaimed guest page cache");
        }
        let Some((free, _)) = control(&vm, Control::Meminfo).await.ok().as_deref().and_then(memory::parse_meminfo) else {
            continue;
        };
        idle_ticks = if free > headroom { idle_ticks + 1 } else { 0 };
        if let Some(next) = memory::next_target(target, max, min, free, headroom, idle_ticks) {
            idle_ticks = 0;
            vm.set_balloon_target(next);
            if next.abs_diff(target) >= 256 << 20 {
                log!("balloon: guest may use {} MiB (was {} MiB, {} MiB free)", next >> 20, target >> 20, free >> 20);
            }
            target = next;
            *status.balloon.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = target >> 20;
            status.write();
        }
    }
}

/// One control connection reused for every notify batch (a vsock connect per
/// batch was a measurable part of the Mac→container latency).
type NotifyConn = tokio::sync::Mutex<Option<BufReader<crate::vm::Conn>>>;

/// Directories warmed right after boot: the most recently bind-mounted ones,
/// so the first `compose up` after `rekcod start` finds a warm cache.
const WARM_AT_BOOT: usize = 8;

async fn warm_recent(vm: Vm, file: PathBuf) {
    let list = std::fs::read_to_string(file).unwrap_or_default();
    let dirs: Vec<&str> = list.lines().filter(|l| std::path::Path::new(l).is_dir()).take(WARM_AT_BOOT).collect();
    for dir in &dirs {
        let _ = control(&vm, Control::Warm(dir.to_string())).await;
    }
    if !dirs.is_empty() {
        log!("warming {} recently used bind source(s)", dirs.len());
    }
}

/// Paths to re-send after a failed batch: each file's directory as a tree (`dir/`), so the
/// guest drops everything cached under it. Capped by collapsing to fewer, higher dirs.
fn as_trees(paths: &[String]) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = paths
        .iter()
        .map(|p| {
            if p.ends_with('/') {
                p.clone()
            } else {
                format!("{}/", std::path::Path::new(p).parent().map_or("/".into(), |d| d.display().to_string()).trim_end_matches('/'))
            }
        })
        .collect();
    while out.len() > 256 {
        out = out
            .iter()
            .map(|t| {
                format!(
                    "{}/",
                    std::path::Path::new(t.trim_end_matches('/'))
                        .parent()
                        .map_or("".into(), |d| d.display().to_string())
                        .trim_end_matches('/')
                )
            })
            .collect();
    }
    out
}

/// Re-sends batches that failed (agent restarting, VM busy) every 2 s until they go through.
async fn retry_notify(vm: Vm, conn: Arc<NotifyConn>, failed: Arc<std::sync::Mutex<BTreeSet<String>>>) {
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let batch: Vec<String> =
            std::mem::take(&mut *failed.lock().unwrap_or_else(std::sync::PoisonError::into_inner)).into_iter().collect();
        if batch.is_empty() {
            continue;
        }
        match notify_guest(&vm, &conn, &batch).await {
            Ok(()) => log!("file events: resent {} directory invalidation(s)", batch.len()),
            Err(_) => failed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).extend(batch),
        }
    }
}

/// Forwards Mac-side file changes so watchers inside containers fire.
async fn notify_guest(vm: &Vm, conn: &NotifyConn, changed: &[String]) -> Result<()> {
    let lines: String = changed.iter().map(|p| Control::Notify(p.clone()).encode()).collect();
    let mut slot = conn.lock().await;
    for attempt in 0..2 {
        if slot.is_none() {
            *slot = Some(BufReader::new(vm.connect(CONTROL_PORT).await?));
        }
        let stream = slot.as_mut().expect("connected above");
        let sent = async {
            stream.get_mut().write_all(lines.as_bytes()).await?;
            let mut reply = String::new();
            for _ in changed {
                reply.clear();
                if stream.read_line(&mut reply).await? == 0 {
                    bail!("control connection closed");
                }
            }
            anyhow::Ok(())
        };
        match tokio::time::timeout(Duration::from_secs(10), sent).await {
            Ok(Ok(())) => return Ok(()),
            // Stale connection (agent restarted): reconnect once and resend.
            Ok(Err(_)) if attempt == 0 => *slot = None,
            Ok(Err(e)) => {
                *slot = None;
                return Err(e);
            }
            Err(_) => {
                *slot = None;
                bail!("guest did not acknowledge file events within 10s");
            }
        }
    }
    Ok(())
}

async fn clock_sync(vm: Vm) {
    loop {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        match control(&vm, Control::Time(now)).await {
            Ok(reply) if reply != "ok" => log!("clock: {reply}"),
            Err(e) => log!("clock sync failed: {e:#}"),
            _ => {}
        }
        // Every 30 s, or as soon as the Mac wakes: Instant stops while it sleeps and wall
        // time doesn't, so a gap between them means the guest's clock is now behind (TLS,
        // JWTs and S3 signatures fail until it catches up).
        let (mono, wall) = (Instant::now(), SystemTime::now());
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let slept = wall.elapsed().unwrap_or_default().saturating_sub(mono.elapsed());
            if slept > Duration::from_secs(5) || mono.elapsed() >= Duration::from_secs(30) {
                break;
            }
        }
    }
}

/// A sandbox with an allowlist: names off it are answered REFUSED, and the A records
/// answered for the rest are the addresses its network stack lets it connect to.
struct DnsFilter {
    allow: Vec<String>,
    resolved: Arc<netstack::Resolved>,
}

impl DnsFilter {
    async fn answer(&self, query: &[u8]) -> Option<Vec<u8>> {
        match dns::query_name(query) {
            Some(name) if rekcod_proto::host_allowed(&self.allow, &name) => {
                let answer = resolve(query).await?;
                for (ip, ttl) in dns::answer_a(&answer) {
                    self.resolved.record(ip, ttl);
                }
                Some(answer)
            }
            name => {
                log!("rekcod-egress: refused dns {}", name.as_deref().unwrap_or("(not one plain question)"));
                dns::refused(query)
            }
        }
    }
}

/// Answers the guest's DNS queries with the Mac's current resolvers.
async fn dns_bridge(vm: Vm, filter: Option<Arc<DnsFilter>>) {
    loop {
        if let Err(e) = dns_session(&vm, &filter).await {
            log!("dns bridge: {e:#}");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn dns_session(vm: &Vm, filter: &Option<Arc<DnsFilter>>) -> Result<()> {
    let (mut rd, mut wr) = tokio::io::split(vm.connect(DNS_PORT).await?);
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    // Queries resolving at once; more wait, so a flood can't spawn tasks without limit.
    let in_flight = Arc::new(tokio::sync::Semaphore::new(256));
    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if wr.write_all(&frame).await.is_err() {
                break;
            }
        }
    });
    let result = async {
        loop {
            let mut len = [0u8; 2];
            rd.read_exact(&mut len).await?;
            let mut query = vec![0u8; u16::from_be_bytes(len) as usize];
            rd.read_exact(&mut query).await?;
            let tx = tx.clone();
            let filter = filter.clone();
            let permit = in_flight.clone().acquire_owned().await?;
            tokio::spawn(async move {
                let _permit = permit;
                let answer = match &filter {
                    Some(f) => f.answer(&query).await,
                    None => resolve(&query).await,
                };
                if let Some(frame) = answer.as_deref().and_then(frame) {
                    let _ = tx.send(frame);
                }
            });
        }
        #[allow(unreachable_code)]
        anyhow::Ok(())
    }
    .await;
    writer.abort();
    result
}

/// The Mac's resolvers from `scutil --dns`, re-read at most every 5 s.
fn resolvers() -> Arc<Vec<dns::Resolver>> {
    static CACHE: std::sync::Mutex<Option<(Instant, Arc<Vec<dns::Resolver>>)>> = std::sync::Mutex::new(None);
    let mut c = CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((at, r)) = c.as_ref()
        && at.elapsed() < Duration::from_secs(5)
    {
        return r.clone();
    }
    let text =
        std::process::Command::new("/usr/sbin/scutil").arg("--dns").output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    let r = Arc::new(text.map(|t| dns::parse_scutil(&t)).unwrap_or_default());
    *c = Some((Instant::now(), r.clone()));
    r
}

async fn resolve(query: &[u8]) -> Option<Vec<u8>> {
    let resolvers = tokio::task::spawn_blocking(resolvers).await.unwrap_or_default();
    let mut servers = dns::query_name(query).map(|n| dns::pick(&resolvers, &n)).unwrap_or_default();
    if servers.is_empty() {
        let resolv = std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default();
        servers = dns::nameservers(&resolv, dns::if_index);
    }
    for server in servers {
        let local: SocketAddr = if server.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }.parse().unwrap();
        let Ok(sock) = UdpSocket::bind(local).await else { continue };
        if sock.send_to(query, server).await.is_err() {
            continue;
        }
        let mut buf = vec![0u8; 4096];
        if let Ok(Ok((n, _))) = tokio::time::timeout(Duration::from_secs(2), sock.recv_from(&mut buf)).await {
            buf.truncate(n);
            // Truncated (TC): the full answer only comes over TCP.
            if buf.len() > 2
                && buf[2] & 0x02 != 0
                && let Ok(Ok(full)) = tokio::time::timeout(Duration::from_secs(4), resolve_tcp(query, server)).await
            {
                return Some(full);
            }
            return Some(buf);
        }
    }
    None
}

async fn resolve_tcp(query: &[u8], server: SocketAddr) -> Result<Vec<u8>> {
    let mut tcp = TcpStream::connect(server).await?;
    tcp.write_all(&frame(query).context("query too large")?).await?;
    let mut len = [0u8; 2];
    tcp.read_exact(&mut len).await?;
    let mut answer = vec![0u8; u16::from_be_bytes(len) as usize];
    tcp.read_exact(&mut answer).await?;
    Ok(answer)
}

async fn docker_proxy(vm: Vm, listener: UnixListener) {
    loop {
        let client = match listener.accept().await {
            Ok((c, _)) => c,
            Err(e) => {
                accept_failed("docker socket", e).await;
                continue;
            }
        };
        let vm = vm.clone();
        tokio::spawn(async move {
            match vm.connect(DOCKER_PORT).await {
                Ok(guest) => {
                    let _ = crate::proxy::pipe(client, guest).await;
                }
                Err(e) => log!("docker proxy: {e:#}"),
            }
        });
    }
}

// ---------------------------------------------------------------- ports

#[derive(Serialize)]
struct PortStatus {
    proto: &'static str,
    address: String,
    port: u16,
    container: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct StatusFile<'a> {
    pid: u32,
    /// The daemon's own version; after an upgrade it can differ from the CLI's.
    version: &'static str,
    guest_version: &'a str,
    started_at: u64,
    cpus: usize,
    memory_mib: u64,
    fs_cache: bool,
    balloon_mib: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<&'a std::path::Path>,
    ports: &'a [PortStatus],
}

struct Forward {
    container: String,
    task: Option<JoinHandle<()>>,
    error: Option<String>,
}

struct Status {
    paths: Paths,
    watch: Arc<Watch>,
    started_at: u64,
    cpus: usize,
    memory_mib: u64,
    fs_cache: bool,
    /// Set for a sandbox VM: the only shared directory.
    project: Option<PathBuf>,
    balloon: Mutex<u64>,
    forwards: Mutex<BTreeMap<Binding, Forward>>,
    /// Bind sources seen at the last reconcile; new ones get their caches warmed.
    binds: Mutex<BTreeSet<PathBuf>>,
    /// Ports docker publishes (from the last reconcile).
    published: Mutex<BTreeMap<Binding, String>>,
    /// Running `dev.rekcod.autoports` containers: (id, name).
    autoport: Mutex<Vec<(String, String)>>,
    /// Ports those containers listen on (from the agent).
    listening: Mutex<BTreeMap<Binding, String>>,
}

impl Status {
    fn new(paths: Paths, cpus: usize, memory_mib: u64, fs_cache: bool, watch: Arc<Watch>, project: Option<PathBuf>) -> Status {
        let started_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        Status {
            paths,
            watch,
            started_at,
            cpus,
            memory_mib,
            fs_cache,
            project,
            balloon: Mutex::new(memory_mib),
            forwards: Mutex::default(),
            binds: Mutex::default(),
            published: Mutex::default(),
            autoport: Mutex::default(),
            listening: Mutex::default(),
        }
    }

    fn write(&self) {
        let ports: Vec<PortStatus> = self
            .forwards
            .lock()
            .unwrap()
            .iter()
            .map(|(b, f)| PortStatus {
                proto: match b.proto {
                    Proto::Tcp => "tcp",
                    Proto::Udp => "udp",
                },
                address: b.addr.to_string(),
                port: b.port,
                container: f.container.clone(),
                error: f.error.clone(),
            })
            .collect();
        let guest_version = std::fs::read_to_string(self.paths.guest_version()).unwrap_or_default();
        let file = StatusFile {
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION"),
            guest_version: guest_version.trim(),
            started_at: self.started_at,
            cpus: self.cpus,
            memory_mib: self.memory_mib,
            fs_cache: self.fs_cache,
            balloon_mib: *self.balloon.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
            project: self.project.as_deref(),
            ports: &ports,
        };
        let tmp = self.paths.status_file().with_extension("tmp");
        if std::fs::write(&tmp, serde_json::to_vec_pretty(&file).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(tmp, self.paths.status_file());
        }
    }
}

const EVENTS_PATH: &str = "/events?filters=%7B%22type%22%3A%5B%22container%22%5D%7D";

async fn port_forwarder(vm: Vm, status: Arc<Status>) {
    tokio::spawn(autoports(vm.clone(), status.clone()));
    loop {
        if let Err(e) = watch_events(&vm, &status).await {
            log!("port forwarder: {e:#}");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn watch_events(vm: &Vm, status: &Arc<Status>) -> Result<()> {
    reconcile(vm, status).await?;
    let mut events = http::Lines::open(vm.connect(DOCKER_PORT).await?, EVENTS_PATH).await?;
    while let Some(line) = events.next().await? {
        let Ok(ev) = serde_json::from_slice::<serde_json::Value>(&line) else { continue };
        let action = ev["Action"].as_str().unwrap_or_default();
        if action.starts_with("exec_") || action.starts_with("health_status") {
            continue;
        }
        if let Err(e) = reconcile(vm, status).await {
            log!("port reconcile: {e:#}");
        }
    }
    bail!("event stream ended")
}

async fn reconcile(vm: &Vm, status: &Arc<Status>) -> Result<()> {
    // all=1: created containers too, so a bind source is warmed before `start`.
    let body = http::get(vm.connect(DOCKER_PORT).await?, "/containers/json?all=1").await?;
    *status.published.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = ports::desired(&body)?;
    *status.autoport.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = ports::autoport_containers(&body)?;
    let binds = watch::bind_sources(&body)?;
    if status.fs_cache {
        let new: Vec<PathBuf> = binds
            .difference(&status.binds.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
            .filter(|p| p.is_dir())
            .cloned()
            .collect();
        for path in &new {
            if let Some(p) = path.to_str() {
                let _ = control(vm, Control::Warm(p.to_string())).await;
            }
        }
        if !new.is_empty() {
            let file = status.paths.warm_paths();
            let list = watch::remember(&std::fs::read_to_string(&file).unwrap_or_default(), &new);
            let text: String = list.iter().filter_map(|p| p.to_str()).map(|p| format!("{p}\n")).collect();
            let _ = std::fs::write(file, text);
        }
    }
    *status.binds.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = binds.clone();
    for err in status.watch.set_roots(binds) {
        log!("{err}");
    }
    apply_forwards(vm, status, true);
    Ok(())
}

/// Every second while `dev.rekcod.autoports` containers run (`rekcod x`): ask
/// the agent which ports they listen on, and forward those on the Mac's
/// localhost (v4 and v6). Ports docker already publishes win.
async fn autoports(vm: Vm, status: Arc<Status>) {
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let containers = status.autoport.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let mut want = BTreeMap::new();
        for (id, name) in &containers {
            let Ok(reply) = control(&vm, Control::Listeners(id.clone())).await else { continue };
            let Some(list) = reply.strip_prefix("ok") else { continue };
            for port in list.split_whitespace().filter_map(|p| p.parse::<u16>().ok()) {
                for addr in [std::net::Ipv4Addr::LOCALHOST.into(), std::net::Ipv6Addr::LOCALHOST.into()] {
                    want.insert(Binding { proto: Proto::Tcp, addr, port }, name.clone());
                }
            }
        }
        let published: BTreeSet<u16> =
            status.published.lock().unwrap_or_else(std::sync::PoisonError::into_inner).keys().map(|b| b.port).collect();
        want.retain(|b, _| !published.contains(&b.port));
        let changed = {
            let mut listening = status.listening.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let changed = *listening != want;
            *listening = want;
            changed
        };
        if changed {
            apply_forwards(&vm, &status, false);
        }
    }
}

/// Opens and closes Mac listeners to match published + auto-detected ports.
/// `retry_failed`: try again on ports that couldn't be bound before.
fn apply_forwards(vm: &Vm, status: &Status, retry_failed: bool) {
    let mut want = status.listening.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    want.extend(status.published.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone());
    let mut changed = false;
    {
        let mut active = status.forwards.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // Ports our listeners hold now, including ones closed below (abort is asynchronous).
        let ours: BTreeSet<u16> = active.iter().filter(|(_, f)| f.task.is_some()).map(|(b, _)| b.port).collect();
        active.retain(|binding, fwd| {
            let keep = want.get(binding) == Some(&fwd.container) && (fwd.task.is_some() || !retry_failed);
            if !keep {
                if let Some(task) = fwd.task.take() {
                    task.abort();
                    log!("port {}:{}/{:?} closed", binding.addr, binding.port, binding.proto);
                }
                changed = true;
            }
            keep
        });
        for (binding, container) in want {
            if active.contains_key(&binding) {
                continue;
            }
            changed = true;
            let fwd = match listen(binding, !ours.contains(&binding.port)) {
                Ok(listener) => {
                    log!("port {}:{}/{:?} -> {container}", binding.addr, binding.port, binding.proto);
                    let task = match listener {
                        Listener::Tcp(l) => tokio::spawn(accept_loop(vm.clone(), l, binding.port)),
                        Listener::Udp(s) => tokio::spawn(udp_loop(vm.clone(), s, binding.port)),
                    };
                    Forward { container, task: Some(task), error: None }
                }
                Err(e) => {
                    // Never fight whatever already owns the port on the Mac.
                    log!("port {}:{} for {container} unavailable on the Mac: {e}", binding.addr, binding.port);
                    Forward { container, task: None, error: Some(e.to_string()) }
                }
            };
            active.insert(binding, fwd);
        }
    }
    if changed {
        status.write();
    }
}

enum Listener {
    Tcp(TcpListener),
    Udp(UdpSocket),
}

/// `probe`: refuse if something already answers on the port. Off when one of our own
/// listeners has (or just had) the port, since it would answer the probe itself.
fn listen(b: Binding, probe: bool) -> std::io::Result<Listener> {
    use socket2::{Domain, Socket, Type};
    let addr = SocketAddr::new(b.addr, b.port);
    let kind = if b.proto == Proto::Udp { Type::DGRAM } else { Type::STREAM };
    let socket = Socket::new(Domain::for_address(addr), kind, None)?;
    if addr.is_ipv6() {
        // `[::]` is dual-stack (covers 127.0.0.1 too); `[::1]` stays v6-only.
        socket.set_only_v6(!b.addr.is_unspecified())?;
    }
    socket.set_nonblocking(true)?;
    if b.proto == Proto::Udp {
        socket.bind(&addr.into())?;
        return Ok(Listener::Udp(UdpSocket::from_std(socket.into())?));
    }
    // SO_REUSEADDR (needed to rebind right after a container restarts, past TIME_WAIT) also
    // lets macOS bind 127.0.0.1:P while another app holds *:P, and loopback traffic would
    // then come to us. So first check whether anything answers there.
    let probe_first = probe;
    let probe = if addr.ip().is_unspecified() {
        SocketAddr::new(if addr.is_ipv6() { std::net::Ipv6Addr::LOCALHOST.into() } else { std::net::Ipv4Addr::LOCALHOST.into() }, b.port)
    } else {
        addr
    };
    if probe_first && std::net::TcpStream::connect_timeout(&probe, Duration::from_millis(100)).is_ok() {
        return Err(std::io::Error::from(std::io::ErrorKind::AddrInUse));
    }
    socket.set_reuse_address(true)?;
    socket.bind(&addr.into())?;
    socket.listen(1024)?;
    Ok(Listener::Tcp(TcpListener::from_std(socket.into())?))
}

/// A UDP flow (one Mac client address) closes after this long without traffic.
const UDP_IDLE: Duration = Duration::from_secs(60);

type UdpFlows = Arc<Mutex<HashMap<SocketAddr, mpsc::Sender<Vec<u8>>>>>;

/// Published UDP port: one vsock stream per Mac client address, carrying
/// framed datagrams both ways (the agent relays them to docker-proxy).
async fn udp_loop(vm: Vm, sock: UdpSocket, port: u16) {
    let sock = Arc::new(sock);
    let flows: UdpFlows = Arc::default();
    let mut buf = vec![0u8; 65536];
    loop {
        let Ok((n, from)) = sock.recv_from(&mut buf).await else { continue };
        let packet = buf[..n].to_vec();
        let existing = flows.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&from).cloned();
        let packet = match existing {
            // Full: the guest isn't keeping up, so drop the datagram, as a network would.
            Some(tx) => match tx.try_send(packet) {
                Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => continue,
                Err(mpsc::error::TrySendError::Closed(p)) => p,
            },
            None => packet,
        };
        if flows.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len() >= UDP_FLOWS_MAX {
            continue;
        }
        let (tx, rx) = mpsc::channel(UDP_QUEUE);
        let _ = tx.try_send(packet);
        flows.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(from, tx);
        tokio::spawn(udp_flow(vm.clone(), sock.clone(), from, port, rx, flows.clone()));
    }
}

/// Datagrams queued per UDP flow, and flows per forwarded port.
const UDP_QUEUE: usize = 256;
const UDP_FLOWS_MAX: usize = 1024;

async fn udp_flow(vm: Vm, sock: Arc<UdpSocket>, from: SocketAddr, port: u16, mut rx: mpsc::Receiver<Vec<u8>>, flows: UdpFlows) {
    let last = Mutex::new(Instant::now());
    let result = async {
        let (mut rd, mut wr) = tokio::io::split(vm.connect(FORWARD_PORT).await?);
        wr.write_all(forward_header(Proto::Udp, port).as_bytes()).await?;
        let up = async {
            while let Some(p) = rx.recv().await {
                *last.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
                if let Some(f) = frame(&p) {
                    wr.write_all(&f).await?;
                }
            }
            anyhow::Ok(())
        };
        let down = async {
            loop {
                let mut len = [0u8; 2];
                rd.read_exact(&mut len).await?;
                let mut p = vec![0u8; u16::from_be_bytes(len) as usize];
                rd.read_exact(&mut p).await?;
                *last.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
                sock.send_to(&p, from).await?;
            }
            #[allow(unreachable_code)]
            anyhow::Ok(())
        };
        let idle = async {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                if last.lock().unwrap_or_else(std::sync::PoisonError::into_inner).elapsed() > UDP_IDLE {
                    return anyhow::Ok(());
                }
            }
        };
        tokio::select! {
            r = up => r,
            r = down => r,
            r = idle => r,
        }
    }
    .await;
    flows.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&from);
    if let Err(e) = result {
        log!("forward udp :{port} for {from}: {e:#}");
    }
}

async fn accept_loop(vm: Vm, listener: TcpListener, port: u16) {
    loop {
        let mut tcp = match listener.accept().await {
            Ok((c, _)) => c,
            Err(e) => {
                accept_failed(&format!("port {port}"), e).await;
                continue;
            }
        };
        let vm = vm.clone();
        tokio::spawn(async move {
            let _ = tcp.set_nodelay(true);
            let tunnel = async {
                let mut guest = vm.connect(FORWARD_PORT).await?;
                guest.write_all(forward_header(Proto::Tcp, port).as_bytes()).await?;
                copy_bidirectional(&mut tcp, &mut guest).await?;
                anyhow::Ok(())
            };
            if let Err(e) = tunnel.await {
                log!("forward :{port}: {e:#}");
            }
        });
    }
}

// ------------------------------------------------- host.docker.internal

/// Idle connections kept parked in the guest for `host.docker.internal`.
const REVERSE_POOL: usize = 8;

/// Keeps [`REVERSE_POOL`] connections parked on the agent's REVERSE_PORT. When
/// the agent uses one (`connect <port>`), it is spliced to the Mac's
/// `127.0.0.1:<port>` and a replacement is parked.
async fn reverse_pool(vm: Vm, paths: Paths) {
    let (used_tx, mut used_rx) = mpsc::unbounded_channel::<()>();
    for _ in 0..REVERSE_POOL {
        tokio::spawn(park(vm.clone(), paths.clone(), used_tx.clone()));
    }
    while used_rx.recv().await.is_some() {
        tokio::spawn(park(vm.clone(), paths.clone(), used_tx.clone()));
    }
}

/// Mac ports containers may not reach through `host.docker.internal`: the dashboard's,
/// which runs `rekcod` commands as you.
fn reverse_blocked(paths: &Paths, port: u16) -> bool {
    let f = paths.home.join("run/dashboard.json");
    std::fs::read(f).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()).and_then(|v| v["port"].as_u64())
        == Some(port as u64)
}

async fn park(vm: Vm, paths: Paths, used: mpsc::UnboundedSender<()>) {
    let stream = loop {
        match vm.connect(REVERSE_PORT).await {
            Ok(s) => break s,
            Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    };
    let parked_at = Instant::now();
    let mut guest = BufReader::new(stream);
    let mut line = String::new();
    let n = guest.read_line(&mut line).await.unwrap_or(0);
    if n == 0 && parked_at.elapsed() < Duration::from_secs(1) {
        // Agent closed it right away (restarting); don't spin.
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let _ = used.send(());
    let Some(port) = parse_reverse_header(&line) else { return };
    if reverse_blocked(&paths, port) {
        log!("host.docker.internal:{port}: refused, it's the dashboard");
        return;
    }
    let mac = match TcpStream::connect(("127.0.0.1", port)).await {
        Ok(s) => Ok(s),
        Err(_) => TcpStream::connect(("::1", port)).await,
    };
    match mac {
        Ok(mut mac) => {
            let _ = mac.set_nodelay(true);
            let _ = copy_bidirectional(&mut guest, &mut mac).await;
        }
        Err(e) => log!("host.docker.internal:{port}: nothing listening on the Mac ({e})"),
    }
}

// ------------------------------------------------- research (phase 6)

/// Answers the agent's metadata-latency prototype (see rekcod-agent meta.rs).
async fn meta_server(vm: Vm) {
    loop {
        let session = async {
            let mut conn = BufReader::new(vm.connect(META_PORT).await?);
            let mut line = String::new();
            loop {
                line.clear();
                if conn.read_line(&mut line).await? == 0 {
                    return anyhow::Ok(());
                }
                let reply = meta_reply(line.trim_end_matches('\n'));
                conn.get_mut().write_all(reply.as_bytes()).await?;
            }
        };
        if let Err(e) = session.await {
            log!("meta server: {e:#}");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn meta_reply(req: &str) -> String {
    use std::os::unix::fs::MetadataExt;
    if let Some(p) = req.strip_prefix("lstat ") {
        return match std::fs::symlink_metadata(p) {
            Ok(m) => format!("{} {} {} {}\n", m.size(), m.mtime(), m.mode(), m.ino()),
            Err(_) => "-\n".into(),
        };
    }
    if let Some(p) = req.strip_prefix("readdir ") {
        let mut out = String::new();
        for e in std::fs::read_dir(p).into_iter().flatten().flatten() {
            let Some(name) = e.file_name().to_str().map(str::to_string) else { continue };
            if name.contains(['\t', '\n']) {
                continue;
            }
            out.push(if e.file_type().is_ok_and(|t| t.is_dir()) { 'd' } else { 'f' });
            out.push_str(&name);
            out.push('\t');
        }
        out.push('\n');
        return out;
    }
    "-\n".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_file_events_come_back_as_directories() {
        let t = as_trees(&["/Users/me/app/src/a.rs".into(), "/Users/me/app/src/b.rs".into(), "/Users/me/lib/".into()]);
        assert_eq!(t.into_iter().collect::<Vec<_>>(), ["/Users/me/app/src/", "/Users/me/lib/"]);
        let many: Vec<String> = (0..1000).map(|i| format!("/Users/me/p{i}/f")).collect();
        let t = as_trees(&many);
        assert!(t.len() <= 256 && t.contains("/Users/me/"), "{t:?}");
    }
}
