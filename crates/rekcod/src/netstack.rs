//! Sandbox VM network on the Mac (docs/sandbox-network.md).
//!
//! A sandbox VM's network card is one end of a `socketpair(AF_UNIX, SOCK_DGRAM)`, one
//! datagram per Ethernet frame. This module is the other end: smoltcp plays the guest's
//! gateway with `any_ip`, so every TCP connection the guest opens ends here, and the ones
//! [`Policy`] allows are relayed over a host socket. UDP is relayed per flow without
//! smoltcp. ICMP, IPv6, IP fragments and other protocols are dropped. The guest can do what
//! it likes to its own firewall; nothing leaves the VM any other way.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::socket::tcp;
use smoltcp::wire::{
    EthernetAddress, EthernetFrame, EthernetProtocol, HardwareAddress, IpAddress, IpCidr, IpListenEndpoint, IpProtocol, Ipv4Packet,
    TcpPacket, UdpPacket,
};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::mpsc::error::{TryRecvError, TrySendError};
use tokio::sync::{Notify, mpsc};

pub const GATEWAY: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);
pub const GUEST: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 15);
/// boot.sh configures eth0 from this instead of running udhcpc.
pub const CMDLINE: &str = "rekcod.net=10.0.2.15/24,10.0.2.2";
const GATEWAY_MAC: [u8; 6] = [0x02, 0x72, 0x6b, 0x00, 0x02, 0x02];
const MTU: usize = 1500;
/// Per direction, per connection. smoltcp scales the window to fit.
const TCP_BUFFER: usize = 256 << 10;
const CHUNK: usize = 64 << 10;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const UDP_IDLE: Duration = Duration::from_secs(60);
/// Bytes of a gated connection (TLS, HTTP) held while looking for the name, and how long.
const PEEK_MAX: usize = 16 << 10;
const PEEK_TIMEOUT: Duration = Duration::from_secs(10);
/// Frames waiting for room in the VM's receive buffer; past this smoltcp holds off.
const OUT_QUEUE_MAX: usize = 4096;
/// How long an address from a DNS answer stays usable past the record's TTL: programs
/// cache lookups and keep connection pools longer than they should.
const RESOLVED_GRACE: Duration = Duration::from_secs(600);

pub type Logger = Arc<dyn Fn(String) + Send + Sync>;

/// Addresses the daemon's DNS bridge handed this VM for allowed names, until they expire.
#[derive(Default)]
pub struct Resolved(Mutex<HashMap<Ipv4Addr, Instant>>);

impl Resolved {
    pub fn record(&self, ip: Ipv4Addr, ttl: u32) {
        self.record_until(ip, Instant::now() + Duration::from_secs(ttl.min(86_400).into()) + RESOLVED_GRACE);
    }

    fn record_until(&self, ip: Ipv4Addr, until: Instant) {
        let mut m = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if m.len() > 4096 {
            let now = Instant::now();
            m.retain(|_, t| *t > now);
        }
        let t = m.entry(ip).or_insert(until);
        *t = (*t).max(until);
    }

    pub fn contains(&self, ip: Ipv4Addr) -> bool {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).get(&ip).is_some_and(|t| *t > Instant::now())
    }
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Allow,
    /// Allowed by address; the TLS ClientHello's SNI must name an allowed host too.
    Tls,
    /// Allowed by address; the HTTP Host header must name an allowed host too.
    Http,
    Refuse(&'static str),
}

pub struct Policy {
    /// Allowed names (`rekcod_proto::host_allowed` patterns); None is the open internet.
    pub allow: Option<Vec<String>>,
    /// TCP `IP:PORT`s on the Mac or LAN the VM may reach anyway (`--reach`).
    pub reach: Vec<SocketAddrV4>,
    pub resolved: Arc<Resolved>,
    /// Tests only: 127.0.0.0/8 counts as the internet.
    loopback: bool,
    /// The main VM on `--network user`: everything a NAT would reach, the LAN included.
    everything: bool,
    tls_ports: Vec<u16>,
    http_ports: Vec<u16>,
    /// The Mac's own interface addresses, re-read every 10 s.
    local: Mutex<Option<(Instant, Vec<Ipv4Addr>)>>,
}

impl Policy {
    pub fn new(allow: Option<Vec<String>>, reach: Vec<SocketAddrV4>, resolved: Arc<Resolved>) -> Policy {
        Policy {
            allow: allow.filter(|a| !a.iter().any(|h| h == "*")),
            reach,
            resolved,
            loopback: false,
            everything: false,
            tls_ports: vec![443],
            http_ports: vec![80],
            local: Mutex::default(),
        }
    }

    fn local_addrs(&self) -> Vec<Ipv4Addr> {
        let mut l = self.local.lock().unwrap_or_else(PoisonError::into_inner);
        match &*l {
            Some((at, addrs)) if at.elapsed() < Duration::from_secs(10) => addrs.clone(),
            _ => {
                let addrs = local_ipv4s();
                *l = Some((Instant::now(), addrs.clone()));
                addrs
            }
        }
    }

    /// The main VM's policy for `--network user`: no filtering, as with Virtualization.
    /// framework's NAT, but the connections are the daemon's own, so a VPN that drops
    /// forwarded VM traffic (Mullvad, other full-tunnel clients) still carries them.
    pub fn everything() -> Policy {
        Policy { everything: true, ..Policy::new(None, vec![], Arc::default()) }
    }

    pub fn check(&self, tcp: bool, dst: SocketAddrV4) -> Verdict {
        if self.everything {
            // What NAT reaches, which never includes the Mac's loopback: services there
            // (the dashboard, anything bound to 127.0.0.1) are reachable only through
            // host.docker.internal, which refuses the dashboard's port. A container with
            // NET_RAW could otherwise address 127.0.0.1 straight out of eth0.
            let ip = *dst.ip();
            if ip.is_loopback() || ip.is_unspecified() {
                return Verdict::Refuse("the Mac's loopback (use host.docker.internal)");
            }
            return Verdict::Allow;
        }
        if tcp && self.reach.contains(&dst) {
            return Verdict::Allow;
        }
        // DNS goes over vsock to the daemon, which checks names; a direct resolver would not.
        if dst.port() == 53 {
            return Verdict::Refuse("DNS goes through rekcod");
        }
        let ip = *dst.ip();
        if !(self.loopback && ip.is_loopback()) && (not_internet(ip) || self.local_addrs().contains(&ip)) {
            return Verdict::Refuse("the Mac or local network");
        }
        if self.allow.is_none() {
            return Verdict::Allow;
        }
        if !self.resolved.contains(ip) {
            return Verdict::Refuse("not from an allowed name");
        }
        match dst.port() {
            // QUIC: its ClientHello is in encrypted Initial packets; clients fall back to TCP.
            p if !tcp && self.tls_ports.contains(&p) => Verdict::Refuse("QUIC can't be checked against the allowlist"),
            p if tcp && self.tls_ports.contains(&p) => Verdict::Tls,
            p if tcp && self.http_ports.contains(&p) => Verdict::Http,
            _ => Verdict::Allow,
        }
    }

