use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rekcod_proto::{
    CAP_FS_NOTIFY, CONTROL_PORT, Control, DNS_PORT, DNS_STUB_ADDR, DOCKER_PORT, FORWARD_PORT, dns_frame,
    parse_forward_header,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, copy_bidirectional};
use tokio::net::{TcpStream, UdpSocket, UnixStream};
use tokio::sync::mpsc;
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
            let stub = dns.clone();
            tokio::try_join!(
                serve(DOCKER_PORT, docker),
                serve(FORWARD_PORT, forward),
                serve(CONTROL_PORT, control),
                serve(DNS_PORT, move |s| dns_channel(s, dns.clone())),
                dns_stub(stub),
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
    let port = parse_forward_header(&line).with_context(|| format!("bad forward header {line:?}"))?;
    let mut tcp = TcpStream::connect(("127.0.0.1", port))
        .await
        .with_context(|| format!("connect 127.0.0.1:{port}"))?;
    tcp.set_nodelay(true)?;
    copy_bidirectional(&mut vsock, &mut tcp).await?;
    Ok(())
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
    /// Our query id -> (client's id, client address, sent at).
    pending: Mutex<HashMap<u16, (u16, SocketAddr, Instant)>>,
    next_id: AtomicU16,
}

async fn dns_stub(dns: Arc<Dns>) -> Result<()> {
    let mut buf = [0u8; 4096];
    loop {
        let (n, from) = dns.sock.recv_from(&mut buf).await?;
        let Some(tx) = dns.writer.lock().unwrap().clone() else { continue };
        if n < 12 {
            continue;
        }
        // Rewrite ids so queries from different clients can't collide.
        let id = dns.next_id.fetch_add(1, Ordering::Relaxed);
        let original = u16::from_be_bytes([buf[0], buf[1]]);
        {
            let mut pending = dns.pending.lock().unwrap();
            if pending.len() > 4096 {
                pending.retain(|_, (_, _, at)| at.elapsed() < Duration::from_secs(10));
            }
            pending.insert(id, (original, from, Instant::now()));
        }
        let mut packet = buf[..n].to_vec();
        packet[..2].copy_from_slice(&id.to_be_bytes());
        if let Some(frame) = dns_frame(&packet) {
            let _ = tx.send(frame);
        }
    }
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
            let mut len = [0u8; 2];
            rd.read_exact(&mut len).await?;
            let mut packet = vec![0u8; u16::from_be_bytes(len) as usize];
            rd.read_exact(&mut packet).await?;
            if packet.len() < 12 {
                continue;
            }
            let id = u16::from_be_bytes([packet[0], packet[1]]);
            let Some((original, client, _)) = dns.pending.lock().unwrap().remove(&id) else { continue };
            packet[..2].copy_from_slice(&original.to_be_bytes());
            let _ = dns.sock.send_to(&packet, client).await;
        }
        #[allow(unreachable_code)]
        anyhow::Ok(())
    }
    .await;
    writer.abort();
    result
}
