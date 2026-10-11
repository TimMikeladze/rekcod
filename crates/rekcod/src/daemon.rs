//! `rekcod daemon`: owns the VM and everything that talks to it.
//!
//! - `~/.rekcod/run/docker.sock` -> vsock DOCKER_PORT (raw byte pipe)
//! - published ports: watches `/events`, mirrors each port on the Mac,
//!   tunnels connections to vsock FORWARD_PORT
//! - pushes the Mac clock to the guest every 30 s
//! - SIGTERM: ask the guest to power off, force it after a timeout

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rekcod_proto::{
    CAP_FS_NOTIFY, CONTROL_PORT, Control, DNS_PORT, DOCKER_PORT, FORWARD_PORT, dns_frame, forward_header,
};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, copy_bidirectional};
use tokio::net::{TcpListener, UdpSocket, UnixListener};
use tokio::sync::mpsc;
use tokio::signal::unix::{SignalKind, signal};
use tokio::task::JoinHandle;

use crate::dns;
use crate::http;
use crate::memory;
use crate::paths::Paths;
use crate::ports::{self, Binding};
use crate::vm::{State, Vm, VmConfig};
use crate::watch::{self, Watch};

// virtio-rng must load from the initramfs: without it boot stalls ~5s waiting for entropy.
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
}

pub fn run(paths: Paths, opts: Options) -> Result<()> {
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(serve(paths, opts))
}

async fn serve(paths: Paths, opts: Options) -> Result<()> {
    let t0 = Instant::now();
    let guest = paths.guest_dir()?;
    let cfg = VmConfig {
        cpus: opts.cpus,
        memory_mib: opts.memory_mib,
        kernel: guest.join("Image"),
        initrd: guest.join("initramfs"),
        cmdline: std::env::var("REKCOD_KERNEL_CMDLINE").unwrap_or_else(|_| KERNEL_CMDLINE.into()),
        root_disk: paths.root_disk(),
        data_disk: paths.data_disk(),
        mac_address: std::fs::read_to_string(paths.mac_address())?.trim().to_string(),
        console_log: paths.console_log(),
        shares: ["Users", "Volumes", "private"]
            .iter()
            .filter(|d| std::path::Path::new("/").join(d).exists())
            .map(|d| (d.to_string(), std::path::Path::new("/").join(d)))
            .collect(),
    };
    let vm = Vm::create(&cfg)?;
    vm.start().await?;
    std::fs::write(paths.pid_file(), std::process::id().to_string())?;
    log!("VM started ({} CPUs, {} MiB, rosetta={})", cfg.cpus, cfg.memory_mib, vm.rosetta);

    // Docker socket first so `rekcod start` can poll it while the guest boots.
    let sock = paths.socket();
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).with_context(|| format!("bind {}", sock.display()))?;
    tokio::spawn(docker_proxy(vm.clone(), listener));

    let caps = wait_for_agent(&vm).await?;
    let fs_cache = caps.split_whitespace().any(|c| c == CAP_FS_NOTIFY);
    log!("guest agent up after {:.2}s (cached file sharing: {fs_cache})", t0.elapsed().as_secs_f64());

    let notify_vm = vm.clone();
    let watch = Watch::start(fs_cache, move |changed: Vec<String>| {
        let vm = notify_vm.clone();
        async move {
            if std::env::var_os("REKCOD_DEBUG").is_some() {
                log!("file events: {changed:?}");
            }
            if let Err(e) = notify_guest(&vm, &changed).await {
                log!("file events: {e:#}");
            }
        }
    })?;
    for line in watch.set_roots(Default::default()) {
        log!("{line}");
    }
    let status = Arc::new(Status::new(paths.clone(), cfg.cpus, cfg.memory_mib, fs_cache, watch));
    status.write();
    tokio::spawn(clock_sync(vm.clone()));
    if std::env::var_os("REKCOD_BALLOON").is_some() {
        tokio::spawn(memory_manager(vm.clone(), cfg.memory_mib * 1024 * 1024, status.clone()));
    }
    tokio::spawn(dns_bridge(vm.clone()));
    tokio::spawn(port_forwarder(vm.clone(), status.clone()));

    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            _ = term.recv() => break,
            _ = int.recv() => break,
            _ = tick.tick() => {
                let s = vm.state();
                if matches!(s, State::Stopped | State::Error) {
                    log!("guest stopped on its own ({s:?})");
                    cleanup(&paths);
                    return Ok(());
                }
            }
        }
    }
    shutdown(&vm).await;
    cleanup(&paths);
    Ok(())
}

fn cleanup(paths: &Paths) {
    for f in [paths.socket(), paths.pid_file(), paths.status_file()] {
        let _ = std::fs::remove_file(f);
    }
}

async fn shutdown(vm: &Vm) {
    log!("shutting down guest");
    let asked = control(vm, Control::Shutdown).await;
    if let Err(e) = &asked {
        log!("graceful shutdown request failed: {e:#}");
    }
    let deadline = Instant::now() + Duration::from_secs(20);
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
    let stream = vm.connect(CONTROL_PORT).await?;
    let mut stream = BufReader::new(stream);
    stream.get_mut().write_all(cmd.encode().as_bytes()).await?;
    let mut reply = String::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_line(&mut reply)).await??;
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
        let pressure = tick.is_multiple_of(10)
            && crate::cli::sysctl_u64("kern.memorystatus_vm_pressure_level").unwrap_or(1) & 0xff >= 2;
        if pressure
            && let Some((_, cached)) = control(&vm, Control::Meminfo).await.ok().as_deref().and_then(memory::parse_meminfo)
        {
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
            *status.balloon.lock().unwrap() = target >> 20;
            status.write();
        }
    }
}

