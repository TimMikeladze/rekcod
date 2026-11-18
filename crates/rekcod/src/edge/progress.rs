//! A request body on its way to an instance. It has to keep moving (a chunk at least every
//! `idle`), and it says when it's done, so the response timer starts after the upload
//! instead of counting it.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use hyper::body::{Body as HttpBody, Frame, SizeHint};
use tokio::sync::oneshot;
use tokio::time::Sleep;

use super::{Body, BoxError};

/// The client sent nothing for the body's idle time.
#[derive(Debug)]
pub struct Stalled;

impl std::fmt::Display for Stalled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("request body stalled")
    }
}

impl std::error::Error for Stalled {}

pub struct Progress {
    inner: Body,
    idle: Duration,
    sleep: Pin<Box<Sleep>>,
    /// Dropped when the body has ended (or is dropped itself), which the receiver sees.
    done: Option<oneshot::Sender<()>>,
    watch: Arc<Watch>,
}

/// What [`Progress`] tells the request that sends it.
pub struct Watch {
    /// When the last chunk went through, or the body was first asked for.
    last: std::sync::Mutex<tokio::time::Instant>,
    /// The instance asked for more and the client hasn't sent it yet.
    waiting: AtomicBool,
}

impl Watch {
    /// The instance stopped taking the body: nothing asked for a chunk for `idle`. (A client
    /// that stops sending is the body's own 408.)
    pub fn instance_stalled(&self, idle: Duration) -> bool {
        !self.waiting.load(Ordering::Relaxed) && self.last.lock().unwrap_or_else(std::sync::PoisonError::into_inner).elapsed() >= idle
    }
}

impl Progress {
    pub fn new(inner: Body, idle: Duration) -> (Progress, oneshot::Receiver<()>, Arc<Watch>) {
        let (tx, rx) = oneshot::channel();
        let done = (!inner.is_end_stream()).then_some(tx);
        let watch = Arc::new(Watch { last: std::sync::Mutex::new(tokio::time::Instant::now()), waiting: AtomicBool::new(false) });
        (Progress { inner, idle, sleep: Box::pin(tokio::time::sleep(idle)), done, watch: watch.clone() }, rx, watch)
    }
}

impl HttpBody for Progress {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(None) => {
                this.done = None;
                Poll::Ready(None)
            }
            Poll::Ready(Some(f)) => {
                let now = tokio::time::Instant::now();
                this.sleep.as_mut().reset(now + this.idle);
                *this.watch.last.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = now;
                this.watch.waiting.store(false, Ordering::Relaxed);
                if this.inner.is_end_stream() {
                    this.done = None;
                }
                Poll::Ready(Some(f))
            }
            Poll::Pending => {
                this.watch.waiting.store(true, Ordering::Relaxed);
                match this.sleep.as_mut().poll(cx) {
                    Poll::Ready(()) => Poll::Ready(Some(Err(Box::new(Stalled)))),
                    Poll::Pending => Poll::Pending,
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
