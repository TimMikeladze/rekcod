//! Published port -> container address, so forwarded Mac connections skip docker-proxy
//! (about 30 µs per round trip; docs/perf-plan.md §1).
//!
//! dockerd starts one docker-proxy per published port and binding, with the container
//! address on its command line. Only children of dockerd count: any container can run a
//! process named docker-proxy, and trusting one would let it capture another
//! container's port. Anything not found here falls back to 127.0.0.1:<port>.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;

const DOCKERD_PID: &str = "/var/run/docker.pid";

/// Host port -> (docker-proxy pid, container address). Checked against the pid's
/// command line on every use, so a removed or recreated container is never dialed.
static CACHE: Mutex<Option<HashMap<u16, (u32, SocketAddr)>>> = Mutex::new(None);

/// Where a forwarded TCP connection for `port` should go.
pub fn target(port: u16) -> Option<SocketAddr> {
    let mut cache = CACHE.lock().unwrap();
    let map = cache.get_or_insert_with(HashMap::new);
    if let Some(&(pid, addr)) = map.get(&port)
        && read_cmdline(pid).and_then(|c| parse(&c)).is_some_and(|(p, a)| p == port && a == addr)
        && ppid(pid) == dockerd()
    {
        return Some(addr);
    }
    map.clear();
    let dockerd = dockerd()?;
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|p| p.parse::<u32>().ok()) else { continue };
        if ppid(pid) != Some(dockerd) {
            continue;
        }
        if let Some((p, addr)) = read_cmdline(pid).and_then(|c| parse(&c)) {
            map.insert(p, (pid, addr));
        }
    }
    map.get(&port).map(|&(_, a)| a)
}

fn dockerd() -> Option<u32> {
    std::fs::read_to_string(DOCKERD_PID).ok()?.trim().parse().ok()
}

fn ppid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm may contain spaces and parens; fields resume after the last ')'.
    stat.rsplit_once(')')?.1.split_whitespace().nth(1)?.parse().ok()
}

fn read_cmdline(pid: u32) -> Option<Vec<u8>> {
    std::fs::read(format!("/proc/{pid}/cmdline")).ok()
}

/// `docker-proxy -proto tcp -host-ip 127.0.0.1 -host-port 5432 -container-ip 172.17.0.2
/// -container-port 5432 ...` (NUL-separated) -> (5432, 172.17.0.2:5432). Only TCP proxies
/// whose host IP the Mac-side forward could have reached on loopback.
pub fn parse(cmdline: &[u8]) -> Option<(u16, SocketAddr)> {
    let args: Vec<&str> = cmdline.split(|&b| b == 0).filter_map(|a| std::str::from_utf8(a).ok()).collect();
    if !args.first()?.ends_with("docker-proxy") {
        return None;
    }
    let flag = |name: &str| args.windows(2).find(|w| w[0] == name).map(|w| w[1]);
    if flag("-proto")? != "tcp" {
        return None;
    }
    let host_ip: IpAddr = flag("-host-ip")?.parse().ok()?;
    if !(host_ip.is_loopback() || host_ip.is_unspecified()) {
        return None;
    }
    let host_port = flag("-host-port")?.parse().ok()?;
    let ip: IpAddr = flag("-container-ip")?.parse().ok()?;
    let port = flag("-container-port")?.parse().ok()?;
    Some((host_port, SocketAddr::new(ip, port)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(s: &str) -> Vec<u8> {
        s.replace(' ', "\0").into_bytes()
    }

    #[test]
    fn parses_tcp_proxies_reachable_on_loopback() {
        let c = cmd(
            "/usr/local/bin/docker-proxy -proto tcp -host-ip 127.0.0.1 -host-port 55499 -container-ip 172.17.0.2 -container-port 80 -use-listen-fd",
        );
        assert_eq!(parse(&c), Some((55499, "172.17.0.2:80".parse().unwrap())));
        let c = cmd("docker-proxy -proto tcp -host-ip :: -host-port 5432 -container-ip 172.18.0.3 -container-port 5432");
        assert_eq!(parse(&c), Some((5432, "172.18.0.3:5432".parse().unwrap())));
    }

    #[test]
    fn skips_udp_other_binaries_and_lan_bindings() {
        for c in [
            "docker-proxy -proto udp -host-ip 0.0.0.0 -host-port 53 -container-ip 172.17.0.2 -container-port 53",
            "/bin/sh -proto tcp -host-ip 0.0.0.0 -host-port 80 -container-ip 10.0.0.1 -container-port 80",
            "docker-proxy -proto tcp -host-ip 192.168.64.2 -host-port 80 -container-ip 172.17.0.2 -container-port 80",
            "docker-proxy -proto tcp -host-ip 0.0.0.0 -host-port 80",
        ] {
            assert_eq!(parse(&cmd(c)), None, "{c}");
        }
    }
}
