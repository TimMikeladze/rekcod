//! Who's on the other end: PROXY protocol v2, or the tunnel's forwarded headers.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use anyhow::{Result, bail};
use hyper::HeaderMap;
use hyper::header::HeaderName;
use tokio::io::{AsyncRead, AsyncReadExt};

const SIG: &[u8; 12] = b"\r\n\r\n\0\r\nQUIT\n";

/// Reads a PROXY v2 header off the front of the stream. `None` for a LOCAL command (a
/// health check from the balancer itself), where the peer address is the right one.
pub async fn read_proxy_v2<R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<IpAddr>> {
    let mut head = [0u8; 16];
    r.read_exact(&mut head).await?;
    let len = u16::from_be_bytes([head[14], head[15]]) as usize;
    let mut rest = vec![0u8; len];
    r.read_exact(&mut rest).await?;
    parse_v2(&head, &rest)
}

fn parse_v2(head: &[u8; 16], rest: &[u8]) -> Result<Option<IpAddr>> {
    if &head[..12] != SIG || head[12] >> 4 != 2 {
        bail!("not a PROXY v2 header");
    }
    if head[12] & 0x0f == 0 {
        return Ok(None);
    }
    let ip = match head[13] >> 4 {
        1 if rest.len() >= 12 => IpAddr::V4(Ipv4Addr::new(rest[0], rest[1], rest[2], rest[3])),
        2 if rest.len() >= 36 => IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(&rest[..16])?)),
        1 | 2 => bail!("PROXY v2 header: address block too short"),
        // AF_UNIX or unspecified: nothing useful to rate limit on.
        _ => return Ok(None),
    };
    Ok(Some(ip.to_canonical()))
}

/// One `X-Forwarded-For` entry: `1.2.3.4`, `1.2.3.4:5678`, `2001:db8::1` or `[2001:db8::1]:5678`.
fn hop(s: &str) -> Option<IpAddr> {
    let s = s.trim();
    let ip = s
        .parse::<IpAddr>()
        .ok()
        .or_else(|| s.parse::<SocketAddr>().ok().map(|a| a.ip()))
        .or_else(|| s.strip_prefix('[').and_then(|r| r.strip_suffix(']')).and_then(|r| r.parse().ok()))?;
    Some(ip.to_canonical())
}

/// The client behind a trusted peer. With `header` set (`--client-ip-header`, such as
/// `cf-connecting-ip` behind cloudflared) that header names it. Otherwise every
/// `X-Forwarded-For` line is read from the right: the first address that isn't one of
/// `trusted` is the client. The leftmost entries are whatever the client sent, so taking
/// them would let anyone pick their own IP; an entry that doesn't parse ends the walk the
/// same way. Only call this when the peer is trusted.
pub fn forwarded(h: &HeaderMap, trusted: &[ipnet::IpNet], header: Option<&HeaderName>) -> Option<IpAddr> {
    if let Some(name) = header {
        return h.get(name).and_then(|v| v.to_str().ok()).and_then(hop);
    }
    let entries: Vec<&str> = h.get_all("x-forwarded-for").iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(',')).collect();
    let mut last = None;
    for e in entries.iter().rev() {
        let Some(ip) = hop(e) else { break };
        if !trusted.iter().any(|n| n.contains(&ip)) {
            return Some(ip);
        }
        last = Some(ip);
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_v2() {
        let mut head = [0u8; 16];
        head[..12].copy_from_slice(SIG);
        head[12] = 0x21; // v2, PROXY
        head[13] = 0x11; // TCP over IPv4
        let rest = [203, 0, 113, 7, 10, 0, 0, 1, 0x1f, 0x90, 0, 80];
        assert_eq!(parse_v2(&head, &rest).unwrap(), Some("203.0.113.7".parse().unwrap()));
        // An IPv4-mapped IPv6 address is the IPv4 one.
        head[13] = 0x21;
        let mut v6 = [0u8; 36];
        v6[..16].copy_from_slice(&"::ffff:203.0.113.8".parse::<Ipv6Addr>().unwrap().octets());
        assert_eq!(parse_v2(&head, &v6).unwrap(), Some("203.0.113.8".parse().unwrap()));
        assert!(parse_v2(&head, &rest).is_err(), "IPv6 with an IPv4-sized address block");
        head[13] = 0x11;
        assert!(parse_v2(&head, &rest[..8]).is_err());
        head[12] = 0x20; // LOCAL
        assert_eq!(parse_v2(&head, &rest).unwrap(), None);
        head[0] = b'G';
        assert!(parse_v2(&head, &rest).is_err());
    }

    #[test]
    fn forwarded_headers() {
        let mut h = HeaderMap::new();
        let trusted: Vec<ipnet::IpNet> = vec!["10.0.0.0/8".parse().unwrap()];
        let ip = |s: &str| Some(s.parse::<IpAddr>().unwrap());
        assert_eq!(forwarded(&h, &trusted, None), None);
        h.insert("x-forwarded-for", "198.51.100.2, 10.0.0.1".parse().unwrap());
        assert_eq!(forwarded(&h, &trusted, None), ip("198.51.100.2"));
        // A client that writes its own X-Forwarded-For only adds entries on the left.
        h.insert("x-forwarded-for", "1.2.3.4, 198.51.100.2, 10.0.0.1".parse().unwrap());
        assert_eq!(forwarded(&h, &trusted, None), ip("198.51.100.2"));
        // HAProxy appends its entry as a second line; the client's line comes first.
        h.insert("x-forwarded-for", "6.6.6.6".parse().unwrap());
        h.append("x-forwarded-for", "203.0.113.50".parse().unwrap());
        assert_eq!(forwarded(&h, &trusted, None), ip("203.0.113.50"));
        // A proxy that appends ip:port, and a bracketed IPv6 address.
        h.insert("x-forwarded-for", "6.6.6.8, 203.0.113.52:4711".parse().unwrap());
        assert_eq!(forwarded(&h, &trusted, None), ip("203.0.113.52"));
        h.insert("x-forwarded-for", "6.6.6.8, [2001:db8::7]:4711".parse().unwrap());
        assert_eq!(forwarded(&h, &trusted, None), ip("2001:db8::7"));
        // An entry that doesn't parse ends the walk: the hop to its right is the client.
        h.insert("x-forwarded-for", "6.6.6.9, junk, 10.0.0.2".parse().unwrap());
        assert_eq!(forwarded(&h, &trusted, None), ip("10.0.0.2"));
        h.insert("x-forwarded-for", "::ffff:198.51.100.3".parse().unwrap());
        assert_eq!(forwarded(&h, &trusted, None), ip("198.51.100.3"));
        // CF-Connecting-IP counts only when the edge was told to read it.
        h.insert("x-forwarded-for", "198.51.100.2".parse().unwrap());
        h.insert("cf-connecting-ip", "2001:db8::1".parse().unwrap());
        assert_eq!(forwarded(&h, &trusted, None), ip("198.51.100.2"));
        let cf = HeaderName::from_static("cf-connecting-ip");
        assert_eq!(forwarded(&h, &trusted, Some(&cf)), ip("2001:db8::1"));
    }
}
