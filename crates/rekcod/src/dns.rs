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

pub fn if_index(name: &str) -> u32 {
    let Ok(name) = std::ffi::CString::new(name) else { return 0 };
    unsafe { libc::if_nametoindex(name.as_ptr()) }
}

#[cfg(test)]
mod tests {
    use super::*;

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
