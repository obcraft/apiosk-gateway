//! A database-loaded value kept fresh in the background (#21). A reader gets
//! the last good value at once while one refresh runs, so a question never
//! waits for the directory query while a usable catalogue exists. A value
//! older than its maximum age is never served: the reader waits for a load,
//! or fails at once while failed loads back off. At most one load runs.
//!
//! Only planning reads this value. Execution re-reads the quoted contracts
//! (`CatalogCache::fresh_execution`) and the executor re-checks activation,
//! contract hashes and prices before any reservation, so a plan from an
//! older catalogue fails with `quote_changed` instead of spending.

use crate::telemetry::{self, Cache, ErrorClass, Outcome, Phase};
use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Age at which a background refresh starts, before the value expires.
    pub refresh_after: Duration,
    /// Age after which a served value is reported stale.
    pub ttl: Duration,
    /// Age after which the value is never served.
    pub max_age: Duration,
    /// Wait after the first failed load; it doubles per consecutive failure.
    pub backoff: Duration,
    pub max_backoff: Duration,
}
impl Policy {
    /// A newly activated or blocked source reaches planning within a minute,
    /// as with the former 60-second TTL. Ten minutes matches a quote's
    /// lifetime: no new quote is planned from an older catalogue.
    pub const CATALOG: Policy = Policy {
        refresh_after: Duration::from_secs(45),
        ttl: Duration::from_secs(60),
        max_age: Duration::from_secs(600),
        backoff: Duration::from_secs(2),
        max_backoff: Duration::from_secs(60),
    };
    fn backoff_after(&self, failures: u32) -> Duration {
        self.backoff.saturating_mul(1u32 << failures.saturating_sub(1).min(16)).min(self.max_backoff)
    }
}

