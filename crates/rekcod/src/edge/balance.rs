//! Picks an instance and hands back an HTTP/1.1 connection to it.
//!
//! Power of two choices on peak EWMA latency times in-flight requests. The latency decays
//! with time (half-life about 7 s), so an instance that answered slowly once gets traffic
//! again instead of waiting for a request that would lower it. Connecting happens
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
/// Time constant of the latency decay.
const TAU_MS: f64 = 10_000.0;
static EPOCH: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);

fn now_ms() -> u64 {
    EPOCH.elapsed().as_millis() as u64
}

#[derive(Default)]
struct Stats {
    in_flight: AtomicUsize,
    /// Microseconds, as of `updated_ms`.
    ewma: AtomicU64,
    updated_ms: AtomicU64,
    down_until: Mutex<Option<Instant>>,
    idle: Mutex<Vec<SendRequest<Body>>>,
}

impl Stats {
    /// The latency estimate at `now`, decayed since it was last updated.
    fn latency(&self, now: u64) -> u64 {
        let age = now.saturating_sub(self.updated_ms.load(Ordering::Relaxed)) as f64;
        (self.ewma.load(Ordering::Relaxed) as f64 * (-age / TAU_MS).exp()) as u64
    }

    fn score(&self, now: u64) -> u64 {
        (self.latency(now) + 1) * (self.in_flight.load(Ordering::Relaxed) as u64 + 1)
    }

    fn is_down(&self) -> bool {
        self.down_until.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_some_and(|t| t > Instant::now())
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
    /// The connection was kept from an earlier request, so the instance may have closed it.
    pub pooled: bool,
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
        let now = now_ms();
        let old = self.stats.latency(now);
        // Peak: a slow response counts in full right away, a fast one decays it.
        let new = if rtt > old { rtt } else { (old * 7 + rtt * 3) / 10 };
        self.stats.ewma.store(new, Ordering::Relaxed);
        self.stats.updated_ms.store(now, Ordering::Relaxed);
        if let Some(s) = self.sender.take().filter(|s| reuse && !s.is_closed()) {
            let mut idle = self.stats.idle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
        self.stats.lock().unwrap_or_else(std::sync::PoisonError::into_inner).entry(ep.to_string()).or_default().clone()
    }

    /// Forgets instances no route mentions any more.
    pub fn retain(&self, live: &std::collections::HashSet<String>) {
        self.stats.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|ep, _| live.contains(ep));
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
        self.order_at(eps, now_ms())
    }

    fn order_at(&self, eps: &[String], now: u64) -> Vec<(String, Arc<Stats>)> {
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
                let first = if all[a].1.score(now) <= all[b].1.score(now) { a } else { b };
                all.swap(0, first);
            } else if let Some(&only) = up.first() {
                all.swap(0, only);
            }
            all[1..].sort_by_key(|(_, s)| (s.is_down(), s.score(now)));
        }
        all
    }

    /// A connection to the best instance that answers, or `None` when none did.
    pub async fn lease(&self, eps: &[String]) -> Option<Lease> {
        for (ep, stats) in self.order(eps) {
            let pooled = {
                let mut idle = stats.idle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                idle.retain(|s| !s.is_closed());
                idle.iter().position(|s| s.is_ready()).map(|i| idle.swap_remove(i))
            };
            let was_pooled = pooled.is_some();
            let sender = match pooled {
                Some(s) => Some(s),
                None => connect(&ep).await,
            };
            match sender {
                Some(sender) => {
                    *stats.down_until.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                    stats.in_flight.fetch_add(1, Ordering::Relaxed);
                    return Some(Lease { endpoint: ep, pooled: was_pooled, sender: Some(sender), stats, start: Instant::now() });
                }
                None => *stats.down_until.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now() + DOWN_FOR),
            }
        }
        None
    }
}

impl Pool {
    /// A new connection to `ep`, for a request that failed on a pooled one.
    pub async fn fresh(&self, ep: &str) -> Option<Lease> {
        let stats = self.stats(ep);
        let sender = connect(ep).await?;
        stats.in_flight.fetch_add(1, Ordering::Relaxed);
        Some(Lease { endpoint: ep.to_string(), pooled: false, sender: Some(sender), stats, start: Instant::now() })
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
        *p.stats("b:1").down_until.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now() + DOWN_FOR);
        let o = p.order(&eps);
        assert_eq!(o.iter().map(|(e, _)| e.as_str()).collect::<Vec<_>>(), ["a:1", "b:1"]);
    }

    #[test]
    fn one_slow_answer_wears_off() {
        let p = Pool::default();
        let eps: Vec<String> = vec!["slow:1".into(), "fast:1".into()];
        // slow:1 took 3 s once; fast:1 keeps answering in 20 ms.
        p.stats("slow:1").ewma.store(3_000_000, Ordering::Relaxed);
        p.stats("fast:1").ewma.store(20_000, Ordering::Relaxed);
        assert!((0..20).all(|_| p.order_at(&eps, 0)[0].0 == "fast:1"));
        let later = 6 * TAU_MS as u64;
        p.stats("fast:1").updated_ms.store(later, Ordering::Relaxed);
        assert!((0..20).all(|_| p.order_at(&eps, later)[0].0 == "slow:1"));
    }
}
