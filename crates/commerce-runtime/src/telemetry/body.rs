//! A pass-through `http_body::Body` wrapper that COUNTS bytes without buffering,
//! preserving streaming, backpressure and errors (§2.6). It never copies or
//! deserialises the payload — it only tallies the length of each data frame as
//! it flows through, and fires a one-shot completion callback when the stream
//! ends (normally, on error, or on drop = client disconnect).
//!
//! This is only ever attached when telemetry is ENABLED; when disabled the
//! middleware leaves bodies untouched, so behaviour is byte-identical.

use std::pin::Pin;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use bytes::Bytes;
use http_body::{Body as HttpBody, Frame, SizeHint};

type DoneCb = Box<dyn FnOnce(i64, bool) + Send>;

/// Wraps an `axum::body::Body`, tallying every data byte into a shared counter
/// and invoking `on_done(total_bytes, ended_ok)` exactly once when the stream
/// terminates. `ended_ok` is false if the body was dropped before completion
/// (client disconnect) or ended in an error.
pub struct CountingBody {
    inner: Body,
    counter: Arc<AtomicI64>,
    on_done: Option<DoneCb>,
    ended: bool,
}

impl CountingBody {
    pub fn new(inner: Body, counter: Arc<AtomicI64>) -> Self {
        Self {
            inner,
            counter,
            on_done: None,
            ended: false,
        }
    }

    /// Attach a completion callback. Fired at most once.
    pub fn on_done<F>(mut self, cb: F) -> Self
    where
        F: FnOnce(i64, bool) + Send + 'static,
    {
        self.on_done = Some(Box::new(cb));
        self
    }

    fn finish(&mut self, ok: bool) {
        if self.ended {
            return;
        }
        self.ended = true;
        if let Some(cb) = self.on_done.take() {
            cb(self.counter.load(Ordering::Relaxed), ok);
        }
    }
}

impl HttpBody for CountingBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.as_mut().get_mut();
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    this.counter.fetch_add(data.len() as i64, Ordering::Relaxed);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(e))) => {
                this.finish(false);
                Poll::Ready(Some(Err(e)))
            }
            Poll::Ready(None) => {
                this.finish(true);
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for CountingBody {
    fn drop(&mut self) {
        // If the body was never fully polled (client disconnect / early abort),
        // report what we saw with ended_ok = false.
        self.finish(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[tokio::test]
    async fn counts_streamed_bytes_and_signals_completion() {
        let counter = Arc::new(AtomicI64::new(0));
        let done_bytes = Arc::new(AtomicI64::new(-1));
        let done_ok = Arc::new(AtomicBool::new(false));

        let db = done_bytes.clone();
        let dok = done_ok.clone();
        let body = Body::from("hello world"); // 11 bytes
        let counting = CountingBody::new(body, counter.clone()).on_done(move |bytes, ok| {
            db.store(bytes, Ordering::Relaxed);
            dok.store(ok, Ordering::Relaxed);
        });

        // Drive the body to completion (as the server write-loop would).
        let collected = axum::body::to_bytes(Body::new(counting), usize::MAX)
            .await
            .unwrap();

        // Bytes are preserved exactly (pass-through, no mutation).
        assert_eq!(&collected[..], b"hello world");
        assert_eq!(counter.load(Ordering::Relaxed), 11);
        assert_eq!(done_bytes.load(Ordering::Relaxed), 11);
        assert!(done_ok.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn multi_chunk_stream_preserved_and_totalled() {
        // Two chunks → 3 + 4 = 7 bytes, order preserved.
        let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
            Ok(Bytes::from_static(b"abc")),
            Ok(Bytes::from_static(b"defg")),
        ];
        let body = Body::from_stream(futures_util::stream::iter(chunks));

        let counter = Arc::new(AtomicI64::new(0));
        let counting = CountingBody::new(body, counter.clone());
        let collected = axum::body::to_bytes(Body::new(counting), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&collected[..], b"abcdefg");
        assert_eq!(counter.load(Ordering::Relaxed), 7);
    }

    #[tokio::test]
    async fn dropped_before_completion_signals_not_ok() {
        let counter = Arc::new(AtomicI64::new(0));
        let done_ok = Arc::new(AtomicBool::new(true));
        let dok = done_ok.clone();
        let counting = CountingBody::new(Body::from("data"), counter).on_done(move |_, ok| {
            dok.store(ok, Ordering::Relaxed);
        });
        // Drop without polling to completion.
        drop(counting);
        assert!(!done_ok.load(Ordering::Relaxed));
    }
}
