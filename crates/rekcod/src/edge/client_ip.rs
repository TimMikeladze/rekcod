//! Who's on the other end: PROXY protocol v2, or the tunnel's forwarded headers.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use anyhow::{Result, bail};
use hyper::HeaderMap;
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
    match head[13] >> 4 {
        1 if rest.len() >= 12 => Ok(Some(IpAddr::V4(Ipv4Addr::new(rest[0], rest[1], rest[2], rest[3])))),
        2 if rest.len() >= 36 => Ok(Some(IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(&rest[..16])?)))),
        // AF_UNIX or unspecified: nothing useful to rate limit on.
        _ => Ok(None),
    }
}

/// `CF-Connecting-IP`, else the leftmost `X-Forwarded-For`. Only call this for a listener
/// whose only clients are a trusted tunnel; anyone else can write these headers.
pub fn forwarded(h: &HeaderMap) -> Option<IpAddr> {
    let get = |k: &str| h.get(k).and_then(|v| v.to_str().ok());
    get("cf-connecting-ip")
        .and_then(|v| v.trim().parse().ok())
        .or_else(|| get("x-forwarded-for").and_then(|v| v.split(',').next()).and_then(|v| v.trim().parse().ok()))
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
        head[12] = 0x20; // LOCAL
        assert_eq!(parse_v2(&head, &rest).unwrap(), None);
        head[0] = b'G';
        assert!(parse_v2(&head, &rest).is_err());
    }

    #[test]
    fn forwarded_headers() {
        let mut h = HeaderMap::new();
        assert_eq!(forwarded(&h), None);
        h.insert("x-forwarded-for", "198.51.100.2, 10.0.0.1".parse().unwrap());
        assert_eq!(forwarded(&h), Some("198.51.100.2".parse().unwrap()));
        h.insert("cf-connecting-ip", "2001:db8::1".parse().unwrap());
        assert_eq!(forwarded(&h), Some("2001:db8::1".parse().unwrap()));
    }
}
