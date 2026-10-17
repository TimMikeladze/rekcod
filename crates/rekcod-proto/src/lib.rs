//! Wire constants shared by the host daemon and the guest agent.
//!
//! Everything between macOS and the VM goes over vsock; these are the ports
//! and the one-line headers spoken on them.

/// Guest port bridged to `/var/run/docker.sock`.
pub const DOCKER_PORT: u32 = 2375;
/// Guest port that dials `127.0.0.1:<port>` inside the VM (published ports).
pub const FORWARD_PORT: u32 = 2376;
/// Guest port for control commands (ping, time, shutdown).
pub const CONTROL_PORT: u32 = 2377;
/// Guest port the host keeps one connection open on to answer the guest's DNS
/// queries with the Mac's resolvers. Frames: u16 big-endian length + packet.
pub const DNS_PORT: u32 = 2378;
/// Guest port the host parks idle connections on. The agent takes one per
/// container connection to `host.docker.internal`, sends [`reverse_header`],
/// and the host splices it to `127.0.0.1:<port>` on the Mac.
pub const REVERSE_PORT: u32 = 2379;
/// Guest-local TCP port that iptables redirects `host.docker.internal` traffic to.
pub const REVERSE_LISTEN_PORT: u16 = 2379;
/// Guest port for the metadata-server latency prototype (phase 6 research).
pub const META_PORT: u32 = 2380;
/// Guest-local address (UDP and TCP) the agent serves for dnsmasq's upstream.
pub const DNS_STUB_ADDR: &str = "127.0.0.1:5353";

/// Frames a packet (DNS message, UDP datagram): u16 big-endian length + bytes.
pub fn frame(packet: &[u8]) -> Option<Vec<u8>> {
    let len = u16::try_from(packet.len()).ok()?;
    let mut out = Vec::with_capacity(packet.len() + 2);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(packet);
    Some(out)
}

/// virtio-fs tag for the Mac directories (`/Users`, `/Volumes`, `/private`).
pub const MAC_SHARE_TAG: &str = "mac";
/// virtio-fs tag for Rosetta.
pub const ROSETTA_SHARE_TAG: &str = "rosetta";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Proto {
    Tcp,
    /// The stream carries [`frame`]d datagrams both ways.
    Udp,
}

/// Header sent first on a [`FORWARD_PORT`] connection: `tcp:<port>\n` or `udp:<port>\n`.
pub fn forward_header(proto: Proto, port: u16) -> String {
    match proto {
        Proto::Tcp => format!("tcp:{port}\n"),
        Proto::Udp => format!("udp:{port}\n"),
    }
}

fn parse_port(s: &str) -> Option<u16> {
    match s.parse::<u16>() {
        Ok(0) | Err(_) => None,
        Ok(p) => Some(p),
    }
}

/// Parses a [`forward_header`] line (with or without the trailing newline).
pub fn parse_forward_header(line: &str) -> Option<(Proto, u16)> {
    let line = line.trim_end_matches('\n');
    if let Some(p) = line.strip_prefix("tcp:") {
        return Some((Proto::Tcp, parse_port(p)?));
    }
    Some((Proto::Udp, parse_port(line.strip_prefix("udp:")?)?))
}

/// Sent by the agent on a parked [`REVERSE_PORT`] connection: `connect <port>\n`.
pub fn reverse_header(port: u16) -> String {
    format!("connect {port}\n")
}

pub fn parse_reverse_header(line: &str) -> Option<u16> {
    parse_port(line.trim_end_matches('\n').strip_prefix("connect ")?)
}

/// Ping reply suffix when the guest's virtio-fs caches and takes invalidations
/// (`Control::Notify` then covers every shared path, not only bind sources).
pub const CAP_FS_NOTIFY: &str = "fs-notify";

/// Payload for [`Control::Notify`]: the path plus the host's current view of
/// it (`None` = deleted) so the guest can skip invalidating caches that are
/// already current: `size, mtime s, mtime ns, ctime s, ctime ns`.
pub fn notify_line(path: &str, stat: Option<[i64; 5]>) -> String {
    match stat {
        None => format!("{path}\t-"),
        Some(s) => format!("{path}\t{}\t{}\t{}\t{}\t{}", s[0], s[1], s[2], s[3], s[4]),
    }
}

/// A command sent on [`CONTROL_PORT`], one per line; the agent answers one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    Ping,
    /// Set the guest clock to this many nanoseconds since the Unix epoch.
    Time(u128),
    Shutdown,
    /// A Mac-side change: `<path>[\t<host stat>]`, see [`notify_line`]. A
    /// trailing `/` on the path means "everything below".
    Notify(String),
    /// Reply: `<total> <free> <available> <cached>` in KiB (from /proc/meminfo).
    Meminfo,
    /// Ask the guest kernel to reclaim this many bytes (page cache first).
    Reclaim(u64),
    /// Pre-load the guest's dentry/attribute cache for a shared directory tree
    /// (a container just bind-mounted it). Returns at once; the walk runs in the background.
    Warm(String),
    /// Return unused data-disk blocks to macOS (`fstrim`). Reply: `ok <bytes trimmed>`.
    Trim,
    /// Research: time cold lstats via the host metadata server vs virtio-fs.
    BenchMeta(String),
    /// TCP ports listened on by the processes of this container (by id).
    /// Reply: `ok <port> <port> ...` (`rekcod x` autoports).
    Listeners(String),
    /// Reply: one line of JSON: `{"fs_requests": {"<fuse opcode>": n}, "warming": [path],
    /// "disk_used": bytes, "disk_size": bytes}`; both null when statvfs fails.
    Stats,
}

