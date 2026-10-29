//! Where the evaluator meets the payment path.
//!
//! `src/policy/evaluate.rs` is pure and knows nothing about a database. This is
//! the seam: it gathers the facts a verdict needs — the amount, the destination,
//! the seller, what has already been spent — runs the evaluator, writes the
//! decision row, and turns a refusal into a response the agent can act on.
//!
//! Two rules govern everything here.
//!
//! **The policy can only tighten.** The per-transaction and daily caps in
//! `proxy::wallet` are untouched and still run. A policy sits on top of them and
//! adds refusals; it can never permit a purchase those caps would have stopped.
//!
//! **A missing policy is today's behaviour.** No policy, a disabled policy, an
//! unmigrated database, an unreadable row — all of them mean the compiled
//! columns and nothing else, exactly as before this module existed. The one
//! exception is a policy that is present and unreadable, which fails closed and
//! says so by name: see `POLICY_UNAVAILABLE`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Response, StatusCode};
use bigdecimal::{BigDecimal, ToPrimitive};
use uuid::Uuid;

use crate::db::{PolicyDecisionInsert, PurchasePolicy};
use crate::proxy::domain_policy::origin_host;
use crate::proxy::policy_error_response as json_error_response;
use crate::AppState;

use super::evaluate::decide_purchase;
use super::rules::{PolicyDecision, PurchaseRequest, SpendSnapshot};

/// The error code for a policy that exists but could not be read.
///
/// Not emitted yet: the enforce layer returns it once the policy check is wired
/// into the payment path. Named here so the wire format is decided in one place.
///
/// Named rather than folded into a generic 500 on purpose. "Your rules could not
/// be loaded, so nothing was spent" is a sentence a buyer can act on; a 500 is
/// one they open a support ticket about.
#[allow(dead_code)]
pub const POLICY_UNAVAILABLE: &str = "policy_unavailable";

/// Everything the payment path knows about the purchase being attempted.
pub struct PurchaseContext<'a> {
    pub user_id: Uuid,
    pub token_id: Option<Uuid>,
    pub wallet_id: Option<Uuid>,
    pub wallet_address: Option<&'a str>,
    pub amount_usd: &'a BigDecimal,
    /// The API's upstream origin, not gateway.apiosk.com. A buyer allowlisting
    /// `api.openweathermap.org` means that provider, and every Apiosk API shares
    /// the gateway's hostname.
    pub origin_url: &'a str,
    pub api_slug: &'a str,
    pub asset: Option<&'a str>,
    pub network: Option<&'a str>,
}

