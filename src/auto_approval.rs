//! Automatic approval for connected chatbots.
//!
//! A person can let one chatbot connection approve a priced request without a
//! click, so scheduled and repeated requests ("run this every day") can run.
//! The rule is theirs and narrow: approve when the plan's maximum total is AT
//! MOST an amount they chose, until a date they chose (App migration
//! 20260929120000). It never widens anything. The same budget check as the
//! chat card's Approve must pass — per request, the rolling day, the calendar
//! month and the balance — and a plan that buys from a source without a live
//! response yet is never approved automatically, exactly as the public API's
//! `auto` mode refuses it. Anything that does not qualify waits for Approve,
//! as before. The ledger stays the final authority at dispatch.
//!
//! This module is the decision only; `Executor::auto_approve_connection`
//! reads its inputs under the task lock and records the approval.
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

/// The connection's rule, as `gateway_v2.connection_auto_approval` returns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Approve automatically when the plan costs at most this (micro-USD).
    pub limit_atomic: i64,
    pub expires_at: DateTime<Utc>,
    /// What is left of the calendar month, when the connection has a monthly
    /// limit. `Budget::daily_remaining_atomic` is already bounded by it; this
    /// is here so a refusal names the month rather than the day.
    pub monthly_remaining_atomic: Option<i64>,
}

impl Policy {
    pub fn active(&self, now: DateTime<Utc>) -> bool {
        self.limit_atomic > 0 && self.expires_at > now
    }
}

/// The connection's spending mandate, as `gateway_v2.connection_budget`
/// returns it. `None` there means the connection has no per-request and daily
/// limit, or may not spend in its workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub per_request_atomic: i64,
    pub daily_remaining_atomic: i64,
    pub balance_atomic: i64,
}

/// The priced plan being decided about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quote {
    pub cap_atomic: i64,
    pub expires_at: DateTime<Utc>,
    /// Some step buys from a source that has not answered a live call yet.
    pub unverified_source: bool,
}

/// Why a plan was not approved automatically. Every one of these leaves the
/// plan exactly as it was: waiting for Approve, with nothing charged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denial {
    /// No rule on this connection: ask each time.
    Off,
    /// The rule switched itself off on its date.
    Expired,
    InvalidAmount,
    /// The plan's maximum total is above the automatic amount.
    AboveAutoLimit,
    UnverifiedSource,
    QuoteExpired,
    /// The connection has no per-request and daily limit (or may not spend).
    NoLimits,
    PerRequest,
    Monthly,
    Daily,
    Balance,
    /// Nothing runnable to approve yet: a choice or input is outstanding, the
    /// request was cancelled, or its approval was withdrawn.
    NotReady,
    PurchasesUnavailable,
    SpendingNotPermitted,
}

impl Denial {
    pub fn code(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Expired => "expired",
            Self::InvalidAmount => "invalid_amount",
            Self::AboveAutoLimit => "above_auto_limit",
            Self::UnverifiedSource => "unverified_source",
            Self::QuoteExpired => "quote_expired",
            Self::NoLimits => "no_limits",
            Self::PerRequest => "per_request_limit",
            Self::Monthly => "monthly_limit",
            Self::Daily => "daily_limit",
            Self::Balance => "balance",
            Self::NotReady => "not_ready",
            Self::PurchasesUnavailable => "purchases_unavailable",
            Self::SpendingNotPermitted => "spending_not_permitted",
        }
    }

    /// One sentence a chatbot or the card can show as is. None of them claims
    /// anything was approved or bought.
    pub fn message(self) -> &'static str {
        match self {
            Self::Off => "Automatic approval is off for this connection, so each request needs Approve.",
            Self::Expired => "Automatic approval for this connection has switched itself off. The person can switch it on again in Apiosk > Connections > Limits & setup.",
            Self::AboveAutoLimit => "This request's maximum total is above the connection's automatic-approval amount, so it needs Approve.",
            Self::UnverifiedSource => "This plan uses a source that has not answered a live call yet, so it is never approved automatically. It needs Approve.",
            Self::QuoteExpired => "This price has expired. Get a new price before approving.",
            Self::NoLimits => "Automatic approval needs a per-call and a daily limit on this connection. Set them in Apiosk > Connections > Limits & setup.",
            Self::PerRequest => "This request is above the connection's per-call limit, so it cannot be approved automatically.",
            Self::Monthly => "This request is above what the connection may still spend this month, so it cannot be approved automatically.",
            Self::Daily => "This request is above the connection's remaining daily budget, so it cannot be approved automatically.",
            Self::Balance => "The Apiosk balance is too low for this request's maximum total, so it cannot be approved automatically.",
            Self::InvalidAmount | Self::NotReady => "This request is not ready to be approved yet.",
            Self::PurchasesUnavailable => "Purchases are temporarily unavailable, so this request cannot be approved automatically.",
            Self::SpendingNotPermitted => "This connection may not spend in its workspace, so this request cannot be approved automatically.",
        }
    }
}