    fn name_allowed(&self, name: &str) -> bool {
        self.allow.as_ref().is_none_or(|a| rekcod_proto::host_allowed(a, name))
    }
}

/// Loopback, RFC 1918, CGNAT (Tailscale), link-local (cloud metadata), 0/8, multicast,
/// broadcast and 240/4.
fn not_internet(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || o[0] == 0
        || o[0] >= 240
        || (o[0] == 100 && o[1] & 0xc0 == 64)
}

/// IPv4 addresses on the Mac's interfaces (getifaddrs).
pub fn local_ipv4s() -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut ifap) } != 0 {
        return out;
    }
    let mut p = ifap;
    while !p.is_null() {
        let ifa = unsafe { &*p };
        if !ifa.ifa_addr.is_null() && i32::from(unsafe { (*ifa.ifa_addr).sa_family }) == libc::AF_INET {
            let sin = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
            out.push(Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)));
        }
        p = ifa.ifa_next;
    }
    unsafe { libc::freeifaddrs(ifap) };
    out
}

/// The VM's end and the daemon's end of a new network card.
pub fn socketpair() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let pair = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in [&pair.0, &pair.1] {
        unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
        // Apple's guidance for this attachment: receive buffer at least twice the send buffer.
        let s = socket2::SockRef::from(fd);
        s.set_send_buffer_size(1 << 20)?;
        s.set_recv_buffer_size(4 << 20)?;
    }
    Ok(pair)
}

pub struct Handle(tokio::task::JoinHandle<()>);

impl Drop for Handle {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Serves the network card whose daemon end is `fd` until the handle is dropped.
pub fn start(fd: OwnedFd, policy: Arc<Policy>, log: Logger) -> io::Result<Handle> {
    socket2::SockRef::from(&fd).set_nonblocking(true)?;
    let fd = AsyncFd::new(fd)?;
    let (udp_tx, udp_rx) = mpsc::channel(1024);
    let mut dev = Dev::default();
    let mut cfg = Config::new(HardwareAddress::Ethernet(EthernetAddress(GATEWAY_MAC)));
    cfg.random_seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos() as u64;
    let mut iface = Interface::new(cfg, &mut dev, smoltcp::time::Instant::now());
    iface.update_ip_addrs(|a| {
        let _ = a.push(IpCidr::new(IpAddress::Ipv4(GATEWAY), 24));
    });
    // With any_ip, a route whose gateway is our own address makes every destination ours.
    let _ = iface.routes_mut().add_default_ipv4_route(GATEWAY);
    iface.set_any_ip(true);
    let stack = Stack {
        iface,
        sockets: SocketSet::new(vec![]),
        dev,
        conns: HashMap::new(),
        next_id: 0,
        flows: HashMap::new(),
        guest_mac: None,
        policy,
        log,
        notify: Arc::new(Notify::new()),
        udp_tx,
        ident: 0,
        logged: HashMap::new(),
    };
    Ok(Handle(tokio::spawn(stack.run(fd, udp_rx))))
}

#[derive(Default)]
struct Dev {
    rx: Option<Vec<u8>>,
    out: VecDeque<Vec<u8>>,
}

struct Rx(Vec<u8>);
struct Tx<'a>(&'a mut VecDeque<Vec<u8>>);

impl phy::RxToken for Rx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl phy::TxToken for Tx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut frame = vec![0; len];
        let r = f(&mut frame);
        self.0.push_back(frame);
        r
    }
}

impl Device for Dev {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx<'a>;

    fn receive(&mut self, _: smoltcp::time::Instant) -> Option<(Rx, Tx<'_>)> {
        let frame = self.rx.take()?;
        Some((Rx(frame), Tx(&mut self.out)))
    }

    fn transmit(&mut self, _: smoltcp::time::Instant) -> Option<Tx<'_>> {
        (self.out.len() < OUT_QUEUE_MAX).then_some(Tx(&mut self.out))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = MTU + 14;
        caps
    }
}

enum Msg {
    /// The host task's first message when its connect succeeded.
    Connected,
    Data(Vec<u8>),
    Eof,
    Reset,
}

#[derive(Clone, Copy)]
enum GateKind {
    Tls,
    Http,
}

struct Gate {
    kind: GateKind,
    buf: Vec<u8>,
    deadline: Instant,
}

/// One guest TCP connection and the host task relaying it.
struct Conn {
    dst: SocketAddrV4,
    guest_port: u16,
    /// The guest's SYN, held until the host connect succeeds (then a listener takes it)
    /// or fails (then nothing does, and smoltcp answers with a reset).
    syn: Option<Vec<u8>>,
    handle: Option<SocketHandle>,
    to_host: mpsc::Sender<Msg>,
    from_host: mpsc::Receiver<Msg>,
    /// Host bytes smoltcp hasn't taken yet.
    pending: Vec<u8>,
    sent: usize,
    host_eof: bool,
    /// The host task was told the guest side ended (Eof or Reset).
    told_host: bool,
    established: bool,
    /// Seen Closed once: removed next round, after a poll has sent any reset.
    closed: bool,
    gate: Option<Gate>,
}

