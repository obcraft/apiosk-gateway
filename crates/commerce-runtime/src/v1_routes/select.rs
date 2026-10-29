//! `GET /v1/select` — one provider, and why.
//!
//! `/v1/search` returns candidates and leaves the choosing to the caller.
//! That is right for an agent with its own judgement, and useless for one that
//! just wants the job done. This endpoint takes the constraints, applies them
//! in a stated order, picks, and says what it rejected and on which rule.
//!
//! ---------------------------------------------------------------------------
//! Why the reason is the product
//! ---------------------------------------------------------------------------
//!
//! A recommender that answers "use tavily" is a black box, and a black box is
//! exactly where a preference for Apiosk's own settled listings would hide. So
//! every response carries:
//!
//!   * the rule that selected the winner, named,
//!   * every rejected candidate with the constraint that removed it,
//!   * the runners-up, in order, so the caller can disagree cheaply.
//!
//! A caller that can see the reasoning can overrule it. One that cannot has to
//! trust us, and trust is not a feature we have earned yet.
//!
//! The choice is recorded through the same decision record an agent would
//! write by hand (`POST /v1/decisions`), because a selection Apiosk made is
//! exactly the kind of decision the record exists to learn from.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db::{CapabilityOffer, CapabilitySort, DecisionInsert, ProviderStats};
use crate::AppState;

/// Runners-up returned alongside the winner. Enough to disagree with, not so
/// many that the answer becomes a list again.
const MAX_ALTERNATIVES: usize = 4;

#[derive(Debug, Deserialize)]
pub struct SelectQuery {
    /// The need in words. Either this or `capability`.
    pub q: Option<String>,
    /// A capability slug, when the caller already knows it.
    pub capability: Option<String>,

    // --- constraints: hard, a candidate that fails one is removed ---
    /// Never pay more than this per call.
    pub max_price: Option<f64>,
    /// Reject anything whose measured median exceeds this. Unmeasured
    /// candidates fail it too: "fast enough" is a claim, and an unmeasured
    /// provider has not made it.
    pub max_latency_ms: Option<i64>,
    /// Reject anything whose measured success rate is below this (0..1).
    pub min_success_rate: Option<f64>,
    /// `apiosk` to require a proxied listing Apiosk settles, `direct` to
    /// require paying the provider yourself.
    pub settlement: Option<String>,
    /// Require the offer to accept every input the capability defines.
    #[serde(default, deserialize_with = "super::search::lenient_bool")]
    pub require_all_inputs: bool,

    // --- preference: soft, decides between survivors ---
    /// `cheapest` (default) | `fastest` | `most_reliable`.
    pub prefer: Option<String>,

    /// Consider listings Apiosk has quarantined for failing more often than
    /// they work. Off by default; the rejection names them either way.
    #[serde(default, deserialize_with = "super::search::lenient_bool")]
    pub include_unreliable: bool,
}

/// A constraint that removed a candidate, in the words the response uses.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Rejection {
    OverBudget,
    TooSlow,
    Unreliable,
    /// Below Apiosk's own floor, not the caller's.
    Quarantined,
    WrongSettlement,
    MissingInputs,
    NotCallable,
    Unmeasured,
}

impl Rejection {
    fn rule(&self) -> &'static str {
        match self {
            Self::OverBudget => "max_price",
            Self::TooSlow => "max_latency_ms",
            Self::Unreliable => "min_success_rate",
            Self::Quarantined => "reliability",
            Self::WrongSettlement => "settlement",
            Self::MissingInputs => "require_all_inputs",
            Self::NotCallable => "callable",
            Self::Unmeasured => "measured",
        }
    }

    fn because(&self) -> &'static str {
        match self {
            Self::OverBudget => "costs more than max_price",
            Self::TooSlow => "measured median latency is above max_latency_ms",
            Self::Unreliable => "measured success rate is below min_success_rate",
            Self::Quarantined => {
                "fails more often than it works, so Apiosk does not offer it by default; it stays callable by name, and returns on its own once it works. Pass include_unreliable=true to consider it now"
            }
            Self::WrongSettlement => "settles on the other rail than the one required",
            Self::MissingInputs => "does not accept every input this capability defines",
            Self::NotCallable => "has no resolvable call target",
            Self::Unmeasured => {
                "has no measurement, and a latency or reliability floor cannot be met by a claim"
            }
        }
    }
}

