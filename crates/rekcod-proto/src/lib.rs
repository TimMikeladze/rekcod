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

/// Virtio device ID of the elastic-memory device (README.md, "Configure the VM"). Inside the
/// modern virtio-pci range (0x1040 + id), unused by Linux.
pub const ELASTIC_DEVICE_ID: u16 = 63;
/// Unit the guest releases and reuses elastic memory in.
pub const ELASTIC_CHUNK: u64 = 4 << 20;
/// Request ops: `le32 op, le32 count, le32 chunk[count]`, answered with `le32 status`.
pub const ELASTIC_OP_RELEASE: u32 = 1;
pub const ELASTIC_OP_REUSE: u32 = 2;
/// Elastic memory needs at least this much to be worth a device.
pub const ELASTIC_MIN_MIB: u64 = 512;

/// Splits the memory a VM may use into base RAM and elastic swap, in MiB. Base defaults
/// to `max(min(memory, 4 GiB), memory / 2)`; `base` overrides it.
pub fn split_memory(memory_mib: u64, base: Option<u64>) -> (u64, u64) {
    let base = base.unwrap_or_else(|| memory_mib.min(4096).max(memory_mib / 2)).min(memory_mib);
    let elastic = memory_mib - base;
    if elastic < ELASTIC_MIN_MIB { (memory_mib, 0) } else { (base, elastic) }
}

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

/// One allowlist entry: its name pattern, and the TCP port when it names one
/// (`db.example.com:5432`). An entry whose port doesn't parse names no port and, with the
/// colon left in, no host either.
pub fn allow_entry(entry: &str) -> (&str, Option<u16>) {
    match entry.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && !port.is_empty() && port.bytes().all(|c| c.is_ascii_digit()) => {
            match port.parse::<u16>() {
                Ok(p) if p > 0 => (host, Some(p)),
                _ => (entry, None),
            }
        }
        _ => (entry, None),
    }
}