impl Control {
    pub fn encode(&self) -> String {
        match self {
            Control::Ping => "ping\n".into(),
            Control::Time(ns) => format!("time {ns}\n"),
            Control::Shutdown => "shutdown\n".into(),
            Control::Notify(path) => format!("notify {path}\n"),
            Control::Meminfo => "meminfo\n".into(),
            Control::Reclaim(bytes) => format!("reclaim {bytes}\n"),
            Control::Warm(path) => format!("warm {path}\n"),
            Control::Trim => "trim\n".into(),
            Control::BenchMeta(path) => format!("benchmeta {path}\n"),
            Control::Listeners(id) => format!("listeners {id}\n"),
            Control::Stats => "stats\n".into(),
        }
    }

    pub fn parse(line: &str) -> Option<Control> {
        let line = line.trim_end_matches('\n');
        match line.split_once(' ') {
            None if line == "ping" => Some(Control::Ping),
            None if line == "shutdown" => Some(Control::Shutdown),
            None if line == "meminfo" => Some(Control::Meminfo),
            None if line == "trim" => Some(Control::Trim),
            None if line == "stats" => Some(Control::Stats),
            Some(("listeners", id)) if !id.is_empty() && id.bytes().all(|b| b.is_ascii_hexdigit()) => {
                Some(Control::Listeners(id.to_string()))
            }
            Some(("warm", path)) if path.starts_with('/') => Some(Control::Warm(path.to_string())),
            Some(("benchmeta", path)) if path.starts_with('/') => Some(Control::BenchMeta(path.to_string())),
            Some(("reclaim", bytes)) => bytes.parse().ok().map(Control::Reclaim),
            Some(("time", ns)) => ns.parse().ok().map(Control::Time),
            Some(("notify", path)) if path.starts_with('/') => Some(Control::Notify(path.to_string())),
            _ => None,
        }
    }
}

/// Agent sandbox egress allowlist (`rekcod.allow=` on the kernel command line): exact
/// names, `*.example.com` for any subdomain, `*` for everything.
pub fn host_allowed(allow: &[String], name: &str) -> bool {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    allow.iter().any(|p| {
        let p = p.to_ascii_lowercase();
        p == "*" || p == name || p.strip_prefix("*.").is_some_and(|base| name.ends_with(&format!(".{base}")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_header_round_trips() {
        assert_eq!(parse_forward_header(&forward_header(Proto::Tcp, 5432)), Some((Proto::Tcp, 5432)));
        assert_eq!(parse_forward_header(&forward_header(Proto::Udp, 53)), Some((Proto::Udp, 53)));
        assert_eq!(parse_forward_header("tcp:80"), Some((Proto::Tcp, 80)));
        assert_eq!(parse_reverse_header(&reverse_header(3000)), Some(3000));
    }

    #[test]
    fn forward_header_rejects_garbage() {
        for bad in ["", "tcp:", "tcp:0", "tcp:70000", "sctp:53\n", "tcp:-1", "5432", "udp:"] {
            assert_eq!(parse_forward_header(bad), None, "{bad:?}");
        }
        for bad in ["connect", "connect 0", "connect x", "tcp:80"] {
            assert_eq!(parse_reverse_header(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn notify_line_formats_stat_or_missing() {
        assert_eq!(notify_line("/Users/a b/x.ts", Some([12, 1, 2, 3, 4])), "/Users/a b/x.ts\t12\t1\t2\t3\t4");
        assert_eq!(notify_line("/Users/x", None), "/Users/x\t-");
        let c = Control::Notify(notify_line("/Users/x", None));
        assert_eq!(Control::parse(&c.encode()), Some(c));
    }

    #[test]
    fn frame_prefixes_length() {
        assert_eq!(frame(&[0xab, 0xcd, 0xef]).unwrap(), vec![0, 3, 0xab, 0xcd, 0xef]);
        assert!(frame(&vec![0; 70_000]).is_none());
    }

    #[test]
    fn control_round_trips() {
        let spaced = Control::Notify("/Users/me/My Project/a.ts".into());
        let all = [
            Control::Ping,
            Control::Shutdown,
            Control::Time(1_791_735_420_830_000_000),
            spaced,
            Control::Meminfo,
            Control::Reclaim(1 << 30),
            Control::Trim,
            Control::Warm("/Users/me/My App".into()),
            Control::Stats,
            Control::Listeners("ab12".into()),
        ];
        for c in all {
            assert_eq!(Control::parse(&c.encode()), Some(c));
        }
        assert_eq!(Control::parse("time abc\n"), None);
        assert_eq!(Control::parse("reboot\n"), None);
        assert_eq!(Control::parse("notify relative/path\n"), None);
        assert_eq!(Control::parse("listeners ../etc\n"), None);
    }
}