fn evaluate(
    offer: &CapabilityOffer,
    stats: Option<&ProviderStats>,
    missing: &[String],
    params: &SelectQuery,
) -> Option<Rejection> {
    if offer.call_url.is_none() {
        return Some(Rejection::NotCallable);
    }
    if params.max_price.is_some_and(|cap| offer.price_usd > cap) {
        return Some(Rejection::OverBudget);
    }
    if params.require_all_inputs && !missing.is_empty() {
        return Some(Rejection::MissingInputs);
    }
    match params.settlement.as_deref().map(str::trim) {
        Some("apiosk") if offer.hosted_externally => return Some(Rejection::WrongSettlement),
        Some("direct") if !offer.hosted_externally => return Some(Rejection::WrongSettlement),
        _ => {}
    }

    if !params.include_unreliable && stats.is_some_and(|s| s.is_quarantined()) {
        return Some(Rejection::Quarantined);
    }

    let wants_measurement = params.max_latency_ms.is_some() || params.min_success_rate.is_some();
    match stats {
        None if wants_measurement => return Some(Rejection::Unmeasured),
        Some(s) => {
            match s.p50_ms {
                Some(p50) if params.max_latency_ms.is_some_and(|cap| p50 > cap) => {
                    return Some(Rejection::TooSlow);
                }
                // Too few successful calls to have a median. A ceiling cannot
                // be met by a claim, so this fails it like any unmeasured one.
                None if params.max_latency_ms.is_some() => {
                    return Some(Rejection::Unmeasured);
                }
                _ => {}
            }
            if params
                .min_success_rate
                .is_some_and(|floor| s.current_success_rate() < floor)
            {
                return Some(Rejection::Unreliable);
            }
        }
        None => {}
    }
    None
}

