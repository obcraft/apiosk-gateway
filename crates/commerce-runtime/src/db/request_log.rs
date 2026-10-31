//! Bound best-effort logging without cancelling a database operation mid-flight.
use std::{future::Future, sync::Arc};
use tokio::sync::Semaphore;

// Bound both running writes and tasks waiting for a pool connection.
const MAX_IN_FLIGHT_WRITES: usize = 32;

/// Best-effort request diagnostics only; billing writes must remain awaited.
#[derive(Clone)]
pub(crate) struct RequestLogQueue {
    permits: Arc<Semaphore>,
}

impl RequestLogQueue {
    pub(crate) fn new() -> Self {
        Self {
            permits: Arc::new(Semaphore::new(MAX_IN_FLIGHT_WRITES)),
        }
    }

    /// Returns false immediately on overload without polling the supplied future.
    #[must_use = "handle dropped request diagnostics on overload"]
    pub(crate) fn submit<F>(&self, write: F) -> bool
    where
        F: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let Ok(permit) = self.permits.clone().try_acquire_owned() else {
            return false;
        };
        tokio::spawn(async move {
            let _permit = permit;
            // The pool bounds acquisition, PostgreSQL bounds statement/lock
            // execution. Let SQLx consume the result and return the connection
            // instead of dropping its future at an arbitrary 250ms deadline.
            if let Err(error) = write.await {
                tracing::warn!(error = %error, "Failed to log gateway request event");
            }
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn slow_writes_are_not_cancelled_and_overload_is_bounded() {
        let queue = RequestLogQueue::new();
        let gate = Arc::new(Semaphore::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        for _ in 0..MAX_IN_FLIGHT_WRITES {
            let gate = gate.clone();
            let completed = completed.clone();
            assert!(queue.submit(async move {
                let permit = gate.acquire().await?;
                permit.forget();
                completed.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }));
        }
        assert!(!queue.submit(async { panic!("overflow must not execute") }));
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(completed.load(Ordering::SeqCst), 0);
        assert_eq!(queue.permits.available_permits(), 0);
        gate.add_permits(MAX_IN_FLIGHT_WRITES);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while completed.load(Ordering::SeqCst) != MAX_IN_FLIGHT_WRITES
                || queue.permits.available_permits() != MAX_IN_FLIGHT_WRITES
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(queue.submit(async { Ok(()) }));
    }

    #[tokio::test]
    async fn failure_releases_capacity_without_retrying() {
        let queue = RequestLogQueue::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        assert!(queue.submit(async move {
            observed.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("database unavailable")
        }));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while queue.permits.available_permits() != MAX_IN_FLIGHT_WRITES {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
