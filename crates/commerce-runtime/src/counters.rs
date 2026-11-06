//! In-process counters, flushed on a timer.
//!
//! The shared core of [`crate::discovery_traffic`] and
//! [`crate::webmcp_traffic`]: a buffer behind a mutex, a flush loop on the
//! background task set, a consecutive-failure cut-out, and an opt-out switch.
//! Each tracker supplies only what differs — the keys it counts and the SQL
//! that writes them — through [`CounterBuffer`].
//!
//! Every guarantee the trackers make lives here, once:
//!
//!   * recording is non-blocking and never panics (a poisoned mutex drops the
//!     sample; a disabled tracker is a no-op);
//!   * every buffer is bounded (see [`bump`]), and what the caps drop is
//!     counted and reported once per flush;
//!   * a flush that keeps failing stops the tracker instead of logging the
//!     same missing-relation error every interval forever.

use std::{
    collections::HashMap,
    future::Future,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tracing::{info, warn};

/// How often buffered counters are written. A caller hitting every minute is
/// counted the same whether we flush at 10s or 60s, so this is chosen for
/// bounded memory and few round-trips, not for freshness.
const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_secs(15);

/// Consecutive failed flushes after which a tracker stops recording. Three
/// covers a transient database blip; a persistent failure is a deployment
/// problem (migration not applied), and retrying it every interval only spams
/// the log.
const MAX_CONSECUTIVE_FLUSH_FAILURES: u32 = 3;

/// What one tracker counts, and how it is written.
pub(crate) trait CounterBuffer: Default + Send + 'static {
    /// Nothing to write: the flush is skipped without touching the pool.
    fn is_empty(&self) -> bool;
    /// Updates dropped because a cap was hit, reported once per flush.
    fn dropped(&self) -> u64;
    /// Write the batch. Each aggregate independently, so a failure in one does
    /// not discard the others; `true` only when every write succeeded.
    fn flush(&self, pool: &PgPool) -> impl Future<Output = bool> + Send;
}

/// The knobs one tracker is configured by — its name in the log, the
/// environment switch that turns it off, the interval override, and the
/// migration to name when writes keep failing.
pub(crate) struct Spec {
    pub name: &'static str,
    pub enabled_var: &'static str,
    pub interval_var: &'static str,
    pub migration: &'static str,
}

/// Handle stored on `AppState`. Cloning is cheap; every method is non-blocking.
pub(crate) struct Tracker<B: CounterBuffer> {
    inner: Option<Arc<Inner<B>>>,
}

impl<B: CounterBuffer> Clone for Tracker<B> {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

struct Inner<B> {
    buffer: Mutex<B>,
    pool: PgPool,
    spec: &'static Spec,
    /// Cleared after [`MAX_CONSECUTIVE_FLUSH_FAILURES`] failed flushes.
    live: AtomicBool,
    consecutive_failures: AtomicU32,
}

impl<B: CounterBuffer> Tracker<B> {
    /// A tracker that records nothing. Used when tracking is switched off and
    /// in tests.
    pub fn disabled() -> Self {
        Self { inner: None }
    }

    /// Start the tracker and its flush loop, unless `spec.enabled_var` says
    /// not to.
    pub fn launch(pool: PgPool, tasks: &crate::background::BackgroundTasks, spec: &'static Spec) -> Self {
        if !crate::util::env_bool(spec.enabled_var, true) {
            info!("{} tracking disabled ({}=false)", spec.name, spec.enabled_var);
            return Self::disabled();
        }
        let interval = std::env::var(spec.interval_var)
            .ok()
            .and_then(|raw| raw.trim().parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_FLUSH_INTERVAL);
        let inner = Arc::new(Inner::new(pool, spec));
        let flusher = inner.clone();
        tasks.spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                if !flusher.live.load(Ordering::Relaxed) {
                    break;
                }
                flusher.flush().await;
            }
        });
        info!("{} tracking enabled (flush every {}ms)", spec.name, interval.as_millis());
        Self { inner: Some(inner) }
    }

    /// Record one sample: `record` runs with the buffer locked and the current
    /// time, unless the tracker is disabled, has stopped, or the lock is
    /// poisoned — in which case the sample is dropped, never panicked on.
    pub fn record(&self, record: impl FnOnce(&mut B, DateTime<Utc>)) {
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        if !inner.live.load(Ordering::Relaxed) {
            return;
        }
        let now = Utc::now();
        let Ok(mut guard) = inner.buffer.lock() else {
            return;
        };
        record(&mut guard, now);
    }

    /// A live tracker whose pool is never connected: `flush` is the only thing
    /// that touches it, and tests never call that.
    #[cfg(test)]
    pub fn in_memory(spec: &'static Spec) -> Self {
        let pool = PgPool::connect_lazy("postgres://localhost/postgres").expect("lazy pool");
        Self { inner: Some(Arc::new(Inner::new(pool, spec))) }
    }

    /// Read the buffer as it stands. `None` for a disabled tracker.
    #[cfg(test)]
    pub fn peek<R>(&self, read: impl FnOnce(&B) -> R) -> Option<R> {
        let inner = self.inner.as_ref()?;
        let guard = inner.buffer.lock().ok()?;
        Some(read(&guard))
    }
}

