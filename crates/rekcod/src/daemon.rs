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
    /// Memory the guest may use: base RAM plus elastic swap (README.md, "Configure the VM").
    pub memory_mib: u64,
    /// Base RAM; default from `rekcod_proto::split_memory`.
    pub base_memory_mib: Option<u64>,
    /// Sandbox VM (README.md, "Run it in a separate VM"): share only this directory, and nothing
    /// on the Mac is reachable from the guest.
    pub project: Option<PathBuf>,
    /// Agent sandbox (README.md, "Review an agent's changes before applying them"): the Mac directory shared as `project`,
    /// when it's a copy of the project rather than the project itself.
    pub workspace: Option<PathBuf>,
    /// Egress allowlist (comma-separated names); none means the internet is open.
    pub allow: Option<String>,
    /// `IP:PORT,...` the sandbox may reach although it's on the Mac or LAN.
    pub reach: Option<String>,
    /// Power off after this many seconds without an exec (`run/last-used`) and with no
    /// connection open on the docker socket.
    pub idle_ttl: Option<u64>,
    /// `e2fsck -fy` the data disk at boot (`rekcod start --repair`).
    pub repair: bool,
    /// Main VM: the daemon's userspace network instead of NAT (`rekcod start --network user`).
    pub network_user: bool,
    /// Sandbox VM: the only Mac ports it may listen on, comma-separated; None is any
    /// unprivileged port.
    pub publish: Option<String>,
    /// Sandbox VM: don't forward the ports its autoports containers listen on.
    pub no_autoports: bool,
}

/// `--publish`: ports, comma-separated; empty is none.
fn publish_ports(s: &str) -> Result<BTreeSet<u16>> {
    s.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| p.parse::<u16>().ok().filter(|&p| p > 0).with_context(|| format!("--publish {p}: not a port")))
        .collect()
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
    // A stop request goes through `shutdown` (or force stop), also while booting:
    // left at its default, SIGTERM would end the daemon with the VM and its disks mid-write.
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    // `rekcod sandbox rm`, `rekcod stop --force`: skip the graceful shutdown.
    let mut kill = signal(SignalKind::user_defined1())?;
    let _ = std::fs::remove_file(paths.last_exit());
    crate::lifecycle::raise_fd_limit();
    // Every minute: what a sandbox's guest does reaches this log, limited but not silenced.
    crate::lifecycle::trim_log_every(paths.daemon_log(), Duration::from_secs(60));
    // console.log rotates itself as it's written (vm.rs). The dashboard started in the
    // background has no rekcod process of its own to trim its log.
    crate::lifecycle::trim_log_hourly(paths.home.join("logs/dashboard.log"));
    let guest = paths.guest_dir()?;
    let mut cmdline = std::env::var("REKCOD_KERNEL_CMDLINE").unwrap_or_else(|_| KERNEL_CMDLINE.into());
    let fresh = paths.data_fresh().exists();
    if fresh {
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
    // checked here (README.md, "Network and secrets"). The guest's own firewall stays as a second layer.
    let allow: Option<Vec<String>> = opts
        .allow
        .as_deref()
        .map(|a| a.split(',').map(str::trim).filter(|h| !h.is_empty()).map(String::from).collect())
        .filter(|a: &Vec<String>| !a.iter().any(|h| h == "*"));
    let reach = reach_addrs(opts.reach.as_deref().unwrap_or(""))?;
    let scope = PortScope { sandbox: opts.project.is_some(), allowed: opts.publish.as_deref().map(publish_ports).transpose()? };
    let resolved = Arc::new(netstack::Resolved::default());
    let mut host_net = None;
    let mut main_user_net = false;
    let net = match &opts.project {
        None if opts.network_user => {
            let (vm_end, ours) = netstack::socketpair().context("create the VM network socket")?;
            cmdline.push(' ');
            cmdline.push_str(netstack::CMDLINE);
            host_net = Some(ours);
            main_user_net = true;
            Net::Socket(vm_end)
        }
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
    let network_name = match (&opts.project, main_user_net, host_net.is_some()) {
        (None, true, _) => "user",
        (None, false, _) => "nat",
        (Some(_), _, true) => "filtered",
        (Some(_), _, false) => "none",
    };
    let (base_mib, elastic_mib) = if crate::elastic::available() && crate::elastic::guest_supports(&guest.join("Image")) {
        rekcod_proto::split_memory(opts.memory_mib, opts.base_memory_mib)
    } else {
        (opts.memory_mib, 0)
    };
    let mut cfg = VmConfig {
        cpus: opts.cpus,
        memory_mib: base_mib,
        elastic_mib,
        kernel: guest.join("Image"),
        initrd: Some(guest.join("initramfs")).filter(|p| p.exists()),
        cmdline,
        root_disk: paths.root_disk(),
        data_disk: paths.data_disk(),
        mac_address: std::fs::read_to_string(paths.mac_address())?.trim().to_string(),
        console_log: paths.console_log(),
        console_rate: opts.project.is_some().then_some(SANDBOX_CONSOLE_RATE),
        conn_budget: opts.project.is_some().then_some(SANDBOX_CONN_BUDGET),
        shares,
        net,
    };
    let vm = Vm::create(&cfg)?;
    // The VM holds its own copy of its end.
    cfg.net = Net::None;
    let dns = Arc::new(DnsPolicy {
        sandbox: opts.project.is_some(),
        filter: match (&opts.project, &allow) {
            (Some(_), Some(a)) => Some(DnsFilter { allow: a.clone(), resolved: resolved.clone() }),
            _ => None,
        },
        limit: Mutex::new(rekcod_proto::LogLimit::new(Duration::from_secs(10), 20, "rekcod-egress: ")),
        log: Arc::new(|line| log!("{line}")),
    });
    let _net = match host_net {
        Some(fd) => {
            let policy =
                Arc::new(if main_user_net { netstack::Policy::everything() } else { netstack::Policy::new(allow, reach, resolved) });
            Some(netstack::start(fd, policy, Arc::new(|line| log!("{line}"))).context("start the sandbox network")?)
        }
        None => None,
    };
    log!("kernel command line: {}", cfg.cmdline);
    let boot = async {
        vm.start().await?;
        vm.map_elastic();
        log!("VM started ({} CPUs, {} MiB + {} MiB elastic, rosetta={})", cfg.cpus, cfg.memory_mib, cfg.elastic_mib, vm.rosetta);
        // Docker socket first so `rekcod start` can poll it while the guest boots.
        let sock = paths.socket();
        let _ = std::fs::remove_file(&sock);
        let listener = crate::paths::with_umask(0o077, || UnixListener::bind(&sock)).with_context(|| format!("bind {}", sock.display()))?;
        // Only a VM with an idle TTL (a sandbox) counts its connections.
        tokio::spawn(docker_proxy(vm.clone(), listener, opts.idle_ttl.map(|_| paths.home.join("run/last-used"))));
        if fresh {
            tokio::spawn(forget_fresh_disk(vm.clone(), paths.data_fresh()));
        }
        wait_for_agent(&vm, opts.repair, &paths.console_log()).await
    };
    let stop = tokio::select! {
        caps = boot => match caps {
            Ok(caps) => Ok(caps),
            Err(e) => {
                cleanup(&paths);
                return Err(e);
            }
        },
        _ = term.recv() => Err(false),
        _ = int.recv() => Err(false),
        _ = kill.recv() => Err(true),
    };
    let caps = match stop {
        Ok(caps) => caps,
        Err(force) => {
            log!("asked to stop while booting");
            if force {
                force_stop(&vm).await;
            } else {
                shutdown(&vm, &mut kill).await;
            }
            cleanup(&paths);
            clean_exit(&paths);
            return Ok(());
        }
    };
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
    let role = Role {
        project: opts.project.clone(),
        network: network_name,
        ports: scope,
        watch: match (&opts.project, &opts.workspace) {
            (None, _) => WatchScope::All,
            (Some(dir), None) => WatchScope::Within(dir.clone()),
            (Some(_), Some(_)) => WatchScope::Nothing,
        },
    };
    let status = Arc::new(Status::new(paths.clone(), cfg.cpus, (cfg.memory_mib, cfg.elastic_mib), fs_cache, watch, role));
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
    tokio::spawn(dns_bridge(vm.clone(), dns));
    tokio::spawn(port_forwarder(vm.clone(), status.clone(), !opts.no_autoports));

    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            _ = term.recv() => break,
            _ = int.recv() => break,
            _ = kill.recv() => {
                force_stop(&vm).await;
                cleanup(&paths);
                clean_exit(&paths);
                return Ok(());
            }
            _ = tick.tick() => {
                if let Some(ttl) = opts.idle_ttl
                    && idle(
                        std::fs::metadata(paths.home.join("run/last-used")).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()),
                        ttl,
                        DOCKER_OPEN.load(std::sync::atomic::Ordering::SeqCst),
                    )
                {
                    log!("idle for {ttl}s, stopping");
                    break;
                }
                let Some(s) = probe_state(&vm).await else { continue };
                if matches!(s, State::Stopped | State::Error) {
                    log!("guest stopped on its own ({s:?})");
                    cleanup(&paths);
                    // Not clean: the login agent (`start --install`) boots it again.
                    bail!("guest stopped on its own ({s:?})");
                }
            }
        }
    }
    shutdown(&vm, &mut kill).await;
    cleanup(&paths);
    clean_exit(&paths);
    Ok(())
}

