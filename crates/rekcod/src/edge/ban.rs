//! Banned addresses: the controller's list (every edge) plus auto bans this edge made
//! before the controller's copy comes back on the next refresh.

use std::net::IpAddr;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use ipnet::IpNet;

/// `1.2.3.4` or `10.0.0.0/8`.
pub fn parse_net(s: &str) -> Result<IpNet> {
    let s = s.trim();
    s.parse::<IpNet>()
        .or_else(|_| s.parse::<IpAddr>().map(IpNet::from))
        .map(|n| n.trunc())
        .map_err(|_| anyhow!("{s} isn't an IP address or CIDR range"))
}

/// The form the controller stores, so `unban 1.2.3.4` finds `ban 1.2.3.4/32`.
pub fn normalize(s: &str) -> Result<String> {
    let n = parse_net(s)?;
    Ok(if n.prefix_len() == n.max_prefix_len() { n.addr().to_string() } else { n.to_string() })
}

#[derive(Default)]
pub struct Bans {
    remote: RwLock<Vec<IpNet>>,
    local: Mutex<Vec<(IpNet, Instant)>>,
}

impl Bans {
    pub fn set_remote(&self, nets: Vec<IpNet>) {
        *self.remote.write().unwrap() = nets;
    }

    pub fn ban_for(&self, ip: IpAddr, d: Duration) {
        self.local.lock().unwrap().push((IpNet::from(ip), Instant::now() + d));
    }

    pub fn is_banned(&self, ip: IpAddr) -> bool {
        if self.remote.read().unwrap().iter().any(|n| n.contains(&ip)) {
            return true;
        }
        let mut local = self.local.lock().unwrap();
        local.retain(|(_, until)| *until > Instant::now());
        local.iter().any(|(n, _)| n.contains(&ip))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nets() {
        assert_eq!(normalize("1.2.3.4").unwrap(), "1.2.3.4");
        assert_eq!(normalize("1.2.3.4/32").unwrap(), "1.2.3.4");
        assert_eq!(normalize("10.1.2.3/8").unwrap(), "10.0.0.0/8");
        assert!(normalize("nope").is_err());
        let b = Bans::default();
        b.set_remote(vec![parse_net("10.0.0.0/8").unwrap()]);
        assert!(b.is_banned("10.9.9.9".parse().unwrap()));
        assert!(!b.is_banned("11.0.0.1".parse().unwrap()));
        b.ban_for("11.0.0.1".parse().unwrap(), Duration::from_secs(60));
        assert!(b.is_banned("11.0.0.1".parse().unwrap()));
        b.ban_for("11.0.0.2".parse().unwrap(), Duration::ZERO);
        assert!(!b.is_banned("11.0.0.2".parse().unwrap()));
    }
}
