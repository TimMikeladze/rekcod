//! Picks an instance and hands back an HTTP/1.1 connection to it.
//!
//! Power of two choices on peak EWMA latency times in-flight requests. Connecting happens
//! before the request body is touched, so a dead instance costs one connect and the request
//! moves on to the next one. A failed instance sits out for [`DOWN_FOR`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hyper::client::conn::http1::{self, SendRequest};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;

use super::Body;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
pub const DOWN_FOR: Duration = Duration::from_secs(5);
/// Idle connections kept per instance.
const IDLE_MAX: usize = 32;

#[derive(Default)]
struct Stats {
    in_flight: AtomicUsize,
    /// Microseconds.
    ewma: AtomicU64,
    down_until: Mutex<Option<Instant>>,
    idle: Mutex<Vec<SendRequest<Body>>>,
}

impl Stats {
    fn score(&self) -> u64 {
        (self.ewma.load(Ordering::Relaxed) + 1) * (self.in_flight.load(Ordering::Relaxed) as u64 + 1)
    }

    fn is_down(&self) -> bool {
        self.down_until.lock().unwrap().is_some_and(|t| t > Instant::now())
    }
}

#[derive(Default)]
pub struct Pool {
    stats: Mutex<HashMap<String, Arc<Stats>>>,
    seed: AtomicU64,
}

/// A request in flight to one instance. Report how long the response headers took with
/// [`Lease::done`]; dropping it without that counts nothing toward latency.
pub struct Lease {
    pub endpoint: String,
    sender: Option<SendRequest<Body>>,
    stats: Arc<Stats>,
    start: Instant,
}

impl Lease {
    pub fn sender(&mut self) -> &mut SendRequest<Body> {
        self.sender.as_mut().expect("until done")
    }

    /// Records latency; `reuse` puts the connection back for the next request.
    pub fn done(mut self, reuse: bool) {
        let rtt = self.start.elapsed().as_micros() as u64;
        let old = self.stats.ewma.load(Ordering::Relaxed);
        // Peak: a slow response counts in full right away, a fast one decays it.
        let new = if rtt > old { rtt } else { (old * 7 + rtt * 3) / 10 };
        self.stats.ewma.store(new, Ordering::Relaxed);
        if let Some(s) = self.sender.take().filter(|s| reuse && !s.is_closed()) {
            let mut idle = self.stats.idle.lock().unwrap();
            if idle.len() < IDLE_MAX {
                idle.push(s);
            }
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.stats.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Pool {
    fn stats(&self, ep: &str) -> Arc<Stats> {
        self.stats.lock().unwrap().entry(ep.to_string()).or_default().clone()
    }

    /// Forgets instances no route mentions any more.
    pub fn retain(&self, live: &std::collections::HashSet<String>) {
        self.stats.lock().unwrap().retain(|ep, _| live.contains(ep));
    }

    fn rand(&self) -> u64 {
        // xorshift; spreading load doesn't need more.
        let mut x = self.seed.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed) ^ 0xD1B5_4A32_D192_ED03;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    }

    /// The order to try instances in: P2C winner first, then the rest; instances that
    /// recently failed go last rather than not at all, in case they all did.
    fn order(&self, eps: &[String]) -> Vec<(String, Arc<Stats>)> {
        let mut all: Vec<(String, Arc<Stats>)> = eps.iter().map(|e| (e.clone(), self.stats(e))).collect();
        if all.len() >= 2 {
            let up: Vec<usize> = (0..all.len()).filter(|&i| !all[i].1.is_down()).collect();
            if up.len() >= 2 {
                let n = up.len() as u64;
                let i = (self.rand() % n) as usize;
                let mut j = (self.rand() % (n - 1)) as usize;
                if j >= i {
                    j += 1;
                }
                let (a, b) = (up[i], up[j]);
                let first = if all[a].1.score() <= all[b].1.score() { a } else { b };
                all.swap(0, first);
            } else if let Some(&only) = up.first() {
                all.swap(0, only);
            }
            all[1..].sort_by_key(|(_, s)| (s.is_down(), s.score()));
        }
        all
    }

    /// A connection to the best instance that answers, or `None` when none did.
    pub async fn lease(&self, eps: &[String]) -> Option<Lease> {
        for (ep, stats) in self.order(eps) {
            let pooled = {
                let mut idle = stats.idle.lock().unwrap();
                idle.retain(|s| !s.is_closed());
                idle.iter().position(|s| s.is_ready()).map(|i| idle.swap_remove(i))
            };
            let sender = match pooled {
                Some(s) => Some(s),
                None => connect(&ep).await,
            };
            match sender {
                Some(sender) => {
                    *stats.down_until.lock().unwrap() = None;
                    stats.in_flight.fetch_add(1, Ordering::Relaxed);
                    return Some(Lease { endpoint: ep, sender: Some(sender), stats, start: Instant::now() });
                }
                None => *stats.down_until.lock().unwrap() = Some(Instant::now() + DOWN_FOR),
            }
        }
        None
    }
}

async fn connect(ep: &str) -> Option<SendRequest<Body>> {
    let s = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(ep)).await.ok()?.ok()?;
    let _ = s.set_nodelay(true);
    let (sender, conn) = http1::handshake(TokioIo::new(s)).await.ok()?;
    tokio::spawn(async move {
        let _ = conn.with_upgrades().await;
    });
    Some(sender)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p2c_prefers_less_loaded_and_skips_down() {
        let p = Pool::default();
        let eps: Vec<String> = vec!["a:1".into(), "b:1".into()];
        p.stats("a:1").in_flight.store(50, Ordering::Relaxed);
        assert!((0..20).all(|_| p.order(&eps)[0].0 == "b:1"));
        *p.stats("b:1").down_until.lock().unwrap() = Some(Instant::now() + DOWN_FOR);
        let o = p.order(&eps);
        assert_eq!(o.iter().map(|(e, _)| e.as_str()).collect::<Vec<_>>(), ["a:1", "b:1"]);
    }
}
