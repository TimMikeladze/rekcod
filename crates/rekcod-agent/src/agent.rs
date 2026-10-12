use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rekcod_proto::{
    CAP_FS_NOTIFY, CONTROL_PORT, Control, DNS_PORT, DNS_STUB_ADDR, DOCKER_PORT, FORWARD_PORT, META_PORT, Proto,
    REVERSE_LISTEN_PORT, REVERSE_PORT, frame, parse_forward_header, reverse_header,
};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader, copy_bidirectional};
use tokio::net::{TcpListener, TcpStream, UdpSocket, UnixStream};
use tokio::sync::{mpsc, oneshot};
use tokio_vsock::{VMADDR_CID_ANY, VsockAddr, VsockListener, VsockStream};

const DOCKER_SOCK: &str = "/var/run/docker.sock";
/// Ignore clock corrections smaller than this to avoid needless jumps.
const CLOCK_SLACK_NS: i128 = 100_000_000;

pub fn run() -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(async {
            let dns = Arc::new(Dns {
                sock: UdpSocket::bind(DNS_STUB_ADDR).await.context("bind DNS stub")?,
                writer: Mutex::default(),
                pending: Mutex::default(),
                next_id: AtomicU16::new(1),
            });
            let (stub, tcp_stub) = (dns.clone(), dns.clone());
            let (park, parked) = mpsc::unbounded_channel();
            let parked = Arc::new(tokio::sync::Mutex::new(parked));
            tokio::spawn(trim_periodically());
            tokio::try_join!(
                serve(DOCKER_PORT, docker),
                serve(FORWARD_PORT, forward),
                serve(CONTROL_PORT, control),
                serve(DNS_PORT, move |s| dns_channel(s, dns.clone())),
                serve(REVERSE_PORT, move |s| {
                    let _ = park.send(s);
                    async { Ok(()) }
                }),
                serve(META_PORT, crate::meta::serve_client),
                reverse_listener(parked),
                dns_stub(stub),
                dns_tcp_stub(tcp_stub),
            )?;
            Ok(())
        })
}

async fn serve<F, Fut>(port: u32, handler: F) -> Result<()>
where
    F: Fn(VsockStream) -> Fut,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    let listener = VsockListener::bind(VsockAddr::new(VMADDR_CID_ANY, port))
        .with_context(|| format!("bind vsock port {port}"))?;
    eprintln!("rekcod-agent: listening on vsock port {port}");
    loop {
        let (stream, _) = listener.accept().await?;
        let task = handler(stream);
        tokio::spawn(async move {
            if let Err(e) = task.await {
                eprintln!("rekcod-agent: port {port}: {e:#}");
            }
        });
    }
}

async fn docker(mut vsock: VsockStream) -> Result<()> {
    let mut sock = UnixStream::connect(DOCKER_SOCK).await.context("connect docker.sock")?;
    copy_bidirectional(&mut vsock, &mut sock).await?;
    Ok(())
}

async fn forward(vsock: VsockStream) -> Result<()> {
    // BufReader keeps any bytes the client sent right after the header.
    let mut vsock = BufReader::new(vsock);
    let mut line = String::new();
    vsock.read_line(&mut line).await?;
    let (proto, port) = parse_forward_header(&line).with_context(|| format!("bad forward header {line:?}"))?;
    if proto == Proto::Udp {
        return forward_udp(vsock, port).await;
    }
    let mut tcp = TcpStream::connect(("127.0.0.1", port))
        .await
        .with_context(|| format!("connect 127.0.0.1:{port}"))?;
    tcp.set_nodelay(true)?;
    copy_bidirectional(&mut vsock, &mut tcp).await?;
    Ok(())
}

/// One Mac-side UDP client: framed datagrams on the vsock stream <-> a socket
/// connected to docker-proxy on `127.0.0.1:<port>`. The host closes idle flows.
async fn forward_udp(vsock: BufReader<VsockStream>, port: u16) -> Result<()> {
    let udp = UdpSocket::bind("127.0.0.1:0").await?;
    udp.connect(("127.0.0.1", port)).await?;
    let (mut rd, mut wr) = tokio::io::split(vsock);
    let up = async {
        loop {
            udp.send(&read_frame(&mut rd).await?).await?;
        }
    };
    let down = async {
        let mut buf = vec![0u8; 65536];
        loop {
            let n = udp.recv(&mut buf).await?;
            if let Some(f) = frame(&buf[..n]) {
                wr.write_all(&f).await?;
            }
        }
    };
    tokio::select! {
        r = up => r,
        r = down => r,
    }
}

