//! The shared suite, ported case for case from the buyer portal's
//! `src/lib/policy/evaluate.test.ts`.
//!
//! This file is the contract between two implementations of one decision. Every
//! case here has a twin there, with the same name and the same expectation. A
//! case that behaves differently is a bug in ONE of them, and which one has to
//! be decided before either moves on — that is the whole point of writing the
//! rules down twice in the same words.
//!
//! Adding a rule means adding a case on both sides, in the same week.

use crate::db::PolicyRules;

use super::evaluate::{decide_purchase, evaluate_policy, expand_regions};
use super::rules::{PolicyDecision, PurchaseRequest, SpendSnapshot};

/// The portal's DEFAULT_POLICY_RULES, which most cases then override.
///
/// Note `allowed_assets: ["USDC"]` — the portal's default. Most cases clear it,
/// because a case about a domain rule should not also be a case about assets.
fn defaults() -> PolicyRules {
    PolicyRules {
        max_per_transaction_usd: Some(1.0),
        max_per_day_usd: Some(20.0),
        allowed_assets: vec!["USDC".to_string()],
        ..PolicyRules::default()
    }
}

/// `defaults()` with the asset restriction cleared, which is what nearly every
/// case in the portal suite does with `allowedAssets: []`.
fn policy() -> PolicyRules {
    PolicyRules {
        allowed_assets: Vec::new(),
        ..defaults()
    }
}

fn buy(amount: f64) -> PurchaseRequest {
    PurchaseRequest {
        amount_usd: amount,
        ..PurchaseRequest::default()
    }
}

fn spend(today: f64, month: f64, calls: i64) -> SpendSnapshot {
    SpendSnapshot {
        spent_today_usd: today,
        spent_this_month_usd: month,
        calls_today: calls,
    }
}

// ---------------------------------------------------------------------------
// Amount ceilings
// ---------------------------------------------------------------------------

#[test]
fn allows_a_purchase_inside_every_cap() {
    let verdict = evaluate_policy(&policy(), &buy(0.08), SpendSnapshot::default());
    assert_eq!(verdict.decision, PolicyDecision::Allow);
    assert!(verdict.findings.is_empty());
    assert!(verdict.matched.is_none());
}

#[test]
fn denies_above_the_per_transaction_cap() {
    let verdict = evaluate_policy(&policy(), &buy(1.5), SpendSnapshot::default());
    assert_eq!(verdict.decision, PolicyDecision::Deny);
    assert_eq!(verdict.matched_rule(), Some("maxPerTransactionUsd"));
}

#[test]
fn treats_a_purchase_exactly_at_the_cap_as_allowed() {
    let verdict = evaluate_policy(&policy(), &buy(1.0), SpendSnapshot::default());
    assert_eq!(verdict.decision, PolicyDecision::Allow);
}

#[test]
fn does_not_trip_a_cap_on_floating_point_representation_error() {
    // 0.1 + 0.2 == 0.30000000000000004. A naive `>` refuses a purchase that is
    // exactly at the buyer's limit.
    let rules = PolicyRules {
        max_per_day_usd: Some(0.3),
        max_per_transaction_usd: None,
        ..policy()
    };
    let verdict = evaluate_policy(&rules, &buy(0.2), spend(0.1, 0.1, 1));
    assert_eq!(verdict.decision, PolicyDecision::Allow);
}

#[test]
fn separates_unit_price_from_transaction_total() {
    let rules = PolicyRules {
        max_per_transaction_usd: Some(10.0),
        max_price_per_call_usd: Some(0.05),
        ..policy()
    };
    let request = PurchaseRequest {
        amount_usd: 1.0,
        unit_price_usd: Some(0.2),
        ..PurchaseRequest::default()
    };
    let verdict = evaluate_policy(&rules, &request, SpendSnapshot::default());
    assert_eq!(verdict.decision, PolicyDecision::Deny);
    assert_eq!(verdict.matched_rule(), Some("maxPricePerCallUsd"));
}

#[test]
fn falls_back_to_the_total_when_no_unit_price_is_given() {
    let rules = PolicyRules {
        max_price_per_call_usd: Some(0.05),
        ..policy()
    };
    let verdict = evaluate_policy(&rules, &buy(0.02), SpendSnapshot::default());
    assert_eq!(verdict.decision, PolicyDecision::Allow);
}

// ---------------------------------------------------------------------------
// Budgets
// ---------------------------------------------------------------------------

