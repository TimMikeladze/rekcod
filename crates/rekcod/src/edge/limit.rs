//! Token buckets per client IP: one edge-wide, and one per app that sets `[http] rate`.
//! Buckets live in this process, so with N edges a client gets N times the rate.

use std::collections::HashMap;
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use governor::clock::{Clock, DefaultClock};
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};

use crate::fleet::HttpSettings;

/// 429s from the edge-wide limit for one IP within a minute before it's banned.
pub const STRIKES: u32 = 100;
pub const STRIKE_WINDOW: Duration = Duration::from_secs(60);

/// `20/s`, `600/m`, `1000/h`: the time between tokens.
pub fn parse_rate(s: &str) -> Result<Duration> {
    let (n, unit) = s.trim().split_once('/').ok_or_else(|| anyhow!("rate {s}: use N/s, N/m or N/h"))?;
    let n: u32 = n.trim().parse().map_err(|_| anyhow!("rate {s}: {n} isn't a number"))?;
    let per = match unit.trim() {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        _ => bail!("rate {s}: use N/s, N/m or N/h"),
    };
    if n == 0 {
        bail!("rate {s} lets nothing through");
    }
    Ok(Duration::from_secs(per) / n)
}

/// `100MB`, `1GiB`, `512KB` or plain bytes. MB and MiB both mean 2^20 here.
pub fn parse_size(s: &str) -> Result<u64> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let n: u64 = s[..split].parse().map_err(|_| anyhow!("size {s}: not a number"))?;
    let mul = match s[split..].trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        u => bail!("size {s}: unknown unit {u}"),
    };
    Ok(n * mul)
}

fn quota(every: Duration, burst: u32) -> Quota {
    let q = Quota::with_period(every.max(Duration::from_nanos(1))).expect("non-zero period");
    q.allow_burst(NonZeroU32::new(burst.max(1)).unwrap())
}

type AppLimiter = (HttpSettings, Arc<DefaultKeyedRateLimiter<IpAddr>>);

/// IPv6 clients are limited per /64: one host usually has a whole /64 and can rotate
/// through it at will.
pub fn rate_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) if v6.to_ipv4_mapped().is_none() => {
            let s = v6.segments();
            IpAddr::V6(std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
        IpAddr::V6(v6) => IpAddr::V4(v6.to_ipv4_mapped().unwrap_or(std::net::Ipv4Addr::UNSPECIFIED)),
        v4 => v4,
    }
}

/// What an auto ban covers: the /64 for IPv6, as the limits count it, else the address.
pub fn ban_net(ip: IpAddr) -> ipnet::IpNet {
    match rate_key(ip) {
        IpAddr::V6(v6) => ipnet::IpNet::V6(ipnet::Ipv6Net::new(v6, 64).expect("valid prefix")),
        v4 => ipnet::IpNet::from(v4),
    }
}

/// A refused request: how long until the next token, and whether the edge-wide limit
/// refused it (only those count toward a ban; an app's own limit stays with that app).
#[derive(Debug)]
pub struct Refused {
    pub wait: Duration,
    pub edge_wide: bool,
}

/// Things one client holds at once (connections, tunnels), counted per [`rate_key`].
pub struct PerClient {
    cap: usize,
    held: Mutex<HashMap<IpAddr, usize>>,
}

/// One of a client's slots, given back on drop.
pub struct Held {
    owner: Arc<PerClient>,
    key: IpAddr,
}

impl PerClient {
    pub fn new(cap: usize) -> Arc<PerClient> {
        Arc::new(PerClient { cap, held: Default::default() })
    }

    /// `None` when `ip` already holds `cap`.
    pub fn take(self: &Arc<Self>, ip: IpAddr) -> Option<Held> {
        let key = rate_key(ip);
        let mut held = self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let n = held.entry(key).or_default();
        if *n >= self.cap {
            return None;
        }
        *n += 1;
        Some(Held { owner: self.clone(), key })
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        let mut held = self.owner.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(n) = held.get_mut(&self.key) {
            *n -= 1;
            if *n == 0 {
                held.remove(&self.key);
            }
        }
    }
}

pub struct Limits {
    edge: DefaultKeyedRateLimiter<IpAddr>,
    /// Per app host: the settings its limiter was built from, so a changed rate rebuilds it.
    apps: Mutex<HashMap<String, AppLimiter>>,
    strikes: Mutex<HashMap<IpAddr, (Instant, u32)>>,
}

impl Limits {
    pub fn new(every: Duration, burst: u32) -> Limits {
        Limits { edge: RateLimiter::keyed(quota(every, burst)), apps: Default::default(), strikes: Default::default() }
    }