/// Reads one [`frame`]d packet.
async fn read_frame<R: AsyncRead + Unpin>(rd: &mut R) -> Result<Vec<u8>> {
    let mut len = [0u8; 2];
    rd.read_exact(&mut len).await?;
    let mut packet = vec![0u8; u16::from_be_bytes(len) as usize];
    rd.read_exact(&mut packet).await?;
    Ok(packet)
}

// ------------------------------------------------- host.docker.internal
//
// boot.sh redirects TCP for the Mac's gateway address to REVERSE_LISTEN_PORT.
// Each connection takes a vsock stream the host parked on REVERSE_PORT and asks
// the host to connect it to 127.0.0.1:<original port> on the Mac, so services
// bound to the Mac's loopback are reachable as host.docker.internal.

type Parked = Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<VsockStream>>>;

async fn reverse_listener(parked: Parked) -> Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", REVERSE_LISTEN_PORT)).await.context("bind reverse listener")?;
    loop {
        let (tcp, _) = listener.accept().await?;
        let parked = parked.clone();
        tokio::spawn(async move {
            if let Err(e) = reverse(tcp, parked).await {
                eprintln!("rekcod-agent: host.docker.internal: {e:#}");
            }
        });
    }
}

async fn reverse(mut tcp: TcpStream, parked: Parked) -> Result<()> {
    let port = original_dst_port(&tcp)?;
    if port == REVERSE_LISTEN_PORT {
        anyhow::bail!("direct connection to the reverse listener");
    }
    tcp.set_nodelay(true)?;
    // Parked streams die when the daemon restarts; skip those.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let next = tokio::time::timeout_at(deadline.into(), async { parked.lock().await.recv().await }).await;
        let Ok(Some(mut host)) = next else { anyhow::bail!("no connection from the host (is rekcod's daemon running?)") };
        if host.write_all(reverse_header(port).as_bytes()).await.is_ok() {
            copy_bidirectional(&mut tcp, &mut host).await?;
            return Ok(());
        }
    }
}

/// Destination port before iptables REDIRECT (SO_ORIGINAL_DST).
fn original_dst_port(tcp: &TcpStream) -> Result<u16> {
    use std::os::fd::AsRawFd;
    const SO_ORIGINAL_DST: libc::c_int = 80;
    let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
    // SAFETY: valid fd and an out-buffer of the advertised size.
    let rc = unsafe {
        libc::getsockopt(tcp.as_raw_fd(), libc::SOL_IP, SO_ORIGINAL_DST, (&raw mut addr).cast(), &mut len)
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).context("SO_ORIGINAL_DST");
    }
    Ok(u16::from_be(addr.sin_port))
}

async fn control(vsock: VsockStream) -> Result<()> {
    let mut vsock = BufReader::new(vsock);
    let mut line = String::new();
    while vsock.read_line(&mut line).await? > 0 {
        let reply = match Control::parse(&line) {
            Some(Control::Ping) if fs_notify_available() => format!("pong {CAP_FS_NOTIFY}"),
            Some(Control::Ping) => "pong".to_string(),
            Some(Control::Time(ns)) => set_clock(ns),
            Some(Control::Notify(path)) if fs_notify_available() => fs_notify(&path),
            Some(Control::Notify(line)) if !line.ends_with('/') => poke(line.split('\t').next().unwrap_or_default()),
            Some(Control::Notify(_)) => "ok".into(),
            Some(Control::Meminfo) => meminfo(),
            Some(Control::Warm(path)) => warm(path),
            Some(Control::BenchMeta(dir)) => crate::meta::bench(&dir).await.unwrap_or_else(|e| format!("error: {e:#}")),
            Some(Control::Trim) => tokio::task::spawn_blocking(trim).await.unwrap_or_else(|e| format!("error: {e}")),
            Some(Control::Listeners(id)) => {
                tokio::task::spawn_blocking(move || listeners(&id)).await.unwrap_or_else(|e| format!("error: {e}"))
            }
            Some(Control::Stats) => stats(),
            Some(Control::Reclaim(bytes)) => match std::fs::write("/sys/fs/cgroup/memory.reclaim", bytes.to_string()) {
                // EAGAIN just means less than asked could be reclaimed.
                Ok(()) => "ok".into(),
                Err(e) => format!("partial: {e}"),
            },
            Some(Control::Shutdown) => {
                vsock.get_mut().write_all(b"ok\n").await?;
                // busybox poweroff signals init, which runs the inittab shutdown actions.
                std::process::Command::new("/sbin/poweroff").spawn()?;
                return Ok(());
            }
            None => format!("error: unknown command {:?}", line.trim_end()),
        };
        vsock.get_mut().write_all(format!("{reply}\n").as_bytes()).await?;
        line.clear();
    }
    Ok(())
}

