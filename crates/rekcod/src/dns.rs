//! The Mac's DNS servers, as the guest's upstream.
//!
//! macOS keeps `/etc/resolv.conf` pointed at the current primary resolvers
//! (router, VPN), including link-local IPv6 ones with a `%iface` scope that
//! the VM can't reach itself; the daemon queries them on the guest's behalf.

use std::net::{IpAddr, Ipv6Addr, SocketAddr, SocketAddrV6};

/// `nameserver` entries from resolv.conf text, as port-53 socket addresses.
pub fn nameservers(resolv_conf: &str, if_index: impl Fn(&str) -> u32) -> Vec<SocketAddr> {
    resolv_conf
        .lines()
        .filter_map(|l| l.trim().strip_prefix("nameserver"))
        .filter_map(|addr| {
            let addr = addr.trim();
            match addr.split_once('%') {
                Some((ip, iface)) => {
                    let ip: Ipv6Addr = ip.parse().ok()?;
                    Some(SocketAddr::V6(SocketAddrV6::new(ip, 53, 0, if_index(iface))))
                }
                None => Some(SocketAddr::new(addr.parse::<IpAddr>().ok()?, 53)),
            }
        })
        .collect()
}

/// One resolver from `scutil --dns`: a domain it answers for (None: the default) and its servers.
#[derive(Debug, PartialEq)]
pub struct Resolver {
    pub domain: Option<String>,
    pub servers: Vec<SocketAddr>,
}

/// The first section of `scutil --dns` ("DNS configuration"), skipping mDNS-only entries.
/// Per-domain resolvers there come from `/etc/resolver/*`, VPNs and Tailscale (split DNS),
/// which `/etc/resolv.conf` leaves out.
pub fn parse_scutil(text: &str) -> Vec<Resolver> {
    let section = text.split("DNS configuration (for scoped queries)").next().unwrap_or("");
    let mut out = Vec::new();
    for block in section.split("resolver #").skip(1) {
        let (mut domain, mut servers, mut ifx) = (None, Vec::new(), 0u32);
        for line in block.lines() {
            let Some((k, v)) = line.split_once(':') else { continue };
            let v = v.trim();
            match k.trim() {
                "domain" => domain = Some(v.trim_end_matches('.').to_ascii_lowercase()),
                "if_index" => ifx = v.split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0),
                k if k.starts_with("nameserver[") => {
                    if let Ok(ip) = v.parse::<IpAddr>() {
                        servers.push(SocketAddr::new(ip, 53));
                    }
                }
                _ => {}
            }
        }
        for s in servers.iter_mut() {
            if let SocketAddr::V6(v6) = s
                && v6.ip().is_unicast_link_local()
            {
                v6.set_scope_id(ifx);
            }
        }
        if !servers.is_empty() {
            out.push(Resolver { domain, servers });
        }
    }
    out
}

/// Servers for `name`: the resolver with the longest matching domain, else the default ones.
pub fn pick(resolvers: &[Resolver], name: &str) -> Vec<SocketAddr> {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    let specific = resolvers
        .iter()
        .filter_map(|r| r.domain.as_deref().map(|d| (d, r)))
        .filter(|(d, _)| name == *d || name.ends_with(&format!(".{d}")))
        .max_by_key(|(d, _)| d.len());
    match specific {
        Some((_, r)) => r.servers.clone(),
        None => resolvers.iter().filter(|r| r.domain.is_none()).flat_map(|r| r.servers.iter().copied()).collect(),
    }
}

/// The question name of a DNS query, or None unless it has exactly one question whose name
/// has no compression pointer. Sandboxes refuse anything else, so the name checked is the
/// only one the upstream resolver sees.
pub fn query_name(q: &[u8]) -> Option<String> {
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
        labels.push(std::str::from_utf8(q.get(i + 1..i + 1 + len)?).ok()?.to_string());
        i += 1 + len;
    }
    q.get(i + 1..i + 5)?; // type and class
    Some(labels.join("."))
}

/// End of a (possibly compressed) name starting at `i`.
fn skip_name(p: &[u8], mut i: usize) -> Option<usize> {
    loop {
        let len = *p.get(i)? as usize;
        if len & 0xc0 == 0xc0 {
            return Some(i + 2);
        }
        i += 1 + len;
        if len == 0 {
            return Some(i);
        }
    }
}

/// A records in an answer's answer section, with their TTLs.
pub fn answer_a(p: &[u8]) -> Vec<(std::net::Ipv4Addr, u32)> {
    let mut out = Vec::new();
    let (Some(qd), Some(an)) = (p.get(4..6), p.get(6..8)) else { return out };
    let (qd, an) = (u16::from_be_bytes([qd[0], qd[1]]), u16::from_be_bytes([an[0], an[1]]));
    let mut i = 12;
    for _ in 0..qd {
        let Some(e) = skip_name(p, i) else { return out };
        i = e + 4;
    }
    for _ in 0..an {
        let Some(e) = skip_name(p, i) else { return out };
        let Some(h) = p.get(e..e + 10) else { return out };
        let (rtype, rdlen) = (u16::from_be_bytes([h[0], h[1]]), u16::from_be_bytes([h[8], h[9]]) as usize);
        let ttl = u32::from_be_bytes([h[4], h[5], h[6], h[7]]);
        if rtype == 1
            && rdlen == 4
            && let Some(a) = p.get(e + 10..e + 14)
        {
            out.push((std::net::Ipv4Addr::new(a[0], a[1], a[2], a[3]), ttl));
        }
        i = e + 10 + rdlen;
    }
    out
}