    /// `Err` when `ip` is over the edge's or the app's rate.
    pub fn check(&self, ip: IpAddr, host: &str, http: &HttpSettings) -> Result<(), Refused> {
        let ip = rate_key(ip);
        let clock = DefaultClock::default();
        self.edge.check_key(&ip).map_err(|e| Refused { wait: e.wait_time_from(clock.now()), edge_wide: true })?;
        let Some(every) = http.rate.as_deref().and_then(|r| parse_rate(r).ok()) else { return Ok(()) };
        let lim = {
            let mut apps = self.apps.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match apps.get(host) {
                Some((s, l)) if s == http => l.clone(),
                _ => {
                    let burst = http.burst.unwrap_or_else(|| (Duration::from_secs(1).as_nanos() / every.as_nanos().max(1)).max(1) as u32);
                    let l = Arc::new(RateLimiter::keyed(quota(every, burst)));
                    apps.insert(host.to_string(), (http.clone(), l.clone()));
                    l
                }
            }
        };
        lim.check_key(&ip).map_err(|e| Refused { wait: e.wait_time_from(clock.now()), edge_wide: false })
    }

    /// Counts a 429 for `ip`; true once it has had [`STRIKES`] within [`STRIKE_WINDOW`].
    pub fn strike(&self, ip: IpAddr) -> bool {
        let ip = rate_key(ip);
        let mut s = self.strikes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let e = s.entry(ip).or_insert((Instant::now(), 0));
        if e.0.elapsed() > STRIKE_WINDOW {
            *e = (Instant::now(), 0);
        }
        e.1 += 1;
        if e.1 >= STRIKES {
            s.remove(&ip);
            return true;
        }
        false
    }

    /// Drops buckets that have refilled, so memory follows active clients only.
    pub fn prune(&self) {
        self.edge.retain_recent();
        for (_, l) in self.apps.lock().unwrap_or_else(std::sync::PoisonError::into_inner).values() {
            l.retain_recent();
        }
        self.strikes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|_, (t, _)| t.elapsed() <= STRIKE_WINDOW);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_and_sizes() {
        assert_eq!(parse_rate("20/s").unwrap(), Duration::from_millis(50));
        assert_eq!(parse_rate("2/m").unwrap(), Duration::from_secs(30));
        assert!(parse_rate("0/s").is_err());
        assert!(parse_rate("20").is_err());
        assert_eq!(parse_size("100MB").unwrap(), 100 << 20);
        assert_eq!(parse_size("1GiB").unwrap(), 1 << 30);
        assert_eq!(parse_size("512").unwrap(), 512);
        assert!(parse_size("1TB").is_err());
    }

    #[test]
    fn app_rate_and_strikes() {
        let l = Limits::new(Duration::from_millis(1), 1000);
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        let http = HttpSettings { rate: Some("1/m".into()), burst: Some(2), max_body: None };
        assert!(l.check(ip, "a.test", &http).is_ok());
        assert!(l.check(ip, "a.test", &http).is_ok());
        let r = l.check(ip, "a.test", &http).unwrap_err();
        assert!(r.wait > Duration::from_secs(20) && !r.edge_wide, "{r:?}");
        // Another client, and another app without a rate, aren't affected.
        assert!(l.check("192.0.2.2".parse().unwrap(), "a.test", &http).is_ok());
        assert!(l.check(ip, "b.test", &HttpSettings::default()).is_ok());
        assert!((1..STRIKES).all(|_| !l.strike(ip)));
        assert!(l.strike(ip));
        let tight = Limits::new(Duration::from_secs(60), 1);
        assert!(tight.check(ip, "b.test", &HttpSettings::default()).is_ok());
        assert!(tight.check(ip, "b.test", &HttpSettings::default()).unwrap_err().edge_wide);
    }

    #[test]
    fn per_client_slots() {
        let p = PerClient::new(2);
        let (a, b) = ("2001:db8::1".parse().unwrap(), "2001:db8::2".parse().unwrap());
        let one = p.take(a).unwrap();
        let _two = p.take(b).unwrap();
        assert!(p.take(a).is_none(), "same /64");
        assert!(p.take("192.0.2.1".parse().unwrap()).is_some());
        drop(one);
        assert!(p.take(a).is_some());
    }

    #[test]
    fn bans_cover_what_was_counted() {
        assert_eq!(ban_net("2001:db8:0:1::65".parse().unwrap()).to_string(), "2001:db8:0:1::/64");
        assert_eq!(ban_net("::ffff:192.0.2.1".parse().unwrap()).to_string(), "192.0.2.1/32");
        assert_eq!(ban_net("192.0.2.1".parse().unwrap()).to_string(), "192.0.2.1/32");
    }
}