/// Patched fuse.ko (guest/fuse): invalidates a host-changed path and raises the
/// inotify event itself. Present when the guest image ships the module.
const FS_NOTIFY: &str = "/sys/module/fuse/parameters/rekcod_notify";

fn fs_notify_available() -> bool {
    std::path::Path::new(FS_NOTIFY).exists()
}

fn fs_notify(path: &str) -> String {
    match std::fs::write(FS_NOTIFY, path) {
        Ok(()) => "ok".into(),
        Err(e) => format!("error: {e}"),
    }
}

// Fallback for stock virtio-fs (no long-TTL caching, so only freshness and
// inotify need help), used only for container bind sources.

/// Regular file on tmpfs (a different mount from the shares); see `refresh_dentry`.
const ANCHOR: &str = "/run/rekcod-anchor";

/// Forces virtio-fs to re-look-up one cached name. A Mac-side atomic save
/// (write temp, rename over) leaves the guest's dentry on the old, deleted
/// inode until the entry timeout, so opens fail with ENOENT. `link()` does an
/// exclusive-create lookup, which makes FUSE revalidate the dentry against the
/// host, then fails with EEXIST (path exists) or EXDEV (it doesn't; the anchor
/// is on another mount). Nothing is ever created.
fn refresh_dentry(path: &std::path::Path) {
    if !std::path::Path::new(ANCHOR).exists() {
        let _ = std::fs::write(ANCHOR, b"");
    }
    if std::fs::hard_link(ANCHOR, path).is_ok() {
        let _ = std::fs::remove_file(path); // can't happen across mounts; never leave a stray link
    }
}

/// Same-mode chmod: no change to the file, but guest watchers get IN_ATTRIB and
/// virtio-fs refetches attributes. A deleted path pokes its directory instead.
fn poke(path: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = std::path::Path::new(path);
    refresh_dentry(path);
    let target = match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => return "ok".into(),
        Ok(_) => path,
        Err(_) => match path.parent() {
            Some(parent) => parent,
            None => return "ok".into(),
        },
    };
    let result = std::fs::metadata(target)
        .and_then(|m| std::fs::set_permissions(target, std::fs::Permissions::from_mode(m.permissions().mode() & 0o7777)));
    match result {
        Ok(()) => "ok".into(),
        Err(e) => format!("error: {e}"),
    }
}

// ------------------------------------------------------- cache warming
//
// A container just bind-mounted a Mac directory: list the whole tree once in
// the background. Each readdir is a READDIRPLUS, which fills the guest's
// dentry and attribute caches (day-long TTLs with the rekcod mount option), so
// the container's first stat/open/negative lookup doesn't cross into macOS.

const WARM_MAX_ENTRIES: usize = 200_000;
static WARMING: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn warm(path: String) -> String {
    {
        let mut active = WARMING.lock().unwrap();
        if active.iter().any(|p| path.starts_with(p.as_str())) {
            return "ok already warming".into();
        }
        active.push(path.clone());
    }
    std::thread::spawn(move || {
        // SAFETY: plain syscalls; lower this thread's priority below containers.
        unsafe { libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, 10) };
        let t0 = Instant::now();
        let (entries, dirs) = warm_tree(std::path::Path::new(&path));
        eprintln!(
            "rekcod-agent: warmed {path}: {entries} entries in {dirs} dirs, {} ms",
            t0.elapsed().as_millis()
        );
        WARMING.lock().unwrap().retain(|p| p != &path);
    });
    "ok".into()
}

