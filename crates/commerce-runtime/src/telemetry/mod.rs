//! Gateway telemetry v2 — an internal, best-effort, ASYNCHRONOUS observability
//! pipeline. It is entirely side-car: it never participates in payment,
//! settlement, or the existing `gateway_request_logs` write, and a telemetry
//! failure can NEVER change a request's response.
//!
//! Shape:
//!   request path  → build a typed `TelemetryEvent` → `Telemetry::emit` (try_send,
//!                    non-blocking) → bounded queue → single background `writer`
//!                    → batched INSERTs into the migration-052 tables.
//!
//! Everything is gated by `TELEMETRY_V2_ENABLED` (default false). When disabled,
//! `Telemetry` is a zero-cost no-op: `emit` returns immediately, no queue or
//! writer is created, and the middleware skips all wrapping — so behaviour is
//! byte-identical to today.

pub mod body;
pub mod classify;
pub mod event;
pub mod geo;
pub mod policy_counters;
pub mod queue;
pub mod sanitize;
pub mod writer;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use ring::hmac;
use ring::rand::SystemRandom;
use sqlx::PgPool;
use tokio::sync::mpsc::error::TrySendError;
use tracing::{info, warn};
use uuid::Uuid;

use crate::telemetry::event::{FailureStage, Outcome, TelemetryEvent};
use crate::telemetry::sanitize::TrustedProxies;

/// Immutable configuration, parsed once at startup from the environment.
#[derive(Debug, Clone)]
pub struct TelemetrySettings {
    pub enabled: bool,
    pub queue_capacity: usize,
    pub batch_size: usize,
    pub flush_interval_ms: u64,
    pub db_timeout_ms: u64,
    pub discovery_enabled: bool,
    pub discovery_sample_rate: f64,
    pub geo_enabled: bool,
    pub fingerprint_key: Option<String>,
    pub trusted_proxies: TrustedProxies,
}

impl Default for TelemetrySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            queue_capacity: 10_000,
            batch_size: 100,
            flush_interval_ms: 500,
            db_timeout_ms: 1_000,
            discovery_enabled: false,
            discovery_sample_rate: 1.0,
            geo_enabled: false,
            fingerprint_key: None,
            trusted_proxies: TrustedProxies::default(),
        }
    }
}

impl TelemetrySettings {
    pub fn from_env() -> Self {
        Self {
            enabled: env_bool("TELEMETRY_V2_ENABLED", false),
            queue_capacity: env_usize("TELEMETRY_QUEUE_CAPACITY", 10_000),
            batch_size: env_usize("TELEMETRY_BATCH_SIZE", 100),
            flush_interval_ms: env_u64("TELEMETRY_FLUSH_INTERVAL_MS", 500),
            db_timeout_ms: env_u64("TELEMETRY_DB_TIMEOUT_MS", 1_000),
            discovery_enabled: env_bool("TELEMETRY_DISCOVERY_ENABLED", false),
            discovery_sample_rate: env_f64("TELEMETRY_DISCOVERY_SAMPLE_RATE", 1.0).clamp(0.0, 1.0),
            geo_enabled: env_bool("TELEMETRY_GEO_ENABLED", false),
            fingerprint_key: env_opt("TELEMETRY_FINGERPRINT_KEY"),
            trusted_proxies: TrustedProxies::parse(&env_str("TELEMETRY_TRUSTED_PROXIES", "")),
        }
    }
}

/// Optional per-request enrichment a handler can attach to its `Response` via
/// `response.extensions_mut().insert(TelemetryOutcome { .. })`. The middleware
/// reads it (if present) to set a precise failure stage / stable error code /
/// outcome; when absent, the middleware falls back to status-based inference.
/// This is a NON-invasive seam: handlers that don't set it change nothing.
#[derive(Debug, Clone, Default)]
pub struct TelemetryOutcome {
    pub outcome: Option<Outcome>,
    pub failure_stage: Option<FailureStage>,
    pub error_code: Option<String>,
    pub retryable: Option<bool>,
}

struct Inner {
    tx: queue::Sender,
    dropped: AtomicU64,
    settings: TelemetrySettings,
    hmac_key: hmac::Key,
}

/// A cheap, cloneable handle stored in `AppState`. `None` inner ⇒ disabled/no-op.
#[derive(Clone)]
pub struct Telemetry {
    inner: Option<Arc<Inner>>,
}