/// Forwards Mac-side file changes so watchers inside containers fire.
async fn notify_guest(vm: &Vm, changed: &[String]) -> Result<()> {
    let mut stream = BufReader::new(vm.connect(CONTROL_PORT).await?);
    let lines: String = changed.iter().map(|p| Control::Notify(p.clone()).encode()).collect();
    stream.get_mut().write_all(lines.as_bytes()).await?;
    let mut reply = String::new();
    for _ in changed {
        reply.clear();
        if stream.read_line(&mut reply).await? == 0 {
            break;
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
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

/// Answers the guest's DNS queries with the Mac's current resolvers.
async fn dns_bridge(vm: Vm) {
    loop {
        if let Err(e) = dns_session(&vm).await {
            log!("dns bridge: {e:#}");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn dns_session(vm: &Vm) -> Result<()> {
    let (mut rd, mut wr) = tokio::io::split(vm.connect(DNS_PORT).await?);
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
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
            tokio::spawn(async move {
                if let Some(frame) = resolve(&query).await.as_deref().and_then(dns_frame) {
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

async fn resolve(query: &[u8]) -> Option<Vec<u8>> {
    let resolv = std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default();
    for server in dns::nameservers(&resolv, dns::if_index) {
        let local: SocketAddr = if server.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }.parse().unwrap();
        let Ok(sock) = UdpSocket::bind(local).await else { continue };
        if sock.send_to(query, server).await.is_err() {
            continue;
        }
        let mut buf = vec![0u8; 4096];
        if let Ok(Ok((n, _))) = tokio::time::timeout(Duration::from_secs(2), sock.recv_from(&mut buf)).await {
            buf.truncate(n);
            return Some(buf);
        }
    }
    None
}

async fn docker_proxy(vm: Vm, listener: UnixListener) {
    loop {
        let Ok((mut client, _)) = listener.accept().await else { continue };
        let vm = vm.clone();
        tokio::spawn(async move {
            match vm.connect(DOCKER_PORT).await {
                Ok(mut guest) => {
                    let _ = copy_bidirectional(&mut client, &mut guest).await;
                }
                Err(e) => log!("docker proxy: {e:#}"),
            }
        });
    }
}

// ---------------------------------------------------------------- ports

#[derive(Serialize)]
struct PortStatus {
    address: String,
    port: u16,
    container: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct StatusFile<'a> {
    pid: u32,
    started_at: u64,
    cpus: usize,
    memory_mib: u64,
    fs_cache: bool,
    balloon_mib: u64,
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
    balloon: Mutex<u64>,
    forwards: Mutex<BTreeMap<Binding, Forward>>,
}

impl Status {
    fn new(paths: Paths, cpus: usize, memory_mib: u64, fs_cache: bool, watch: Arc<Watch>) -> Status {
        let started_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        Status { paths, watch, started_at, cpus, memory_mib, fs_cache, balloon: Mutex::new(memory_mib), forwards: Mutex::default() }
    }

    fn write(&self) {
        let ports: Vec<PortStatus> = self
            .forwards
            .lock()
            .unwrap()
            .iter()
            .map(|(b, f)| PortStatus {
                address: b.addr.to_string(),
                port: b.port,
                container: f.container.clone(),
                error: f.error.clone(),
            })
            .collect();
        let file = StatusFile {
            pid: std::process::id(),
            started_at: self.started_at,
            cpus: self.cpus,
            memory_mib: self.memory_mib,
            fs_cache: self.fs_cache,
            balloon_mib: *self.balloon.lock().unwrap(),
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
    let body = http::get(vm.connect(DOCKER_PORT).await?, "/containers/json").await?;
    let want = ports::desired(&body)?;
    for err in status.watch.set_roots(watch::bind_sources(&body)?) {
        log!("{err}");
    }
    let mut changed = false;
    {
        let mut active = status.forwards.lock().unwrap();
        active.retain(|binding, fwd| {
            let keep = want.get(binding) == Some(&fwd.container) && fwd.task.is_some();
            if !keep {
                if let Some(task) = fwd.task.take() {
                    task.abort();
                    log!("port {}:{} closed", binding.addr, binding.port);
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
            let fwd = match listen(binding) {
                Ok(listener) => {
                    log!("port {}:{} -> {container}", binding.addr, binding.port);
                    let task = tokio::spawn(accept_loop(vm.clone(), listener, binding.port));
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
    Ok(())
}

fn listen(b: Binding) -> std::io::Result<TcpListener> {
    use socket2::{Domain, Socket, Type};
    let addr = SocketAddr::new(b.addr, b.port);
    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, None)?;
    if addr.is_ipv6() {
        // `[::]` is dual-stack (covers 127.0.0.1 too); `[::1]` stays v6-only.
        socket.set_only_v6(!b.addr.is_unspecified())?;
    }
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(1024)?;
    TcpListener::from_std(socket.into())
}

async fn accept_loop(vm: Vm, listener: TcpListener, port: u16) {
    loop {
        let Ok((mut tcp, _)) = listener.accept().await else { continue };
        let vm = vm.clone();
        tokio::spawn(async move {
            let _ = tcp.set_nodelay(true);
            let tunnel = async {
                let mut guest = vm.connect(FORWARD_PORT).await?;
                guest.write_all(forward_header(port).as_bytes()).await?;
                copy_bidirectional(&mut tcp, &mut guest).await?;
                anyhow::Ok(())
            };
            if let Err(e) = tunnel.await {
                log!("forward :{port}: {e:#}");
            }
        });
    }
}