impl Conn {
    /// Moves bytes between the socket and the host task. Returns a denial to log, if any.
    fn service(&mut self, s: &mut tcp::Socket, policy: &Policy) -> Option<String> {
        use tcp::State;
        if matches!(s.state(), State::Listen | State::SynReceived) {
            // Host bytes wait in the channel until the handshake is done.
            return None;
        }
        if s.state() != State::Closed {
            self.established = true;
        }
        // Host to guest.
        loop {
            if self.sent == self.pending.len() {
                if self.host_eof {
                    break;
                }
                match self.from_host.try_recv() {
                    Ok(Msg::Connected) => continue,
                    Ok(Msg::Data(d)) => (self.pending, self.sent) = (d, 0),
                    Ok(Msg::Eof) => {
                        self.host_eof = true;
                        break;
                    }
                    Ok(Msg::Reset) | Err(TryRecvError::Disconnected) => {
                        s.abort();
                        self.told_host = true;
                        return None;
                    }
                    Err(TryRecvError::Empty) => break,
                }
            }
            if !s.may_send() {
                break;
            }
            match s.send_slice(&self.pending[self.sent..]) {
                Ok(n) if n > 0 => self.sent += n,
                _ => break,
            }
        }
        if self.host_eof && self.sent == self.pending.len() && s.may_send() {
            s.close();
        }
        // Guest to host.
        if let Some(g) = &mut self.gate {
            while g.buf.len() < PEEK_MAX && s.can_recv() {
                let room = PEEK_MAX - g.buf.len();
                let _ = s.recv(|b| {
                    let n = b.len().min(room);
                    g.buf.extend_from_slice(&b[..n]);
                    (n, ())
                });
            }
            let (what, peek) = match g.kind {
                GateKind::Tls => ("tls", tls_sni(&g.buf)),
                GateKind::Http => ("http", http_host(&g.buf)),
            };
            let name = match peek {
                Peek::More if g.buf.len() < PEEK_MAX && Instant::now() < g.deadline && s.may_recv() => return None,
                Peek::Name(n) if policy.name_allowed(&n) => None,
                Peek::Name(n) => Some(n),
                Peek::More | Peek::NoName => Some(format!("(no {})", if what == "tls" { "SNI" } else { "Host" })),
            };
            if let Some(name) = name {
                s.abort();
                let _ = self.to_host.try_send(Msg::Reset);
                self.told_host = true;
                return Some(format!("refused {what} {name} at {}", self.dst.ip()));
            }
            // Nothing went to the host yet, so the channel has room.
            let _ = self.to_host.try_send(Msg::Data(std::mem::take(&mut g.buf)));
            self.gate = None;
        }
        if self.told_host {
            return None;
        }
        while s.can_recv() {
            let Ok(permit) = self.to_host.try_reserve() else { break };
            if let Ok(d) = s.recv(|b| {
                let n = b.len().min(CHUNK);
                (n, b[..n].to_vec())
            }) {
                permit.send(Msg::Data(d));
            }
        }
        if self.established && !s.may_recv() && s.recv_queue() == 0 {
            let msg = if s.state() == State::Closed { Msg::Reset } else { Msg::Eof };
            self.told_host = self.to_host.try_send(msg).is_ok();
        }
        None
    }
}

type UdpBack = (u16, SocketAddrV4, Vec<u8>);

struct Stack {
    iface: Interface,
    sockets: SocketSet<'static>,
    dev: Dev,
    conns: HashMap<u64, Conn>,
    next_id: u64,
    /// (guest port, destination) -> the host task's inbox.
    flows: HashMap<(u16, SocketAddrV4), mpsc::Sender<Vec<u8>>>,
    guest_mac: Option<EthernetAddress>,
    policy: Arc<Policy>,
    log: Logger,
    /// Host tasks poke this after moving data, so the loop services their connection.
    notify: Arc<Notify>,
    udp_tx: mpsc::Sender<UdpBack>,
    ident: u16,
    /// Denials logged recently, so a retry loop doesn't flood daemon.log.
    logged: HashMap<String, Instant>,
}

fn now() -> smoltcp::time::Instant {
    smoltcp::time::Instant::now()
}

impl Stack {
    async fn run(mut self, fd: AsyncFd<OwnedFd>, mut udp_rx: mpsc::Receiver<UdpBack>) {
        let raw = fd.as_raw_fd();
        let notify = self.notify.clone();
        let mut buf = vec![0u8; 65536];
        let mut frames: Vec<Vec<u8>> = Vec::new();
        let mut back: Vec<UdpBack> = Vec::new();
        loop {
            self.iface.poll(now(), &mut self.dev, &mut self.sockets);
            self.service();
            self.iface.poll(now(), &mut self.dev, &mut self.sockets);
            let blocked = self.flush(raw);
            let delay = if blocked {
                Duration::from_millis(1)
            } else {
                self.iface.poll_delay(now(), &self.sockets).map_or(Duration::from_secs(1), |d| Duration::from_micros(d.total_micros()))
            }
            .min(Duration::from_secs(1));
            tokio::select! {
                r = fd.readable() => {
                    let Ok(mut guard) = r else { return };
                    while frames.len() < 256 {
                        match guard.try_io(|f| recv(f.get_ref().as_raw_fd(), &mut buf)) {
                            Ok(Ok(0)) => {}
                            Ok(Ok(n)) => frames.push(buf[..n].to_vec()),
                            Ok(Err(e)) => {
                                (self.log)(format!("rekcod-egress: network card closed: {e}"));
                                return;
                            }
                            Err(_would_block) => break,
                        }
                    }
                }
                Some(d) = udp_rx.recv() => back.push(d),
                _ = notify.notified() => {}
                _ = tokio::time::sleep(delay) => {}
            }
            for f in frames.drain(..) {
                self.ingress(f);
            }
            while let Ok(d) = udp_rx.try_recv() {
                back.push(d);
            }
            for (port, from, data) in back.drain(..) {
                self.udp_back(port, from, &data);
            }
        }
    }

    fn deny(&mut self, what: String) {
        let now = Instant::now();
        if self.logged.len() > 1024 {
            self.logged.retain(|_, t| now.duration_since(*t) < Duration::from_secs(10));
        }
        if self.logged.get(&what).is_some_and(|t| now.duration_since(*t) < Duration::from_secs(10)) {
            return;
        }
        (self.log)(format!("rekcod-egress: {what}"));
        self.logged.insert(what, now);
    }

    /// Sends queued frames to the VM; true if its buffer is full and some are left.
    fn flush(&mut self, fd: RawFd) -> bool {
        while let Some(f) = self.dev.out.front() {
            let n = unsafe { libc::send(fd, f.as_ptr().cast(), f.len(), 0) };
            if n < 0 && matches!(io::Error::last_os_error().raw_os_error(), Some(libc::EAGAIN | libc::ENOBUFS)) {
                return true;
            }
            self.dev.out.pop_front();
        }
        false
    }

