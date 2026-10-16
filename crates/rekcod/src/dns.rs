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

/// The question name of a DNS query, or None if it doesn't parse.
pub fn query_name(q: &[u8]) -> Option<String> {
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
    Some(labels.join("."))
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
        let q = [&[0u8; 12][..], b"\x03git\x04corp\x07example\x00\x00\x01\x00\x01"].concat();
        assert_eq!(query_name(&q).as_deref(), Some("git.corp.example"));
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