impl<B: CounterBuffer> Inner<B> {
    fn new(pool: PgPool, spec: &'static Spec) -> Self {
        Self {
            buffer: Mutex::new(B::default()),
            pool,
            spec,
            live: AtomicBool::new(true),
            consecutive_failures: AtomicU32::new(0),
        }
    }

    /// Write the buffered counters, then reset or advance the failure count.
    async fn flush(&self) {
        let batch = {
            let Ok(mut buffer) = self.buffer.lock() else {
                return;
            };
            if buffer.is_empty() {
                return;
            }
            std::mem::take(&mut *buffer)
        };
        if batch.dropped() > 0 {
            warn!(
                "{} tracking dropped {} counter updates (buffer caps hit)",
                self.spec.name,
                batch.dropped()
            );
        }
        if batch.flush(&self.pool).await {
            self.consecutive_failures.store(0, Ordering::Relaxed);
            return;
        }
        let failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        if failures >= MAX_CONSECUTIVE_FLUSH_FAILURES {
            self.live.store(false, Ordering::Relaxed);
            warn!(
                "{} tracking stopped after {failures} failed flushes — most likely migration {} \
                 has not been applied. Apply it and restart the gateway to resume counting.",
                self.spec.name, self.spec.migration
            );
        }
    }
}

/// Add `by` to `key`'s count, or insert it — unless the map is at `max_keys`,
/// in which case the update is dropped and counted. An existing key keeps
/// counting after the cap is hit.
pub(crate) fn bump<K: std::hash::Hash + Eq>(
    map: &mut HashMap<K, i64>,
    key: K,
    by: i64,
    max_keys: usize,
    dropped: &mut u64,
) {
    if let Some(count) = map.get_mut(&key) {
        *count += by;
    } else if map.len() < max_keys {
        map.insert(key, by);
    } else {
        *dropped += 1;
    }
}

/// `true` when the write succeeded; a failure is logged with the aggregate's
/// name and the batch size, and reported as `false`.
pub(crate) fn report(name: &str, keys: usize, result: Result<impl Sized, sqlx::Error>) -> bool {
    match result {
        Ok(_) => true,
        Err(err) => {
            warn!("{name} flush failed ({keys} keys): {err}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_caps_drop_new_keys_instead_of_growing_without_bound() {
        let mut map: HashMap<u32, i64> = HashMap::new();
        let mut dropped = 0;
        bump(&mut map, 1, 1, 2, &mut dropped);
        bump(&mut map, 2, 1, 2, &mut dropped);
        bump(&mut map, 3, 1, 2, &mut dropped);
        assert_eq!(map.len(), 2);
        assert_eq!(dropped, 1);
        // An existing key keeps counting after the cap is hit.
        bump(&mut map, 1, 1, 2, &mut dropped);
        assert_eq!(map[&1], 2);
        assert_eq!(dropped, 1);
    }

    #[derive(Default)]
    struct Hits(i64);
    impl CounterBuffer for Hits {
        fn is_empty(&self) -> bool {
            self.0 == 0
        }
        fn dropped(&self) -> u64 {
            0
        }
        async fn flush(&self, _pool: &PgPool) -> bool {
            true
        }
    }
    static HITS: Spec = Spec {
        name: "hits",
        enabled_var: "HITS_TRACKING_ENABLED",
        interval_var: "HITS_FLUSH_INTERVAL_MS",
        migration: "none",
    };

    #[test]
    fn a_disabled_tracker_records_nothing_and_never_panics() {
        let tracker: Tracker<Hits> = Tracker::disabled();
        tracker.record(|hits, _| hits.0 += 1);
        assert!(tracker.peek(|hits| hits.0).is_none());
    }

    #[tokio::test]
    async fn a_live_tracker_counts_under_the_lock() {
        let tracker = Tracker::<Hits>::in_memory(&HITS);
        tracker.record(|hits, _| hits.0 += 2);
        tracker.record(|hits, _| hits.0 += 3);
        assert_eq!(tracker.peek(|hits| hits.0), Some(5));
    }
}