#[test]
fn refuses_the_purchase_that_would_breach_the_daily_budget_not_the_one_after() {
    let rules = PolicyRules {
        max_per_day_usd: Some(20.0),
        max_per_transaction_usd: None,
        ..policy()
    };
    let already = spend(19.5, 19.5, 40);
    assert_eq!(
        evaluate_policy(&rules, &buy(0.5), already).decision,
        PolicyDecision::Allow
    );
    assert_eq!(
        evaluate_policy(&rules, &buy(0.51), already).decision,
        PolicyDecision::Deny
    );
}

#[test]
fn reports_headroom_left_after_the_purchase() {
    let rules = PolicyRules {
        max_per_day_usd: Some(20.0),
        max_per_transaction_usd: Some(5.0),
        ..policy()
    };
    let verdict = evaluate_policy(&rules, &buy(2.0), spend(3.0, 3.0, 2));
    assert!((verdict.remaining.day_usd.unwrap() - 15.0).abs() < 1e-6);
    assert!((verdict.remaining.transaction_usd.unwrap() - 3.0).abs() < 1e-6);
    assert_eq!(verdict.remaining.month_usd, None);
}

#[test]
fn enforces_a_monthly_budget_independently_of_the_daily_one() {
    let rules = PolicyRules {
        max_per_day_usd: None,
        max_per_month_usd: Some(100.0),
        max_per_transaction_usd: None,
        ..policy()
    };
    let verdict = evaluate_policy(&rules, &buy(5.0), spend(0.0, 98.0, 0));
    assert_eq!(verdict.decision, PolicyDecision::Deny);
    assert_eq!(verdict.matched_rule(), Some("maxPerMonthUsd"));
}

#[test]
fn caps_purchase_count_per_day() {
    let rules = PolicyRules {
        max_calls_per_day: Some(3),
        ..policy()
    };
    let at = |calls: i64| evaluate_policy(&rules, &buy(0.01), spend(0.0, 0.0, calls)).decision;
    assert_eq!(at(2), PolicyDecision::Allow);
    assert_eq!(at(3), PolicyDecision::Deny);
}

// ---------------------------------------------------------------------------
// Destination and seller
// ---------------------------------------------------------------------------

fn to(host: &str) -> PurchaseRequest {
    PurchaseRequest {
        amount_usd: 0.01,
        destination_host: Some(host.to_string()),
        ..PurchaseRequest::default()
    }
}

#[test]
fn allows_any_host_when_the_allowlist_is_empty() {
    let verdict = evaluate_policy(&policy(), &to("api.anything.com"), SpendSnapshot::default());
    assert_eq!(verdict.decision, PolicyDecision::Allow);
}

#[test]
fn matches_wildcard_subdomains() {
    let rules = PolicyRules {
        allowed_domains: vec!["*.example.com".to_string()],
        ..policy()
    };
    let decide = |host: &str| evaluate_policy(&rules, &to(host), SpendSnapshot::default()).decision;
    assert_eq!(decide("api.example.com"), PolicyDecision::Allow);
    assert_eq!(decide("example.com"), PolicyDecision::Allow);
    // The suffix trap: a host that merely ENDS with the allowed name is not it.
    assert_eq!(decide("example.com.evil.net"), PolicyDecision::Deny);
}

#[test]
fn fails_closed_when_the_host_is_unreadable_and_an_allowlist_is_set() {
    let rules = PolicyRules {
        allowed_domains: vec!["api.example.com".to_string()],
        ..policy()
    };
    let verdict = evaluate_policy(&rules, &buy(0.01), SpendSnapshot::default());
    assert_eq!(verdict.decision, PolicyDecision::Deny);
    assert_eq!(verdict.matched_rule(), Some("allowedDomains"));
}

#[test]
fn lets_a_blocklist_refuse_a_host_even_with_no_allowlist() {
    let rules = PolicyRules {
        blocked_domains: vec!["*.sketchy.io".to_string()],
        ..policy()
    };
    let verdict = evaluate_policy(&rules, &to("api.sketchy.io"), SpendSnapshot::default());
    assert_eq!(verdict.decision, PolicyDecision::Deny);
    assert_eq!(verdict.matched_rule(), Some("blockedDomains"));
}

#[test]
fn restricts_sellers_case_insensitively() {
    let rules = PolicyRules {
        allowed_sellers: vec!["Weather-API".to_string()],
        ..policy()
    };
    let from = |seller: &str| PurchaseRequest {
        amount_usd: 0.01,
        seller: Some(seller.to_string()),
        ..PurchaseRequest::default()
    };
    assert_eq!(
        evaluate_policy(&rules, &from("weather-api"), SpendSnapshot::default()).decision,
        PolicyDecision::Allow
    );
    assert_eq!(
        evaluate_policy(&rules, &from("other-api"), SpendSnapshot::default()).decision,
        PolicyDecision::Deny
    );
}