impl Telemetry {
    /// A disabled, no-op handle. Used when the flag is off or init fails — the
    /// gateway must always start.
    pub fn disabled() -> Self {
        Telemetry { inner: None }
    }

    /// Build the handle and, if enabled, spawn the background writer. Never
    /// panics: any failure returns a disabled handle.
    pub fn start(pool: PgPool, settings: TelemetrySettings, tasks: &crate::background::BackgroundTasks) -> Self {
        if !settings.enabled {
            info!("telemetry v2 disabled (TELEMETRY_V2_ENABLED=false)");
            return Telemetry::disabled();
        }

        let hmac_key = match settings.fingerprint_key.as_deref() {
            Some(k) if !k.is_empty() => hmac::Key::new(hmac::HMAC_SHA256, k.as_bytes()),
            _ => {
                let rng = SystemRandom::new();
                match hmac::Key::generate(hmac::HMAC_SHA256, &rng) {
                    Ok(k) => {
                        warn!(
                            "TELEMETRY_FINGERPRINT_KEY unset — using an ephemeral per-process key \
                             (proof/client fingerprints won't correlate across restarts)"
                        );
                        k
                    }
                    Err(_) => {
                        warn!("failed to generate telemetry HMAC key — disabling telemetry v2");
                        return Telemetry::disabled();
                    }
                }
            }
        };

        let (tx, rx) = queue::channel(settings.queue_capacity);
        let writer_settings = settings.clone();
        let writer_pool = pool;
        tasks.spawn(async move {
            writer::run_writer(rx, writer_pool, writer_settings).await;
        });

        info!(
            "telemetry v2 ENABLED (queue={} batch={} flush={}ms db_timeout={}ms discovery={} sample={:.2} geo={})",
            settings.queue_capacity,
            settings.batch_size,
            settings.flush_interval_ms,
            settings.db_timeout_ms,
            settings.discovery_enabled,
            settings.discovery_sample_rate,
            settings.geo_enabled,
        );

        Telemetry {
            inner: Some(Arc::new(Inner {
                tx,
                dropped: AtomicU64::new(0),
                settings,
                hmac_key,
            })),
        }
    }

    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }

    pub fn settings(&self) -> Option<&TelemetrySettings> {
        self.inner.as_ref().map(|i| &i.settings)
    }

    pub fn trusted_proxies(&self) -> Option<&TrustedProxies> {
        self.inner.as_ref().map(|i| &i.settings.trusted_proxies)
    }

    /// Enqueue an event. NEVER blocks, NEVER errors to the caller. On a full
    /// queue the event is dropped and counted.
    pub fn emit(&self, event: TelemetryEvent) {
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        match inner.tx.try_send(event) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                let n = inner.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                // Rate-limit the warning so a saturated queue can't spam logs.
                if n == 1 || n % 1_000 == 0 {
                    warn!("telemetry queue full — dropped {} events so far", n);
                }
            }
            Err(TrySendError::Closed(_)) => {
                // Writer gone; treat as disabled for the rest of the process.
            }
        }
    }

    /// Total events dropped due to a full queue (for the realtime "dropped rate"
    /// admin panel and health checks).
    pub fn dropped_count(&self) -> u64 {
        self.inner
            .as_ref()
            .map(|i| i.dropped.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Keyed (HMAC-SHA256) fingerprint of arbitrary bytes — for correlating a
    /// payment proof or an ip+ua pair WITHOUT storing the raw value. Returns hex.
    /// `None` when telemetry is disabled.
    pub fn fingerprint(&self, data: &[u8]) -> Option<String> {
        let inner = self.inner.as_ref()?;
        let tag = hmac::sign(&inner.hmac_key, data);
        Some(hex::encode(tag.as_ref()))
    }

    /// Deterministic discovery sampling keyed on the request id, so a request is
    /// either fully sampled or not (no partial rows). Honours
    /// `TELEMETRY_DISCOVERY_ENABLED` + `TELEMETRY_DISCOVERY_SAMPLE_RATE`.
    pub fn should_sample_discovery(&self, request_id: Uuid) -> bool {
        let Some(inner) = self.inner.as_ref() else {
            return false;
        };
        if !inner.settings.discovery_enabled {
            return false;
        }
        let rate = inner.settings.discovery_sample_rate;
        if rate >= 1.0 {
            return true;
        }
        if rate <= 0.0 {
            return false;
        }
        // FNV-1a over the uuid → a stable fraction in [0,1).
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in request_id.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        let frac = (h >> 11) as f64 / (1u64 << 53) as f64;
        frac < rate
    }
}

// --- tiny env helpers (kept local to avoid leaking into config.rs) ---------

fn env_str(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn env_opt(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            let s = v.trim().to_ascii_lowercase();
            s == "1" || s == "true" || s == "yes" || s == "on"
        })
        .unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
impl Telemetry {
    /// Build an ENABLED handle wired to a live channel but with NO writer, so
    /// tests can observe queue-full / drop behaviour deterministically. Returns
    /// the receiver so the channel stays open (dropping it would close it).
    pub(crate) fn test_pair(
        capacity: usize,
        settings: TelemetrySettings,
    ) -> (Telemetry, queue::Receiver) {
        let (tx, rx) = queue::channel(capacity);
        let rng = SystemRandom::new();
        let hmac_key = hmac::Key::generate(hmac::HMAC_SHA256, &rng).unwrap();
        (
            Telemetry {
                inner: Some(Arc::new(Inner {
                    tx,
                    dropped: AtomicU64::new(0),
                    settings,
                    hmac_key,
                })),
            },
            rx,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_event() -> TelemetryEvent {
        TelemetryEvent::Lifecycle(event::LifecycleEvent {
            request_id: Uuid::nil(),
            occurred_at: None,
            event_type: "request_received".into(),
            duration_ms: None,
            status: None,
            error_code: None,
            metadata: serde_json::json!({}),
        })
    }

    #[test]
    fn queue_full_drops_without_blocking() {
        // Capacity 2, nothing draining (we hold rx). emit is try_send: the first
        // 2 buffer, the next 8 are dropped — and it NEVER blocks the caller.
        let (t, _rx) = Telemetry::test_pair(2, TelemetrySettings::default());
        for _ in 0..10 {
            t.emit(sample_event());
        }
        assert_eq!(t.dropped_count(), 8);
        assert!(t.is_enabled());
    }

    #[test]
    fn fingerprint_is_keyed_and_deterministic() {
        let (t, _rx) = Telemetry::test_pair(4, TelemetrySettings::default());
        let a = t.fingerprint(b"proof-bytes");
        let b = t.fingerprint(b"proof-bytes");
        let c = t.fingerprint(b"other");
        assert!(a.is_some());
        assert_eq!(a, b); // deterministic for a given key
        assert_ne!(a, c); // different input → different tag
        assert_eq!(a.unwrap().len(), 64); // hex of HMAC-SHA256
    }

    #[test]
    fn discovery_sampling_honours_rate() {
        let off = TelemetrySettings::default(); // discovery_enabled = false
        let (t_off, _r1) = Telemetry::test_pair(4, off);
        assert!(!t_off.should_sample_discovery(Uuid::new_v4()));

        let all = TelemetrySettings {
            discovery_enabled: true,
            discovery_sample_rate: 1.0,
            ..TelemetrySettings::default()
        };
        let (t_all, _r2) = Telemetry::test_pair(4, all);
        assert!(t_all.should_sample_discovery(Uuid::new_v4()));

        let none = TelemetrySettings {
            discovery_enabled: true,
            discovery_sample_rate: 0.0,
            ..TelemetrySettings::default()
        };
        let (t_none, _r3) = Telemetry::test_pair(4, none);
        assert!(!t_none.should_sample_discovery(Uuid::new_v4()));
    }

    #[test]
    fn disabled_handle_is_noop() {
        let t = Telemetry::disabled();
        assert!(!t.is_enabled());
        // emit / fingerprint / sampling are all safe no-ops.
        t.emit(TelemetryEvent::Lifecycle(event::LifecycleEvent {
            request_id: Uuid::nil(),
            occurred_at: None,
            event_type: "request_received".into(),
            duration_ms: None,
            status: None,
            error_code: None,
            metadata: serde_json::json!({}),
        }));
        assert_eq!(t.dropped_count(), 0);
        assert!(t.fingerprint(b"x").is_none());
        assert!(!t.should_sample_discovery(Uuid::nil()));
    }
}
