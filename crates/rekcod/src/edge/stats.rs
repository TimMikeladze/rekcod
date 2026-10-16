//! Requests, 5xx and latency per app host per minute, and per client IP, for the controller
//! (`POST /routes/activity`). A minute is reported once it's over; clients every report.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;

use serde_json::{Value, json};

/// Latencies kept per host per minute for the percentiles; later ones only count.
const MAX_SAMPLES: usize = 10_000;
/// Client IPs sent per report, busiest first.
const TOP_CLIENTS: usize = 20;

#[derive(Default)]
struct Minute {
    requests: u64,
    errors: u64,
    ms: Vec<u32>,
}

#[derive(Default)]
pub struct Stats {
    minutes: Mutex<HashMap<(String, i64), Minute>>,
    clients: Mutex<HashMap<IpAddr, (u64, u64)>>,
}

fn percentile(sorted: &[u32], p: f64) -> u32 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

impl Stats {
    /// One answered request. `host` is only counted for a known app; `client` is `None` for
    /// a trusted proxy (banning it would ban everyone behind it, so it isn't reported).
    pub fn request(&self, minute: i64, host: Option<&str>, client: Option<IpAddr>, status: u16, ms: u32) {
        if let Some(ip) = client {
            self.clients.lock().unwrap().entry(ip).or_default().0 += 1;
        }
        let Some(host) = host else { return };
        let mut m = self.minutes.lock().unwrap();
        let e = m.entry((host.to_string(), minute)).or_default();
        e.requests += 1;
        if status >= 500 {
            e.errors += 1;
        }
        if e.ms.len() < MAX_SAMPLES {
            e.ms.push(ms);
        }
    }

    /// A request refused with 429.
    pub fn limited(&self, client: IpAddr) {
        self.clients.lock().unwrap().entry(client).or_default().1 += 1;
    }

    /// Finished minutes (before `now_minute`) and the clients since the last call; both are
    /// removed from here.
    pub fn take(&self, now_minute: i64) -> (Vec<Value>, Vec<Value>) {
        let mut m = self.minutes.lock().unwrap();
        let done: Vec<(String, i64)> = m.keys().filter(|(_, min)| *min < now_minute).cloned().collect();
        let traffic = done
            .into_iter()
            .filter_map(|k| {
                let mut v = m.remove(&k)?;
                v.ms.sort_unstable();
                Some(json!({"host": k.0, "minute": k.1, "requests": v.requests, "errors": v.errors,
                            "p50_ms": percentile(&v.ms, 0.5), "p95_ms": percentile(&v.ms, 0.95)}))
            })
            .collect();
        drop(m);
        let mut c: Vec<(IpAddr, (u64, u64))> = self.clients.lock().unwrap().drain().collect();
        c.sort_by_key(|x| std::cmp::Reverse(x.1.0 + x.1.1));
        let clients =
            c.into_iter().take(TOP_CLIENTS).map(|(ip, (n, l))| json!({"ip": ip.to_string(), "requests": n, "limited": l})).collect();
        (traffic, clients)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_finished_minutes_with_percentiles() {
        let s = Stats::default();
        let ip: IpAddr = "203.0.113.9".parse().unwrap();
        for ms in 1..=100 {
            s.request(10, Some("web.apps.test"), Some(ip), if ms > 98 { 502 } else { 200 }, ms);
        }
        s.request(11, Some("web.apps.test"), Some(ip), 200, 5);
        s.request(11, None, Some(ip), 404, 1);
        s.request(11, None, None, 200, 1);
        s.limited(ip);
        let (t, c) = s.take(11);
        assert_eq!(t.len(), 1, "minute 11 isn't over yet");
        assert_eq!(t[0]["requests"], 100);
        assert_eq!(t[0]["errors"], 2);
        assert_eq!(t[0]["p50_ms"], 51);
        assert_eq!(t[0]["p95_ms"], 95);
        assert_eq!(c[0]["requests"], 102);
        assert_eq!(c[0]["limited"], 1);
        let (t, c) = s.take(12);
        assert_eq!(t[0]["minute"], 11);
        assert!(c.is_empty());
    }
}