#[test]
fn refuses_an_unknown_seller_when_sellers_are_restricted() {
    let rules = PolicyRules {
        allowed_sellers: vec!["weather-api".to_string()],
        ..policy()
    };
    assert_eq!(
        evaluate_policy(&rules, &buy(0.01), SpendSnapshot::default()).decision,
        PolicyDecision::Deny
    );
}

// ---------------------------------------------------------------------------
// Jurisdiction, rail and shape
// ---------------------------------------------------------------------------

#[test]
fn expands_eu_to_member_states() {
    let eu = expand_regions(&["EU".to_string()]);
    assert!(eu.contains("NL"));
    assert!(eu.contains("DE"));
    assert_eq!(eu.len(), 27);
    assert!(!eu.contains("US"));
    assert!(!eu.contains("GB"));
}

#[test]
fn allows_an_eu_seller_and_refuses_a_non_eu_one() {
    let rules = PolicyRules {
        allowed_regions: vec!["EU".to_string()],
        ..policy()
    };
    let in_region = |code: &str| PurchaseRequest {
        amount_usd: 0.01,
        seller_region: Some(code.to_string()),
        ..PurchaseRequest::default()
    };
    assert_eq!(
        evaluate_policy(&rules, &in_region("NL"), SpendSnapshot::default()).decision,
        PolicyDecision::Allow
    );
    assert_eq!(
        evaluate_policy(&rules, &in_region("US"), SpendSnapshot::default()).decision,
        PolicyDecision::Deny
    );
}

#[test]
fn refuses_when_the_seller_jurisdiction_is_unknown() {
    let rules = PolicyRules {
        allowed_regions: vec!["EU".to_string()],
        ..policy()
    };
    let verdict = evaluate_policy(&rules, &buy(0.01), SpendSnapshot::default());
    assert_eq!(verdict.decision, PolicyDecision::Deny);
    assert_eq!(verdict.matched_rule(), Some("allowedRegions"));
}

#[test]
fn mixes_eu_with_explicit_iso_codes() {
    let rules = PolicyRules {
        allowed_regions: vec!["EU".to_string(), "GB".to_string()],
        ..policy()
    };
    let request = PurchaseRequest {
        amount_usd: 0.01,
        seller_region: Some("GB".to_string()),
        ..PurchaseRequest::default()
    };
    assert_eq!(
        evaluate_policy(&rules, &request, SpendSnapshot::default()).decision,
        PolicyDecision::Allow
    );
}

#[test]
fn restricts_the_settlement_asset() {
    let rules = defaults();
    let paid_in = |asset: &str| PurchaseRequest {
        amount_usd: 0.01,
        asset: Some(asset.to_string()),
        ..PurchaseRequest::default()
    };
    assert_eq!(
        evaluate_policy(&rules, &paid_in("USDC"), SpendSnapshot::default()).decision,
        PolicyDecision::Allow
    );
    assert_eq!(
        evaluate_policy(&rules, &paid_in("DAI"), SpendSnapshot::default()).decision,
        PolicyDecision::Deny
    );
}

#[test]
fn refuses_a_recurring_charge_unless_subscriptions_are_allowed() {
    let strict = policy();
    let relaxed = PolicyRules {
        allow_subscriptions: true,
        ..policy()
    };
    let charge = |recurring: bool| PurchaseRequest {
        amount_usd: 0.01,
        recurring,
        ..PurchaseRequest::default()
    };
    assert_eq!(
        evaluate_policy(&strict, &charge(true), SpendSnapshot::default()).decision,
        PolicyDecision::Deny
    );
    assert_eq!(
        evaluate_policy(&strict, &charge(false), SpendSnapshot::default()).decision,
        PolicyDecision::Allow
    );
    assert_eq!(
        evaluate_policy(&relaxed, &charge(true), SpendSnapshot::default()).decision,
        PolicyDecision::Allow
    );
}

// ---------------------------------------------------------------------------
// Approvals
// ---------------------------------------------------------------------------

#[test]
fn holds_a_purchase_at_or_above_the_approval_threshold() {
    let rules = PolicyRules {
        approval_threshold_usd: Some(1.0),
        max_per_transaction_usd: None,
        ..policy()
    };
    assert_eq!(
        evaluate_policy(&rules, &buy(0.99), SpendSnapshot::default()).decision,
        PolicyDecision::Allow
    );

    let held = evaluate_policy(&rules, &buy(1.0), SpendSnapshot::default());
    assert_eq!(held.decision, PolicyDecision::RequireApproval);
    assert_eq!(held.matched_rule(), Some("approvalThresholdUsd"));
}