/// Why no value could be served.
#[derive(Debug, thiserror::Error)]
pub enum Unavailable {
    #[error("the last load failed; the next attempt is in {0:?}")]
    BackingOff(Duration),
    #[error(transparent)]
    Load(#[from] anyhow::Error),
}

struct Loaded<T> {
    value: Arc<T>,
    at: Instant,
}
struct State<T> {
    current: Option<Loaded<T>>,
    failures: u32,
    retry_at: Option<Instant>,
    refreshing: bool,
}

type Clock = Box<dyn Fn() -> Instant + Send + Sync>;

pub struct Refreshing<T> {
    policy: Policy,
    clock: Clock,
    state: Mutex<State<T>>,
    /// Held for the duration of every load: the single flight.
    flight: tokio::sync::Mutex<()>,
}

/// How a read is answered: now, or after a load the reader waits for.
enum Read<T> {
    Ready(Arc<T>, Cache),
    Wait(Cache),
}

impl<T: Send + Sync + 'static> Refreshing<T> {
    pub fn new(policy: Policy) -> Arc<Self> {
        Self::with_clock(policy, Box::new(Instant::now))
    }
    fn with_clock(policy: Policy, clock: Clock) -> Arc<Self> {
        Arc::new(Self {
            policy,
            clock,
            state: Mutex::new(State { current: None, failures: 0, retry_at: None, refreshing: false }),
            flight: tokio::sync::Mutex::new(()),
        })
    }
    fn state(&self) -> std::sync::MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    fn servable<'a>(&self, state: &'a State<T>, now: Instant) -> Option<&'a Loaded<T>> {
        state.current.as_ref().filter(|current| now.duration_since(current.at) < self.policy.max_age)
    }

    /// Serves the current value, or waits for a load when none is young
    /// enough. The cache state says which.
    pub async fn get<F, Fut>(self: &Arc<Self>, load: F) -> (Result<Arc<T>, Unavailable>, Cache)
    where
        F: Fn() -> Fut,
        Fut: Future<Output = anyhow::Result<T>> + Send + 'static,
    {
        match self.read(&load) {
            Read::Ready(value, cache) => (Ok(value), cache),
            Read::Wait(cache) => (self.load(load()).await, cache),
        }
    }

    /// The current value when it is young enough, starting one background
    /// refresh once it is due.
    fn read<Fut>(self: &Arc<Self>, load: impl FnOnce() -> Fut) -> Read<T>
    where
        Fut: Future<Output = anyhow::Result<T>> + Send + 'static,
    {
        let now = (self.clock)();
        let mut state = self.state();
        let Some(current) = self.servable(&state, now) else {
            return Read::Wait(if state.current.is_some() { Cache::RefreshWait } else { Cache::FirstLoad });
        };
        let age = now.duration_since(current.at);
        let served = Read::Ready(current.value.clone(), if age < self.policy.ttl { Cache::Hit } else { Cache::StaleServed });
        let backing_off = state.retry_at.is_some_and(|retry| now < retry);
        if age >= self.policy.refresh_after && !state.refreshing && !backing_off {
            state.refreshing = true;
            let work = load();
            let this = self.clone();
            // Deliberately outside the reader's trace: the refresh serves every
            // later question, not the one that happened to start it.
            tokio::spawn(async move { this.refresh(work).await });
        }
        served
    }

    /// Loads a value for readers that have none to serve. Readers waiting
    /// together share one load; while failed loads back off, it fails at once.
    async fn load(&self, work: impl Future<Output = anyhow::Result<T>>) -> Result<Arc<T>, Unavailable> {
        let _flight = self.flight.lock().await;
        {
            let now = (self.clock)();
            let state = self.state();
            if let Some(current) = self.servable(&state, now) {
                return Ok(current.value.clone());
            }
            if let Some(retry) = state.retry_at.filter(|retry| now < *retry) {
                return Err(Unavailable::BackingOff(retry - now));
            }
        }
        self.settle(work.await).map_err(Unavailable::Load)
    }

    /// Loads now, whatever the current value's age, and keeps the result
    /// for later readers. It waits for a load already in flight.
    pub async fn reload(&self, work: impl Future<Output = anyhow::Result<T>>) -> anyhow::Result<Arc<T>> {
        let _flight = self.flight.lock().await;
        self.settle(work.await)
    }

    async fn refresh(&self, work: impl Future<Output = anyhow::Result<T>>) {
        let _flight = self.flight.lock().await;
        let due = self.state().current.as_ref().is_none_or(|current| (self.clock)().duration_since(current.at) >= self.policy.refresh_after);
        if !due {
            // A waiting reader's load finished first; its value is fresh.
            self.state().refreshing = false;
            return;
        }
        let span = telemetry::span(Phase::Catalog).label("background_refresh");
        match self.settle(work.await) {
            Ok(_) => span.finish(Outcome::Ok),
            Err(error) => {
                tracing::warn!(error = %error, "background catalog refresh failed; serving the last loaded value");
                span.finish(Outcome::Failed(ErrorClass::Db));
            }
        }
    }

    fn settle(&self, result: anyhow::Result<T>) -> anyhow::Result<Arc<T>> {
        let now = (self.clock)();
        let mut state = self.state();
        state.refreshing = false;
        match result {
            Ok(value) => {
                let value = Arc::new(value);
                state.current = Some(Loaded { value: value.clone(), at: now });
                state.failures = 0;
                state.retry_at = None;
                Ok(value)
            }
            Err(error) => {
                state.failures = state.failures.saturating_add(1);
                state.retry_at = Some(now + self.policy.backoff_after(state.failures));
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::SeqCst};
    use tokio::sync::Notify;

    const POLICY: Policy = Policy::CATALOG;

    /// A clock the test moves forward by hand.
    struct Time {
        start: Instant,
        offset_ms: AtomicU64,
    }
    impl Time {
        fn new() -> Arc<Self> {
            Arc::new(Self { start: Instant::now(), offset_ms: AtomicU64::new(0) })
        }
        fn advance(&self, by: Duration) {
            self.offset_ms.fetch_add(by.as_millis() as u64, SeqCst);
        }
    }
    fn cell(time: &Arc<Time>) -> Arc<Refreshing<u32>> {
        let time = time.clone();
        Refreshing::with_clock(POLICY, Box::new(move || time.start + Duration::from_millis(time.offset_ms.load(SeqCst))))
    }

    /// A loader that counts its loads and returns the next version, or
    /// fails while `failing`. A gated loader holds each load until released.
    #[derive(Clone, Default)]
    struct Source {
        loads: Arc<AtomicUsize>,
        failing: Arc<std::sync::atomic::AtomicBool>,
        gate: Option<Arc<Notify>>,
    }
    impl Source {
        fn gated() -> Self {
            Self { gate: Some(Arc::new(Notify::new())), ..Default::default() }
        }
        fn load(&self) -> impl Future<Output = anyhow::Result<u32>> + Send + 'static {
            let (loads, failing, gate) = (self.loads.clone(), self.failing.clone(), self.gate.clone());
            async move {
                if let Some(gate) = gate {
                    gate.notified().await;
                }
                let version = loads.fetch_add(1, SeqCst) as u32 + 1;
                if failing.load(SeqCst) { anyhow::bail!("directory query failed") } else { Ok(version) }
            }
        }
        fn loads(&self) -> usize {
            self.loads.load(SeqCst)
        }
    }
    async fn settled(cell: &Arc<Refreshing<u32>>) {
        for _ in 0..200 {
            if !cell.state().refreshing {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("the background refresh did not finish");
    }

    #[tokio::test]
    async fn a_due_value_is_served_at_once_while_one_background_refresh_replaces_it() {
        let (time, source) = (Time::new(), Source::default());
        let cell = cell(&time);
        assert!(matches!(cell.get(|| source.load()).await, (Ok(value), Cache::FirstLoad) if *value == 1));
        time.advance(Duration::from_secs(30));
        assert!(matches!(cell.get(|| source.load()).await, (Ok(value), Cache::Hit) if *value == 1));
        assert_eq!(source.loads(), 1, "a young value needs no refresh");
        time.advance(Duration::from_secs(20));
        for _ in 0..3 {
            let (value, cache) = cell.get(|| source.load()).await;
            assert_eq!((*value.unwrap(), cache), (1, Cache::Hit), "served before the TTL expires, without waiting");
        }
        settled(&cell).await;
        assert_eq!(source.loads(), 2, "one refresh for every reader");
        assert!(matches!(cell.get(|| source.load()).await, (Ok(value), Cache::Hit) if *value == 2));
    }

    #[tokio::test]
    async fn a_failing_refresh_serves_the_last_value_as_stale_until_its_maximum_age() {
        let (time, source) = (Time::new(), Source::default());
        let cell = cell(&time);
        cell.get(|| source.load()).await.0.unwrap();
        source.failing.store(true, SeqCst);
        time.advance(Duration::from_secs(61));
        let (value, cache) = cell.get(|| source.load()).await;
        assert_eq!((*value.unwrap(), cache), (1, Cache::StaleServed));
        settled(&cell).await;
        time.advance(POLICY.max_age);
        let (value, cache) = cell.get(|| source.load()).await;
        assert_eq!(cache, Cache::RefreshWait, "a value past its maximum age is never served");
        assert!(matches!(value, Err(Unavailable::Load(_))), "the reader waited for a load, which failed");
        source.failing.store(false, SeqCst);
        time.advance(POLICY.max_backoff);
        let (value, cache) = cell.get(|| source.load()).await;
        assert_eq!((*value.unwrap(), cache), (4, Cache::RefreshWait));
    }

    #[tokio::test]
    async fn failed_loads_back_off_exponentially_without_querying() {
        let (time, source) = (Time::new(), Source::default());
        let cell = cell(&time);
        source.failing.store(true, SeqCst);
        assert!(matches!(cell.get(|| source.load()).await, (Err(Unavailable::Load(_)), Cache::FirstLoad)));
        assert!(matches!(cell.get(|| source.load()).await.0, Err(Unavailable::BackingOff(wait)) if wait == POLICY.backoff));
        assert_eq!(source.loads(), 1, "no query while backing off");
        time.advance(POLICY.backoff);
        assert!(matches!(cell.get(|| source.load()).await.0, Err(Unavailable::Load(_))));
        assert!(matches!(cell.get(|| source.load()).await.0, Err(Unavailable::BackingOff(wait)) if wait == POLICY.backoff * 2));
        assert_eq!(POLICY.backoff_after(30), POLICY.max_backoff);
        source.failing.store(false, SeqCst);
        time.advance(POLICY.backoff * 2);
        assert!(matches!(cell.get(|| source.load()).await, (Ok(value), Cache::FirstLoad) if *value == 3));
        assert_eq!(cell.state().failures, 0, "a success resets the backoff");
        // A due refresh that fails backs off too, and is not retried per read.
        source.failing.store(true, SeqCst);
        time.advance(POLICY.refresh_after);
        cell.get(|| source.load()).await.0.unwrap();
        settled(&cell).await;
        for _ in 0..3 {
            assert_eq!(*cell.get(|| source.load()).await.0.unwrap(), 3);
        }
        assert_eq!(source.loads(), 4, "one failed refresh, then the backoff holds further attempts");
    }

    #[tokio::test]
    async fn an_explicit_reload_replaces_a_young_value_for_later_readers() {
        let (time, source) = (Time::new(), Source::default());
        let cell = cell(&time);
        cell.get(|| source.load()).await.0.unwrap();
        time.advance(Duration::from_secs(5));
        assert_eq!(*cell.reload(source.load()).await.unwrap(), 2);
        assert!(matches!(cell.get(|| source.load()).await, (Ok(value), Cache::Hit) if *value == 2));
        assert_eq!(source.loads(), 2);
    }

    #[tokio::test]
    async fn concurrent_readers_share_one_load() {
        let (time, source) = (Time::new(), Source::gated());
        let cell = cell(&time);
        let readers: Vec<_> = (0..8).map(|_| {
            let (cell, source) = (cell.clone(), source.clone());
            tokio::spawn(async move { cell.get(|| source.load()).await })
        }).collect();
        tokio::task::yield_now().await;
        source.gate.as_ref().unwrap().notify_one();
        for reader in readers {
            let (value, cache) = reader.await.unwrap();
            assert_eq!((*value.unwrap(), cache), (1, Cache::FirstLoad));
        }
        assert_eq!(source.loads(), 1, "eight cold readers, one directory query");
        // A background refresh in flight is shared too: past the maximum age a
        // reader waits for that refresh instead of starting another load.
        time.advance(POLICY.refresh_after);
        assert_eq!(*cell.get(|| source.load()).await.0.unwrap(), 1);
        time.advance(POLICY.max_age);
        let waiting = {
            let (cell, source) = (cell.clone(), source.clone());
            tokio::spawn(async move { cell.get(|| source.load()).await })
        };
        tokio::task::yield_now().await;
        source.gate.as_ref().unwrap().notify_one();
        let (value, cache) = waiting.await.unwrap();
        assert_eq!((*value.unwrap(), cache), (2, Cache::RefreshWait));
        assert_eq!(source.loads(), 2);
    }
}