/// Lists every directory and lstats every entry (8 threads): readdir alone
/// doesn't populate the dentry cache through Apple's server (measured), so each
/// name needs its own LOOKUP, which is what a container's first stat would pay.
fn warm_tree(root: &std::path::Path) -> (usize, usize) {
    use std::sync::atomic::AtomicUsize;
    let queue = Mutex::new(vec![root.to_path_buf()]);
    let (entries, dirs, busy) = (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| loop {
                let next = {
                    let mut q = queue.lock().unwrap();
                    let d = q.pop();
                    if d.is_some() {
                        busy.fetch_add(1, Ordering::SeqCst);
                    }
                    d
                };
                let Some(dir) = next else {
                    if busy.load(Ordering::SeqCst) == 0 {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                };
                if entries.load(Ordering::Relaxed) < WARM_MAX_ENTRIES
                    && let Ok(rd) = std::fs::read_dir(&dir)
                {
                    dirs.fetch_add(1, Ordering::Relaxed);
                    for entry in rd.flatten() {
                        entries.fetch_add(1, Ordering::Relaxed);
                        if std::fs::symlink_metadata(entry.path()).is_ok_and(|m| m.is_dir()) {
                            queue.lock().unwrap().push(entry.path());
                        }
                    }
                }
                busy.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    (entries.into_inner(), dirs.into_inner())
}

// ------------------------------------------------------------ listeners
//
// `rekcod x` runs containers on the VM's host network; the daemon asks which
// TCP ports a container's processes listen on and forwards them to the Mac's
// localhost through FORWARD_PORT (which dials 127.0.0.1 here).

fn listeners(id: &str) -> String {
    let mut inodes = std::collections::HashSet::new();
    let mut netns_pid = None;
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|p| p.parse::<u32>().ok()) else { continue };
        let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap_or_default();
        if !cgroup.contains(id) {
            continue;
        }
        netns_pid.get_or_insert(pid);
        for fd in std::fs::read_dir(format!("/proc/{pid}/fd")).into_iter().flatten().flatten() {
            let Ok(link) = std::fs::read_link(fd.path()) else { continue };
            if let Some(ino) = link.to_str().and_then(|l| l.strip_prefix("socket:[")?.strip_suffix(']')?.parse::<u64>().ok()) {
                inodes.insert(ino);
            }
        }
    }
    let Some(pid) = netns_pid else { return "ok".into() };
    let mut ports = std::collections::BTreeSet::new();
    for table in ["tcp", "tcp6"] {
        let text = std::fs::read_to_string(format!("/proc/{pid}/net/{table}")).unwrap_or_default();
        ports.extend(listening_ports(&text, &inodes));
    }
    let list: Vec<String> = ports.iter().map(u16::to_string).collect();
    format!("ok {}", list.join(" ")).trim_end().to_string()
}

/// Ports in LISTEN state (`0A`) owned by `inodes`, from `/proc/net/tcp{,6}`.
fn listening_ports(table: &str, inodes: &std::collections::HashSet<u64>) -> Vec<u16> {
    table
        .lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.get(3) != Some(&"0A") || !inodes.contains(&f.get(9)?.parse().ok()?) {
                return None;
            }
            u16::from_str_radix(f.get(1)?.rsplit(':').next()?, 16).ok()
        })
        .collect()
}

// ---------------------------------------------------------------- stats

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// One JSON line for `rekcod status`: host requests by FUSE opcode (cache
/// misses since boot), trees being warmed, data-disk usage.
fn stats() -> String {
    let ops = std::fs::read_to_string("/sys/module/virtiofs/parameters/rekcod_stats").unwrap_or_default();
    let ops: Vec<String> = ops
        .lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(op, n)| format!("{}:{}", json_str(op), n.trim().parse::<u64>().unwrap_or(0)))
        .collect();
    let warming: Vec<String> = WARMING.lock().unwrap().iter().map(|p| json_str(p)).collect();
    let (mut used, mut size) = (0u64, 0u64);
    let path = std::ffi::CString::new("/var/lib/docker").unwrap();
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: valid path and out-pointer.
    if unsafe { libc::statvfs(path.as_ptr(), &mut st) } == 0 {
        size = st.f_blocks as u64 * st.f_frsize as u64;
        used = size.saturating_sub(st.f_bfree as u64 * st.f_frsize as u64);
    }
    format!(
        "{{\"fs_requests\":{{{}}},\"warming\":[{}],\"disk_used\":{used},\"disk_size\":{size}}}",
        ops.join(","),
        warming.join(",")
    )
}