/// Whether this plan may be approved automatically under this rule. The order
/// is the order a person would reason in: is the rule on, is this request
/// small enough for it, is it safe to buy, and does the mandate still cover it.
pub fn decide(
    quote: &Quote,
    policy: Option<&Policy>,
    budget: Option<&Budget>,
    now: DateTime<Utc>,
) -> Result<(), Denial> {
    let policy = policy.filter(|p| p.limit_atomic > 0).ok_or(Denial::Off)?;
    if !policy.active(now) {
        return Err(Denial::Expired);
    }
    let cap = quote.cap_atomic;
    if cap < 0 {
        return Err(Denial::InvalidAmount);
    }
    if cap > policy.limit_atomic {
        return Err(Denial::AboveAutoLimit);
    }
    if quote.unverified_source {
        return Err(Denial::UnverifiedSource);
    }
    if quote.expires_at <= now {
        return Err(Denial::QuoteExpired);
    }
    let budget = budget.ok_or(Denial::NoLimits)?;
    if cap > budget.per_request_atomic {
        return Err(Denial::PerRequest);
    }
    if policy.monthly_remaining_atomic.is_some_and(|month| cap > month) {
        return Err(Denial::Monthly);
    }
    if cap > budget.daily_remaining_atomic {
        return Err(Denial::Daily);
    }
    if cap > budget.balance_atomic {
        return Err(Denial::Balance);
    }
    Ok(())
}