    fn service(&mut self) {
        // Connections whose host connect finished: accept or reset the held SYN.
        let (mut connected, mut failed) = (Vec::new(), Vec::new());
        for (id, c) in self.conns.iter_mut().filter(|(_, c)| c.handle.is_none()) {
            match c.from_host.try_recv() {
                Ok(Msg::Connected) => connected.push(*id),
                Err(TryRecvError::Empty) => {}
                Ok(_) | Err(TryRecvError::Disconnected) => failed.push(*id),
            }
        }
        for id in connected {
            self.accept(id);
        }
        for id in failed {
            if let Some(syn) = self.conns.remove(&id).and_then(|c| c.syn) {
                self.inject(syn);
            }
        }
        let mut done = Vec::new();
        let mut denials = Vec::new();
        for (id, c) in self.conns.iter_mut() {
            let Some(h) = c.handle else { continue };
            let s = self.sockets.get_mut::<tcp::Socket>(h);
            if let Some(d) = c.service(s, &self.policy) {
                denials.push(d);
            }
            match s.state() {
                tcp::State::TimeWait => done.push(*id),
                tcp::State::Closed if c.closed => done.push(*id),
                tcp::State::Closed => c.closed = true,
                _ => {}
            }
        }
        for id in done {
            self.drop_conn(id);
        }
        for d in denials {
            self.deny(d);
        }
        if self.flows.len() > 256 {
            self.flows.retain(|_, tx| !tx.is_closed());
        }
    }

    fn drop_conn(&mut self, id: u64) {
        let Some(c) = self.conns.remove(&id) else { return };
        if !c.told_host {
            let _ = c.to_host.try_send(Msg::Reset);
        }
        if let Some(h) = c.handle {
            self.sockets.remove(h);
        }
    }

    fn inject(&mut self, frame: Vec<u8>) {
        self.dev.rx = Some(frame);
        self.iface.poll_ingress_single(now(), &mut self.dev, &mut self.sockets);
    }

    /// The host side is connected: a listener on exactly the guest's destination takes the
    /// held SYN.
    fn accept(&mut self, id: u64) {
        let Some(c) = self.conns.get_mut(&id) else { return };
        let mut s = tcp::Socket::new(tcp::SocketBuffer::new(vec![0; TCP_BUFFER]), tcp::SocketBuffer::new(vec![0; TCP_BUFFER]));
        s.set_nagle_enabled(false);
        s.set_ack_delay(None);
        // Probes a guest that went quiet, so a vanished connection doesn't hold a socket forever.
        s.set_keep_alive(Some(smoltcp::time::Duration::from_secs(60)));
        s.set_timeout(Some(smoltcp::time::Duration::from_secs(180)));
        if s.listen(IpListenEndpoint { addr: Some(IpAddress::Ipv4(*c.dst.ip())), port: c.dst.port() }).is_err() {
            return self.drop_conn(id);
        }
        let h = self.sockets.add(s);
        c.handle = Some(h);
        if let Some(g) = &mut c.gate {
            g.deadline = Instant::now() + PEEK_TIMEOUT;
        }
        if let Some(syn) = c.syn.take() {
            self.inject(syn);
        }
        if self.sockets.get::<tcp::Socket>(h).state() == tcp::State::Listen {
            self.drop_conn(id);
        }
    }

    fn ingress(&mut self, frame: Vec<u8>) {
        let Ok(eth) = EthernetFrame::new_checked(&frame[..]) else { return };
        if eth.ethertype() == EthernetProtocol::Ipv4 {
            let Ok(ip) = Ipv4Packet::new_checked(eth.payload()) else { return };
            if ip.src_addr() != GUEST || ip.more_frags() || ip.frag_offset() != 0 || !ip.verify_checksum() {
                return;
            }
            if eth.src_addr().is_unicast() {
                self.guest_mac = Some(eth.src_addr());
            }
            let dst = ip.dst_addr();
            match ip.next_header() {
                IpProtocol::Udp => {
                    let Ok(udp) = UdpPacket::new_checked(ip.payload()) else { return };
                    if udp.verify_checksum(&IpAddress::Ipv4(GUEST), &IpAddress::Ipv4(dst)) {
                        let (port, to, data) = (udp.src_port(), SocketAddrV4::new(dst, udp.dst_port()), udp.payload().to_vec());
                        self.udp_out(port, to, data);
                    }
                    return;
                }
                IpProtocol::Tcp => {
                    let Ok(tcp) = TcpPacket::new_checked(ip.payload()) else { return };
                    if tcp.syn() && !tcp.ack() {
                        let (to, port) = (SocketAddrV4::new(dst, tcp.dst_port()), tcp.src_port());
                        return self.syn(to, port, frame);
                    }
                }
                _ => return,
            }
        } else if eth.ethertype() != EthernetProtocol::Arp {
            return;
        }
        self.dev.rx = Some(frame);
        self.iface.poll_ingress_single(now(), &mut self.dev, &mut self.sockets);
    }

    /// A guest SYN. Allowed: a host task connects to the real destination while the SYN
    /// waits ([`Stack::accept`]). Refused: smoltcp gets it with no listener and answers RST.
    fn syn(&mut self, dst: SocketAddrV4, guest_port: u16, frame: Vec<u8>) {
        if let Some(c) = self.conns.values().find(|c| c.dst == dst && c.guest_port == guest_port) {
            // A retransmit: dropped while the host connects, smoltcp's once it has a socket.
            if c.handle.is_some() {
                self.inject(frame);
            }
            return;
        }
        let kind = match self.policy.check(true, dst) {
            Verdict::Refuse(why) => {
                self.deny(format!("refused tcp {dst} ({why})"));
                return self.inject(frame);
            }
            Verdict::Allow => None,
            Verdict::Tls => Some(GateKind::Tls),
            Verdict::Http => Some(GateKind::Http),
        };
        let (to_host, host_rx) = mpsc::channel(8);
        let (host_tx, from_host) = mpsc::channel(8);
        tokio::spawn(host_tcp(dst, host_rx, host_tx, self.notify.clone()));
        let gate = kind.map(|kind| Gate { kind, buf: Vec::new(), deadline: Instant::now() });
        let conn = Conn {
            dst,
            guest_port,
            syn: Some(frame),
            handle: None,
            to_host,
            from_host,
            pending: Vec::new(),
            sent: 0,
            host_eof: false,
            told_host: false,
            established: false,
            closed: false,
            gate,
        };
        self.next_id += 1;
        self.conns.insert(self.next_id, conn);
    }

