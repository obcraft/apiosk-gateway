//! One counter per policy verdict.
//!
//! A rule that suddenly blocks everything is the failure mode this exists for.
//! It looks, from every other angle, exactly like a quiet day: no errors, no
//! 500s, no latency, just agents that stopped buying. Counting allow, deny and
//! require_approval separately makes a policy change that refuses the world
//! visible in minutes rather than when somebody writes in.
//!
//! Process-local and free: three atomics incremented on a path that already
//! writes a database row, and read by `/health`. Deliberately NOT part of the
//! telemetry queue in the parent module — that pipeline is opt-in behind
//! `TELEMETRY_V2_ENABLED` and drops under load, and a safety counter that can
//! be switched off or silently dropped is not one.

use std::sync::atomic::{AtomicU64, Ordering};

static ALLOWED: AtomicU64 = AtomicU64::new(0);
static DENIED: AtomicU64 = AtomicU64::new(0);
static HELD: AtomicU64 = AtomicU64::new(0);

/// Count one verdict. `decision` is the same string written to
/// `purchase_policy_decisions.decision`.
pub fn record(decision: &str) {
    let counter = match decision {
        "allow" => &ALLOWED,
        "deny" => &DENIED,
        "require_approval" => &HELD,
        _ => return,
    };
    counter.fetch_add(1, Ordering::Relaxed);
}

/// Verdicts counted since this process started: (allowed, denied, held).
pub fn snapshot() -> (u64, u64, u64) {
    (
        ALLOWED.load(Ordering::Relaxed),
        DENIED.load(Ordering::Relaxed),
        HELD.load(Ordering::Relaxed),
    )
}