/// `context_view.auto_approval`: the rule, and what it did for this request.
/// `applied` is true only when the CURRENT plan's approval was recorded as
/// automatic; `reason` says why it was not, when that is known.
pub fn context_view(policy: &Policy, now: DateTime<Utc>, applied: bool, denial: Option<Denial>) -> Value {
    json!({
        "active": policy.active(now),
        "limit_atomic": policy.limit_atomic.to_string(),
        "currency": "USD",
        "expires_at": policy.expires_at,
        "applied": applied,
        "reason": denial.map(Denial::code),
        "message": denial.map(Denial::message),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn now() -> DateTime<Utc> {
        "2026-09-29T10:00:00Z".parse().unwrap()
    }
    fn policy(limit: i64) -> Policy {
        Policy { limit_atomic: limit, expires_at: now() + Duration::days(30), monthly_remaining_atomic: None }
    }
    fn quote(cap: i64) -> Quote {
        Quote { cap_atomic: cap, expires_at: now() + Duration::minutes(10), unverified_source: false }
    }
    const BUDGET: Budget = Budget { per_request_atomic: 1_000_000, daily_remaining_atomic: 2_000_000, balance_atomic: 5_000_000 };

    #[test]
    fn a_plan_at_or_below_the_amount_inside_every_limit_is_approved() {
        assert_eq!(decide(&quote(500_000), Some(&policy(500_000)), Some(&BUDGET), now()), Ok(()));
        assert_eq!(decide(&quote(1), Some(&policy(500_000)), Some(&BUDGET), now()), Ok(()));
        assert_eq!(decide(&quote(0), Some(&policy(500_000)), Some(&BUDGET), now()), Ok(()));
    }

    #[test]
    fn no_rule_means_ask_each_time() {
        assert_eq!(decide(&quote(1), None, Some(&BUDGET), now()), Err(Denial::Off));
        assert_eq!(decide(&quote(1), Some(&policy(0)), Some(&BUDGET), now()), Err(Denial::Off));
    }

    #[test]
    fn an_expired_rule_approves_nothing() {
        let ended = Policy { expires_at: now(), ..policy(500_000) };
        assert_eq!(decide(&quote(1), Some(&ended), Some(&BUDGET), now()), Err(Denial::Expired));
        assert!(!ended.active(now()));
        assert!(policy(500_000).active(now()));
    }

    #[test]
    fn anything_above_the_amount_still_needs_approve() {
        assert_eq!(decide(&quote(500_001), Some(&policy(500_000)), Some(&BUDGET), now()), Err(Denial::AboveAutoLimit));
        assert_eq!(decide(&quote(-1), Some(&policy(500_000)), Some(&BUDGET), now()), Err(Denial::InvalidAmount));
    }

    #[test]
    fn an_unverified_source_is_never_bought_automatically() {
        let unverified = Quote { unverified_source: true, ..quote(1) };
        assert_eq!(decide(&unverified, Some(&policy(500_000)), Some(&BUDGET), now()), Err(Denial::UnverifiedSource));
    }

    #[test]
    fn an_expired_quote_is_not_approved() {
        let stale = Quote { expires_at: now(), ..quote(1) };
        assert_eq!(decide(&stale, Some(&policy(500_000)), Some(&BUDGET), now()), Err(Denial::QuoteExpired));
    }

    #[test]
    fn every_existing_limit_still_applies() {
        let p = policy(500_000);
        assert_eq!(decide(&quote(400_000), Some(&p), None, now()), Err(Denial::NoLimits));
        for (budget, denial) in [
            (Budget { per_request_atomic: 300_000, ..BUDGET }, Denial::PerRequest),
            (Budget { daily_remaining_atomic: 399_999, ..BUDGET }, Denial::Daily),
            (Budget { balance_atomic: 100, ..BUDGET }, Denial::Balance),
        ] {
            assert_eq!(decide(&quote(400_000), Some(&p), Some(&budget), now()), Err(denial));
        }
        let month = Policy { monthly_remaining_atomic: Some(399_999), ..p };
        // The rolling day is already bounded by the month; the month is named.
        let bounded = Budget { daily_remaining_atomic: 399_999, ..BUDGET };
        assert_eq!(decide(&quote(400_000), Some(&month), Some(&bounded), now()), Err(Denial::Monthly));
        let room = Policy { monthly_remaining_atomic: Some(400_000), ..p };
        assert_eq!(decide(&quote(400_000), Some(&room), Some(&BUDGET), now()), Ok(()));
    }

    #[test]
    fn the_view_says_what_happened_without_claiming_an_approval() {
        let view = context_view(&policy(500_000), now(), false, Some(Denial::AboveAutoLimit));
        assert_eq!(view["active"], true);
        assert_eq!(view["limit_atomic"], "500000");
        assert_eq!(view["applied"], false);
        assert_eq!(view["reason"], "above_auto_limit");
        assert!(view["message"].as_str().unwrap().contains("needs Approve"));
        let applied = context_view(&policy(500_000), now(), true, None);
        assert_eq!(applied["applied"], true);
        assert!(applied["reason"].is_null());
        for denial in [Denial::Off, Denial::Expired, Denial::AboveAutoLimit, Denial::UnverifiedSource, Denial::QuoteExpired, Denial::NoLimits,
            Denial::PerRequest, Denial::Monthly, Denial::Daily, Denial::Balance, Denial::NotReady, Denial::InvalidAmount,
            Denial::PurchasesUnavailable, Denial::SpendingNotPermitted] {
            let message = denial.message().to_lowercase();
            for claim in ["was approved", "has been approved", "is running", "was bought", "was charged"] {
                assert!(!message.contains(claim), "{message}");
            }
        }
    }
}