/// `GET /v1/select`
pub async fn select(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<SelectQuery>,
) -> Result<Response, StatusCode> {
    let query = params.q.as_deref().unwrap_or("").trim().to_string();
    let explicit = params
        .capability
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_string();
    if query.is_empty() && explicit.is_empty() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "query_required",
                "message": "Say what you need: /v1/select?q=read a web page, or ?capability=web.page-to-text. Constraints: max_price, max_latency_ms, min_success_rate, settlement=apiosk|direct, require_all_inputs. Preference: prefer=cheapest|fastest|most_reliable.",
            })),
        )
            .into_response());
    }

    // Resolve to one capability. A selection over several capabilities would be
    // answering a question the caller did not ask: "which job" is theirs.
    let info = if !explicit.is_empty() {
        state.db.get_capability(&explicit).await.map_err(|err| {
            tracing::error!("capability lookup failed: {}", err);
            StatusCode::INTERNAL_SERVER_ERROR
        })?
    } else {
        let matches = state
            .db
            .search_capabilities(&query, 1)
            .await
            .map_err(|err| {
                tracing::error!("capability search failed: {}", err);
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        match matches.first() {
            Some(m) => state.db.get_capability(&m.slug).await.map_err(|err| {
                tracing::error!("capability lookup failed: {}", err);
                StatusCode::INTERNAL_SERVER_ERROR
            })?,
            None => None,
        }
    };

    let Some(info) = info else {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": "no_capability",
                "message": "Nothing in the catalogue matches that. Browse GET /v1/capabilities, or try /v1/search which also searches the wider x402 ecosystem.",
                "query": if explicit.is_empty() { query } else { explicit },
            })),
        )
            .into_response());
    };

    let offers = state
        .db
        .list_capability_offers(&info.slug, &CapabilitySort::Price, None)
        .await
        .map_err(|err| {
            tracing::error!("offers unavailable for {}: {}", info.slug, err);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let stats = state.cache.peek_provider_stats().await.unwrap_or_default();

    // Apply the hard constraints, keeping every rejection and its rule.
    let mut survivors: Vec<Value> = Vec::new();
    let mut rejected: Vec<Value> = Vec::new();
    // Everything that was on the table, in the order it was served, before any
    // constraint removed anything. This is what the comparison record stores:
    // a decision whose menu is unknown cannot be judged against the
    // alternatives later, and judging it is the reason the record exists.
    let mut considered: Vec<Value> = Vec::new();
    for offer in &offers {
        let missing = super::capabilities::missing_inputs(&info.input_contract, &offer.input_map);
        let stat = stats.get(&offer.api_slug);
        let shaped = json!({
            "api_slug": offer.api_slug,
            "api_name": offer.api_name,
            "method": offer.endpoint_method,
            "path": offer.endpoint_path,
            "price_usd": offer.price_usd,
            "call_url": offer.call_url,
            "settlement": "apiosk-proxied",
            "inputs": offer.input_map,
            "fixed_params": offer.fixed_params,
            "missing_inputs": missing,
            "notes": offer.notes,
            "performance": super::search::performance_json(stat, offer.hosted_externally),
        });

        considered.push(shaped.clone());
        match evaluate(offer, stat, &missing, &params) {
            Some(reason) => {
                let mut r = shaped;
                r["rejected_by"] = json!(reason.rule());
                r["because"] = json!(reason.because());
                rejected.push(r);
            }
            None => survivors.push(shaped),
        }
    }

    // Recorded even when nothing survives. A comparison that produced no
    // usable candidate is the most informative row in the set: it says demand
    // existed and the catalogue could not serve it.
    let comparison_id = crate::decision_record::record_comparison_now(
        &state,
        &headers,
        &info.slug,
        "select",
        params.max_price,
        &considered,
        None,
    )
    .await;

    if survivors.is_empty() {
        return Ok(Json(json!({
            "capability": info.slug,
            "comparison_id": comparison_id,
            "chosen": Value::Null,
            "why": "Every candidate failed a constraint. The rejections below name which one, so a caller can relax the binding constraint rather than guess.",
            "rejected": rejected,
            "meta": { "considered": offers.len(), "prefer": params.prefer.as_deref().unwrap_or("cheapest") },
        }))
        .into_response());
    }

    super::search::rank_offers(&mut survivors, params.prefer.as_deref());
    let chosen = survivors.remove(0);
    let alternatives: Vec<Value> = survivors.into_iter().take(MAX_ALTERNATIVES).collect();

    let why = explain(&chosen, &params, preference_label(params.prefer.as_deref()));

    // The selection is a decision, so it is recorded like one. Detached: the
    // caller is waiting on an answer that is already complete.
    let decision_id = record(
        &state,
        &headers,
        &info.slug,
        comparison_id,
        &chosen,
        &rejected,
        &why,
    )
    .await;

    Ok(Json(json!({
        "capability": info.slug,
        "capability_name": info.name,
        "chosen": chosen,
        "why": why,
        "alternatives": alternatives,
        "rejected": rejected,
        "decision_id": decision_id,
        "comparison_id": comparison_id,
        "meta": {
            "considered": offers.len(),
            "prefer": params.prefer.as_deref().unwrap_or("cheapest"),
            "constraints_applied": applied(&params),
            "outcome_url": decision_id.map(|id| format!("https://gateway.apiosk.com/v1/decisions/{id}/outcome")),
            "disagreeing": "Every rejection names the rule that removed it and every runner-up is listed in order, so overruling this costs one read rather than another request.",
            "not_considered": "Result quality is not measured anywhere, and federated candidates are never timed by Apiosk because it does not proxy them.",
        },
    }))
    .into_response())
}

fn preference_label(prefer: Option<&str>) -> &'static str {
    match prefer.map(str::trim) {
        Some("fastest") => "the lowest measured median latency",
        Some("most_reliable") | Some("most-reliable") => "the highest measured success rate",
        _ => "the lowest price",
    }
}

/// One sentence naming the rule that picked the winner, and the constraints it
/// had to clear first.
fn explain(chosen: &Value, params: &SelectQuery, preference: &str) -> String {
    let slug = chosen["api_slug"].as_str().unwrap_or("it");
    let constraints = applied(params);
    if constraints.is_empty() {
        return format!("{slug} has {preference} of every candidate for this capability.");
    }
    format!(
        "{slug} has {preference} among the candidates that satisfied {}.",
        constraints.join(", ")
    )
}