/// `query` turned into an answer with RCODE REFUSED and no records.
pub fn refused(query: &[u8]) -> Option<Vec<u8>> {
    query.get(..12)?;
    let mut out = query.to_vec();
    out[2] |= 0x80; // QR
    out[3] = (out[3] & 0xf0) | 5;
    out[6..12].fill(0);
    Some(out)
}

pub fn if_index(name: &str) -> u32 {
    let Ok(name) = std::ffi::CString::new(name) else { return 0 };
    unsafe { libc::if_nametoindex(name.as_ptr()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_dns_picks_the_longest_domain() {
        let text = "DNS configuration\n\nresolver #1\n  nameserver[0] : 192.168.1.1\n  if_index : 15 (en0)\n\n\
                    resolver #2\n  domain   : local\n  options  : mdns\n\n\
                    resolver #3\n  domain   : tail1234.ts.net.\n  nameserver[0] : 100.100.100.100\n\n\
                    resolver #4\n  domain   : corp.example\n  nameserver[0] : fe80::1\n  if_index : 20 (utun3)\n\n\
                    DNS configuration (for scoped queries)\n\nresolver #1\n  nameserver[0] : 9.9.9.9\n";
        let r = parse_scutil(text);
        assert_eq!(r.len(), 3, "{r:?}");
        let s = |n| pick(&r, n).iter().map(|a| a.to_string()).collect::<Vec<_>>();
        assert_eq!(s("box.tail1234.ts.net"), ["100.100.100.100:53"]);
        assert_eq!(s("git.corp.example."), ["[fe80::1%20]:53"]);
        assert_eq!(s("example.com"), ["192.168.1.1:53"]);
        let q = [&[0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0][..], b"\x03git\x04corp\x07example\x00\x00\x01\x00\x01"].concat();
        assert_eq!(query_name(&q).as_deref(), Some("git.corp.example"));
    }

    #[test]
    fn sandbox_queries_need_one_plain_question() {
        let q = [&[0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0][..], b"\x03npm\x03org\x00\x00\x01\x00\x01"].concat();
        assert_eq!(query_name(&q).as_deref(), Some("npm.org"));
        // Two questions: the second could name anything.
        let mut two = q.clone();
        two[5] = 2;
        two.extend_from_slice(b"\x04evil\x03com\x00\x00\x01\x00\x01");
        assert_eq!(query_name(&two), None);
        let mut none = q.clone();
        none[5] = 0;
        assert_eq!(query_name(&none), None);
        // "npm" then a pointer to whatever offset 12 spells.
        let ptr = [&q[..12], b"\x03npm\xc0\x0c\x00\x01\x00\x01"].concat();
        assert_eq!(query_name(&ptr), None);
        assert_eq!(query_name(&q[..q.len() - 2]), None, "type/class cut off");
        let r = refused(&q).unwrap();
        assert_eq!((r[2] & 0x80, r[3] & 0x0f, &r[4..12]), (0x80, 5, &[0, 1, 0, 0, 0, 0, 0, 0][..]));
    }

    #[test]
    fn answer_a_reads_addresses_and_ttls() {
        // x.b.io: a CNAME, then an A record with TTL 300 (compressed names).
        let mut p = vec![0x12, 0x34, 0x81, 0x80, 0, 1, 0, 2, 0, 0, 0, 0];
        p.extend([1, b'x', 1, b'b', 2, b'i', b'o', 0, 0, 1, 0, 1]);
        p.extend([0xc0, 12, 0, 5, 0, 1, 0, 0, 0, 60, 0, 2, 0xc0, 14]);
        p.extend([0xc0, 14, 0, 1, 0, 1, 0, 0, 1, 44, 0, 4, 93, 184, 216, 34]);
        assert_eq!(answer_a(&p), vec![(std::net::Ipv4Addr::new(93, 184, 216, 34), 300)]);
        assert_eq!(answer_a(&p[..40]), vec![]);
    }

    #[test]
    fn parses_v4_v6_and_scoped_link_local() {
        let conf = "# macOS\nnameserver fe80::1aa5:ffff:fe06:249c%en10\nnameserver 192.168.12.1\nsearch lan\nnameserver 2606:4700::1111\nnameserver bogus\n";
        let got = nameservers(conf, |iface| if iface == "en10" { 7 } else { 0 });
        assert_eq!(
            got.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
            vec!["[fe80::1aa5:ffff:fe06:249c%7]:53", "192.168.12.1:53", "[2606:4700::1111]:53"]
        );
    }
}