// ------------------------------------------------------------- trimming

/// `fstrim` the data disk: Virtualization.framework punches the freed blocks
/// out of `data.img`, so deleted images and volumes give disk back to macOS.
fn trim() -> String {
    match std::process::Command::new("fstrim").args(["-v", "/var/lib/docker"]).output() {
        Ok(o) if o.status.success() => {
            let out = String::from_utf8_lossy(&o.stdout);
            // "/var/lib/docker: 123 bytes trimmed"
            let bytes = out.split_whitespace().find_map(|w| w.parse::<u64>().ok()).unwrap_or(0);
            format!("ok {bytes}")
        }
        Ok(o) => format!("error: {}", String::from_utf8_lossy(&o.stderr).trim()),
        Err(e) => format!("error: fstrim: {e}"),
    }
}

async fn trim_periodically() {
    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
        let reply = tokio::task::spawn_blocking(trim).await.unwrap_or_default();
        eprintln!("rekcod-agent: hourly trim: {reply}");
    }
}

/// `<total> <free> <available> <cached>` in KiB.
fn meminfo() -> String {
    let text = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(':'))
            .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
            .unwrap_or(0)
    };
    format!("{} {} {} {}", field("MemTotal"), field("MemFree"), field("MemAvailable"), field("Cached"))
}

fn set_clock(host_ns: u128) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let drift = host_ns as i128 - now as i128;
    if drift.abs() < CLOCK_SLACK_NS {
        return "ok".into();
    }
    let ts = libc::timespec {
        tv_sec: (host_ns / 1_000_000_000) as _,
        tv_nsec: (host_ns % 1_000_000_000) as _,
    };
    // SAFETY: plain syscall with a valid timespec.
    if unsafe { libc::clock_settime(libc::CLOCK_REALTIME, &ts) } == 0 {
        format!("ok adjusted {}ms", drift / 1_000_000)
    } else {
        format!("error: clock_settime: {}", std::io::Error::last_os_error())
    }
}

// ------------------------------------------------------------------ DNS
//
// dnsmasq (which answers host.docker.internal) uses this stub as upstream.
// Queries go to the Mac over the one channel the host keeps open on DNS_PORT,
// so the guest resolves exactly like the Mac does, including VPN resolvers.

struct Dns {
    sock: UdpSocket,
    writer: Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>,
    /// Our query id -> (client's id, client, sent at).
    pending: Mutex<HashMap<u16, (u16, Client, Instant)>>,
    next_id: AtomicU16,
}

enum Client {
    Udp(SocketAddr),
    Tcp(oneshot::Sender<Vec<u8>>),
}

/// Largest answer handed to a UDP client; bigger ones are truncated (TC) so
/// the client retries over TCP.
const UDP_ANSWER_MAX: usize = 4096;

impl Dns {
    /// Sends a client's query to the host; false if the host isn't connected.
    fn submit(&self, query: &[u8], client: Client) -> bool {
        let Some(tx) = self.writer.lock().unwrap().clone() else { return false };
        if query.len() < 12 {
            return false;
        }
        // Rewrite ids so queries from different clients can't collide.
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let original = u16::from_be_bytes([query[0], query[1]]);
        {
            let mut pending = self.pending.lock().unwrap();
            if pending.len() > 4096 {
                pending.retain(|_, (_, _, at)| at.elapsed() < Duration::from_secs(10));
            }
            pending.insert(id, (original, client, Instant::now()));
        }
        let mut packet = query.to_vec();
        packet[..2].copy_from_slice(&id.to_be_bytes());
        frame(&packet).is_some_and(|f| tx.send(f).is_ok())
    }
}