/// Run the buyer's policy, if they have one.
///
/// `Ok(None)` means nothing stood in the way: no policy, or a policy that
/// allowed it. `Ok(Some(approval_request))` is the hold branch, handled by
/// gateway/02. `Err(response)` is a refusal, already shaped for the agent.
pub async fn enforce_policy(
    state: &Arc<AppState>,
    ctx: PurchaseContext<'_>,
) -> Result<Option<HeldPurchase>, Response<Body>> {
    let Some(policy) = load_policy(state, &ctx).await else {
        return Ok(None);
    };

    let (spent_today, spent_month, calls_today) = match ctx.wallet_address {
        Some(address) => state.db.get_wallet_policy_spend(address).await,
        // No wallet address resolved yet means no spend to count against a
        // budget. The per-transaction rules still apply, which is the half of
        // the policy that does not need history.
        None => (BigDecimal::from(0), BigDecimal::from(0), 0),
    };

    let seller_seen_before = if policy.rules.require_approval_for_new_sellers {
        state
            .db
            .has_paid_seller_before(ctx.user_id, ctx.api_slug)
            .await
    } else {
        None
    };

    let request = PurchaseRequest {
        amount_usd: to_f64(ctx.amount_usd),
        unit_price_usd: None,
        destination_host: origin_host(ctx.origin_url),
        seller: Some(ctx.api_slug.to_string()),
        // Not yet resolved on this path; a policy restricting regions therefore
        // refuses, which is the fail-closed behaviour the evaluator promises
        // rather than a gap.
        seller_region: None,
        asset: ctx.asset.map(str::to_string),
        network: ctx.network.map(str::to_string),
        recurring: false,
        seller_seen_before,
    };

    let spend = SpendSnapshot {
        spent_today_usd: to_f64(&spent_today),
        spent_this_month_usd: to_f64(&spent_month),
        calls_today,
    };

    let verdict = decide_purchase(policy.enabled, &policy.rules, &request, spend);

    // Written on every verdict, including allow. A log that only records
    // refusals cannot answer "why did this go through", which is the question
    // asked after money has already moved.
    state
        .db
        .insert_policy_decision(PolicyDecisionInsert {
            user_id: ctx.user_id,
            policy_id: Some(policy.id),
            policy_version: Some(policy.version),
            token_id: ctx.token_id,
            wallet_id: ctx.wallet_id,
            decision: verdict.decision.as_str(),
            matched_rule: verdict.matched_rule(),
            reason: verdict.reason(),
            amount_usd: Some(ctx.amount_usd.clone()),
            destination_host: request.destination_host.as_deref(),
            seller: Some(ctx.api_slug),
        })
        .await;

    crate::telemetry::policy_counters::record(verdict.decision.as_str());

    if !verdict.unevaluated_rules.is_empty() {
        // Not an error, and not silent either. A buyer whose newest rule is not
        // being applied because this binary predates it deserves a way to find
        // that out that is not "money left the account".
        tracing::info!(
            policy_id = %policy.id,
            rules = ?verdict.unevaluated_rules,
            "purchase policy carries rules this gateway cannot evaluate"
        );
    }

    match verdict.decision {
        PolicyDecision::Allow => Ok(None),
        PolicyDecision::RequireApproval => Ok(Some(HeldPurchase {
            policy_id: policy.id,
            matched_rule: verdict
                .matched_rule()
                .unwrap_or("approvalThresholdUsd")
                .to_string(),
            reason: verdict
                .reason()
                .unwrap_or("This purchase needs your approval.")
                .to_string(),
            destination_host: request.destination_host,
        })),
        PolicyDecision::Deny => {
            tracing::warn!(
                policy_id = %policy.id,
                rule = verdict.matched_rule().unwrap_or("?"),
                "purchase policy refused a paid call to {}",
                ctx.api_slug
            );
            Err(json_error_response(
                StatusCode::FORBIDDEN,
                "policy_denied",
                &format!(
                    "{} Nothing was paid and nothing was called. The buyer can change this rule in the Apiosk buyer portal under Rules.",
                    verdict.reason().unwrap_or("This purchase breaks the buyer's spending rules.")
                ),
            ))
        }
    }
}

/// A purchase the policy wants a human to approve. Turned into a hold, and a
/// response the agent can poll, by gateway/02.
#[derive(Debug, Clone)]
pub struct HeldPurchase {
    pub policy_id: Uuid,
    pub matched_rule: String,
    pub reason: String,
    pub destination_host: Option<String>,
}

/// The connection's policy, falling back to the wallet's.
///
/// A connection's own policy wins. The wallet's is the fallback for a connection
/// nobody gave one to, not something that stacks on top — two policies both
/// applying would be two answers to the same question and no way to say which
/// one refused a purchase.
async fn load_policy(state: &Arc<AppState>, ctx: &PurchaseContext<'_>) -> Option<PurchasePolicy> {
    if let Some(token_id) = ctx.token_id {
        if let Some(policy) = state.db.load_policy_for_connect_token(token_id).await {
            return Some(policy);
        }
    }
    if let Some(wallet_id) = ctx.wallet_id {
        return state.db.load_policy_for_wallet(wallet_id).await;
    }
    None
}

/// USD amounts are compared as f64 in the evaluator, mirroring the portal, so a
/// value that will not convert is treated as zero rather than as a panic. A zero
/// amount can only make the policy more permissive on the ceilings and less on
/// nothing, and the wallet caps underneath still hold.
fn to_f64(value: &BigDecimal) -> f64 {
    value.to_f64().unwrap_or(0.0)
}
