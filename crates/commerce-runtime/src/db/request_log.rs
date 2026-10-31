//! Bound best-effort logging without cancelling a database operation mid-flight.
use std::{future::Future, sync::Arc};
use tokio::sync::Semaphore;

#[derive(Clone)]
pub(crate) struct RequestLogQueue(Arc<Semaphore>);

impl RequestLogQueue {
    pub(crate) fn new() -> Self {
        Self(Arc::new(Semaphore::new(32)))
    }

    pub(crate) fn submit<F>(&self, write: F) -> bool
    where
        F: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let Ok(permit) = self.0.clone().try_acquire_owned() else {
            return false;
        };
        tokio::spawn(async move {
            let _permit = permit;
            // The pool bounds acquisition, PostgreSQL bounds statement/lock
            // execution. Let SQLx consume the result and return the connection
            // instead of dropping its future at an arbitrary 250ms deadline.
            if let Err(error) = write.await {
                tracing::warn!("Failed to log gateway request event: {error}");
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
        for _ in 0..32 {
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
        assert_eq!(queue.0.available_permits(), 0);
        gate.add_permits(32);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while completed.load(Ordering::SeqCst) != 32 || queue.0.available_permits() != 32 {
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
            while queue.0.available_permits() != 32 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