async fn dns_stub(dns: Arc<Dns>) -> Result<()> {
    let mut buf = [0u8; 4096];
    loop {
        let (n, from) = dns.sock.recv_from(&mut buf).await?;
        dns.submit(&buf[..n], Client::Udp(from));
    }
}

async fn dns_tcp_stub(dns: Arc<Dns>) -> Result<()> {
    let listener = TcpListener::bind(DNS_STUB_ADDR).await.context("bind DNS TCP stub")?;
    loop {
        let (mut tcp, _) = listener.accept().await?;
        let dns = dns.clone();
        tokio::spawn(async move {
            while let Ok(query) = read_frame(&mut tcp).await {
                let (tx, rx) = oneshot::channel();
                if !dns.submit(&query, Client::Tcp(tx)) {
                    break;
                }
                let Ok(Ok(answer)) = tokio::time::timeout(Duration::from_secs(10), rx).await else { break };
                let Some(f) = frame(&answer) else { break };
                if tcp.write_all(&f).await.is_err() {
                    break;
                }
            }
        });
    }
}

/// Header + question of `answer`, with TC set and no records: tells a UDP
/// client to ask again over TCP.
fn truncated(answer: &[u8]) -> Option<Vec<u8>> {
    let mut end = 12;
    let qdcount = u16::from_be_bytes([*answer.get(4)?, *answer.get(5)?]);
    for _ in 0..qdcount {
        loop {
            let len = *answer.get(end)? as usize;
            if len & 0xc0 == 0xc0 {
                end += 2;
                break;
            }
            end += 1 + len;
            if len == 0 {
                break;
            }
        }
        end += 4; // type + class
    }
    let mut out = answer.get(..end)?.to_vec();
    out[2] |= 0x02; // TC
    out[6..12].fill(0); // ANCOUNT, NSCOUNT, ARCOUNT
    Some(out)
}

async fn dns_channel(vsock: VsockStream, dns: Arc<Dns>) -> Result<()> {
    let (mut rd, mut wr) = tokio::io::split(vsock);
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    *dns.writer.lock().unwrap() = Some(tx);
    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if wr.write_all(&frame).await.is_err() {
                break;
            }
        }
    });
    let result = async {
        loop {
            let mut packet = read_frame(&mut rd).await?;
            if packet.len() < 12 {
                continue;
            }
            let id = u16::from_be_bytes([packet[0], packet[1]]);
            let Some((original, client, _)) = dns.pending.lock().unwrap().remove(&id) else { continue };
            packet[..2].copy_from_slice(&original.to_be_bytes());
            match client {
                Client::Tcp(tx) => {
                    let _ = tx.send(packet);
                }
                Client::Udp(addr) => {
                    let packet = if packet.len() > UDP_ANSWER_MAX { truncated(&packet) } else { Some(packet) };
                    if let Some(packet) = packet {
                        let _ = dns.sock.send_to(&packet, addr).await;
                    }
                }
            }
        }
        #[allow(unreachable_code)]
        anyhow::Ok(())
    }
    .await;
    writer.abort();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listening_ports_filters_state_and_owner() {
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 111 1 0
   1: 00000000:1435 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 222 1 0
   2: 0100007F:0BB8 0100007F:D431 01 00000000:00000000 00:00000000 00000000     0        0 111 1 0
";
        let mine: std::collections::HashSet<u64> = [111].into();
        assert_eq!(listening_ports(table, &mine), vec![3000]);
    }

    #[test]
    fn json_strings_are_escaped() {
        assert_eq!(json_str("a\"b\\c\n"), "\"a\\\"b\\\\c\\u000a\"");
    }

    #[test]
    fn truncated_keeps_question_and_sets_tc() {
        // id, flags (QR), 1 question, 1 answer; question example.com A IN; then an answer.
        let mut a = vec![0x12, 0x34, 0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0];
        let q = b"\x07example\x03com\x00\x00\x01\x00\x01";
        a.extend_from_slice(q);
        a.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 1, 2, 3, 4]);
        let t = truncated(&a).unwrap();
        assert_eq!(t.len(), 12 + q.len());
        assert_eq!(t[2] & 0x02, 0x02);
        assert_eq!(&t[4..12], &[0, 1, 0, 0, 0, 0, 0, 0]);
        assert!(truncated(&a[..14]).is_none());
    }
}