    fn udp_out(&mut self, guest_port: u16, dst: SocketAddrV4, data: Vec<u8>) {
        let key = (guest_port, dst);
        let data = match self.flows.get(&key) {
            None => data,
            Some(tx) => match tx.try_send(data) {
                Ok(()) | Err(TrySendError::Full(_)) => return,
                Err(TrySendError::Closed(d)) => d,
            },
        };
        self.flows.remove(&key);
        if let Verdict::Refuse(why) = self.policy.check(false, dst) {
            return self.deny(format!("refused udp {dst} ({why})"));
        }
        let (tx, rx) = mpsc::channel(64);
        let _ = tx.try_send(data);
        tokio::spawn(host_udp(dst, guest_port, rx, self.udp_tx.clone()));
        self.flows.insert(key, tx);
    }

    /// A datagram from `from` for the guest's `port`, as one or more IPv4 frames.
    fn udp_back(&mut self, port: u16, from: SocketAddrV4, data: &[u8]) {
        let Some(mac) = self.guest_mac else { return };
        if data.len() > 65_507 {
            return;
        }
        let mut l4 = vec![0u8; 8 + data.len()];
        {
            let mut u = UdpPacket::new_unchecked(&mut l4[..]);
            u.set_src_port(from.port());
            u.set_dst_port(port);
            u.set_len((8 + data.len()) as u16);
            u.set_checksum(0);
            u.payload_mut()[..data.len()].copy_from_slice(data);
            u.fill_checksum(&IpAddress::Ipv4(*from.ip()), &IpAddress::Ipv4(GUEST));
        }
        self.ident = self.ident.wrapping_add(1);
        for f in ipv4_frames(mac, *from.ip(), IpProtocol::Udp, self.ident, &l4) {
            if self.dev.out.len() < OUT_QUEUE_MAX {
                self.dev.out.push_back(f);
            }
        }
    }
}

/// `payload` from `src` to the guest, fragmented to the MTU.
fn ipv4_frames(mac: EthernetAddress, src: Ipv4Addr, proto: IpProtocol, ident: u16, payload: &[u8]) -> Vec<Vec<u8>> {
    let max = (MTU - 20) & !7;
    let mut out = Vec::new();
    let mut off = 0;
    loop {
        let end = (off + max).min(payload.len());
        let chunk = &payload[off..end];
        let mut f = vec![0u8; 14 + 20 + chunk.len()];
        let mut eth = EthernetFrame::new_unchecked(&mut f[..]);
        eth.set_src_addr(EthernetAddress(GATEWAY_MAC));
        eth.set_dst_addr(mac);
        eth.set_ethertype(EthernetProtocol::Ipv4);
        let mut ip = Ipv4Packet::new_unchecked(eth.payload_mut());
        ip.set_version(4);
        ip.set_header_len(20);
        ip.set_dscp(0);
        ip.set_ecn(0);
        ip.set_total_len((20 + chunk.len()) as u16);
        ip.set_ident(ident);
        ip.clear_flags();
        ip.set_more_frags(end < payload.len());
        ip.set_frag_offset(off as u16);
        ip.set_hop_limit(64);
        ip.set_next_header(proto);
        ip.set_src_addr(src);
        ip.set_dst_addr(GUEST);
        ip.fill_checksum();
        ip.payload_mut()[..chunk.len()].copy_from_slice(chunk);
        out.push(f);
        off = end;
        if off >= payload.len() {
            return out;
        }
    }
}

fn recv(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
    let n = unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
    if n < 0 { Err(io::Error::last_os_error()) } else { Ok(n as usize) }
}

/// Connects to `dst` and relays until both sides are done. A reset from either side
/// resets the other (SO_LINGER 0 on the host socket).
async fn host_tcp(dst: SocketAddrV4, mut rx: mpsc::Receiver<Msg>, tx: mpsc::Sender<Msg>, notify: Arc<Notify>) {
    let stream = match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(dst)).await {
        Ok(Ok(s)) => s,
        _ => {
            let _ = tx.send(Msg::Reset).await;
            notify.notify_one();
            return;
        }
    };
    let _ = stream.set_nodelay(true);
    if tx.send(Msg::Connected).await.is_err() {
        return;
    }
    notify.notify_one();
    let (mut rd, mut wr) = stream.into_split();
    let up = async {
        loop {
            let m = rx.recv().await;
            notify.notify_one();
            match m {
                Some(Msg::Data(d)) => {
                    if wr.write_all(&d).await.is_err() {
                        return Err(());
                    }
                }
                Some(Msg::Eof) => {
                    let _ = wr.shutdown().await;
                    return Ok(());
                }
                Some(Msg::Connected) => {}
                Some(Msg::Reset) | None => {
                    let _ = socket2::SockRef::from(wr.as_ref()).set_linger(Some(Duration::ZERO));
                    return Err(());
                }
            }
        }
    };
    let down = async {
        let mut buf = vec![0u8; CHUNK];
        loop {
            let (msg, end) = match rd.read(&mut buf).await {
                Ok(0) => (Msg::Eof, Some(Ok(()))),
                Ok(n) => (Msg::Data(buf[..n].to_vec()), None),
                Err(_) => (Msg::Reset, Some(Err(()))),
            };
            let sent = tx.send(msg).await;
            notify.notify_one();
            if sent.is_err() {
                return Err(());
            }
            if let Some(end) = end {
                return end;
            }
        }
    };
    tokio::pin!(up, down);
    let (mut up_done, mut down_done) = (false, false);
    while !(up_done && down_done) {
        tokio::select! {
            r = &mut up, if !up_done => match r { Ok(()) => up_done = true, Err(()) => return },
            r = &mut down, if !down_done => match r { Ok(()) => down_done = true, Err(()) => return },
        }
    }
}

