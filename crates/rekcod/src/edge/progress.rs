//! A request body on its way to an instance. It has to keep moving (a chunk at least every
//! `idle`), and it says when it's done, so the response timer starts after the upload
//! instead of counting it.

use std::pin::Pin;
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
}

impl Progress {
    pub fn new(inner: Body, idle: Duration) -> (Progress, oneshot::Receiver<()>) {
        let (tx, rx) = oneshot::channel();
        let done = (!inner.is_end_stream()).then_some(tx);
        (Progress { inner, idle, sleep: Box::pin(tokio::time::sleep(idle)), done }, rx)
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
                let deadline = tokio::time::Instant::now() + this.idle;
                this.sleep.as_mut().reset(deadline);
                if this.inner.is_end_stream() {
                    this.done = None;
                }
                Poll::Ready(Some(f))
            }
            Poll::Pending => match this.sleep.as_mut().poll(cx) {
                Poll::Ready(()) => Poll::Ready(Some(Err(Box::new(Stalled)))),
                Poll::Pending => Poll::Pending,
            },
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