/// Whether `entry` is something the allowlist understands: `*`, a name, `*.name`, either
/// of those with `:PORT`.
pub fn valid_allow_entry(entry: &str) -> bool {
    let (host, port) = allow_entry(entry);
    if host == "*" {
        return port.is_none();
    }
    let name = host.strip_prefix("*.").unwrap_or(host);
    !name.is_empty() && name.split('.').all(|l| !l.is_empty() && l.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'))
}

fn name_matches(pattern: &str, port: Option<u16>, name: &str) -> bool {
    let p = pattern.to_ascii_lowercase();
    (p == "*" && port.is_none()) || p == name || p.strip_prefix("*.").is_some_and(|base| name.ends_with(&format!(".{base}")))
}

/// Agent sandbox egress allowlist (`rekcod.allow=` on the kernel command line): exact
/// names, `*.example.com` for any subdomain, `*` for everything. An entry with a port
/// (`db.example.com:5432`) allows the name too.
pub fn host_allowed(allow: &[String], name: &str) -> bool {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    allow.iter().any(|e| {
        let (pattern, port) = allow_entry(e);
        name_matches(pattern, port, &name)
    })
}

/// The TCP ports `name`'s addresses may carry anything on, not only TLS or HTTP: the ports
/// of the entries that name it (`db.example.com:5432`).
pub fn raw_ports(allow: &[String], name: &str) -> Vec<u16> {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    let mut ports: Vec<u16> = allow
        .iter()
        .filter_map(|e| match allow_entry(e) {
            (pattern, Some(port)) if name_matches(pattern, Some(port), &name) => Some(port),
            _ => None,
        })
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// A DNS query's one question: its name, dotted, and the offset just past it. None unless
/// the query has exactly one question whose name has no compression pointer and whose
/// labels are letters, digits, `-` and `_`. The whole packet goes upstream, so the name
/// checked against an allowlist has to be the only one in it, and it ends up in logs.
pub fn dns_question(q: &[u8]) -> Option<(String, usize)> {
    if q.get(4..6)? != [0, 1] {
        return None;
    }
    let mut i = 12;
    let mut labels = Vec::new();
    loop {
        let len = *q.get(i)? as usize;
        if len == 0 {
            break;
        }
        if len & 0xc0 != 0 {
            return None;
        }
        let label = q.get(i + 1..i + 1 + len)?;
        if !label.iter().all(|&c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_') {
            return None;
        }
        labels.push(String::from_utf8_lossy(label).into_owned());
        i += 1 + len;
    }
    q.get(i + 1..i + 5)?; // type and class
    Some((labels.join("."), i + 5))
}

/// Largest query a sandbox may send upstream.
pub const DNS_QUERY_MAX: usize = 512;

/// A standard query a sandbox may send upstream: QR 0, opcode QUERY, one plain question
/// ([`dns_question`]), no answer or authority records, and at most one additional record,
/// EDNS's OPT. UPDATE, NOTIFY and the rest would reach the Mac's resolvers, VPN and
/// corporate ones included, from the Mac's address.
pub fn dns_plain_query(q: &[u8]) -> bool {
    let Some((_, end)) = dns_question(q) else { return false };
    let count = |i: usize| u16::from_be_bytes([q[i], q[i + 1]]);
    let opt = match count(10) {
        0 => end == q.len(),
        // Root name, then TYPE 41.
        1 => q.get(end..end + 3) == Some(&[0, 0, 41][..]),
        _ => false,
    };
    q.len() <= DNS_QUERY_MAX && q[2] & 0xf8 == 0 && count(6) == 0 && count(8) == 0 && opt
}

/// The answer REFUSED to `query`: its header and question only, never more than the
/// client sent. None if it's shorter than a header.
pub fn dns_refused(query: &[u8]) -> Option<Vec<u8>> {
    let mut out = query.get(..12)?.to_vec();
    out[2] |= 0x80; // QR
    out[3] = (out[3] & 0xf0) | 5;
    out[4..12].fill(0);
    if let Some((_, end)) = dns_question(query) {
        out[5] = 1;
        out.extend_from_slice(&query[12..end]);
    }
    Some(out)
}

/// Rate limit for log lines a guest can cause: the same line at most once per `window`,
/// and at most `burst` lines per window in all. What's held back is counted and reported
/// with the next line logged after the window, as `{prefix}N similar line(s) ...`.
pub struct LogLimit {
    prefix: &'static str,
    window: std::time::Duration,
    burst: u32,
    start: std::time::Instant,
    lines: u32,
    held: u64,
    seen: std::collections::HashMap<String, std::time::Instant>,
}

impl LogLimit {
    pub fn new(window: std::time::Duration, burst: u32, prefix: &'static str) -> LogLimit {
        LogLimit { prefix, window, burst, start: std::time::Instant::now(), lines: 0, held: 0, seen: Default::default() }
    }

    /// The lines to log for `line` now: none, or `line` after a count of what was held back.
    pub fn lines(&mut self, line: String, now: std::time::Instant) -> Vec<String> {
        let mut out = Vec::new();
        if now.duration_since(self.start) >= self.window {
            if self.held > 0 {
                out.push(format!("{}{} similar line(s) not logged in the last {} s", self.prefix, self.held, self.window.as_secs()));
            }
            (self.start, self.lines, self.held) = (now, 0, 0);
            let window = self.window;
            self.seen.retain(|_, t| now.duration_since(*t) < window);
        }
        if self.seen.get(&line).is_some_and(|t| now.duration_since(*t) < self.window) || self.lines >= self.burst {
            self.held += 1;
            return out;
        }
        self.lines += 1;
        self.seen.insert(line.clone(), now);
        out.push(line);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_dns_queries_only() {
        let q = [&[0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0][..], b"\x03npm\x03org\x00\x00\x01\x00\x01"].concat();
        assert_eq!(dns_question(&q), Some(("npm.org".into(), q.len())));
        assert!(dns_plain_query(&q));
        let with_opt = [&q[..11], &[1], &q[12..], &[0, 0, 41, 4, 0, 0, 0, 0, 0, 0, 0]].concat();
        assert!(dns_plain_query(&with_opt));
        // UPDATE (opcode 5) for a corporate zone, and a response.
        let mut update = q.clone();
        update[2] = 0x28;
        assert!(dns_question(&update).is_some() && !dns_plain_query(&update));
        let mut response = q.clone();
        response[2] |= 0x80;
        assert!(!dns_plain_query(&response));
        let mut answers = q.clone();
        answers[7] = 1;
        assert!(!dns_plain_query(&answers));
        assert!(!dns_plain_query(&[&q[..], &[0; 600]].concat()), "trailing bytes");
        // A label that would write a fake line, or clear a terminal, when logged.
        let esc = [&q[..12], b"\x0a\x1b[2Jx\nfake:\x01y\x03com\x00\x00\x01\x00\x01"].concat();
        assert_eq!(dns_question(&esc), None);

        let r = dns_refused(&[&q[..], &[0xff; 300]].concat()).unwrap();
        assert_eq!(r.len(), q.len(), "header and question only");
        assert_eq!((r[2] & 0x80, r[3] & 0x0f, &r[4..12]), (0x80, 5, &[0, 1, 0, 0, 0, 0, 0, 0][..]));
        assert_eq!(dns_refused(&[1, 2, 3]), None);
        assert_eq!(dns_refused(&update[..12]).unwrap()[4..12], [0; 8]);
    }

    #[test]
    fn allowlist_entries_may_name_a_port() {
        let allow: Vec<String> =
            ["registry.npmjs.org", "db.example.com:5432", "*.corp.example:22000", "x.io:99999"].map(String::from).into();
        assert!(host_allowed(&allow, "db.example.com"), "a port entry allows the name");
        assert!(host_allowed(&allow, "git.corp.example"));
        assert!(!host_allowed(&allow, "x.io"), "a port that doesn't parse names nothing");
        assert_eq!(raw_ports(&allow, "DB.example.com."), [5432]);
        assert_eq!(raw_ports(&allow, "git.corp.example"), [22000]);
        assert!(raw_ports(&allow, "registry.npmjs.org").is_empty());
        assert!(!host_allowed(&["*:5432".into()], "anything.example"), "no port for every name");
        for ok in ["*", "a.com", "*.a.com", "a.com:5432", "*.a.com:1"] {
            assert!(valid_allow_entry(ok), "{ok}");
        }
        for bad in ["*:5432", "a.com:0", "a.com:99999", "a.com:", "", "a..com", "a com", "http://a.com"] {
            assert!(!valid_allow_entry(bad), "{bad}");
        }
    }

    #[test]
    fn log_lines_a_guest_causes_are_rate_limited() {
        let t0 = std::time::Instant::now();
        let mut l = LogLimit::new(std::time::Duration::from_secs(10), 3, "x: ");
        let mut logged = Vec::new();
        for i in 0..1000 {
            logged.extend(l.lines(format!("refused dns n{}.evil", i % 5), t0));
        }
        assert_eq!(logged, ["refused dns n0.evil", "refused dns n1.evil", "refused dns n2.evil"]);
        let next = l.lines("refused dns n0.evil".into(), t0 + std::time::Duration::from_secs(11));
        assert_eq!(next, ["x: 997 similar line(s) not logged in the last 10 s", "refused dns n0.evil"]);
    }

    #[test]
    fn splits_memory_into_base_and_elastic() {
        assert_eq!(split_memory(8192, None), (4096, 4096));
        assert_eq!(split_memory(16384, None), (8192, 8192));
        assert_eq!(split_memory(4096, None), (4096, 0), "small VMs stay all RAM");
        assert_eq!(split_memory(4400, None), (4400, 0), "elastic part too small to bother");
        assert_eq!(split_memory(8192, Some(6144)), (6144, 2048));
        assert_eq!(split_memory(8192, Some(10240)), (8192, 0), "base can't exceed memory");
    }

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