/// One UDP flow: a host socket connected to `dst`, closed after [`UDP_IDLE`].
async fn host_udp(dst: SocketAddrV4, guest_port: u16, mut rx: mpsc::Receiver<Vec<u8>>, back: mpsc::Sender<UdpBack>) {
    let Ok(sock) = UdpSocket::bind("0.0.0.0:0").await else { return };
    if sock.connect(dst).await.is_err() {
        return;
    }
    let mut buf = vec![0u8; 65536];
    loop {
        tokio::select! {
            m = rx.recv() => match m {
                Some(d) => { let _ = sock.send(&d).await; }
                None => return,
            },
            r = sock.recv(&mut buf) => {
                // Errors are ICMP unreachables for an earlier datagram; the guest times out.
                if let Ok(n) = r && back.send((guest_port, dst, buf[..n].to_vec())).await.is_err() {
                    return;
                }
            }
            _ = tokio::time::sleep(UDP_IDLE) => return,
        }
    }
}

#[derive(Debug, PartialEq)]
enum Peek {
    More,
    Name(String),
    /// Complete, or malformed, and names no host.
    NoName,
}

fn u16_at(b: &[u8], i: usize) -> Option<usize> {
    Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]) as usize)
}

fn hostname(b: &[u8]) -> Peek {
    match std::str::from_utf8(b) {
        Ok(s) if !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphanumeric() || b"-._".contains(&c)) => Peek::Name(s.to_ascii_lowercase()),
        _ => Peek::NoName,
    }
}

/// The server name in a TLS ClientHello at the start of `buf`.
fn tls_sni(buf: &[u8]) -> Peek {
    // A handshake message may span records: gather the handshake bytes that have arrived.
    let mut hs = Vec::new();
    let mut i = 0;
    while let Some(h) = buf.get(i..i + 5) {
        if h[0] != 0x16 {
            return Peek::NoName;
        }
        let len = u16::from_be_bytes([h[3], h[4]]) as usize;
        let body = &buf[i + 5..buf.len().min(i + 5 + len)];
        hs.extend_from_slice(body);
        if body.len() < len {
            break;
        }
        i += 5 + len;
    }
    let complete = hs.len() >= 4 && hs.len() >= 4 + (u32::from_be_bytes([0, hs[1], hs[2], hs[3]]) as usize);
    match client_hello_sni(&hs) {
        Some(p) => p,
        None if complete => Peek::NoName,
        None => Peek::More,
    }
}

/// None: `hs` ends before the answer.
fn client_hello_sni(hs: &[u8]) -> Option<Peek> {
    if *hs.first()? != 1 {
        return Some(Peek::NoName);
    }
    let mut p = 4 + 2 + 32;
    p += 1 + *hs.get(p)? as usize; // session id
    p += 2 + u16_at(hs, p)?; // cipher suites
    p += 1 + *hs.get(p)? as usize; // compression methods
    let end = p + 2 + u16_at(hs, p)?;
    p += 2;
    while p + 4 <= end {
        let (kind, len) = (u16_at(hs, p)?, u16_at(hs, p + 2)?);
        p += 4;
        if kind == 0 {
            let ext = hs.get(p..p + len)?;
            // server_name_list: u16 length, then (type, u16 length, name); type 0 is a host name.
            if ext.get(2) != Some(&0) {
                return Some(Peek::NoName);
            }
            let n = u16_at(ext, 3).unwrap_or(usize::MAX);
            return Some(ext.get(5..5usize.saturating_add(n)).map_or(Peek::NoName, hostname));
        }
        p += len;
    }
    if hs.len() < end { None } else { Some(Peek::NoName) }
}