fn applied(params: &SelectQuery) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(p) = params.max_price {
        out.push(format!("max_price={p}"));
    }
    if let Some(l) = params.max_latency_ms {
        out.push(format!("max_latency_ms={l}"));
    }
    if let Some(r) = params.min_success_rate {
        out.push(format!("min_success_rate={r}"));
    }
    if let Some(s) = params.settlement.as_deref() {
        out.push(format!("settlement={s}"));
    }
    if params.require_all_inputs {
        out.push("require_all_inputs".to_string());
    }
    out
}

/// Writes the selection into the decision record. Failure is logged and
/// dropped: the answer is already correct without it.
#[allow(clippy::too_many_arguments)]
async fn record(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    capability: &str,
    comparison_id: Option<uuid::Uuid>,
    chosen: &Value,
    rejected: &[Value],
    why: &str,
) -> Option<uuid::Uuid> {
    let row = DecisionInsert {
        comparison_id,
        capability_slug: capability.to_string(),
        chosen_api_slug: chosen["api_slug"].as_str()?.to_string(),
        chosen_method: chosen["method"].as_str().map(str::to_string),
        chosen_path: chosen["path"].as_str().map(str::to_string),
        chosen_price_usd: chosen["price_usd"].as_f64(),
        reason: Some(why.to_string()),
        // Marks who did the choosing. A decision Apiosk made must be
        // distinguishable from one an agent made, or the record cannot be used
        // to judge Apiosk's own selections later.
        policy: Some("apiosk-select".to_string()),
        rejected: Value::Array(
            rejected
                .iter()
                .map(|r| {
                    json!({
                        "api_slug": r["api_slug"],
                        "path": r["path"],
                        "reason": r["because"],
                    })
                })
                .collect(),
        ),
        supersedes: None,
        agent_name: headers
            .get("x-agent-name")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
        wallet_address: headers
            .get("x-wallet-address")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_lowercase()),
        request_id: None,
    };

    match state.db.insert_decision(&row).await {
        Ok(id) => Some(id),
        Err(err) => {
            tracing::warn!("selection not recorded: {}", err);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q() -> SelectQuery {
        SelectQuery {
            include_unreliable: false,
            q: None,
            capability: None,
            max_price: None,
            max_latency_ms: None,
            min_success_rate: None,
            settlement: None,
            require_all_inputs: false,
            prefer: None,
        }
    }

    fn offer(slug: &str, price: f64, federated: bool, callable: bool) -> CapabilityOffer {
        CapabilityOffer {
            capability_slug: "test.capability".into(),
            endpoint_id: Some(uuid::Uuid::new_v4()),
            api_slug: slug.into(),
            api_name: slug.into(),
            endpoint_method: "GET".into(),
            endpoint_path: "/x".into(),
            endpoint_description: None,
            price_usd: price,
            payment_required: true,
            hosted_externally: federated,
            call_url: callable.then(|| "https://x.dev/x".to_string()),
            input_map: json!({}),
            fixed_params: json!({}),
            request_body: json!({}),
            query_params: json!({}),
            notes: None,
            source: "curated".into(),
            confidence: 1.0,
            provider_name: None,
            provider_username: None,
            search_text: None,
        }
    }

    fn stats(p50: i64, rate: f64) -> ProviderStats {
        ProviderStats {
            api_slug: "x".into(),
            attempts: 500,
            successes: (500.0 * rate) as i64,
            recent_attempts: 100,
            recent_successes: (100.0 * rate) as i64,
            success_rate: rate,
            p50_ms: Some(p50),
            p95_ms: Some(p50 * 3),
        }
    }

    #[test]
    fn an_uncallable_candidate_is_never_selected() {
        let o = offer("dead", 0.001, true, false);
        assert_eq!(evaluate(&o, None, &[], &q()), Some(Rejection::NotCallable));
    }

    #[test]
    fn budget_is_a_hard_constraint() {
        let o = offer("dear", 0.5, false, true);
        let mut p = q();
        p.max_price = Some(0.1);
        assert_eq!(evaluate(&o, None, &[], &p), Some(Rejection::OverBudget));
        p.max_price = Some(0.5);
        assert_eq!(evaluate(&o, None, &[], &p), None, "equal to the cap passes");
    }

    #[test]
    fn a_latency_floor_rejects_the_unmeasured_rather_than_assuming_them_fast() {
        // The failure that matters: treating "unknown" as "meets your bar"
        // would hand the caller the least-known provider precisely when it
        // asked for a guarantee.
        let o = offer("unknown", 0.001, true, true);
        let mut p = q();
        p.max_latency_ms = Some(500);
        assert_eq!(evaluate(&o, None, &[], &p), Some(Rejection::Unmeasured));
    }

    #[test]
    fn measured_candidates_are_judged_on_their_numbers() {
        let o = offer("slow", 0.001, false, true);
        let mut p = q();
        p.max_latency_ms = Some(500);
        assert_eq!(
            evaluate(&o, Some(&stats(1415, 0.99)), &[], &p),
            Some(Rejection::TooSlow)
        );
        assert_eq!(evaluate(&o, Some(&stats(198, 0.99)), &[], &p), None);

        // Above Apiosk's own floor, below the caller's: the rejection has to
        // be the caller's, or the test says nothing about their constraint.
        let mut p2 = q();
        p2.min_success_rate = Some(0.9);
        assert_eq!(
            evaluate(&o, Some(&stats(198, 0.72)), &[], &p2),
            Some(Rejection::Unreliable)
        );
    }

    #[test]
    fn a_listing_that_fails_more_than_it_works_is_not_offered() {
        // countrylayer, as measured 2026-08-30: 269 served of 1621 tried, and
        // still on the shelf. Nobody asked for a reliability floor here — that
        // is the point. Handing this to somebody who asked for a working
        // answer is the failure the gate exists to stop.
        let o = offer("countrylayer", 0.001, false, true);
        assert_eq!(
            evaluate(&o, Some(&stats(198, 0.17)), &[], &q()),
            Some(Rejection::Quarantined)
        );

        // Named, not hidden: a caller who disagrees gets it back in one retry.
        let mut allowed = q();
        allowed.include_unreliable = true;
        assert_eq!(evaluate(&o, Some(&stats(198, 0.17)), &[], &allowed), None);
    }

    #[test]
    fn settlement_can_be_required_in_either_direction() {
        let managed = offer("m", 0.05, false, true);
        let federated = offer("f", 0.001, true, true);
        let mut p = q();
        p.settlement = Some("apiosk".into());
        assert_eq!(evaluate(&managed, None, &[], &p), None);
        assert_eq!(
            evaluate(&federated, None, &[], &p),
            Some(Rejection::WrongSettlement)
        );
        p.settlement = Some("direct".into());
        assert_eq!(evaluate(&federated, None, &[], &p), None);
        assert_eq!(
            evaluate(&managed, None, &[], &p),
            Some(Rejection::WrongSettlement)
        );
    }

    #[test]
    fn partial_offers_are_kept_unless_the_caller_refuses_them() {
        let o = offer("partial", 0.001, false, true);
        let missing = vec!["quote".to_string()];
        assert_eq!(evaluate(&o, None, &missing, &q()), None);
        let mut p = q();
        p.require_all_inputs = true;
        assert_eq!(
            evaluate(&o, None, &missing, &p),
            Some(Rejection::MissingInputs)
        );
    }

    #[test]
    fn every_rejection_names_a_rule_and_a_cause() {
        for r in [
            Rejection::OverBudget,
            Rejection::TooSlow,
            Rejection::Unreliable,
            Rejection::WrongSettlement,
            Rejection::MissingInputs,
            Rejection::NotCallable,
            Rejection::Unmeasured,
        ] {
            assert!(!r.rule().is_empty());
            assert!(!r.because().is_empty());
        }
    }

    #[test]
    fn the_explanation_names_the_constraints_it_cleared() {
        let chosen = json!({"api_slug": "tavily"});
        let mut p = q();
        p.max_price = Some(0.08);
        p.require_all_inputs = true;
        let why = explain(&chosen, &p, "the lowest price");
        assert!(why.contains("tavily"));
        assert!(why.contains("max_price=0.08"));
        assert!(why.contains("require_all_inputs"));

        // With nothing bound it must not imply constraints that were not set.
        let plain = explain(&chosen, &q(), "the lowest price");
        assert!(plain.contains("every candidate"));
        assert!(!plain.contains("max_price"));
    }
}