#[test]
fn holds_the_first_purchase_from_an_unseen_seller() {
    let rules = PolicyRules {
        require_approval_for_new_sellers: true,
        ..policy()
    };
    let from = |seller: &str, seen: bool| PurchaseRequest {
        amount_usd: 0.01,
        seller: Some(seller.to_string()),
        seller_seen_before: Some(seen),
        ..PurchaseRequest::default()
    };
    assert_eq!(
        evaluate_policy(&rules, &from("new-api", false), SpendSnapshot::default()).decision,
        PolicyDecision::RequireApproval
    );
    assert_eq!(
        evaluate_policy(&rules, &from("known-api", true), SpendSnapshot::default()).decision,
        PolicyDecision::Allow
    );
}

#[test]
fn does_not_hold_when_the_caller_cannot_say_whether_the_seller_is_new() {
    // seller_seen_before is None — the caller simply does not know yet. Holding
    // every purchase on missing history would wedge the agent over a failed
    // lookup.
    let rules = PolicyRules {
        require_approval_for_new_sellers: true,
        ..policy()
    };
    let request = PurchaseRequest {
        amount_usd: 0.01,
        seller: Some("some-api".to_string()),
        ..PurchaseRequest::default()
    };
    assert_eq!(
        evaluate_policy(&rules, &request, SpendSnapshot::default()).decision,
        PolicyDecision::Allow
    );
}

#[test]
fn denies_rather_than_asks_when_a_purchase_breaks_a_hard_cap_too() {
    let rules = PolicyRules {
        max_per_transaction_usd: Some(1.0),
        approval_threshold_usd: Some(0.5),
        ..policy()
    };
    let verdict = evaluate_policy(&rules, &buy(5.0), SpendSnapshot::default());
    assert_eq!(verdict.decision, PolicyDecision::Deny);
    assert_eq!(verdict.matched_rule(), Some("maxPerTransactionUsd"));
    // The hold is still reported — the buyer sees every reason, not just the
    // winner.
    assert!(verdict
        .findings
        .iter()
        .any(|f| f.rule.as_str() == "approvalThresholdUsd"));
}

#[test]
fn keeps_every_violated_rule_in_the_findings() {
    let rules = PolicyRules {
        max_per_transaction_usd: Some(1.0),
        allowed_domains: vec!["api.example.com".to_string()],
        allowed_assets: vec!["USDC".to_string()],
        ..policy()
    };
    let request = PurchaseRequest {
        amount_usd: 9.0,
        destination_host: Some("api.other.com".to_string()),
        asset: Some("DAI".to_string()),
        ..PurchaseRequest::default()
    };
    let verdict = evaluate_policy(&rules, &request, SpendSnapshot::default());
    assert_eq!(verdict.decision, PolicyDecision::Deny);

    let mut names: Vec<&str> = verdict.findings.iter().map(|f| f.rule.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec!["allowedAssets", "allowedDomains", "maxPerTransactionUsd"]
    );
}

// ---------------------------------------------------------------------------
// decide_purchase
// ---------------------------------------------------------------------------

#[test]
fn applies_the_rules_of_an_enabled_policy() {
    let verdict = decide_purchase(true, &policy(), &buy(5.0), SpendSnapshot::default());
    assert_eq!(verdict.decision, PolicyDecision::Deny);
}

#[test]
fn waves_everything_through_when_the_policy_is_switched_off() {
    // "Disabled" is a buyer parking a policy, not arming a kill switch — the
    // agent falls back to its wallet's own caps rather than stopping dead.
    let verdict = decide_purchase(false, &policy(), &buy(5.0), SpendSnapshot::default());
    assert_eq!(verdict.decision, PolicyDecision::Allow);
    assert!(verdict.findings.is_empty());
}

// ---------------------------------------------------------------------------
// Rules this binary has never heard of
// ---------------------------------------------------------------------------

#[test]
fn reports_a_rule_it_cannot_evaluate_rather_than_pretending_to_have_applied_it() {
    // The portal can ship a rule type without a migration, so an older gateway
    // reading a newer policy is ordinary. Passing the purchase is the honest
    // answer — this gateway genuinely cannot judge it — but the verdict says so
    // out loud, which is what stops "why did that go through" being unanswerable.
    let raw = serde_json::json!({
        "maxPerTransactionUsd": 5.0,
        "requireNotarizedReceipt": true,
    });
    let rules: PolicyRules = serde_json::from_value(raw).expect("must parse");

    let verdict = evaluate_policy(&rules, &buy(1.0), SpendSnapshot::default());
    assert_eq!(verdict.decision, PolicyDecision::Allow);
    assert_eq!(
        verdict.unevaluated_rules,
        vec!["requireNotarizedReceipt".to_string()]
    );
}
