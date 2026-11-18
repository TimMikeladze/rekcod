//! When a connection last moved a byte, and when its current request head started.
//!
//! `edge::conn` closes a connection that moves nothing for the idle timeout, including one
//! whose response the client stopped reading, and one whose request head takes longer than
//! the header timeout from its first byte. hyper's own header timer starts as soon as a
//! keep-alive connection goes idle, so it gets the idle timeout instead.

use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// When a connection last moved a byte, and how many of its requests are being handled.
pub struct Activity {
    start: Instant,
    last_ms: AtomicU64,
    in_flight: AtomicUsize,
    /// 1 + ms since `start` when the first byte of a request head arrived; 0 between requests.
    head_ms: AtomicU64,
    read_any: AtomicBool,
    /// HTTP/2 frames between requests aren't a request head.
    h2: AtomicBool,
}

impl Activity {
    pub fn new() -> Activity {
        Activity {
            start: Instant::now(),
            last_ms: AtomicU64::new(0),
            in_flight: AtomicUsize::new(0),
            head_ms: AtomicU64::new(0),
            read_any: AtomicBool::new(false),
            h2: AtomicBool::new(false),
        }
    }

    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn touch(&self) {
        self.last_ms.store(self.now_ms(), Ordering::Relaxed);
    }

    fn read(&self, bytes: &[u8]) {
        self.touch();
        if !self.read_any.swap(true, Ordering::Relaxed) {
            const PREFACE: &[u8] = b"PRI * HTTP/2.0";
            let n = bytes.len().min(PREFACE.len());
            self.h2.store(bytes[..n] == PREFACE[..n], Ordering::Relaxed);
        }
        if self.in_flight.load(Ordering::Relaxed) == 0 && !self.h2.load(Ordering::Relaxed) {
            let _ = self.head_ms.compare_exchange(0, self.now_ms() + 1, Ordering::Relaxed, Ordering::Relaxed);
        }
    }

    /// A request head has been arriving for longer than `limit` without reaching the handler.
    pub fn head_overdue(&self, limit: Duration) -> bool {
        let at = self.head_ms.load(Ordering::Relaxed);
        at != 0 && self.in_flight.load(Ordering::Relaxed) == 0 && self.now_ms().saturating_sub(at - 1) >= limit.as_millis() as u64
    }

    /// Zero while a request is being handled, however long the instance takes.
    pub fn idle(&self) -> Duration {
        if self.in_flight.load(Ordering::Relaxed) > 0 {
            return Duration::ZERO;
        }
        self.start.elapsed().saturating_sub(Duration::from_millis(self.last_ms.load(Ordering::Relaxed)))
    }

    pub fn busy(&self) -> Busy<'_> {
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        self.head_ms.store(0, Ordering::Relaxed);
        Busy(self)
    }
}

pub struct Busy<'a>(&'a Activity);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::Relaxed);
        self.0.head_ms.store(0, Ordering::Relaxed);
        self.0.touch();
    }
}

/// The connection's stream, noting every read and write in `Activity`.
pub struct Tracked<T> {
    pub io: T,
    pub activity: std::sync::Arc<Activity>,
}

impl<T: AsyncRead + Unpin> AsyncRead for Tracked<T> {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let r = Pin::new(&mut this.io).poll_read(cx, buf);
        if buf.filled().len() > before {
            this.activity.read(&buf.filled()[before..]);
        }
        r
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Tracked<T> {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let r = Pin::new(&mut this.io).poll_write(cx, buf);
        if matches!(r, Poll::Ready(Ok(n)) if n > 0) {
            this.activity.touch();
        }
        r
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_head_is_timed_from_its_first_byte_until_the_handler() {
        let a = Activity::new();
        assert!(!a.head_overdue(Duration::ZERO));
        a.read(b"GET / HT");
        assert!(a.head_overdue(Duration::ZERO));
        drop(a.busy());
        assert!(!a.head_overdue(Duration::ZERO), "a finished request clears it");
        let h2 = Activity::new();
        h2.read(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n");
        h2.read(b"ping frame");
        assert!(!h2.head_overdue(Duration::ZERO));
    }
}
