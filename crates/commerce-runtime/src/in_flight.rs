//! Tracks admitted work independently of the HTTP caller's lifetime.

use std::{
    future::Future,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::Notify;
use tracing::Instrument;

#[derive(Default)]
struct Inner {
    pending: AtomicUsize,
    idle: Notify,
}

/// Shared by outer v2 dispatch and internal commerce calls. Registration happens
/// before spawning, so shutdown also sees tasks that have not been polled yet.
#[derive(Clone, Default)]
pub struct InFlight(Arc<Inner>);

/// A producer normally awaits its admitted nodes. If that producer is stopped,
/// dropping these JoinHandles detaches the nodes instead of aborting them.
/// Their tracker guards cover the complete operation, including receipt writes
/// after an internal provider request has already returned.
pub(crate) struct AdmittedTasks {
    in_flight: InFlight,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl AdmittedTasks {
    pub fn new(in_flight: InFlight) -> Self {
        Self {
            in_flight,
            tasks: Vec::new(),
        }
    }

    pub fn spawn(&mut self, future: impl Future<Output = ()> + Send + 'static) {
        self.tasks
            .push(tokio::spawn(self.in_flight.track(future).in_current_span()));
    }

    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    pub async fn join(self) -> Vec<tokio::task::JoinError> {
        let mut failures = Vec::new();
        for task in self.tasks {
            if let Err(error) = task.await {
                failures.push(error);
            }
        }
        failures
    }
}

struct CallGuard(Arc<Inner>);

impl Drop for CallGuard {
    fn drop(&mut self) {
        if self.0.pending.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}

impl InFlight {
    /// Wrap the work before spawning it. The returned future owns its guard
    /// and does not borrow this handle.
    pub fn track<F: Future>(&self, future: F) -> impl Future<Output = F::Output> {
        self.0.pending.fetch_add(1, Ordering::AcqRel);
        let guard = CallGuard(self.0.clone());
        async move {
            let _guard = guard;
            future.await
        }
    }

    /// Call after external admissions and background producers have stopped.
    /// Returns false when work remains at the supplied shutdown deadline.
    pub async fn drain(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let idle = self.0.idle.notified();
            tokio::pin!(idle);
            // Register before checking the counter: do not lose the last
            // completion notification between the check and the await.
            idle.as_mut().enable();
            if self.0.pending.load(Ordering::Acquire) == 0 {
                return true;
            }
            if tokio::time::timeout_at(deadline, idle).await.is_err() {
                return self.0.pending.load(Ordering::Acquire) == 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unpolled_calls_are_registered_and_dropping_them_releases_the_guard() {
        let calls = InFlight::default();
        let waiting = calls.track(std::future::pending::<()>());
        assert!(!calls.drain(Duration::ZERO).await);
        drop(waiting);
        assert!(calls.drain(Duration::ZERO).await);
    }

    #[tokio::test]
    async fn drain_waits_for_detached_work_and_obeys_its_deadline() {
        let calls = InFlight::default();
        let (finish, finished) = tokio::sync::oneshot::channel();
        let work = tokio::spawn(calls.track(async { finished.await.unwrap() }));
        drop(work); // The HTTP caller disappears; admitted work remains alive.
        assert!(!calls.drain(Duration::ZERO).await);
        finish.send(()).unwrap();
        assert!(calls.drain(Duration::from_secs(1)).await);
    }

    #[tokio::test]
    async fn stopping_a_producer_keeps_admitted_nodes_alive_through_receipt_persistence() {
        let calls = InFlight::default();
        let background = crate::background::BackgroundTasks::default();
        let (provider_done, provider_result) = tokio::sync::oneshot::channel();
        let (persist, allowed_to_persist) = tokio::sync::oneshot::channel();
        let (receipt_saved, receipt) = tokio::sync::oneshot::channel();
        let mut nodes = AdmittedTasks::new(calls.clone());
        background.spawn(async move {
            nodes.spawn(async {
                provider_done.send(()).unwrap();
                allowed_to_persist.await.unwrap();
                receipt_saved.send("durable receipt").unwrap();
            });
            assert!(nodes.join().await.is_empty());
        });
        provider_result.await.unwrap();
        background.stop().await;
        assert!(!calls.drain(Duration::ZERO).await);
        persist.send(()).unwrap();
        assert_eq!(receipt.await.unwrap(), "durable receipt");
        assert!(calls.drain(Duration::from_secs(1)).await);
    }
}