/// The Host header of an HTTP/1 request at the start of `buf`, without the port.
fn http_host(buf: &[u8]) -> Peek {
    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        return if buf.len() >= PEEK_MAX { Peek::NoName } else { Peek::More };
    };
    let Ok(head) = std::str::from_utf8(&buf[..end]) else { return Peek::NoName };
    let mut lines = head.split("\r\n");
    if !lines.next().is_some_and(|l| l.split(' ').nth(2).is_some_and(|v| v.starts_with("HTTP/1."))) {
        return Peek::NoName;
    }
    let hosts: Vec<&str> =
        lines.filter_map(|l| l.split_once(':')).filter(|(k, _)| k.trim().eq_ignore_ascii_case("host")).map(|(_, v)| v.trim()).collect();
    let [host] = hosts[..] else { return Peek::NoName };
    let host = match host.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.bytes().all(|c| c.is_ascii_digit()) => h,
        _ => host,
    };
    hostname(host.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use smoltcp::socket::udp;

    /// The VM's side: a second smoltcp interface on the other end of the socketpair.
    struct Guest {
        iface: Interface,
        sockets: SocketSet<'static>,
        dev: Dev,
        fd: OwnedFd,
        next_port: u16,
    }

    impl Guest {
        fn new(fd: OwnedFd) -> Guest {
            socket2::SockRef::from(&fd).set_nonblocking(true).unwrap();
            let mut dev = Dev::default();
            let cfg = Config::new(HardwareAddress::Ethernet(EthernetAddress([0x02, 0, 0, 0, 0, 0x0f])));
            let mut iface = Interface::new(cfg, &mut dev, now());
            iface.update_ip_addrs(|a| a.push(IpCidr::new(IpAddress::Ipv4(GUEST), 24)).unwrap());
            iface.routes_mut().add_default_ipv4_route(GATEWAY).unwrap();
            Guest { iface, sockets: SocketSet::new(vec![]), dev, fd, next_port: 40000 }
        }

        fn step(&mut self) {
            let mut buf = vec![0u8; 65536];
            while let Ok(n) = recv(self.fd.as_raw_fd(), &mut buf) {
                self.dev.rx = Some(buf[..n].to_vec());
                self.iface.poll_ingress_single(now(), &mut self.dev, &mut self.sockets);
            }
            self.iface.poll(now(), &mut self.dev, &mut self.sockets);
            while let Some(f) = self.dev.out.pop_front() {
                unsafe { libc::send(self.fd.as_raw_fd(), f.as_ptr().cast(), f.len(), 0) };
            }
        }

        /// Steps until `f` returns Some, for up to 15 s.
        fn until<T>(&mut self, mut f: impl FnMut(&mut SocketSet<'static>) -> Option<T>) -> Option<T> {
            let end = Instant::now() + Duration::from_secs(15);
            while Instant::now() < end {
                self.step();
                if let Some(t) = f(&mut self.sockets) {
                    return Some(t);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            None
        }

        /// Connects to `to`, sends `msg`, half-closes and returns everything read back, or
        /// Err if the connection was reset.
        fn exchange(&mut self, to: SocketAddrV4, msg: &[u8]) -> Result<Vec<u8>, &'static str> {
            let mut s = tcp::Socket::new(tcp::SocketBuffer::new(vec![0; 65536]), tcp::SocketBuffer::new(vec![0; 65536]));
            s.set_ack_delay(None);
            self.next_port += 1;
            s.connect(self.iface.context(), (IpAddress::Ipv4(*to.ip()), to.port()), self.next_port).unwrap();
            let h = self.sockets.add(s);
            let (mut sent, mut got, mut was_open) = (0, Vec::new(), false);
            let done = self.until(|set| {
                let s = set.get_mut::<tcp::Socket>(h);
                was_open |= s.may_send() || s.may_recv();
                if s.state() == tcp::State::Closed && !was_open {
                    return Some(Err("reset"));
                }
                if sent < msg.len() && s.can_send() {
                    sent += s.send_slice(&msg[sent..]).unwrap();
                    if sent == msg.len() {
                        s.close();
                    }
                }
                while s.can_recv() {
                    s.recv(|b| {
                        got.extend_from_slice(b);
                        (b.len(), ())
                    })
                    .unwrap();
                }
                if was_open && !s.may_recv() && matches!(s.state(), tcp::State::Closed | tcp::State::TimeWait) {
                    return Some(if s.state() == tcp::State::Closed && got.is_empty() { Err("reset") } else { Ok(()) });
                }
                None
            });
            self.sockets.remove(h);
            done.unwrap_or(Err("timed out")).map(|()| got)
        }
    }

    fn net(policy: Policy) -> (Guest, Handle, Arc<Mutex<Vec<String>>>) {
        let (vm, ours) = socketpair().unwrap();
        let logs: Arc<Mutex<Vec<String>>> = Arc::default();
        let l = logs.clone();
        let handle = start(ours, Arc::new(policy), Arc::new(move |s| l.lock().unwrap().push(s))).unwrap();
        (Guest::new(vm), handle, logs)
    }

    fn with_loopback(mut p: Policy) -> Policy {
        p.loopback = true;
        p
    }

    /// Reads everything, writes it back, closes.
    async fn echo_server() -> SocketAddrV4 {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let std::net::SocketAddr::V4(addr) = l.local_addr().unwrap() else { unreachable!() };
        tokio::spawn(async move {
            while let Ok((mut c, _)) = l.accept().await {
                tokio::spawn(async move {
                    let mut all = Vec::new();
                    let _ = c.read_to_end(&mut all).await;
                    let _ = c.write_all(&all).await;
                });
            }
        });
        addr
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tcp_relays_both_ways_with_backpressure() {
        let to = echo_server().await;
        let (mut g, _h, _) = net(with_loopback(Policy::new(None, vec![], Arc::default())));
        // More than both sides' buffers, so the window has to close and reopen.
        let msg: Vec<u8> = (0..1_500_000u32).map(|i| (i % 251) as u8).collect();
        let got = tokio::task::spawn_blocking(move || g.exchange(to, &msg).map(|got| got == msg)).await.unwrap();
        assert_eq!(got, Ok(true));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn refused_syn_gets_a_reset_and_is_logged() {
        let to = echo_server().await;
        let (mut g, _h, logs) = net(Policy::new(None, vec![], Arc::default()));
        let got = tokio::task::spawn_blocking(move || g.exchange(to, b"hi")).await.unwrap();
        assert_eq!(got, Err("reset"));
        assert_eq!(logs.lock().unwrap().clone(), vec![format!("rekcod-egress: refused tcp {to} (the Mac or local network)")]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn host_connect_refused_resets_the_guest() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let std::net::SocketAddr::V4(to) = l.local_addr().unwrap() else { unreachable!() };
        drop(l);
        let (mut g, _h, logs) = net(with_loopback(Policy::new(None, vec![], Arc::default())));
        let got = tokio::task::spawn_blocking(move || g.exchange(to, b"hi")).await.unwrap();
        assert_eq!(got, Err("reset"));
        assert!(logs.lock().unwrap().is_empty(), "allowed, so nothing is logged");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn allowlist_needs_an_address_from_dns() {
        let to = echo_server().await;
        let resolved = Arc::new(Resolved::default());
        let (mut g, _h, logs) = net(with_loopback(Policy::new(Some(vec!["x.test".into()]), vec![], resolved.clone())));
        let got = tokio::task::spawn_blocking(move || {
            let before = g.exchange(to, b"hi");
            resolved.record(*to.ip(), 30);
            (before, g.exchange(to, b"hi"))
        })
        .await
        .unwrap();
        assert_eq!(got, (Err("reset"), Ok(b"hi".to_vec())));
        assert!(logs.lock().unwrap()[0].ends_with("(not from an allowed name)"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tls_needs_an_allowed_sni() {
        let to = echo_server().await;
        let hello = include_bytes!("../testdata/clienthello-curl.bin").to_vec();
        let no_sni = include_bytes!("../testdata/clienthello-no-sni.bin").to_vec();
        let run = move |allow: &str, msg: Vec<u8>| {
            let resolved = Arc::new(Resolved::default());
            resolved.record(*to.ip(), 30);
            let mut p = with_loopback(Policy::new(Some(vec![allow.into()]), vec![], resolved));
            p.tls_ports = vec![to.port()];
            let (mut g, h, logs) = net(p);
            let got = g.exchange(to, &msg);
            drop(h);
            (got, logs.lock().unwrap().clone())
        };
        let (h1, h2) = (hello.clone(), hello.clone());
        let rt = tokio::runtime::Handle::current();
        let out = tokio::task::spawn_blocking(move || {
            let _guard = rt.enter();
            (run("*.npmjs.org", h1), run("example.com", h2), run("*.npmjs.org", no_sni))
        })
        .await
        .unwrap();
        assert_eq!(out.0, (Ok(hello), vec![]));
        assert_eq!(out.1, (Err("reset"), vec!["rekcod-egress: refused tls registry.npmjs.org at 127.0.0.1".to_string()]));
        assert_eq!(out.2, (Err("reset"), vec!["rekcod-egress: refused tls (no SNI) at 127.0.0.1".to_string()]));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn udp_round_trip() {
        let echo = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let std::net::SocketAddr::V4(to) = echo.local_addr().unwrap() else { unreachable!() };
        tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            while let Ok((n, from)) = echo.recv_from(&mut buf).await {
                let _ = echo.send_to(&buf[..n], from).await;
            }
        });
        let (mut g, _h, _) = net(with_loopback(Policy::new(None, vec![], Arc::default())));
        let got = tokio::task::spawn_blocking(move || {
            let buffer = || udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0; 4096]);
            let mut s = udp::Socket::new(buffer(), buffer());
            s.bind(5000).unwrap();
            s.send_slice(b"ping", (IpAddress::Ipv4(*to.ip()), to.port())).unwrap();
            let h = g.sockets.add(s);
            g.until(|set| set.get_mut::<udp::Socket>(h).recv().ok().map(|(d, m)| (d.to_vec(), m.endpoint.port)))
        })
        .await
        .unwrap();
        assert_eq!(got, Some((b"ping".to_vec(), to.port())));
    }

    #[test]
    fn the_main_vm_policy_reaches_everything_nat_would() {
        let p = Policy::everything();
        for dst in ["1.1.1.1:443", "192.168.1.10:445", "10.0.0.5:5432", "8.8.8.8:53"] {
            assert!(matches!(p.check(true, dst.parse().unwrap()), Verdict::Allow), "{dst}");
        }
        // Never the Mac's loopback, which NAT doesn't reach either (the dashboard lives there).
        for dst in ["127.0.0.1:4500", "127.1.2.3:22", "0.0.0.0:80"] {
            assert!(matches!(p.check(true, dst.parse().unwrap()), Verdict::Refuse(_)), "{dst}");
            assert!(matches!(p.check(false, dst.parse().unwrap()), Verdict::Refuse(_)), "{dst}");
        }
        // Sandboxes keep refusing the LAN.
        let s = Policy::new(None, vec![], Arc::default());
        assert!(matches!(s.check(true, "192.168.1.10:445".parse().unwrap()), Verdict::Refuse(_)));
    }

    #[test]
    fn policy_keeps_the_mac_and_lan_out() {
        let p = Policy::new(None, vec!["192.168.1.5:9000".parse().unwrap()], Arc::default());
        let check = |a: &str| p.check(true, a.parse().unwrap());
        for a in [
            "127.0.0.1:80",
            "10.0.2.2:22",
            "10.1.2.3:443",
            "172.16.0.1:443",
            "192.168.1.5:9001",
            "169.254.169.254:80",
            "100.100.100.100:443",
            "0.1.2.3:80",
            "224.0.0.1:80",
            "255.255.255.255:80",
        ] {
            assert_eq!(check(a), Verdict::Refuse("the Mac or local network"), "{a}");
        }
        assert_eq!(check("192.168.1.5:9000"), Verdict::Allow);
        assert_eq!(check("1.1.1.1:443"), Verdict::Allow);
        assert_eq!(check("1.1.1.1:53"), Verdict::Refuse("DNS goes through rekcod"));
        if let Some(mine) = local_ipv4s().into_iter().find(|ip| !not_internet(*ip)) {
            assert_eq!(check(&format!("{mine}:80")), Verdict::Refuse("the Mac or local network"));
        }
        let resolved = Arc::new(Resolved::default());
        let p = Policy::new(Some(vec!["registry.npmjs.org".into()]), vec![], resolved.clone());
        assert_eq!(p.check(true, "104.16.1.1:443".parse().unwrap()), Verdict::Refuse("not from an allowed name"));
        resolved.record("104.16.1.1".parse().unwrap(), 60);
        assert_eq!(p.check(true, "104.16.1.1:443".parse().unwrap()), Verdict::Tls);
        assert_eq!(p.check(true, "104.16.1.1:80".parse().unwrap()), Verdict::Http);
        assert_eq!(p.check(true, "104.16.1.1:22".parse().unwrap()), Verdict::Allow);
        assert!(matches!(p.check(false, "104.16.1.1:443".parse().unwrap()), Verdict::Refuse(_)));
        assert_eq!(Policy::new(Some(vec!["*".into()]), vec![], Arc::default()).allow, None);
    }

    #[test]
    fn resolved_addresses_expire() {
        let r = Resolved::default();
        let ip = Ipv4Addr::new(93, 184, 216, 34);
        r.record_until(ip, Instant::now() - Duration::from_secs(1));
        assert!(!r.contains(ip));
        r.record(ip, 0);
        assert!(r.contains(ip), "TTL 0 still gets the grace period");
    }

    #[test]
    fn sni_from_real_client_hellos() {
        // curl to https://registry.npmjs.org, and LibreSSL's s_client to an IP (no SNI).
        let hello = include_bytes!("../testdata/clienthello-curl.bin");
        assert_eq!(tls_sni(hello), Peek::Name("registry.npmjs.org".into()));
        for cut in [0, 3, 5, 40, 200] {
            assert_eq!(tls_sni(&hello[..cut]), Peek::More, "cut at {cut}");
        }
        // The name is read as soon as its extension has arrived.
        assert_eq!(tls_sni(&hello[..hello.len() - 1]), Peek::Name("registry.npmjs.org".into()));
        // The same hello split over two records.
        let body = &hello[5..];
        let mut split = vec![0x16, 3, 1, 0, 100];
        split.extend_from_slice(&body[..100]);
        split.extend_from_slice(&[0x16, 3, 1, 0, (body.len() - 100) as u8]);
        split.extend_from_slice(&body[100..]);
        assert_eq!(tls_sni(&split), Peek::Name("registry.npmjs.org".into()));
        assert_eq!(tls_sni(include_bytes!("../testdata/clienthello-no-sni.bin")), Peek::NoName);
        assert_eq!(tls_sni(b"GET / HTTP/1.1\r\n"), Peek::NoName);
    }

    #[test]
    fn host_header() {
        let req = |h: &str| http_host(format!("GET / HTTP/1.1\r\n{h}User-Agent: x\r\n\r\n").as_bytes());
        assert_eq!(req("Host: Registry.NPMjs.org:8080\r\n"), Peek::Name("registry.npmjs.org".into()));
        assert_eq!(req("host:example.com\r\n"), Peek::Name("example.com".into()));
        assert_eq!(req(""), Peek::NoName);
        assert_eq!(req("Host: a.com\r\nHost: b.com\r\n"), Peek::NoName);
        assert_eq!(req("Host: [::1]:80\r\n"), Peek::NoName);
        assert_eq!(http_host(b"GET / HTTP/1.1\r\nHost: a.com\r\n"), Peek::More);
        assert_eq!(http_host(b"\x16\x03\x01\x00\x10\r\n\r\n"), Peek::NoName);
    }
}