/// The VM's state, or None if its dispatch queue (which `vm.state()` waits on) didn't
/// answer within 2 s: a stuck VM can hold it.
async fn probe_state(vm: &Vm) -> Option<State> {
    let probe = vm.clone();
    tokio::time::timeout(Duration::from_secs(2), tokio::task::spawn_blocking(move || probe.state())).await.ok()?.ok()
}

/// Powers the VM off now. Bounded: when the VM's queue is stuck the daemon exits anyway,
/// which ends the VM with it.
async fn force_stop(vm: &Vm) {
    log!("force stop");
    match tokio::time::timeout(Duration::from_secs(10), vm.force_stop()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => log!("force stop failed: {e:#}"),
        Err(_) => log!("force stop did not answer within 10s; exiting"),
    }
}

/// Tells `rekcod start --foreground` (the login agent) that this stop was on purpose.
fn clean_exit(paths: &Paths) {
    let _ = std::fs::write(paths.last_exit(), b"clean");
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

/// Asks the guest to power off and waits up to 30 s, then forces it. SIGUSR1 while it
/// waits forces it at once.
async fn shutdown(vm: &Vm, kill: &mut tokio::signal::unix::Signal) {
    log!("shutting down guest");
    let graceful = async {
        if let Err(e) = control(vm, Control::Shutdown).await {
            log!("graceful shutdown request failed: {e:#}");
            return false;
        }
        // The guest waits up to 18 s for dockerd (which gives containers 10 s), then unmounts.
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if matches!(probe_state(vm).await, Some(State::Stopped | State::Error)) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    };
    let done = tokio::select! {
        done = graceful => done,
        _ = kill.recv() => false,
    };
    if done {
        log!("guest powered off");
    } else {
        force_stop(vm).await;
    }
}

/// Removes the fresh-disk marker once dockerd answers. From then on the disk holds data and
/// boot.sh must not format it, also when the `rekcod start` that booted us was interrupted
/// before it saw dockerd answer.
async fn forget_fresh_disk(vm: Vm, marker: PathBuf) {
    loop {
        if let Ok(c) = vm.connect(DOCKER_PORT).await
            && tokio::time::timeout(Duration::from_secs(5), http::get(c, "/_ping")).await.is_ok_and(|b| b.is_ok_and(|b| b == b"OK"))
        {
            let _ = std::fs::remove_file(&marker);
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Unread guest bytes all of a sandbox VM's connections may hold together. The main VM's
/// limits are sized for your own `docker logs`; a sandbox's guest may write to a connection
/// the daemon isn't reading on purpose.
const SANDBOX_CONN_BUDGET: u64 = 256 << 20;

/// A sandbox's console output kept per second; the rest is dropped.
const SANDBOX_CONSOLE_RATE: u64 = 64 << 10;

/// Longest any boot waits, `--repair` included: e2fsck -fy may run from every backup superblock.
pub(crate) const BOOT_WAIT_MAX: Duration = Duration::from_secs(1800);

/// Whether a boot that has waited `waited`, `checking` of it while the guest ran e2fsck
/// on its data disk, has run out of its `limit`. The disk check doesn't count: it takes as
/// long as the disk needs, and stopping the VM in the middle of it helps nobody.
pub(crate) fn boot_wait_over(waited: Duration, checking: Duration, limit: Duration) -> bool {
    waited > BOOT_WAIT_MAX || waited.saturating_sub(checking) > limit
}

/// Waits for the agent; returns its capabilities (the rest of the ping reply). 90 s,
/// plus the time the guest spends checking its data disk; `repair` waits up to
/// [`BOOT_WAIT_MAX`], as `rekcod start --repair` does.
async fn wait_for_agent(vm: &Vm, repair: bool, console_log: &std::path::Path) -> Result<String> {
    let limit = if repair { BOOT_WAIT_MAX } else { Duration::from_secs(90) };
    let (start, mut checking, mut in_check, mut looked) = (Instant::now(), Duration::ZERO, false, Instant::now());
    loop {
        let before = Instant::now();
        match control(vm, Control::Ping).await {
            Ok(reply) if reply.starts_with("pong") => return Ok(reply.trim_start_matches("pong").trim().to_string()),
            Ok(reply) => bail!("unexpected ping reply {reply:?}"),
            Err(e) if boot_wait_over(start.elapsed(), checking, limit) => {
                return Err(e.context("guest agent did not come up (see ~/.rekcod/logs/console.log)"));
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
        if looked.elapsed() >= Duration::from_secs(1) {
            looked = Instant::now();
            in_check = crate::vm::checking_disk(console_log);
        }
        if in_check {
            checking += before.elapsed();
        }
        if matches!(probe_state(vm).await, Some(State::Stopped | State::Error)) {
            bail!("guest stopped during boot (see ~/.rekcod/logs/console.log)");
        }
    }
}

async fn control(vm: &Vm, cmd: Control) -> Result<String> {
    control_within(vm, cmd, Duration::from_secs(5)).await
}

async fn control_within(vm: &Vm, cmd: Control, timeout: Duration) -> Result<String> {
    ask(vm.connect(CONTROL_PORT).await?, &cmd, timeout).await
}

/// Sends `cmd` on a fresh control connection and reads the one-line reply.
async fn ask<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(stream: S, cmd: &Control, timeout: Duration) -> Result<String> {
    let mut stream = BufReader::new(stream);
    stream.get_mut().write_all(cmd.encode().as_bytes()).await?;
    let mut reply = String::new();
    tokio::time::timeout(timeout, read_line_max(&mut stream, &mut reply)).await??;
    Ok(reply.trim_end().to_string())
}

/// Longest reply line read from the guest agent.
const REPLY_MAX: u64 = 64 << 10;

/// `read_line`, but a line that hasn't ended within [`REPLY_MAX`] is an error: a sandbox's
/// guest decides what it sends, and would otherwise grow the daemon's memory at will.
async fn read_line_max<R: tokio::io::AsyncBufRead + Unpin>(r: &mut R, line: &mut String) -> Result<usize> {
    let mut buf = Vec::new();
    let n = (&mut *r).take(REPLY_MAX).read_until(b'\n', &mut buf).await?;
    if n as u64 == REPLY_MAX && buf.last() != Some(&b'\n') {
        bail!("guest reply over {} KiB without a line end", REPLY_MAX >> 10);
    }
    line.push_str(std::str::from_utf8(&buf).context("guest reply is not UTF-8")?);
    Ok(n)
}

/// Returns once the client has closed its end (anything it sends is ignored).
async fn closed<R: tokio::io::AsyncRead + Unpin>(r: &mut R) {
    let mut buf = [0u8; 256];
    while matches!(r.read(&mut buf).await, Ok(n) if n > 0) {}
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
            let relay = async {
                match Control::parse(&line) {
                    Some(Control::Stats) => {
                        let reply =
                            control_within(&vm, Control::Stats, Duration::from_secs(600)).await.unwrap_or_else(|e| format!("error: {e:#}"));
                        with_elastic_held(reply, vm.elastic_held())
                    }
                    Some(cmd @ (Control::Trim | Control::BenchMeta(_))) => {
                        control_within(&vm, cmd, Duration::from_secs(600)).await.unwrap_or_else(|e| format!("error: {e:#}"))
                    }
                    _ => format!("error: unsupported command {:?}", line.trim_end()),
                }
            };
            // A client that gave up (the CLI waits 5 s for stats) takes the guest side with it.
            let reply = tokio::select! {
                reply = relay => reply,
                () = closed(&mut conn) => return,
            };
            let _ = conn.get_mut().write_all(format!("{reply}\n").as_bytes()).await;
        });
    }
}

/// The guest's stats JSON plus what elastic memory holds on the Mac, which only the host knows.
fn with_elastic_held(reply: String, held: Option<u64>) -> String {
    match (held, reply.strip_suffix('}')) {
        (Some(h), Some(body)) if reply.starts_with('{') => format!("{body},\"elastic_held\":{h}}}"),
        _ => reply,
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
                if read_line_max(stream, &mut reply).await? == 0 {
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

/// What the DNS bridge answers. Sandboxes get standard queries for one plain name only
/// ([`rekcod_proto::dns_plain_query`]), whether or not they have an allowlist.
struct DnsPolicy {
    sandbox: bool,
    filter: Option<DnsFilter>,
    /// Refusals are logged through this: a guest can send them as fast as it likes.
    limit: Mutex<rekcod_proto::LogLimit>,
    log: netstack::Logger,
}

impl DnsPolicy {
    async fn answer(&self, query: &[u8]) -> Option<Vec<u8>> {
        let name = dns::query_name(query);
        let refuse = if self.sandbox && !rekcod_proto::dns_plain_query(query) {
            true
        } else if let Some(f) = &self.filter {
            !name.as_deref().is_some_and(|n| rekcod_proto::host_allowed(&f.allow, n))
        } else {
            false
        };
        if refuse {
            // The name is the guest's: escaped, cut short, and rate-limited.
            let line = format!("rekcod-egress: refused dns {}", dns::printable(name.as_deref().unwrap_or("(not one plain query)")));
            let lines = self.limit.lock().unwrap_or_else(std::sync::PoisonError::into_inner).lines(line, Instant::now());
            for l in lines {
                (self.log)(l);
            }
            return dns::refused(query);
        }
        let answer = resolve(query).await?;
        if let Some(f) = &self.filter {
            let raw = name.as_deref().map(|n| rekcod_proto::raw_ports(&f.allow, n)).unwrap_or_default();
            for (ip, ttl) in dns::answer_a(&answer) {
                f.resolved.record_ports(ip, ttl, &raw);
            }
        }
        Some(answer)
    }
}

/// Answers the guest's DNS queries with the Mac's current resolvers.
async fn dns_bridge(vm: Vm, policy: Arc<DnsPolicy>) {
    loop {
        let session = async { dns_session(vm.connect(DNS_PORT).await?, &policy).await };
        if let Err(e) = session.await {
            log!("dns bridge: {e:#}");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn dns_session<S>(stream: S, policy: &Arc<DnsPolicy>) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
{
    let policy = policy.clone();
    dns_serve(stream, move |query: Vec<u8>| {
        let policy = policy.clone();
        async move { policy.answer(&query).await }
    })
    .await
}

/// Queries answered at once; more wait, so a flood can't spawn tasks without limit.
const DNS_IN_FLIGHT: usize = 256;

/// Length-prefixed DNS queries in, `answer`'s replies out, until the stream ends.
async fn dns_serve<S, F, Fut>(stream: S, answer: F) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
    F: Fn(Vec<u8>) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Option<Vec<u8>>> + Send + 'static,
{
    let (mut rd, mut wr) = tokio::io::split(stream);
    // Bounded, and a query's permit is held until its answer is queued: a guest that stops
    // reading answers stops having its queries read.
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(DNS_IN_FLIGHT);
    let in_flight = Arc::new(tokio::sync::Semaphore::new(DNS_IN_FLIGHT));
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
            let permit = in_flight.clone().acquire_owned().await?;
            let answering = answer(query);
            tokio::spawn(async move {
                // Held until the answer is queued.
                let _permit = permit;
                if let Some(frame) = answering.await.as_deref().and_then(frame) {
                    let _ = tx.send(frame).await;
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
        if let Some(buf) = dns::ask_udp(query, server, Duration::from_secs(2)).await {
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
    if !dns::answers(query, &answer) {
        bail!("the resolver's answer is for another query");
    }
    Ok(answer)
}

/// Connections open on the docker socket. A `docker exec`, attach or shell holds one for as
/// long as it runs, so a sandbox isn't idle while one is open, however long ago it started.
static DOCKER_OPEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Whether a sandbox has been idle past its TTL: nothing connected to its docker socket and
/// `run/last-used` older than `ttl` seconds.
fn idle(last_used_age: Option<Duration>, ttl: u64, open: usize) -> bool {
    open == 0 && last_used_age.is_some_and(|a| a.as_secs() > ttl)
}

/// Counts a docker connection in [`DOCKER_OPEN`] while it lives; closing it is use, so the
/// TTL then starts from that moment.
struct DockerOpen(PathBuf);

impl DockerOpen {
    fn new(used: PathBuf) -> DockerOpen {
        DOCKER_OPEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        DockerOpen(used)
    }
}

impl Drop for DockerOpen {
    fn drop(&mut self) {
        if let Ok(f) = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&self.0) {
            let _ = f.set_modified(SystemTime::now());
        }
        DOCKER_OPEN.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

async fn docker_proxy(vm: Vm, listener: UnixListener, used: Option<PathBuf>) {
    loop {
        let client = match listener.accept().await {
            Ok((c, _)) => c,
            Err(e) => {
                accept_failed("docker socket", e).await;
                continue;
            }
        };
        let (vm, open) = (vm.clone(), used.clone().map(DockerOpen::new));
        tokio::spawn(async move {
            let _open = open;
            match vm.connect(DOCKER_PORT).await {
                Ok(guest) => {
                    if let Err(e) = crate::proxy::pipe(client, guest).await
                        && e.downcast_ref::<crate::proxy::ClientGone>().is_none()
                    {
                        log!("docker proxy: {e:#}");
                    }
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
    /// `nat` or `user` (main VM), `filtered` or `none` (sandboxes).
    network: &'a str,
    guest_version: &'a str,
    started_at: u64,
    cpus: usize,
    /// Base RAM plus elastic.
    memory_mib: u64,
    elastic_mib: u64,
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
    network: String,
    watch: Arc<Watch>,
    started_at: u64,
    cpus: usize,
    memory_mib: u64,
    elastic_mib: u64,
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
    scope: PortScope,
    watch_scope: WatchScope,
    /// One `apply_forwards` at a time: it waits for closed listeners between deciding and binding.
    applying: tokio::sync::Mutex<()>,
    /// Bindings `scope` refused at the last apply, so each is logged once.
    refused: Mutex<BTreeSet<Binding>>,
}

/// Most Mac ports one sandbox VM may listen on.
const SANDBOX_PORTS_MAX: usize = 64;

/// Which Mac listeners a VM may open. The main VM gets what its Docker publishes. A
/// sandbox runs code nobody vetted and its guest reports the ports, so it gets loopback
/// only (a wildcard becomes 127.0.0.1 and ::1, any other address is refused), the ports the
/// host side allowed (or any from 1024 up), and at most [`SANDBOX_PORTS_MAX`] of them.
#[derive(Default)]
struct PortScope {
    sandbox: bool,
    allowed: Option<BTreeSet<u16>>,
}

impl PortScope {
    /// The listeners to open: Docker's published ports, then the autoports ones (on ports
    /// nothing publishes), narrowed to what this VM may open; and the bindings refused.
    fn apply(
        &self,
        published: BTreeMap<Binding, String>,
        mut listening: BTreeMap<Binding, String>,
    ) -> (BTreeMap<Binding, String>, BTreeSet<Binding>) {
        if !self.sandbox {
            listening.extend(published);
            return (listening, BTreeSet::new());
        }
        let (mut out, mut refused, mut ports) = (BTreeMap::new(), BTreeSet::new(), BTreeSet::new());
        // Published first, so a guest listening on many ports can't crowd them out of the cap.
        for (b, name) in published.into_iter().chain(listening) {
            let addrs: Vec<std::net::IpAddr> = if b.addr.is_unspecified() {
                vec![std::net::Ipv4Addr::LOCALHOST.into(), std::net::Ipv6Addr::LOCALHOST.into()]
            } else if b.addr.is_loopback() {
                vec![b.addr]
            } else {
                vec![]
            };
            let port_ok = self.allowed.as_ref().map_or(b.port >= 1024, |a| a.contains(&b.port));
            if addrs.is_empty() || !port_ok || (!ports.contains(&b.port) && ports.len() >= SANDBOX_PORTS_MAX) {
                refused.insert(b);
                continue;
            }
            ports.insert(b.port);
            for addr in addrs {
                out.entry(Binding { addr, ..b }).or_insert_with(|| name.clone());
            }
        }
        (out, refused)
    }
}

/// Mac directories whose bind mounts get file events and cache warming.
enum WatchScope {
    /// The main VM: every bind source under the shared roots.
    All,
    /// A sandbox sharing the live project: bind sources inside it. Its guest decides
    /// what `/containers/json` says, so a source outside would hand it events (names,
    /// sizes, times) for Mac files it can't otherwise see.
    Within(PathBuf),
    /// A sandbox sharing a copy: its files change only from inside the guest.
    Nothing,
}

impl WatchScope {
    fn filter(&self, binds: BTreeSet<PathBuf>) -> BTreeSet<PathBuf> {
        match self {
            WatchScope::All => binds,
            WatchScope::Nothing => BTreeSet::new(),
            // Resolved first: a symlink inside the project may point anywhere.
            WatchScope::Within(root) => {
                let Ok(root) = std::fs::canonicalize(root) else { return BTreeSet::new() };
                binds.into_iter().filter(|p| std::fs::canonicalize(p).is_ok_and(|c| c.starts_with(&root))).collect()
            }
        }
    }
}

/// What this daemon's VM is: for the status file, and for what its guest may ask of the Mac.
struct Role {
    /// A sandbox's shared directory.
    project: Option<PathBuf>,
    network: &'static str,
    ports: PortScope,
    watch: WatchScope,
}

impl Status {
    fn new(paths: Paths, cpus: usize, (base_mib, elastic_mib): (u64, u64), fs_cache: bool, watch: Arc<Watch>, role: Role) -> Status {
        let started_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let Role { project, network, ports: scope, watch: watch_scope } = role;
        Status {
            paths,
            network: network.to_string(),
            watch,
            watch_scope,
            started_at,
            cpus,
            memory_mib: base_mib + elastic_mib,
            elastic_mib,
            fs_cache,
            project,
            balloon: Mutex::new(base_mib),
            forwards: Mutex::default(),
            binds: Mutex::default(),
            published: Mutex::default(),
            autoport: Mutex::default(),
            listening: Mutex::default(),
            scope,
            applying: tokio::sync::Mutex::new(()),
            refused: Mutex::default(),
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
            network: &self.network,
            started_at: self.started_at,
            cpus: self.cpus,
            memory_mib: self.memory_mib,
            elastic_mib: self.elastic_mib,
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
/// How long one Docker API round trip in the guest may take before the port forwarder
/// gives up on it and starts over.
const GUEST_API_TIMEOUT: Duration = Duration::from_secs(30);

async fn port_forwarder(vm: Vm, status: Arc<Status>, autoports_on: bool) {
    if autoports_on {
        tokio::spawn(autoports(vm.clone(), status.clone()));
    }
    loop {
        if let Err(e) = watch_events(&vm, &status).await {
            log!("port forwarder: {e:#}");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn watch_events(vm: &Vm, status: &Arc<Status>) -> Result<()> {
    reconcile(vm, status).await?;
    let open = async { http::Lines::open(vm.connect(DOCKER_PORT).await?, EVENTS_PATH).await };
    let mut events = tokio::time::timeout(GUEST_API_TIMEOUT, open).await.context("the guest's /events did not answer")??;
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
    let list = async { http::get(vm.connect(DOCKER_PORT).await?, "/containers/json?all=1").await };
    let body = tokio::time::timeout(GUEST_API_TIMEOUT, list).await.context("the guest's /containers/json did not answer")??;
    *status.published.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = ports::desired(&body)?;
    *status.autoport.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = ports::autoport_containers(&body)?;
    let binds = status.watch_scope.filter(watch::bind_sources(&body)?);
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
    apply_forwards(vm, status, true).await;
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
            apply_forwards(&vm, &status, false).await;
        }
    }
}

/// Opens and closes Mac listeners to match published + auto-detected ports.
/// `retry_failed`: try again on ports that couldn't be bound before.
/// What to do with a forward already in place, given the container that wants its binding now.
#[derive(Debug, PartialEq)]
enum Next {
    Keep,
    /// Same binding, another container name (`docker rename`, a swap seen in one step): the
    /// tunnel goes by protocol and port, so the listener stays and only the name changes.
    Rename(String),
    Close,
}

fn next(fwd: &Forward, wanted: Option<&String>, retry_failed: bool) -> Next {
    match wanted {
        Some(name) if fwd.task.is_some() && *name == fwd.container => Next::Keep,
        Some(name) if fwd.task.is_some() => Next::Rename(name.clone()),
        Some(name) if *name == fwd.container && !retry_failed => Next::Keep,
        _ => Next::Close,
    }
}

async fn apply_forwards(vm: &Vm, status: &Status, retry_failed: bool) {
    let _one = status.applying.lock().await;
    let listening = status.listening.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let published = status.published.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let (want, refused) = status.scope.apply(published, listening);
    {
        let mut last = status.refused.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for b in refused.difference(&last) {
            log!("port {}:{}/{:?}: not opened, outside what this sandbox may listen on", b.addr, b.port, b.proto);
        }
        *last = refused;
    }
    let mut changed = false;
    let mut closing = Vec::new();
    let dashboard = crate::dashboard::serving_port(&status.paths.home);
    // Ports our listeners hold now, including ones closed below.
    let ours: BTreeSet<u16> = {
        let mut active = status.forwards.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let ours = active.iter().filter(|(_, f)| f.task.is_some()).map(|(b, _)| b.port).collect();
        active.retain(|binding, fwd| match next(fwd, want.get(binding), retry_failed) {
            Next::Keep => true,
            Next::Rename(name) => {
                log!("port {}:{}/{:?} -> {name} (was {})", binding.addr, binding.port, binding.proto, fwd.container);
                fwd.container = name;
                changed = true;
                true
            }
            Next::Close => {
                if let Some(task) = fwd.task.take() {
                    task.abort();
                    closing.push(task);
                    log!("port {}:{}/{:?} closed", binding.addr, binding.port, binding.proto);
                }
                changed = true;
                false
            }
        });
        ours
    };
    // Abort is asynchronous: the socket is free once the task has ended, and a listener for
    // another address on the same port binds only then.
    for task in closing {
        let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
    }
    {
        let mut active = status.forwards.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for (binding, container) in want {
            if active.contains_key(&binding) {
                continue;
            }
            changed = true;
            let fwd = match listen(binding, !ours.contains(&binding.port), dashboard) {
                Ok(listener) => {
                    log!("port {}:{}/{:?} -> {container}", binding.addr, binding.port, binding.proto);
                    let task = match listener {
                        Listener::Tcp(l) => tokio::spawn(accept_loop(vm.clone(), l, binding.port)),
                        Listener::Udp(s) => {
                            let vm = vm.clone();
                            let connect = move || {
                                let vm = vm.clone();
                                async move { vm.connect(FORWARD_PORT).await }
                            };
                            tokio::spawn(udp_loop(connect, s, binding.port))
                        }
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
/// listeners has (or just had) the port, since it would answer the probe itself. `dashboard`:
/// the dashboard's port, refused on any address (it listens on 127.0.0.1 only, so a probe of
/// `[::1]` finds nothing there).
fn listen(b: Binding, probe: bool, dashboard: Option<u16>) -> std::io::Result<Listener> {
    use socket2::{Domain, Socket, Type};
    if dashboard == Some(b.port) {
        return Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, "the rekcod dashboard's port"));
    }
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
    // `[::]` is dual-stack, so it would take 127.0.0.1's traffic too (the dashboard's, say).
    let v4 = SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, b.port));
    let probes = match (addr.ip().is_unspecified(), addr.is_ipv6()) {
        (true, true) => vec![SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, b.port)), v4],
        (true, false) => vec![v4],
        (false, _) => vec![addr],
    };
    if probe && probes.iter().any(|p| std::net::TcpStream::connect_timeout(p, Duration::from_millis(100)).is_ok()) {
        return Err(std::io::Error::from(std::io::ErrorKind::AddrInUse));
    }
    socket.set_reuse_address(true)?;
    socket.bind(&addr.into())?;
    socket.listen(1024)?;
    Ok(Listener::Tcp(TcpListener::from_std(socket.into())?))
}

/// A UDP flow (one Mac client address) closes after this long without traffic.
const UDP_IDLE: Duration = Duration::from_secs(60);

/// Published UDP port: one vsock stream per Mac client address, carrying
/// framed datagrams both ways (the agent relays them to docker-proxy). The flows are
/// this task's and only it holds the socket, so aborting it frees the port at once and
/// ends every flow with it.
async fn udp_loop<C, F, S>(connect: C, sock: UdpSocket, port: u16)
where
    C: Fn() -> F,
    F: std::future::Future<Output = Result<S>> + Send + 'static,
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
{
    let mut flows: HashMap<SocketAddr, mpsc::Sender<Vec<u8>>> = HashMap::new();
    let mut tasks = tokio::task::JoinSet::new();
    let (reply, mut replies) = mpsc::channel::<(Vec<u8>, SocketAddr)>(UDP_QUEUE);
    let mut buf = vec![0u8; 65536];
    loop {
        tokio::select! {
            r = sock.recv_from(&mut buf) => {
                let Ok((n, from)) = r else { continue };
                let packet = buf[..n].to_vec();
                let packet = match flows.get(&from) {
                    // Full: the guest isn't keeping up, so drop the datagram, as a network would.
                    Some(tx) => match tx.try_send(packet) {
                        Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => continue,
                        Err(mpsc::error::TrySendError::Closed(p)) => p,
                    },
                    None => packet,
                };
                if flows.len() >= UDP_FLOWS_MAX {
                    continue;
                }
                let (tx, rx) = mpsc::channel(UDP_QUEUE);
                let _ = tx.try_send(packet);
                flows.insert(from, tx);
                tasks.spawn(udp_flow(connect(), reply.clone(), from, port, rx));
            }
            Some((p, to)) = replies.recv() => {
                let _ = sock.send_to(&p, to).await;
            }
            Some(done) = tasks.join_next(), if !tasks.is_empty() => {
                // A new flow for the same client may have replaced this one already.
                if let Ok(from) = done
                    && flows.get(&from).is_some_and(|tx| tx.is_closed())
                {
                    flows.remove(&from);
                }
            }
        }
    }
}

/// Datagrams queued per UDP flow, and flows per forwarded port.
const UDP_QUEUE: usize = 256;
const UDP_FLOWS_MAX: usize = 1024;

/// One client's datagrams to the guest, and the guest's replies to `reply` for the
/// listener to send. Returns the client's address when the flow ends.
async fn udp_flow<S>(
    guest: impl std::future::Future<Output = Result<S>>,
    reply: mpsc::Sender<(Vec<u8>, SocketAddr)>,
    from: SocketAddr,
    port: u16,
    mut rx: mpsc::Receiver<Vec<u8>>,
) -> SocketAddr
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite,
{
    let last = Mutex::new(Instant::now());
    let result = async {
        let (mut rd, mut wr) = tokio::io::split(guest.await?);
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
                reply.send((p, from)).await.map_err(|_| anyhow::anyhow!("the port was closed"))?;
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
    if let Err(e) = result {
        log!("forward udp :{port} for {from}: {e:#}");
    }
    from
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
    fn an_open_docker_connection_keeps_a_sandbox_up() {
        let hour = Some(Duration::from_secs(3600));
        assert!(idle(hour, 60, 0));
        assert!(!idle(hour, 60, 1), "a long exec still running");
        assert!(!idle(Some(Duration::from_secs(10)), 60, 0));
        assert!(!idle(None, 60, 0));
        let used = std::env::temp_dir().join(format!("rekcod-dopen-{}", std::process::id()));
        let _ = std::fs::remove_file(&used);
        let before = DOCKER_OPEN.load(std::sync::atomic::Ordering::SeqCst);
        let open = DockerOpen::new(used.clone());
        assert!(DOCKER_OPEN.load(std::sync::atomic::Ordering::SeqCst) > before);
        drop(open);
        // Closing counts as use.
        assert!(std::fs::metadata(&used).unwrap().modified().unwrap().elapsed().unwrap() < Duration::from_secs(5));
        let _ = std::fs::remove_file(&used);
    }

    #[test]
    fn stats_carry_elastic_held_only_with_elastic_memory() {
        assert_eq!(with_elastic_held(r#"{"disk_used":1}"#.into(), Some(5)), r#"{"disk_used":1,"elastic_held":5}"#);
        assert_eq!(with_elastic_held(r#"{"disk_used":1}"#.into(), None), r#"{"disk_used":1}"#);
        assert_eq!(with_elastic_held("error: agent gone".into(), Some(5)), "error: agent gone");
    }

    /// Runs the guest's `data_disk_ready` (data-disk.sh) on a scratch image with stub
    /// `blkid`, `mkfs.ext4` and `e2fsck`; nothing real is formatted. The stubs work on the
    /// image's bytes: blkid reports ext4 when the superblock magic (0xEF53 at byte 1080) is
    /// there, mkfs writes it, and e2fsck writes it when `recoverable` (a backup superblock
    /// it could use). Returns whether it left an ext4 disk, and the tools it ran.
    fn data_disk_ready(image: &[u8], cmdline: &str, ext4: bool, recoverable: bool) -> (bool, String) {
        use std::os::unix::fs::PermissionsExt;
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let t = std::env::temp_dir().join(format!("rekcod-data-disk-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(t.join("bin")).unwrap();
        let mut image = image.to_vec();
        if ext4 {
            image[1080..1082].copy_from_slice(&[0x53, 0xef]);
        }
        std::fs::write(t.join("vdb"), &image).unwrap();
        if recoverable {
            std::fs::write(t.join("recoverable"), b"").unwrap();
        }
        // The device is the last argument of each tool.
        let magic = r#"for d; do :; done; printf '\123\357' | dd of="$d" bs=1 seek=1080 conv=notrunc 2>/dev/null"#;
        let blkid = r#"[ "$(dd if="$1" bs=1 skip=1080 count=2 2>/dev/null | od -An -tx1 | tr -d ' \n')" = 53ef ] && echo "$1: LABEL=\"rekcod-data\" TYPE=\"ext4\"""#;
        let mkfs = format!(r#"echo "mkfs.ext4 $*" >>"$T/ran"; {magic}"#);
        let e2fsck = format!(r#"echo "e2fsck $*" >>"$T/ran"; [ -e "$T/recoverable" ] && {{ {magic}; exit 1; }}; exit 8"#);
        let stubs = [("blkid", blkid.to_string()), ("mkfs.ext4", mkfs), ("e2fsck", e2fsck)];
        for (name, body) in stubs {
            let p = t.join("bin").join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../../guest/rootfs/etc/rekcod/data-disk.sh");
        let status = std::process::Command::new("/bin/sh")
            .args(["-c", r#"log() { :; }; . "$1"; data_disk_ready "$2" "$3""#, "sh", script])
            .arg(t.join("vdb"))
            .arg(cmdline)
            .env("PATH", format!("{}:/usr/bin:/bin", t.join("bin").display()))
            .env("T", &t)
            .env("REKCOD_CONSOLE", "/dev/null")
            .status()
            .unwrap();
        let ran = std::fs::read_to_string(t.join("ran")).unwrap_or_default();
        let after = std::fs::read(t.join("vdb")).unwrap();
        std::fs::remove_dir_all(&t).unwrap();
        // What the script decided is what the disk now says.
        assert_eq!(status.success(), after[1080..1082] == [0x53, 0xef], "{ran}");
        (status.success(), ran)
    }

    #[test]
    fn data_disk_is_formatted_only_when_the_mac_just_made_it() {
        let cmdline = |extra: &str| format!("{KERNEL_CMDLINE} {extra}");
        let zeros = vec![0u8; 5 << 20];
        // Zeros where the superblock and group descriptors were, ext4 data behind them.
        let mut damaged = zeros.clone();
        damaged[(4 << 20) + 100..(4 << 20) + 104].copy_from_slice(b"data");

        let (ok, ran) = data_disk_ready(&zeros, &cmdline("rekcod.fresh=1"), false, false);
        assert!(ok && ran.starts_with("mkfs.ext4 "), "{ran}");

        let (ok, ran) = data_disk_ready(&damaged, &cmdline(""), false, false);
        assert!(!ok && !ran.contains("mkfs") && ran.contains("e2fsck -p -b 32768"), "{ran}");
        let (ok, ran) = data_disk_ready(&zeros, &cmdline(""), false, false);
        assert!(!ok && !ran.contains("mkfs"), "a blank disk without the marker: {ran}");
        let (ok, ran) = data_disk_ready(&damaged, &cmdline(""), false, true);
        assert!(ok && !ran.contains("mkfs"), "recovered from the backup: {ran}");

        // The marker but data written since: never formatted.
        let mut written = zeros.clone();
        written[1024..1028].copy_from_slice(b"\x53\xef\0\0");
        let (ok, ran) = data_disk_ready(&written, &cmdline("rekcod.fresh=1"), false, false);
        assert!(!ok && !ran.contains("mkfs"), "{ran}");

        let (ok, ran) = data_disk_ready(&damaged, &cmdline("rekcod.fsck=yes"), false, false);
        assert!(!ok && !ran.contains("mkfs") && ran.contains("e2fsck -fy -b 7962624"), "--repair walks every backup: {ran}");

        let (ok, ran) = data_disk_ready(&damaged, &cmdline("rekcod.fresh=1"), true, false);
        assert!(ok && ran.is_empty(), "an ext4 disk is left alone: {ran}");
    }

    /// Runs data-disk.sh's `orphan_file_ok` with stub e2fsprogs: dumpe2fs names an orphan
    /// file inode when the disk has the feature, debugfs says whether that inode is in use.
    /// Returns whether it succeeded, and the tools that changed the disk.
    fn orphan_file_ok(feature: bool, testi: &str) -> (bool, String) {
        use std::os::unix::fs::PermissionsExt;
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let t = std::env::temp_dir().join(format!("rekcod-orphan-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(t.join("bin")).unwrap();
        let dumpe2fs = if feature {
            "echo 'Filesystem features:      has_journal orphan_file'; echo 'Orphan file inode:        12'"
        } else {
            "echo 'Filesystem features:      has_journal'"
        };
        let debugfs = format!("echo '{testi}'");
        let log = r#"echo "$(basename "$0") $*" >>"$T/ran""#;
        for (name, body) in [("dumpe2fs", dumpe2fs), ("debugfs", &debugfs), ("tune2fs", log), ("e2fsck", log)] {
            let p = t.join("bin").join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../../guest/rootfs/etc/rekcod/data-disk.sh");
        let ok = std::process::Command::new("/bin/sh")
            .args(["-c", r#"log() { :; }; . "$1"; orphan_file_ok vdb"#, "sh", script])
            .env("PATH", format!("{}:/usr/bin:/bin", t.join("bin").display()))
            .env("T", &t)
            .env("REKCOD_CONSOLE", "/dev/null")
            .status()
            .unwrap()
            .success();
        let ran = std::fs::read_to_string(t.join("ran")).unwrap_or_default();
        std::fs::remove_dir_all(&t).unwrap();
        (ok, ran)
    }

    #[test]
    fn a_data_disk_whose_orphan_file_is_gone_loses_the_feature() {
        let (used, free) = ("Inode 12 is marked in use", "Inode 12 is not in use");
        assert_eq!(orphan_file_ok(false, free), (true, String::new()), "no orphan_file");
        assert_eq!(orphan_file_ok(true, used), (true, String::new()), "a healthy disk is left alone");
        let unreadable = "debugfs: Block bitmap checksum does not match bitmap while reading allocation bitmaps";
        assert_eq!(orphan_file_ok(true, unreadable), (true, String::new()), "only a free inode counts");
        assert_eq!(orphan_file_ok(true, free), (true, "tune2fs -O ^orphan_file vdb\ne2fsck -fy vdb\n".into()));
    }

    #[tokio::test]
    async fn removing_a_udp_port_frees_it_with_a_flow_open() {
        // Below macOS's ephemeral range, so another test's `bind(:0)` can't take it while it's free.
        let mut port = 20_000 + (std::process::id() % 20_000) as u16;
        let sock = loop {
            match UdpSocket::bind(("127.0.0.1", port)).await {
                Ok(s) => break s,
                Err(_) => port += 1,
            }
        };
        let addr = sock.local_addr().unwrap();
        let (guests_tx, mut guests) = mpsc::unbounded_channel();
        let connect = move || {
            let (ours, guest) = tokio::io::duplex(1 << 16);
            let _ = guests_tx.send(guest);
            async move { anyhow::Ok(ours) }
        };
        let task = tokio::spawn(udp_loop(connect, sock, addr.port()));
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.send_to(b"ping", addr).await.unwrap();
        let mut guest = guests.recv().await.unwrap();
        let header = forward_header(Proto::Udp, addr.port());
        let mut got = vec![0u8; header.len() + 6];
        guest.read_exact(&mut got).await.unwrap();
        assert_eq!(&got[header.len()..], b"\0\x04ping");
        guest.write_all(&frame(b"pong").unwrap()).await.unwrap();
        let mut buf = [0u8; 16];
        let (n, _) = tokio::time::timeout(Duration::from_secs(5), client.recv_from(&mut buf)).await.unwrap().unwrap();
        assert_eq!(&buf[..n], b"pong");

        // What apply_forwards does when the port goes away.
        task.abort();
        let _ = task.await;
        // A process another test spawns at the wrong moment can inherit the socket (macOS
        // sets close-on-exec after socket()); it lets go within moments. A flow holding it
        // would keep it as long as the guest end stays open.
        let deadline = Instant::now() + Duration::from_secs(2);
        while let Err(e) = UdpSocket::bind(addr).await {
            assert!(Instant::now() < deadline, "the port is still taken: {e}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let closed = tokio::time::timeout(Duration::from_secs(5), guest.read(&mut buf)).await;
        assert!(matches!(closed, Ok(Ok(0))), "the flow ended with it: {closed:?}");
    }

    #[tokio::test]
    async fn forwards_leave_the_dashboards_port_alone() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = held.local_addr().unwrap().port();
        let any = Binding { proto: Proto::Tcp, addr: std::net::Ipv6Addr::UNSPECIFIED.into(), port };
        assert_eq!(listen(any, true, None).err().map(|e| e.kind()), Some(std::io::ErrorKind::AddrInUse));
        // The dashboard holds 127.0.0.1 only; [::1] at its port is refused too.
        let v6 = Binding { proto: Proto::Tcp, addr: std::net::Ipv6Addr::LOCALHOST.into(), port };
        assert_eq!(listen(v6, true, Some(port)).err().map(|e| e.kind()), Some(std::io::ErrorKind::AddrInUse));
        drop(held);
        assert!(listen(v6, true, None).is_ok(), "any other [::1] port is fine");
    }

    #[tokio::test]
    async fn a_renamed_container_keeps_its_listener() {
        let open = || Forward { container: "web".into(), task: Some(tokio::spawn(async {})), error: None };
        let failed = Forward { container: "web".into(), task: None, error: Some("in use".into()) };
        let (web, api) = ("web".to_string(), "api".to_string());
        assert_eq!(next(&open(), Some(&web), true), Next::Keep);
        assert_eq!(next(&open(), Some(&api), true), Next::Rename("api".into()), "docker rename");
        assert_eq!(next(&open(), None, true), Next::Close);
        assert_eq!(next(&failed, Some(&web), false), Next::Keep, "not retried between docker events");
        assert_eq!(next(&failed, Some(&web), true), Next::Close, "retried on the next event");
        assert_eq!(next(&failed, Some(&api), false), Next::Close);
    }

    #[test]
    fn a_sandbox_listens_on_loopback_ports_it_was_allowed() {
        let b = |addr: &str, port: u16| Binding { proto: Proto::Tcp, addr: addr.parse().unwrap(), port };
        let want: BTreeMap<Binding, String> =
            [b("::", 8080), b("192.168.1.5", 9000), b("0.0.0.0", 22), b("127.0.0.1", 5432)].into_iter().map(|b| (b, "x".into())).collect();
        let none = BTreeMap::new;
        let main = PortScope::default();
        assert_eq!(main.apply(want.clone(), none()).0, want, "the main VM opens what Docker publishes");

        let sandbox = PortScope { sandbox: true, allowed: None };
        let (open, refused) = sandbox.apply(want.clone(), none());
        assert_eq!(open.keys().copied().collect::<Vec<_>>(), [b("127.0.0.1", 5432), b("127.0.0.1", 8080), b("::1", 8080)]);
        assert_eq!(refused, [b("0.0.0.0", 22), b("192.168.1.5", 9000)].into());

        let fleet = PortScope { sandbox: true, allowed: Some([5432].into()) };
        assert_eq!(fleet.apply(want, none()).0.keys().copied().collect::<Vec<_>>(), [b("127.0.0.1", 5432)]);

        // A guest that listens on everything gets a few ports, not the Mac's loopback, and
        // the port Docker publishes still opens.
        let many: BTreeMap<Binding, String> = (2000..3000).map(|p| (b("127.0.0.1", p), "x".into())).collect();
        let web: BTreeMap<Binding, String> = [(b("127.0.0.1", 9999), "web".into())].into();
        let (open, refused) = sandbox.apply(web, many);
        assert_eq!((open.len(), refused.len()), (SANDBOX_PORTS_MAX, 1001 - SANDBOX_PORTS_MAX));
        assert!(open.contains_key(&b("127.0.0.1", 9999)));
        assert!(publish_ports("3000, 8080").is_ok_and(|p| p == [3000, 8080].into()));
        assert!(publish_ports("").is_ok_and(|p| p.is_empty()));
        assert!(publish_ports("0").is_err() && publish_ports("http").is_err());
    }

    #[test]
    fn a_sandbox_watches_only_inside_its_live_project() {
        let t = std::env::temp_dir().join(format!("rekcod-watch-scope-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let (app, docs) = (t.join("app"), t.join("Documents"));
        std::fs::create_dir_all(app.join("src")).unwrap();
        std::fs::create_dir_all(&docs).unwrap();
        std::os::unix::fs::symlink(&docs, app.join("link")).unwrap();
        let t = t.canonicalize().unwrap();
        let (app, docs) = (t.join("app"), t.join("Documents"));
        let binds: BTreeSet<PathBuf> = [app.clone(), app.join("src"), docs.clone(), app.join("link"), app.join("../Documents")].into();

        assert_eq!(WatchScope::All.filter(binds.clone()), binds);
        assert_eq!(WatchScope::Within(app.clone()).filter(binds.clone()), [app.clone(), app.join("src")].into());
        assert!(WatchScope::Nothing.filter(binds).is_empty(), "a copy-mode sandbox watches nothing");
        std::fs::remove_dir_all(&t).unwrap();
    }

    fn dns_query(id: u16, name: &str, flags: u8) -> Vec<u8> {
        let mut q = vec![(id >> 8) as u8, id as u8, flags, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        for l in name.split('.') {
            q.push(l.len() as u8);
            q.extend_from_slice(l.as_bytes());
        }
        q.extend_from_slice(&[0, 0, 1, 0, 1]);
        q
    }

    #[tokio::test]
    async fn a_guest_that_never_reads_dns_answers_stops_being_read() {
        let logged = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let l = logged.clone();
        let policy = Arc::new(DnsPolicy {
            sandbox: true,
            filter: Some(DnsFilter { allow: vec!["npmjs.org".into()], resolved: Arc::default() }),
            limit: Mutex::new(rekcod_proto::LogLimit::new(Duration::from_secs(10), 20, "rekcod-egress: ")),
            log: Arc::new(move |_| {
                l.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }),
        });
        let (ours, mut guest) = tokio::io::duplex(1 << 16);
        tokio::spawn(async move { dns_session(ours, &policy).await });
        let flood = async {
            for i in 0..10_000u16 {
                // Refused at once: a name off the list, or an UPDATE.
                let q = if i % 2 == 0 { dns_query(i, &format!("n{i}.evil.example"), 1) } else { dns_query(i, "npmjs.org", 0x28) };
                guest.write_all(&frame(&q).unwrap()).await.unwrap();
            }
        };
        assert!(tokio::time::timeout(Duration::from_secs(3), flood).await.is_err(), "the bridge kept reading");
        assert!(logged.load(std::sync::atomic::Ordering::Relaxed) <= 20, "{logged:?} lines");
    }

    #[tokio::test]
    async fn guest_replies_have_a_size_limit() {
        let (ours, mut guest) = tokio::io::duplex(1 << 16);
        tokio::spawn(async move {
            let junk = vec![b'x'; 1 << 16];
            for _ in 0..16 {
                if guest.write_all(&junk).await.is_err() {
                    return;
                }
            }
            let _ = guest.write_all(b"\n").await;
        });
        let err = ask(ours, &Control::Ping, Duration::from_secs(5)).await.unwrap_err();
        assert!(err.to_string().contains("over 64 KiB"), "{err}");

        let (ours, mut guest) = tokio::io::duplex(1 << 16);
        tokio::spawn(async move { guest.write_all(b"pong fs-notify\n").await });
        assert_eq!(ask(ours, &Control::Ping, Duration::from_secs(5)).await.unwrap(), "pong fs-notify");
    }

    #[test]
    fn a_boot_waits_out_the_disk_check() {
        let s = Duration::from_secs;
        assert!(!boot_wait_over(s(80), s(0), s(90)));
        assert!(boot_wait_over(s(91), s(0), s(90)));
        assert!(!boot_wait_over(s(400), s(350), s(90)), "e2fsck ran for most of it");
        assert!(boot_wait_over(s(400), s(250), s(90)));
        assert!(!boot_wait_over(s(1200), s(0), BOOT_WAIT_MAX), "--repair");
        assert!(boot_wait_over(s(1801), s(1801), s(90)), "never past the cap");

        let t = std::env::temp_dir().join(format!("rekcod-console-{}", std::process::id()));
        let line = |m: &str| format!("rekcod-boot: [1.2s] {m}\n");
        for (lines, want) in [
            (vec![line("init"), line("checking data disk"), "e2fsck 1.47: /dev/vdb: 12/400 files\n".into()], true),
            (vec![line("checking data disk"), line("data disk mounted")], false),
            (vec![line("data disk has no readable ext4 superblock; trying the backup"), line("trying backup superblock 98304")], true),
            (vec![line("checking data disk"), line("data disk error: e2fsck found errors")], false),
            (vec![line("init")], false),
        ] {
            std::fs::write(&t, lines.concat()).unwrap();
            assert_eq!(crate::vm::checking_disk(&t), want, "{lines:?}");
        }
        std::fs::remove_file(&t).unwrap();
    }

    #[test]
    fn failed_file_events_come_back_as_directories() {
        let t = as_trees(&["/Users/me/app/src/a.rs".into(), "/Users/me/app/src/b.rs".into(), "/Users/me/lib/".into()]);
        assert_eq!(t.into_iter().collect::<Vec<_>>(), ["/Users/me/app/src/", "/Users/me/lib/"]);
        let many: Vec<String> = (0..1000).map(|i| format!("/Users/me/p{i}/f")).collect();
        let t = as_trees(&many);
        assert!(t.len() <= 256 && t.contains("/Users/me/"), "{t:?}");
    }
}
