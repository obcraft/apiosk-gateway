//! `discover` → `compare` → `decide`.
//!
//! Three functions that chain, rather than three endpoints that each re-derive
//! the world. `discover` returns candidate ids; `compare` takes those ids and
//! turns them into evidence; `decide` turns the evidence into one purchase.
//!
//! The chaining is the point. Without stable ids each step has to redo the
//! match, which means the set an agent compared is not provably the set it
//! discovered, and the decision cannot be attributed to a menu. With them,
//! Apiosk records the sequence it is uniquely placed to see:
//!
//! ```text
//! discovered → considered → filtered → ranked → selected → rejected
//! ```
//!
//! A vendor sees only the transactions it wins. This sees the choice.
//!
//! ---------------------------------------------------------------------------
//! On the score
//! ---------------------------------------------------------------------------
//!
//! `compare` returns a 0..100 score, which is a number that invites being
//! trusted without being understood. So it is never returned alone: every
//! response carries the weights that produced it and each candidate's
//! contribution per dimension. A score you can recompute is an argument; one
//! you cannot is an advertisement.
//!
//! Dimensions that are not measured do not silently score zero, which would
//! punish a provider for Apiosk's blind spot. They are dropped from the
//! weighting and the response says which, so a score over two dimensions is
//! never mistaken for a score over four.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::db::{CapabilityInfo, CapabilityOffer, CapabilitySort, DecisionInsert, ProviderStats};
use crate::query_plan::{PlanSource, QueryPlan};
use crate::AppState;

/// An agent should never receive the whole catalogue. This is the ceiling on
/// what one discover call will hand back, whatever it asks for.
const MAX_CANDIDATES: usize = 25;
const DEFAULT_CANDIDATES: usize = 8;

/// Capabilities one discover call will draw candidates from.
///
/// More than one because a request is often more than one need: "the bitcoin
/// price and the news about it" is two, and answering it from a single
/// capability drops half the answer without saying so. Capped low because the
/// point is a candidate set, not a directory.
const MAX_CAPABILITIES: usize = 4;

/// Widened terms one rescue search will spend a query on. The enrichment can
/// return four needs with a dozen labels each; searching all of them would put
/// fifty round trips behind a request that has already missed once.
const MAX_WIDENED_TERMS: usize = 12;

/// The rank a widened term's match has to clear. Twice `MIN_RANK`, because the
/// words came from the enrichment rather than from the caller — see the note at
/// the filter itself.
const WIDENED_MIN_RANK: f64 = crate::db::MIN_RANK * 2.0;

/// Capabilities taken per need. Two rather than one so a near-tie does not turn
/// on a hundredth of a `ts_rank`; the relative floor in `search_capabilities`
/// already drops anything that is not genuinely close to the best match.
const CAPABILITIES_PER_TASK: i64 = 2;

/// How much each dimension is worth when it is available. Published with every
/// score so the number can be recomputed by whoever reads it.
const W_PRICE: f64 = 0.4;
const W_LATENCY: f64 = 0.25;
const W_RELIABILITY: f64 = 0.25;
const W_INPUTS: f64 = 0.1;

// ---------------------------------------------------------------------------
// Shared shapes
// ---------------------------------------------------------------------------

/// Constraints and preferences, identical across the three functions so an
/// agent states them once and can pass the same query string down the chain.
#[derive(Debug, Default, Deserialize)]
pub struct Requirements {
    pub max_price: Option<f64>,
    pub max_latency_ms: Option<i64>,
    /// 0..1, or 0..100 for callers that think in percent. Both are accepted
    /// because both get sent, and rejecting a provider for a unit mistake is a
    /// bad way to learn about it.
    pub min_reliability: Option<f64>,
    /// `apiosk` (proxied and settled here) or `direct` (pay the provider).
    pub settlement: Option<String>,
    #[serde(default, deserialize_with = "super::search::lenient_bool")]
    pub require_all_inputs: bool,
    /// `price` (default) | `latency` | `reliability` | `balanced`.
    pub optimize_for: Option<String>,
    /// `true` to consider listings Apiosk has quarantined for failing more
    /// often than they work. Off by default, and named rather than silent:
    /// the rejection says which listing was held back and why, so a caller
    /// who disagrees can ask for it in one retry.
    #[serde(default, deserialize_with = "super::search::lenient_bool")]
    pub include_unreliable: bool,
}

impl Requirements {
    /// Reliability as a fraction, whichever unit the caller used.
    fn reliability_floor(&self) -> Option<f64> {
        self.min_reliability
            .map(|v| if v > 1.0 { v / 100.0 } else { v })
    }

    fn weights(&self) -> (f64, f64, f64, f64) {
        match self.optimize_for.as_deref().map(str::trim) {
            Some("latency") => (0.15, 0.55, 0.2, 0.1),
            Some("reliability") => (0.15, 0.2, 0.55, 0.1),
            Some("balanced") => (0.25, 0.25, 0.25, 0.25),
            _ => (W_PRICE, W_LATENCY, W_RELIABILITY, W_INPUTS),
        }
    }

    /// Constraints the caller stated in prose rather than as parameters.
    ///
    /// An explicit parameter always wins. The caller typed `max_price=0.01`;
    /// the other value is a model's reading of a sentence, and a reading must
    /// never override a statement.
    pub(super) fn with_plan_defaults(mut self, plan: &QueryPlan) -> Self {
        if self.max_price.is_none() {
            self.max_price = plan.max_price_usd;
        }
        if self.optimize_for.is_none() {
            self.optimize_for = plan.optimize_for.clone();
        }
        self
    }

    fn optimize_label(&self) -> &'static str {
        match self.optimize_for.as_deref().map(str::trim) {
            Some("latency") => "latency",
            Some("reliability") => "reliability",
            Some("balanced") => "balanced",
            _ => "price",
        }
    }
}

/// Why a candidate was excluded, in the words the response uses.
fn excluded_by(
    offer: &CapabilityOffer,
    stats: Option<&ProviderStats>,
    missing: &[String],
    req: &Requirements,
) -> Option<(&'static str, String)> {
    if offer.call_url.is_none() {
        return Some((
            "callable",
            "no resolvable call target, so it cannot be bought".into(),
        ));
    }
    if let Some(cap) = req.max_price {
        if offer.price_usd > cap {
            return Some((
                "max_price",
                format!("costs ${:.6}, above the ${cap:.6} ceiling", offer.price_usd),
            ));
        }
    }
    if req.require_all_inputs && !missing.is_empty() {
        return Some((
            "require_all_inputs",
            format!("does not accept {}", missing.join(", ")),
        ));
    }
    // `settlement=apiosk` used to exclude federated listings and
    // `settlement=direct` used to exclude managed ones. Every listing settles
    // through Apiosk now, so only the `direct` filter can still reject, and it
    // rejects everything — there is no direct-to-provider rail to ask for.
    if req.settlement.as_deref().map(str::trim) == Some("direct") {
        return Some((
            "settlement",
            "is settled through Apiosk; there is no direct-to-provider rail".into(),
        ));
    }

    // Apiosk's own floor, applied whether or not the caller thought to ask
    // for one. A listing measured below it is not a cheap option with a
    // caveat, it is an outage with a price, and handing one to somebody who
    // asked for a working answer is the failure this gate exists to stop.
    // Named in `rejected` like every other exclusion, never dropped quietly.
    if !req.include_unreliable {
        if let Some(s) = stats.filter(|s| s.is_quarantined()) {
            return Some((
                "reliability",
                format!(
                    "is failing right now — {:.0}% of its last {} calls came back, below the {:.0}% floor Apiosk offers at. Still callable by name, and returns on its own once it works again; pass include_unreliable=true to consider it now",
                    s.recent_success_rate().unwrap_or_default() * 100.0,
                    s.recent_attempts,
                    crate::db::QUARANTINE_FLOOR * 100.0,
                ),
            ));
        }
    }

    let floor = req.reliability_floor();
    let wants_measurement = req.max_latency_ms.is_some() || floor.is_some();
    match stats {
        None if wants_measurement => Some((
            "measured",
            "has no measurement, and a latency or reliability floor cannot be met by a claim"
                .into(),
        )),
        Some(s) => {
            if let Some(cap) = req.max_latency_ms {
                // Judged on p95 rather than the median, because a ceiling is a
                // promise about the slow case. A provider whose median clears
                // the bar and whose tail does not will still blow the caller's
                // timeout one request in twenty.
                match s.p95_ms {
                    Some(p95) if p95 > cap => {
                        return Some((
                            "max_latency_ms",
                            format!("p95 is {p95}ms, above the {cap}ms ceiling"),
                        ));
                    }
                    // Measured for reliability, not yet timed. A ceiling
                    // cannot be met by a claim, so it fails the same way an
                    // entirely unmeasured candidate does.
                    None => {
                        return Some((
                            "measured",
                            "has too few successful calls to have a latency, and a ceiling cannot be met by a claim".into(),
                        ));
                    }
                    _ => {}
                }
            }
            if let Some(f) = floor {
                // The current rate, for the same reason the gate uses it: a
                // caller asking for 90% is asking about now, not about July.
                let rate = s.current_success_rate();
                if rate < f {
                    return Some((
                        "min_reliability",
                        format!(
                            "succeeds {:.1}% of the time, below the {:.1}% floor",
                            rate * 100.0,
                            f * 100.0
                        ),
                    ));
                }
            }
            None
        }
        None => None,
    }
}

/// Per-dimension contributions and the total, on 0..100.
///
/// Each dimension is scored relative to the field being compared, not against
/// an absolute Apiosk knows nothing about: the cheapest candidate scores 1 on
/// price and the dearest 0. That makes the number meaningful only within one
/// comparison, which is exactly what it is for, and the response says so.
fn score(
    offer: &CapabilityOffer,
    stats: Option<&ProviderStats>,
    missing: &[String],
    field: &Field,
    req: &Requirements,
) -> (f64, Value, Vec<&'static str>) {
    let (wp, wl, wr, wi) = req.weights();
    let mut parts: Vec<(&'static str, f64, f64)> = Vec::new();
    let mut unscored: Vec<&'static str> = Vec::new();

    parts.push(("price", wp, field.price_score(offer.price_usd)));

    match stats {
        Some(s) => {
            // Split, because the two are now measurable independently: a
            // listing with nine successes out of eighty has a reliability
            // record and no timing at all.
            match s.p95_ms {
                Some(p95) => parts.push(("latency", wl, field.latency_score(p95))),
                None => unscored.push("latency"),
            }
            parts.push(("reliability", wr, s.current_success_rate().clamp(0.0, 1.0)));
        }
        None => {
            // Not zero. Zero would rank a provider Apiosk has never proxied
            // below one it has measured badly, which says more about our
            // coverage than about them.
            unscored.push("latency");
            unscored.push("reliability");
        }
    }
    parts.push(("inputs", wi, if missing.is_empty() { 1.0 } else { 0.0 }));

    let total_weight: f64 = parts.iter().map(|(_, w, _)| w).sum();
    let raw: f64 = parts.iter().map(|(_, w, v)| w * v).sum();
    let normalised = if total_weight > 0.0 {
        raw / total_weight
    } else {
        0.0
    };

    let breakdown = json!({
        "dimensions": parts.iter().map(|(name, w, v)| json!({
            "dimension": name,
            "weight": (w / total_weight * 1000.0).round() / 1000.0,
            "value": (v * 1000.0).round() / 1000.0,
            // On the same 0..100 scale as the total, so the contributions add
            // up to it. Published on a 0..1 scale they silently did not, which
            // defeats the one thing the breakdown is for: a score you can
            // recompute is an argument, one you cannot is an advertisement.
            "contribution": (w * v / total_weight * 10_000.0).round() / 100.0,
        })).collect::<Vec<_>>(),
        "not_scored": unscored,
    });

    ((normalised * 100.0).round(), breakdown, unscored)
}

/// Whether a candidate can be judged on the dimension the caller optimised for.
///
/// Asking for the fastest and receiving a provider nobody has ever timed is not
/// an answer to the question. Dropping the unmeasured dimension from the
/// weighting keeps a provider from being punished for Apiosk's blind spot, but
/// it also lets that provider win a race it never ran. So when the caller names
/// latency or reliability, candidates measured on it sort above those that are
/// not, and the score still says which is which.
fn ranks_ahead_on_optimised_dimension(req: &Requirements, measured: bool) -> u8 {
    match req.optimize_for.as_deref().map(str::trim) {
        Some("latency") | Some("reliability") if !measured => 1,
        _ => 0,
    }
}

/// The range each dimension is normalised against, taken from the candidates
/// actually being compared.
struct Field {
    min_price: f64,
    max_price: f64,
    min_latency: f64,
    max_latency: f64,
}

impl Field {
    fn of(
        offers: &[&CapabilityOffer],
        stats: &std::collections::HashMap<String, ProviderStats>,
    ) -> Self {
        let prices: Vec<f64> = offers.iter().map(|o| o.price_usd).collect();
        let lats: Vec<f64> = offers
            .iter()
            .filter_map(|o| stats.get(&o.api_slug))
            // p95, not the median. An agent sets a timeout against the slow
            // case, and a provider with a fast median and a long tail is the
            // one that breaks it. The median flatters exactly the providers a
            // decision should be wary of.
            .filter_map(|s| s.p95_ms)
            .map(|p95| p95 as f64)
            .collect();
        Self {
            min_price: prices.iter().cloned().fold(f64::INFINITY, f64::min),
            max_price: prices.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
            min_latency: lats.iter().cloned().fold(f64::INFINITY, f64::min),
            max_latency: lats.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        }
    }

    /// 1 for the cheapest, 0 for the dearest, 1 when they all cost the same.
    fn price_score(&self, price: f64) -> f64 {
        if !self.max_price.is_finite() || (self.max_price - self.min_price).abs() < f64::EPSILON {
            return 1.0;
        }
        (1.0 - (price - self.min_price) / (self.max_price - self.min_price)).clamp(0.0, 1.0)
    }

    fn latency_score(&self, p95: i64) -> f64 {
        if !self.max_latency.is_finite()
            || (self.max_latency - self.min_latency).abs() < f64::EPSILON
        {
            return 1.0;
        }
        (1.0 - (p95 as f64 - self.min_latency) / (self.max_latency - self.min_latency))
            .clamp(0.0, 1.0)
    }
}

// ---------------------------------------------------------------------------
// 0. parse — the reading alone
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ParseOnlyQuery {
    pub q: Option<String>,
    pub token: Option<String>,
}

/// The tuning endpoints are for the testbench, not for agents, the n8n node,
/// or MCP. A request without the configured token gets a bare 404 — the same
/// answer an unknown route gives, so the endpoints don't advertise their own
/// existence. No token configured means no access for anyone.
fn debug_endpoints_gate(state: &AppState, token: Option<&str>) -> Option<Response> {
    let authorized = matches!(
        (&state.config.debug_endpoints_token, token),
        (Some(expected), Some(given)) if expected == given
    );
    if authorized {
        None
    } else {
        Some(StatusCode::NOT_FOUND.into_response())
    }
}

/// `GET /v1/parse` — step one in isolation: the job text read into needs and
/// search terms, and nothing else. No catalogue search runs and no candidates
/// are returned; the endpoint exists so the reading itself can be inspected
/// and tuned without the rest of the chain in the way.
///
/// Always parses — there is no keyword gate here, because a caller asking for
/// the parse explicitly wants the model's reading even of a short query.
pub async fn parse_only(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ParseOnlyQuery>,
) -> Result<Response, StatusCode> {
    if let Some(denied) = debug_endpoints_gate(&state, params.token.as_deref()) {
        return Ok(denied);
    }
    let query = params.q.as_deref().unwrap_or("").trim().to_string();
    if query.is_empty() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "q_required",
                "message": "Pass the job text: GET /v1/parse?q=<job in words>.",
            })),
        )
            .into_response());
    }

    let plan = crate::query_plan::plan(&state, &query).await;
    Ok(Json(json!({
        "query": query,
        // What the automatic gate WOULD have decided on /v1/discover — shown
        // for context; this endpoint parses regardless.
        "gate_would_parse": crate::query_plan::should_parse(&query),
        "interpretation": plan.to_json(),
        "note": "The reading only. Nothing was searched; feed these terms to /v1/discover to see what they match.",
    }))
    .into_response())
}

#[derive(Debug, Deserialize)]
pub struct MatchTermsQuery {
    /// Comma-separated search terms. Each term is searched on its own, so the
    /// caller sees what every phrase matches individually — the joined-query
    /// dilution that hides inside discover cannot hide here.
    pub terms: Option<String>,
    /// Capabilities returned per term. Defaults to the same figure discover
    /// uses per task.
    pub per_term: Option<i64>,
    pub token: Option<String>,
}

/// `GET /v1/match?terms=a,b,c` — step 1c in isolation: search terms to
/// catalogue capabilities, with the raw score each term earned. No model call,
/// no offers, no candidates. Chain it after `/v1/parse` by hand to walk the
/// discover pipeline one substep at a time.
pub async fn match_terms(
    State(state): State<Arc<AppState>>,
    Query(params): Query<MatchTermsQuery>,
) -> Result<Response, StatusCode> {
    if let Some(denied) = debug_endpoints_gate(&state, params.token.as_deref()) {
        return Ok(denied);
    }
    let terms: Vec<String> = params
        .terms
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    if terms.is_empty() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "terms_required",
                "message": "Pass comma-separated search terms: GET /v1/match?terms=crypto market data,token price.",
            })),
        )
            .into_response());
    }

    let per_term = params
        .per_term
        .unwrap_or(CAPABILITIES_PER_TASK)
        .clamp(1, 10);
    let mut per_term_results = Vec::new();
    let mut merged: Vec<String> = Vec::new();
    for term in &terms {
        let started = std::time::Instant::now();
        let hits = state
            .db
            .search_capabilities(term, per_term)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        per_term_results.push(json!({
            "term": term,
            "ms": started.elapsed().as_millis() as u64,
            "matches": hits.iter().map(|h| json!({
                "slug": h.slug,
                "name": h.name,
                "score": h.score,
                "matched_via": h.matched_via,
                "offer_count": h.offer_count,
            })).collect::<Vec<_>>(),
        }));
        for hit in hits {
            if !merged.contains(&hit.slug) {
                merged.push(hit.slug);
            }
        }
    }

    Ok(Json(json!({
        "terms": terms,
        "per_term": per_term_results,
        "merged_capabilities": merged,
        "thresholds": {
            "min_rank": crate::db::MIN_RANK,
            "relative_floor": crate::db::RELATIVE_FLOOR,
        },
        "note": "Matching only. Feed a merged slug to /v1/capabilities/{slug} for its offers, or to /v1/compare to score them.",
    }))
    .into_response())
}

#[derive(Debug, Deserialize)]
pub struct EnrichOnlyQuery {
    pub q: Option<String>,
    pub token: Option<String>,
}

/// `GET /v1/enrich` — step 1b in isolation: the parsed job, extended with the
/// vocabulary a catalogue files it under — tags, categories, sectors, extra
/// phrasings. Runs the parse first (cached, so a chained 1a → 1b costs one
/// model call for the parse, not two) and then one further call on the same
/// model. Nothing is searched; `search_terms` is the merged list to hand to
/// `/v1/find`.
pub async fn enrich_only(
    State(state): State<Arc<AppState>>,
    Query(params): Query<EnrichOnlyQuery>,
) -> Result<Response, StatusCode> {
    if let Some(denied) = debug_endpoints_gate(&state, params.token.as_deref()) {
        return Ok(denied);
    }
    let query = params.q.as_deref().unwrap_or("").trim().to_string();
    if query.is_empty() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "q_required",
                "message": "Pass the job text: GET /v1/enrich?q=<job in words>.",
            })),
        )
            .into_response());
    }

    let plan = crate::query_plan::plan(&state, &query).await;
    // The catalogue's own labels, handed to the model as vocabulary. An empty
    // list on a db hiccup only costs grounding, not the enrichment itself.
    let known_categories = state.db.capability_categories().await.unwrap_or_default();
    let enrichment = crate::enrich::enrich(&state, &query, &plan, &known_categories).await;

    // Parser keywords first — they are the caller's own words and the
    // strongest signal — then every label the enrichment added.
    let mut search_terms = plan.keywords();
    for term in enrichment.search_terms() {
        if !search_terms.iter().any(|t| t.eq_ignore_ascii_case(&term)) {
            search_terms.push(term);
        }
    }

    Ok(Json(json!({
        "query": query,
        "parse": plan.to_json(),
        "enrichment": enrichment.to_json(),
        "known_categories": known_categories,
        "search_terms": search_terms,
        "note": "The extension only. Nothing was searched; feed search_terms to /v1/find to see what they match, on Apiosk and across the x402 ecosystem.",
    }))
    .into_response())
}

#[derive(Debug, Deserialize)]
pub struct FindQuery {
    /// Comma-separated search terms — parser keywords plus enrichment labels.
    /// Semicolons separate needs, and each need then gets its own share of
    /// the queried indexes' HTTP slots.
    pub terms: Option<String>,
    /// Capabilities returned per term against the Apiosk catalogue.
    pub per_term: Option<i64>,
    /// Also sweep the external x402 indexes (Bazaar and friends). On by
    /// default: the point of this step is every source, not just the shelf.
    #[serde(
        default = "super::search::default_true",
        deserialize_with = "super::search::lenient_bool"
    )]
    pub external: bool,
    pub token: Option<String>,
}

/// `GET /v1/find?terms=a,b,c` — step 1c in isolation: the extended vocabulary
/// searched everywhere at once. The Apiosk catalogue is matched per term (the
/// same scoring `/v1/match` shows), and the free x402 indexes — Coinbase
/// Bazaar, thirdweb, PayAI and the rest — are swept concurrently with it, so
/// wall clock is the slower of the two rather than the sum. Catalogue hits are
/// reviewed capabilities; external hits are unverified offers and stay in
/// their own block, labelled per source.
pub async fn find_all(
    State(state): State<Arc<AppState>>,
    Query(params): Query<FindQuery>,
) -> Result<Response, StatusCode> {
    if let Some(denied) = debug_endpoints_gate(&state, params.token.as_deref()) {
        return Ok(denied);
    }
    // Commas separate phrases, semicolons separate needs — the same grammar
    // `/v1/discover?terms=` uses (see `term_groups`). Need structure matters
    // here: each need gets its own share of the queried indexes' HTTP slots.
    let groups = term_groups(params.terms.as_deref().unwrap_or(""));
    let mut terms: Vec<String> = Vec::new();
    for term in groups.iter().flatten() {
        if !terms.iter().any(|t| t.eq_ignore_ascii_case(term)) {
            terms.push(term.clone());
        }
    }
    if terms.is_empty() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "terms_required",
                "message": "Pass comma-separated search terms: GET /v1/find?terms=nvidia earnings,stock news. Semicolons separate needs — terms=bitcoin price;bitcoin news gives each need its own share of the queried indexes.",
            })),
        )
            .into_response());
    }

    let per_term = params
        .per_term
        .unwrap_or(CAPABILITIES_PER_TASK)
        .clamp(1, 10);

    let catalog_search = async {
        let mut per_term_results = Vec::new();
        let mut merged: Vec<String> = Vec::new();
        for term in &terms {
            let started = std::time::Instant::now();
            let hits = state.db.search_capabilities(term, per_term).await?;
            per_term_results.push(json!({
                "term": term,
                "ms": started.elapsed().as_millis() as u64,
                "matches": hits.iter().map(|h| json!({
                    "slug": h.slug,
                    "name": h.name,
                    "score": h.score,
                    "matched_via": h.matched_via,
                    "offer_count": h.offer_count,
                })).collect::<Vec<_>>(),
            }));
            for hit in hits {
                if !merged.contains(&hit.slug) {
                    merged.push(hit.slug);
                }
            }
        }

        // The merged slugs again, but as full catalogue cards: description,
        // category, price range across offers, and the input/output contracts
        // — so the caller sees per result what it is, what it costs and what
        // it takes/returns, next to the external hits that mostly publish
        // none of that.
        let all = state.db.list_capabilities().await?;
        let capabilities: Vec<Value> = merged
            .iter()
            .filter_map(|slug| all.iter().find(|c| &c.slug == slug))
            .map(|c| {
                json!({
                    "source": "apiosk",
                    "verified": true,
                    "slug": c.slug,
                    "name": c.name,
                    "description": c.description,
                    "category": c.category,
                    "min_price_usd": c.min_price_usd,
                    "max_price_usd": c.max_price_usd,
                    "offer_count": c.offer_count,
                    "listing_count": c.listing_count,
                    "input_contract": c.input_contract,
                    "output_contract": c.output_contract,
                })
            })
            .collect();
        Ok::<_, anyhow::Error>((per_term_results, merged, capabilities))
    };

    let external_sweep = async {
        if !params.external {
            return (crate::sources::Sweep::default(), 0u64);
        }
        let started = std::time::Instant::now();
        let needs: Vec<crate::sources::NeedTerms> = if groups.len() > 1 {
            groups
                .iter()
                .map(|group| crate::sources::NeedTerms {
                    need: group.join(", "),
                    queried: group.clone(),
                    index: Vec::new(),
                })
                .collect()
        } else {
            vec![crate::sources::NeedTerms::flat(&terms)]
        };
        let swept = crate::sources::sweep_needs_with_status(
            &state,
            &needs,
            &[],
            crate::sources::MAX_RETURNED,
        )
        .await;
        (swept, started.elapsed().as_millis() as u64)
    };

    let (catalog, (swept, external_ms)) = tokio::join!(catalog_search, external_sweep);
    let external_offers = &swept.offers;
    let (per_term_results, merged, capabilities) =
        catalog.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let source_ids = crate::sources::source_ids(&state).await;

    Ok(Json(json!({
        "terms": terms,
        "catalog": {
            "source": "apiosk",
            "per_term": per_term_results,
            "merged_capabilities": merged,
            "capabilities": capabilities,
            "thresholds": {
                "min_rank": crate::db::MIN_RANK,
                "relative_floor": crate::db::RELATIVE_FLOOR,
            },
        },
        "external": {
            "swept": params.external,
            "reach": crate::sources::reach_sentence_for(&source_ids),
            "sources": source_ids,
            "source_status": swept.sources_json(),
            "sources_failed": swept.sources_failed(),
            "need_coverage": swept.need_coverage_json(),
            "ms": external_ms,
            "offers": crate::sources::settled_json(&state, external_offers).await,
        },
        "note": "Searching only. Catalogue hits are reviewed capabilities — feed a slug to /v1/capabilities/{slug} or /v1/compare. External offers are unverified; read their own descriptions before calling.",
    }))
    .into_response())
}

// ---------------------------------------------------------------------------
// 1. discover
// ---------------------------------------------------------------------------

/// Query strings are not self-describing, so `#[serde(flatten)]` cannot be used
/// here: flattening a struct into a urlencoded query makes every request
/// carrying one of its fields fail with a 400. The fields are therefore spelled
/// out and folded into [`Requirements`] by hand.
#[derive(Debug, Deserialize)]
pub struct DiscoverQuery {
    pub q: Option<String>,
    pub capability: Option<String>,
    pub max_candidates: Option<usize>,
    pub max_price: Option<f64>,
    pub max_latency_ms: Option<i64>,
    pub min_reliability: Option<f64>,
    #[serde(default, deserialize_with = "super::search::lenient_bool")]
    pub include_unreliable: bool,
    pub settlement: Option<String>,
    #[serde(default, deserialize_with = "super::search::lenient_bool")]
    pub require_all_inputs: bool,
    pub optimize_for: Option<String>,
    /// Also sweep the wider x402 ecosystem, not just the reviewed catalogue.
    ///
    /// Defaults to on, like `/v1/search`. An agent asking "what can do this?"
    /// is asking about the world, not about our inventory, and a discovery
    /// engine that only ever answers with its own shelf is a catalogue with a
    /// search box. The results stay in a separate block because they are a
    /// different kind of thing — see `external_candidates` in the response.
    #[serde(
        default = "super::search::default_true",
        deserialize_with = "super::search::lenient_bool"
    )]
    pub include_external: bool,
    /// How many external offers to return, up to [`crate::sources::MAX_LIMIT`].
    ///
    /// Defaults to [`crate::sources::MAX_RETURNED`]. Caps only the external
    /// half: the reviewed half has `max_candidates`, and the two are different
    /// kinds of list. A cross-source explore page is the caller that wants
    /// more than an agent's twenty-five.
    pub limit: Option<usize>,
    /// Whether to read `q` with the query parser (see `crate::query_plan`).
    ///
    /// Three states, which is why this is a string rather than a bool: absent
    /// means "decide from the query" — a sentence or a non-English prompt is
    /// parsed, a keyword query is not, so crawlers never pay for a model call.
    /// `parse=false` forces the literal path; `parse=true` forces a parse even
    /// for a short query, which is the escape hatch when the automatic gate
    /// reads a query wrong.
    pub parse: Option<String>,
    /// Whether an empty catalogue match may be widened with the vocabulary a
    /// marketplace files the job under (see `crate::enrich`, step 1b of the
    /// pipeline the testbench walks).
    ///
    /// Three states, like `parse`. Absent means "only when the query was
    /// parsed": a sentence that matched nothing is exactly the case where the
    /// caller's own words are the problem, while a bare keyword miss stays free
    /// of model calls, which is what keeps the crawler majority of this
    /// endpoint's traffic cheap. `enrich=false` never widens; `enrich=true`
    /// widens even a literal miss.
    pub enrich: Option<String>,
    /// Search terms the caller already worked out, in place of the parse.
    ///
    /// `terms=spot price,ohlcv;crypto news,market news` — commas separate the
    /// phrases within one need, semicolons separate the needs. A flat
    /// comma-separated list is one need, which is what a caller with no need
    /// structure to preserve sends.
    ///
    /// This exists so a surface can own the reading — the Gumloop flow that
    /// runs steps 1a and 1b, say — without losing the rest of the pipeline or
    /// needing the testbench's debug token, which is not something a public
    /// bundle can carry. When it is present no model runs here: no parse, and
    /// no widening either, since a caller who wrote the vocabulary has already
    /// done that half. `enrich=true` still forces the widening if they want it.
    ///
    /// It buys words and nothing else. The retrieval, the ordering and the
    /// candidate set are the same deterministic code every other request goes
    /// through — see [`crate::query_plan::PlanSource::Supplied`].
    pub terms: Option<String>,
    /// `debug=1` adds a `debug` block tracing every substep: the parse gate's
    /// decision, each task's search text with per-capability scores, and the
    /// thresholds those scores were held against. For tuning the discovery
    /// step; costs nothing to compute beyond what the request already did.
    #[serde(default, deserialize_with = "super::search::lenient_bool")]
    pub debug: bool,
}

/// `spot price,ohlcv;crypto news` into the keyword groups a plan is built from.
///
/// Empty phrases and empty groups fall out here; the caps and the deduplication
/// are [`QueryPlan::supplied`]'s job, so there is one place that decides what a
/// caller-authored term is allowed to be.
pub(super) fn term_groups(raw: &str) -> Vec<Vec<String>> {
    raw.split(';')
        .map(|group| {
            group
                .split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|group: &Vec<String>| !group.is_empty())
        .collect()
}

/// How each half of a discovery answer is ordered, stated in the response so
/// a caller never has to guess. There is deliberately no composite score
/// across the two halves — see the module note on `/v1/search`.
const RANKING_NOTE: &str = "Two orderings, never blended. `candidates`: capabilities in full-text-search rank order for q (see `interpretation` and `debug.search_passes`), offers within each by price. `external_candidates.offers`: by relevance to q — a whole phrase of q in the offer's own words outranks scattered words — then curator-preferred providers within equal relevance, then by price, with unpriced offers last.";

/// Read one of the three-state string flags (`parse`, `enrich`). `None` means
/// the caller said nothing and the endpoint decides.
fn tri_state(raw: Option<&str>) -> Option<bool> {
    let raw = raw?.trim().to_ascii_lowercase();
    match raw.as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

impl DiscoverQuery {
    /// The caller's explicit instruction about parsing, if they gave one.
    fn parse_override(&self) -> Option<bool> {
        tri_state(self.parse.as_deref())
    }

    /// The caller's explicit instruction about widening, if they gave one.
    fn enrich_override(&self) -> Option<bool> {
        tri_state(self.enrich.as_deref())
    }
}

impl DiscoverQuery {
    /// The caller's own search terms, if they sent any usable ones.
    fn supplied_plan(&self) -> Option<QueryPlan> {
        let raw = self.terms.as_deref()?.trim();
        if raw.is_empty() {
            return None;
        }
        QueryPlan::supplied(&term_groups(raw))
    }

    /// The external page size: the caller's `limit`, clamped, or the default.
    fn external_limit(&self) -> usize {
        self.limit
            .unwrap_or(crate::sources::MAX_RETURNED)
            .clamp(1, crate::sources::MAX_LIMIT)
    }

    fn requirements(&self) -> Requirements {
        Requirements {
            max_price: self.max_price,
            max_latency_ms: self.max_latency_ms,
            min_reliability: self.min_reliability,
            settlement: self.settlement.clone(),
            require_all_inputs: self.require_all_inputs,
            optimize_for: self.optimize_for.clone(),
            include_unreliable: self.include_unreliable,
        }
    }
}

/// `GET /v1/discover` — what can perform this task?
pub async fn discover(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<DiscoverQuery>,
) -> Result<Response, StatusCode> {
    let query = params.q.as_deref().unwrap_or("").trim().to_string();

    // The reading comes first, and it is published with the answer. Everything
    // after this line is the same deterministic retrieval a bare keyword query
    // gets — the parser supplies words, never results. See `crate::query_plan`.
    // Reading the job is the only step here that spends money, and this
    // endpoint is public by design. The budget decides whether THIS caller may
    // spend it; running out is not an error, it is a blunter reading — see
    // `crate::parse_budget`. It overrides `parse=true` rather than deferring to
    // it, because a caller that could opt back in has no budget at all.
    // Asked only when the answer can change something. Naming the caller costs
    // a round trip to Supabase, and on the cheap path — a keyword query
    // answered straight from the index — that round trip is most of the
    // request. A caller who sent `parse=false` has already settled the parse;
    // the only spend left is the widening below, and that only runs on an
    // explicit `enrich=true`, because its own default follows a plan that will
    // be `Verbatim`. With both settled there is nothing left to authorise.
    let settled_without_spending =
        params.parse_override() == Some(false) && params.enrich_override() != Some(true);
    let may_parse = if settled_without_spending {
        false
    } else {
        crate::parse_budget::allows_parse(&state, &headers).await
    };
    let parse_override = if may_parse {
        params.parse_override()
    } else {
        Some(false)
    };

    let (plan, mut infos, mut trace) = resolve_set_traced(
        &state,
        &query,
        params.capability.as_deref(),
        parse_override,
        params.supplied_plan(),
    )
    .await?;

    // An empty catalogue match is not the same fact as "nobody can do this",
    // and it used to be answered as though it were: a 404 that never searched
    // a second phrasing and never swept a single external index. A question
    // about a named company's analyst estimates is filed here under news and
    // finance, not under the company — and two dozen x402 endpoints outside
    // this catalogue serve it either way.
    //
    // So before that answer is given, the vocabulary a marketplace would file
    // the job under is asked for and searched (step 1b, `crate::enrich`), and
    // if the shelf still holds nothing the sweep answers instead. "Nobody has
    // reviewed these, here they are" beats "nothing performs that", because
    // only one of the two is true.
    let mut enrichment: Option<crate::enrich::Enrichment> = None;
    let mut widened_terms: Vec<String> = Vec::new();
    if params.capability.is_none()
        && may_parse
        && params
            .enrich_override()
            .unwrap_or(plan.source == PlanSource::Parsed)
    {
        let (extension, terms) = extend_job(&state, &query, &plan).await;
        widened_terms = terms;
        enrichment = Some(extension);
    }
    // The widened words search the shelf only when the caller's own words came
    // back with nothing. A first pass that already matched is not improved by
    // adding broader phrasings to it — it is diluted.
    if infos.is_empty() && !widened_terms.is_empty() {
        let (widened, pass) = search_widened(&state, &widened_terms).await;
        if let Some(passes) = trace.get_mut("search_passes").and_then(Value::as_array_mut) {
            passes.push(pass);
        }
        infos = widened;
    }

    let Some(info) = infos.first().cloned() else {
        return Ok(catalogue_miss(&state, &query, &plan, enrichment, trace, &params).await);
    };

    let requirements = params.requirements().with_plan_defaults(&plan);

    let limit = params
        .max_candidates
        .unwrap_or(DEFAULT_CANDIDATES)
        .clamp(1, MAX_CANDIDATES);
    let slugs: Vec<String> = infos.iter().map(|i| i.slug.clone()).collect();
    let offers = state
        .db
        .list_capability_offers_many(&slugs, &CapabilitySort::Price, requirements.max_price)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    // Logged, not swallowed. An empty stats map is indistinguishable from
    // "nothing is measured", so a failure here silently turns every candidate
    // unmeasured and the caller cannot tell.
    let stats = state.cache.peek_provider_stats().await.unwrap_or_default();

    let picked = interleave(&offers, &slugs, limit);
    let mut candidates: Vec<Value> = candidates_with_schemas(&state, &picked, &stats).await;

    // The rest of Apiosk's own shelf.
    //
    // The capability layer answers for the endpoints a human has mapped to one
    // of a couple of dozen tasks. Everything else Apiosk carries — the large
    // majority of it — was invisible to Apiosk's own search while the external
    // sweep read every index it reaches end to end. That is not a stricter
    // standard, it is a different search: one half matched on a curated task,
    // the other on words. This half now matches on words too, and the answer is
    // one list of things that can be bought here rather than two searches with
    // different reach pretending to be one.
    //
    // Mapped offers still lead: they carry a reviewed input contract and are
    // comparable through /v1/compare, which a word match is not.
    let taken: std::collections::HashSet<uuid::Uuid> =
        offers.iter().filter_map(|o| o.endpoint_id).collect();
    let terms = catalog_terms(&query, &plan, &widened_terms);
    candidates.extend(
        catalog_candidates(
            &state,
            &terms,
            requirements.max_price,
            limit.saturating_sub(candidates.len()),
            &taken,
            &stats,
        )
        .await,
    );

    // The world, not the shelf.
    //
    // An agent asking what can perform a task is asking about every payable
    // endpoint that exists, not about the subset Apiosk happens to have
    // reviewed. Answering only from the catalogue makes this a catalogue with a
    // search box; sweeping outward makes it a search engine for x402, which is
    // the only version of this endpoint worth an agent's default.
    //
    // Several phrasings go out at once and are merged on the resource URL,
    // because the words a capability is filed under here are rarely the words a
    // provider chose over there. `search_many` runs them concurrently under one
    // budget, so the caller waits for the slowest rather than the sum, and the
    // catalogue half of the answer is never held hostage to a third party being
    // slow — a failed source costs its own offers only, and `meta.sources`
    // says per index whether it answered, timed out or errored.
    let known_hosts: Vec<String> = offers
        .iter()
        .filter_map(|o| o.call_url.as_deref())
        .filter_map(|u| u.split("://").nth(1))
        .filter_map(|rest| rest.split('/').next())
        .map(|h| h.to_ascii_lowercase())
        .collect();

    let swept = if params.include_external {
        sweep_external_with_status(
            &state,
            &query,
            &plan,
            &info,
            enrichment.as_ref(),
            &known_hosts,
            params.external_limit(),
        )
        .await
    } else {
        crate::sources::Sweep::default()
    };
    let external = &swept.offers;

    // What each candidate would need before it can run: the lookup step, and
    // the total including it. Read by the confirm surface, which shows the
    // pre-step in grey above the price and puts the combined figure on the
    // button.
    annotate_pre_steps(&state, &mut candidates, &query).await;

    // The candidate set is the record. What an agent was able to consider is
    // the fact a vendor can never see about itself.
    let discovery_id = crate::decision_record::record_comparison(
        &state,
        &headers,
        &info.slug,
        "discover",
        requirements.max_price,
        &candidates,
        None,
    );

    let source_ids = crate::sources::source_ids(&state).await;
    let mut body = json!({
        "discovery_id": discovery_id,
        // The best-matching capability, kept at the top level because that is
        // where it has always been. When a request turned out to be more than
        // one need, the rest are in `capabilities` and every candidate names
        // its own.
        "capability": info.slug,
        "capability_name": info.name,
        "input_contract": info.input_contract,
        "capabilities": infos.iter().map(|c| json!({
            "capability": c.slug,
            "name": c.name,
            "input_contract": c.input_contract,
        })).collect::<Vec<_>>(),
        // How the request was read. Published rather than applied silently: a
        // misread question and an empty catalogue produce the same short
        // answer, and only this block tells them apart.
        "interpretation": plan.to_json(),
        "candidates": candidates,
        // Kept in their own block rather than mixed into `candidates`, because
        // they are a different kind of thing and merging them would quietly
        // launder "nobody has checked this" into "a candidate". They have no
        // candidate_id for the same reason: an id here is a promise that
        // /v1/compare can score the thing, and it cannot — there is no input
        // mapping or measurement for an unreviewed endpoint. Payment
        // eligibility is assessed separately. An agent that needs
        // the job done would rather hear "nobody reviewed this, here it is"
        // than "no results".
        "external_candidates": {
            "searched": params.include_external,
            "count": external.len(),
            "offers": crate::sources::settled_json(&state, external).await,
            "source": crate::sources::reach_sentence_for(&source_ids),
            "sources_swept": source_ids,
            // See `sources::coverage_json`: how deep into each index this
            // answer actually reached, and whether anything was left unread.
            "index_depth": crate::sources::coverage_json(&state).await,
            "why_separate": "Unreviewed: no trusted input mapping or measured latency or reliability. These search hits carry no candidate_id and cannot be passed to /v1/compare or /v1/decide. Each offer states its payment eligibility and indicative prices. A usable input contract and validated quote are required before purchase through Apiosk.",
            "turn_off": "include_external=false returns the reviewed catalogue alone.",
        },
        "meta": {
            "returned": candidates.len(),
            "available": offers.len(),
            "max_candidates": limit,
            "matched_capabilities": infos.len(),
            "applied_max_price_usd": requirements.max_price,
            "external_limit": params.external_limit(),
            // How every enabled index fared, one line each, so a thin answer
            // can be told apart from a thin reach. See `sources::SourceStatus`.
            "sources": swept.sources_json(),
            "sources_failed": swept.sources_failed(),
            "ranking": RANKING_NOTE,
            "next": "Pass candidate_id values to GET /v1/compare?candidates=id,id to turn this set into evidence.",
            "note": "A candidate set, not the catalogue. `indicative_price_usd` is what YOU pay through Apiosk — the provider's list price plus the 15% every Apiosk purchase carries — and `provider_price_usd` is the list price it was derived from. The live 402 for your exact request stays authoritative.",
            "candidates_note": "Every candidate is settled by Apiosk and runs through POST /v1/do by its candidate_id. A candidate with a `capability` was reviewed against that task and shares its input contract, so it can be passed to /v1/compare and /v1/decide. A candidate with `capability: null` is a catalogue endpoint matched on the words of the request: same settlement, same execution, but no shared contract to be scored against — it carries its own `input_schema` instead.",
            "scope": "Both halves of the answer: what Apiosk carries and settles in `candidates`, and everything else this search could reach in `external_candidates`. Comparison needs a capability, and the response never pretends otherwise: only the candidates that name one can be compared.",
        },
    });
    // Step 1b, published beside step 1a's `interpretation` for the same reason:
    // a caller is entitled to see how its request was read AND what vocabulary
    // that reading was extended into before anything was searched. Absent only
    // when the extension did not run — a named capability, or a keyword query
    // that never reached the parser.
    // Which needs the sweep answered, when the job parsed into several — the
    // line that stops a half-answered multi-need job reading as a full one.
    if !swept.need_coverage.is_empty() {
        body["meta"]["need_coverage"] = json!(swept.need_coverage_json());
    }
    if let Some(extension) = &enrichment {
        body["extension"] = extension.to_json();
    }
    if params.debug {
        body["debug"] = trace;
    }
    Ok(Json(body).into_response())
}

/// The words a catalogue endpoint search is run with.
///
/// Individual words, not phrasings: the sweep hands whole questions to indexes
/// that do their own retrieval, while this one matches term against text, so a
/// four-word phrase would match nothing a single word would not. The parser's
/// keywords and the widened vocabulary go in beside the caller's own words for
/// the same reason they go to the sweep — the words a provider chose are rarely
/// the words the question used.
fn catalog_terms(query: &str, plan: &QueryPlan, widened: &[String]) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut push = |text: &str| {
        for word in text.split(|c: char| !c.is_alphanumeric()) {
            let word = word.trim().to_ascii_lowercase();
            if word.len() >= 3 && seen.insert(word.clone()) {
                words.push(word);
            }
        }
    };
    push(query);
    for keyword in plan.keywords() {
        push(&keyword);
    }
    for term in widened {
        push(term);
    }
    words
}

/// One candidate, in the single shape both halves of the reviewed set use.
///
/// A capability offer and a catalogue endpoint are the same thing to everything
/// downstream — the same id, the same call target, the same fee rule, the same
/// `/v1/do` — so they are described the same way here. `capability` is null on
/// a catalogue hit, which is the difference and the whole of it: nobody has
/// mapped this endpoint to a task, so there is no shared input contract to
/// compare it against. `input_schema` is carried instead, so a caller can still
/// build the form the endpoint actually documents.
pub(crate) fn candidate_json(
    offer: &crate::db::CapabilityOffer,
    measured: bool,
    input_schema: Option<Value>,
) -> Value {
    // The price a buyer pays, not the price a provider asks.
    //
    // These two used to be the same field, and it was the wrong one:
    // `e.price` is the provider's list price, and every purchase through
    // Apiosk costs list + 15%. A comparison screen quoting the list price is
    // quoting a number nobody is ever charged, and it reads as cheaper than
    // the external half of the same response, where `buyer_price_usd` has
    // always carried the markup. Same rule on both halves now, from the one
    // fee module that also charges.
    //
    // The provider leg stays published beside it: an agent settling the
    // provider's own 402 needs it, and the ledger needs both legs to re-sum.
    // What changes is which one is the headline.
    let buyer_price_usd = buyer_price_usd(offer);
    let mut value = json!({
        "candidate_id": offer.endpoint_id,
        "provider": offer.provider_name.clone().unwrap_or_else(|| offer.api_slug.clone()),
        "api_slug": offer.api_slug,
        "capability": if offer.capability_slug.is_empty() {
            Value::Null
        } else {
            Value::String(offer.capability_slug.clone())
        },
        "description": offer.endpoint_description,
        "method": offer.endpoint_method,
        "path": offer.endpoint_path,
        "indicative_price_usd": buyer_price_usd,
        "provider_price_usd": offer.price_usd,
        "availability": if offer.call_url.is_some() { "callable" } else { "no-call-target" },
        "settlement": "apiosk-proxied",
        "measured": measured,
    });
    if let Some(schema) = input_schema {
        value["input_schema"] = schema;
    }
    value
}

/// The one buyer-total calculation used by discovery and canonical Ask.
pub(crate) fn buyer_price_usd(offer: &crate::db::CapabilityOffer) -> f64 {
    let fee = crate::fees::quote(
        crate::fees::usdc_to_atomic(
            &bigdecimal::BigDecimal::try_from(offer.price_usd).unwrap_or_default(),
        ),
        crate::fees::ProviderClass::from_listing_type(Some(if offer.hosted_externally {
            "federated"
        } else {
            "managed"
        })),
    );
    fee.buyer_total_usdc()
        .to_string()
        .parse()
        .unwrap_or(offer.price_usd)
}

/// Annotate candidates with the lookup step each would need before it can run.
///
/// A KVK profile call needs a `kvkNummer`; a job that only names a company
/// gets that number from a paid KVK search first. The confirm surface reads
/// its price from here, so the pre-step and the combined total have to travel
/// with the candidate — otherwise the button says 0.07 and the wallet says
/// 0.09.
///
/// Prediction, deliberately: it reads the job's shape, not a binding. Wrong
/// here costs a grey line, never a charge — `/v1/do` charges only what runs.
/// One query for the whole page; silent no-op when nothing is declared.
pub(crate) async fn annotate_pre_steps(state: &Arc<AppState>, candidates: &mut [Value], job: &str) {
    if candidates.is_empty() {
        return;
    }
    let endpoint_ids: Vec<uuid::Uuid> = candidates
        .iter()
        .filter_map(|c| c.get("candidate_id").and_then(Value::as_str))
        .filter_map(|id| uuid::Uuid::parse_str(id).ok())
        .collect();
    let capabilities: Vec<String> = candidates
        .iter()
        .filter_map(|c| c.get("capability").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    let rows = match state.db.pre_step_rows(&endpoint_ids, &capabilities).await {
        Ok(rows) if !rows.is_empty() => rows,
        Ok(_) => return,
        Err(err) => {
            // A candidate list without the line is still a correct candidate
            // list; a failed annotation must never fail discovery.
            tracing::warn!("pre-step annotation unavailable: {}", err);
            return;
        }
    };
    let no_inputs = Value::Object(serde_json::Map::new());
    for candidate in candidates.iter_mut() {
        let endpoint_id = candidate
            .get("candidate_id")
            .and_then(Value::as_str)
            .and_then(|id| uuid::Uuid::parse_str(id).ok());
        let capability = candidate
            .get("capability")
            .and_then(Value::as_str)
            .map(str::to_string);
        let pre_steps = crate::param_resolver::pre_steps_for(
            &rows,
            endpoint_id,
            capability.as_deref(),
            job,
            &no_inputs,
        );
        if pre_steps.is_empty() {
            continue;
        }
        let price = candidate
            .get("indicative_price_usd")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        candidate["combined_buyer_price_usd"] = json!(
            crate::param_resolver::combined_buyer_price_usd(price, &pre_steps)
        );
        candidate["pre_steps"] = Value::Array(pre_steps);
    }
}

/// The catalogue's own endpoints for this job, as candidates.
///
/// Run on every discovery, next to the capability match rather than instead of
/// it: a mapped offer carries a reviewed input contract and is the better
/// answer where one exists, and this is what makes the other 97% of the shelf
/// reachable at all. Deduplicated on endpoint id, so an endpoint that is both
/// mapped and matched appears once, under its capability.
async fn catalog_candidates(
    state: &Arc<AppState>,
    terms: &[String],
    max_price: Option<f64>,
    limit: usize,
    already: &std::collections::HashSet<uuid::Uuid>,
    stats: &std::collections::HashMap<String, crate::db::ProviderStats>,
) -> Vec<Value> {
    if limit == 0 || terms.is_empty() {
        return Vec::new();
    }
    // Over-fetch by the number already taken, so a page's worth survives the
    // dedupe rather than coming back short of it.
    let fetch = (limit + already.len()).min(200) as i64;
    let hits = match state
        .db
        .search_catalog_endpoints(terms, max_price, fetch)
        .await
    {
        Ok(hits) => hits,
        Err(err) => {
            // A failure here narrows the answer; it does not empty it. The
            // capability half and the sweep are unaffected.
            tracing::error!("catalogue endpoint search failed: {}", err);
            return Vec::new();
        }
    };

    let fresh: Vec<&crate::db::CapabilityOffer> = hits
        .iter()
        .filter(|o| o.endpoint_id.is_some_and(|id| !already.contains(&id)))
        .take(limit)
        .collect();
    if fresh.is_empty() {
        return Vec::new();
    }

    candidates_with_schemas(state, &fresh, stats).await
}

/// Offers → candidates, each carrying the schema its own endpoint publishes.
///
/// BOTH LANES, and that is the point of it being one function. The capability
/// lane used to pass no schema at all and leave the caller to describe the call
/// from the capability's `input_contract` instead. That works only while the
/// capability HAS one: the 141 rows migration `096_data_capabilities` generated
/// carry an empty contract, so a candidate matched through that taxonomy
/// arrived with nothing to build a form from — the KVK basisprofiel among them,
/// whose one required `kvkNummer` is the field the paid company lookup
/// (`param_resolvers`) exists to fill. A confirm card with no fields on it is
/// not a cheaper form, it is a call nobody was shown before it was paid for.
///
/// The capability contract is still the shared shape offers are COMPARED by and
/// is published beside this, under `/v1/capabilities`; the endpoint schema is
/// what one named endpoint actually takes. A caller wants both, and neither is
/// a substitute for the other.
///
/// One query for the whole page, not one per candidate.
async fn candidates_with_schemas(
    state: &Arc<AppState>,
    offers: &[&crate::db::CapabilityOffer],
    stats: &std::collections::HashMap<String, crate::db::ProviderStats>,
) -> Vec<Value> {
    if offers.is_empty() {
        return Vec::new();
    }
    let schemas = crate::request_binding::schemas_for(state, offers).await;
    offers
        .iter()
        .map(|o| {
            let schema = o
                .endpoint_id
                .and_then(|id| schemas.get(&id))
                .and_then(|s| s.query_params.clone().or_else(|| s.request_body.clone()));
            candidate_json(o, stats.get(&o.api_slug).is_some(), schema)
        })
        .collect()
}

/// Step 1b: extend the parsed job with the tags, categories and sectors a
/// catalogue files it under.
///
/// Runs on every parsed discovery, not only on a miss, because it is a step of
/// the answer and not a rescue: the caller is owed the reading AND the
/// vocabulary that reading was widened into, whether or not the shelf happened
/// to match the first time. The words also go to the external sweep, which is
/// where a shelf label like "finance" earns its keep.
///
/// Returns the extension for publication, and the subset of it worth searching
/// this catalogue with. Every failure degrades to "no extension": an empty one
/// searches nothing extra and the caller is no worse off than before it ran.
async fn extend_job(
    state: &Arc<AppState>,
    query: &str,
    plan: &QueryPlan,
) -> (crate::enrich::Enrichment, Vec<String>) {
    let known_categories = state.db.capability_categories().await.unwrap_or_default();
    let enrichment = crate::enrich::enrich(state, query, plan, &known_categories).await;
    // A term that IS one of the catalogue's own category labels names the aisle,
    // not the product. "finance" ranks a currency converter and an IBAN checker
    // above the analyst-estimate endpoint that the question was actually about,
    // because they are equally in finance and their text is shorter. The label
    // still goes to the external sweep, where the indexes are wide enough for a
    // shelf name to be a useful filter.
    let terms: Vec<String> = enrichment
        .catalogue_terms()
        .into_iter()
        .filter(|term| {
            !known_categories
                .iter()
                .any(|category| category.eq_ignore_ascii_case(term))
        })
        .collect();
    (enrichment, terms)
}

/// Search the shelf again on the extended vocabulary, after the caller's own
/// words came back empty.
async fn search_widened(state: &Arc<AppState>, terms: &[String]) -> (Vec<CapabilityInfo>, Value) {
    // Agreement across phrasings decides, not first past the post.
    //
    // The widened vocabulary is broad on purpose, and broad words match badly:
    // "revenue forecast" pulls in a weather forecast, "equity research" pulls
    // in a page-to-text reader. One term hitting a capability proves very
    // little; three different phrasings of the same need hitting it is the
    // signal. So every term is searched and the capabilities are ordered by how
    // many of them agreed, with the search's own score breaking ties.
    let mut agreement: Vec<(String, usize, f64)> = Vec::new();
    let mut per_term: Vec<Value> = Vec::new();
    for term in terms.iter().take(MAX_WIDENED_TERMS) {
        let started = std::time::Instant::now();
        let hits = match state
            .db
            .search_capabilities(term, CAPABILITIES_PER_TASK)
            .await
        {
            Ok(hits) => hits,
            Err(err) => {
                tracing::error!("widened capability search failed: {}", err);
                Vec::new()
            }
        };
        per_term.push(json!({
            "term": term,
            "ms": started.elapsed().as_millis() as u64,
            "matches": hits.iter().map(|h| json!({
                "slug": h.slug,
                "score": h.score,
                "matched_via": h.matched_via,
            })).collect::<Vec<_>>(),
        }));
        for hit in hits {
            // A widened term is a word the model wrote, not one the caller
            // did, so a bare-minimum match on it is worth less than a
            // bare-minimum match on the caller's own words. `MIN_RANK` lets
            // the best match for a term through even when that best match is
            // itself noise — which is how "consensus estimate" came back
            // holding an IP geolocation endpoint. Doubling the floor here
            // costs a weak true match and buys the absence of a confident
            // wrong one.
            if hit.score < WIDENED_MIN_RANK {
                continue;
            }
            match agreement.iter_mut().find(|(slug, _, _)| slug == &hit.slug) {
                Some(entry) => {
                    entry.1 += 1;
                    entry.2 = entry.2.max(hit.score);
                }
                None => agreement.push((hit.slug, 1, hit.score)),
            }
        }
    }
    agreement.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then(b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
    });
    let mut slugs: Vec<String> = agreement.into_iter().map(|(slug, _, _)| slug).collect();
    slugs.truncate(MAX_CAPABILITIES);

    let infos = if slugs.is_empty() {
        Vec::new()
    } else {
        match state.db.list_capabilities().await {
            Ok(all) => slugs
                .iter()
                .filter_map(|slug| all.iter().find(|c| &c.slug == slug).cloned())
                .collect(),
            Err(err) => {
                tracing::error!("capability load after widening failed: {}", err);
                Vec::new()
            }
        }
    };

    let pass = json!({
        "phase": "extended_after_empty",
        "terms": terms,
        "tasks": per_term,
    });
    (infos, pass)
}

/// The answer when the reviewed catalogue holds nothing for the job.
///
/// It is still an answer. The ecosystem is swept on the caller's own keywords
/// plus whatever the widening added, and whatever that finds is returned with
/// `candidates` empty — a 200, because "no reviewed candidate, twenty-five
/// unreviewed endpoints" is a result and not a missing page. Only when both
/// halves are empty does this stay a 404.
async fn catalogue_miss(
    state: &Arc<AppState>,
    query: &str,
    plan: &QueryPlan,
    enrichment: Option<crate::enrich::Enrichment>,
    trace: Value,
    params: &DiscoverQuery,
) -> Response {
    // No capability matched — which says nothing about whether Apiosk carries
    // an endpoint for this. Seventeen curated tasks cover a few dozen of the
    // catalogue's endpoints; the rest are found the way the sweep finds
    // things, by words. Searched here for exactly that reason: "no capability"
    // used to be published as "the reviewed catalogue is empty for this job"
    // when the shelf was not empty at all.
    let widened = enrichment
        .as_ref()
        .map(crate::enrich::Enrichment::search_terms)
        .unwrap_or_default();
    let stats = state.cache.peek_provider_stats().await.unwrap_or_default();
    let mut candidates = catalog_candidates(
        state,
        &catalog_terms(query, plan, &widened),
        params.requirements().max_price,
        params
            .max_candidates
            .unwrap_or(DEFAULT_CANDIDATES)
            .clamp(1, MAX_CANDIDATES),
        &std::collections::HashSet::new(),
        &stats,
    )
    .await;
    // Same annotation the main exit applies (see the call beside
    // `record_comparison` above). This exit runs whenever nothing cleared the
    // capability-match floor — the word lane, which is ~97% of the catalogue
    // — so skipping it here would mean a KVK-shaped candidate reached here
    // with no `pre_steps` and no `combined_buyer_price_usd`, and the confirm
    // button would show the primary's price while /v1/do went on to charge a
    // resolver on top of it.
    annotate_pre_steps(state, &mut candidates, query).await;

    let swept = if params.include_external {
        // Per-need vocabulary, rationed by the sweep itself: every parsed
        // need gets a queried slot before any gets a second, the widening
        // earns slots beside the parser's own words, and the raw sentence
        // trails — keyword indexes match a sentence worst. See `need_terms`.
        let needs = need_terms(query, plan, enrichment.as_ref(), &[]);
        crate::sources::sweep_needs_with_status(state, &needs, &[], params.external_limit()).await
    } else {
        crate::sources::Sweep::default()
    };
    let external = &swept.offers;

    if external.is_empty() && candidates.is_empty() {
        let mut body = json!({
            "error": "no_capability",
            "message": if params.include_external {
                "Nothing in the catalogue performs that, and the x402 indexes this search reaches returned nothing either. Browse GET /v1/capabilities."
            } else {
                "Nothing in the catalogue performs that. Browse GET /v1/capabilities, or repeat with include_external=true to sweep the wider ecosystem."
            },
            "query": query,
            "interpretation": plan.to_json(),
            // Even a 404 says which indexes were asked and which answered: an
            // empty answer from seven live indexes and an empty answer from
            // seven timeouts are different facts.
            "meta": {
                "sources": swept.sources_json(),
                "sources_failed": swept.sources_failed(),
            },
        });
        if !swept.need_coverage.is_empty() {
            body["meta"]["need_coverage"] = json!(swept.need_coverage_json());
        }
        if let Some(widening) = &enrichment {
            body["extension"] = widening.to_json();
        }
        if params.debug {
            body["debug"] = trace;
        }
        return (StatusCode::NOT_FOUND, Json(body)).into_response();
    }

    let source_ids = crate::sources::source_ids(state).await;
    let candidate_count = candidates.len();
    let mut body = json!({
        "capability": Value::Null,
        "capability_name": Value::Null,
        "capabilities": [],
        "candidates": candidates,
        "interpretation": plan.to_json(),
        "external_candidates": {
            "searched": true,
            "count": external.len(),
            "offers": crate::sources::settled_json(state, external).await,
            "source": crate::sources::reach_sentence_for(&source_ids),
            "sources_swept": source_ids,
            // See `sources::coverage_json`: how deep into each index this
            // answer actually reached, and whether anything was left unread.
            "index_depth": crate::sources::coverage_json(state).await,
            "why_separate": "Unreviewed: no trusted input mapping or measured latency or reliability. These search hits carry no candidate_id and cannot be passed to /v1/compare or /v1/decide. Each offer states its payment eligibility and indicative prices. A usable input contract and validated quote are required before purchase through Apiosk.",
            "turn_off": "include_external=false returns the reviewed catalogue alone.",
        },
        "meta": {
            "returned": candidate_count,
            "available": candidate_count,
            "matched_capabilities": 0,
            "external_limit": params.external_limit(),
            "sources": swept.sources_json(),
            "sources_failed": swept.sources_failed(),
            "ranking": RANKING_NOTE,
            "note": if candidate_count == 0 {
                "No reviewed capability matches this job and no catalogue endpoint matches its words, so there is nothing here to settle. What the sweep found is listed anyway: unreviewed endpoints that exist and may do the job."
            } else {
                "No reviewed CAPABILITY matches this job, so these candidates carry no shared input contract and cannot be scored against one another by /v1/compare. They are Apiosk-settled listings matched on the words of the request, each with its own `input_schema`, and every one of them is payable through POST /v1/do by its candidate_id."
            },
            "next": if candidate_count == 0 {
                "Read the external offers' own descriptions and parameters. To buy through Apiosk instead, ask the provider to list, or restate the job in terms of a reviewed capability from GET /v1/capabilities."
            } else {
                "POST /v1/do with a candidate_id from `candidates` to run one. Comparison needs a capability: browse GET /v1/capabilities."
            },
            "scope": "Both halves of the answer: what Apiosk carries and settles in `candidates`, and everything else this search could reach in `external_candidates`.",
        },
    });
    if !swept.need_coverage.is_empty() {
        body["meta"]["need_coverage"] = json!(swept.need_coverage_json());
    }
    if let Some(widening) = &enrichment {
        body["extension"] = widening.to_json();
    }
    if params.debug {
        body["debug"] = trace;
    }
    Json(body).into_response()
}

// ---------------------------------------------------------------------------
// 2. compare
// ---------------------------------------------------------------------------

/// Same spelling-out as [`DiscoverQuery`], and for the same reason.
#[derive(Debug, Deserialize)]
pub struct CompareQuery {
    /// Comma-separated candidate ids from `discover`.
    pub candidates: Option<String>,
    /// Or compare everything under one capability directly.
    pub capability: Option<String>,
    pub q: Option<String>,
    pub max_price: Option<f64>,
    pub max_latency_ms: Option<i64>,
    pub min_reliability: Option<f64>,
    #[serde(default, deserialize_with = "super::search::lenient_bool")]
    pub include_unreliable: bool,
    pub settlement: Option<String>,
    #[serde(default, deserialize_with = "super::search::lenient_bool")]
    pub require_all_inputs: bool,
    pub optimize_for: Option<String>,
    /// Whether to read `q` with the query parser. See [`DiscoverQuery::parse`].
    pub parse: Option<String>,
    /// `bind=1` on `/v1/decide`: fill the winning endpoint's own parameters
    /// from `q` and return a request that can be sent.
    ///
    /// Off by default and opt-in rather than automatic, because it is a second
    /// model call on top of the parse. A caller that already knows its
    /// parameters wants the decision, not the fill; a caller handing over a job
    /// description wants both, and asks.
    #[serde(default, deserialize_with = "super::search::lenient_bool")]
    pub bind: bool,
}

impl CompareQuery {
    fn requirements(&self) -> Requirements {
        Requirements {
            max_price: self.max_price,
            max_latency_ms: self.max_latency_ms,
            min_reliability: self.min_reliability,
            settlement: self.settlement.clone(),
            require_all_inputs: self.require_all_inputs,
            optimize_for: self.optimize_for.clone(),
            include_unreliable: self.include_unreliable,
        }
    }

    /// Same three states as on [`DiscoverQuery`].
    fn parse_override(&self) -> Option<bool> {
        let raw = self.parse.as_deref()?.trim().to_ascii_lowercase();
        match raw.as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        }
    }
}

/// `GET /v1/compare` — how do these perform against my requirements?
pub async fn compare(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<CompareQuery>,
) -> Result<Response, StatusCode> {
    let Gathered {
        offers,
        capability,
        contract,
        plan,
    } = match gather(&state, &headers, &params).await? {
        Some(v) => v,
        None => {
            return Ok(not_found(
                params.q.as_deref().unwrap_or_default(),
                Some(&QueryPlan::verbatim(params.q.as_deref().unwrap_or(""))),
            ))
        }
    };
    // Logged, not swallowed. An empty stats map is indistinguishable from
    // "nothing is measured", so a failure here silently turns every candidate
    // unmeasured and the caller cannot tell.
    let stats = state.cache.peek_provider_stats().await.unwrap_or_default();

    let refs: Vec<&CapabilityOffer> = offers.iter().collect();
    let field = Field::of(&refs, &stats);

    let mut ranked: Vec<Value> = Vec::new();
    let mut excluded: Vec<Value> = Vec::new();

    for o in &offers {
        let missing = super::capabilities::missing_inputs(&contract, &o.input_map);
        let stat = stats.get(&o.api_slug);
        let base = json!({
            "candidate_id": o.endpoint_id,
            "api_slug": o.api_slug,
            "provider": o.provider_name.clone().unwrap_or_else(|| o.api_slug.clone()),
            "capability": o.capability_slug,
            "method": o.endpoint_method,
            "path": o.endpoint_path,
            "price_usd": o.price_usd,
            "call_url": o.call_url,
            "settlement": "apiosk-proxied",
            "performance": super::search::performance_json(stat, o.hosted_externally),
            "inputs": o.input_map,
            "missing_inputs": missing,
            "notes": o.notes,
        });

        match excluded_by(o, stat, &missing, &params.requirements()) {
            Some((rule, because)) => {
                let mut e = base;
                e["excluded_by"] = json!(rule);
                e["because"] = json!(because);
                excluded.push(e);
            }
            None => {
                let (total, breakdown, _) =
                    score(o, stat, &missing, &field, &params.requirements());
                let mut r = base;
                r["score"] = json!(total);
                r["score_breakdown"] = breakdown;
                ranked.push(r);
            }
        }
    }

    let req = params.requirements();
    ranked.sort_by(|a, b| {
        let ga =
            ranks_ahead_on_optimised_dimension(&req, a["performance"]["measured"] == json!(true));
        let gb =
            ranks_ahead_on_optimised_dimension(&req, b["performance"]["measured"] == json!(true));
        ga.cmp(&gb)
            .then_with(|| {
                b["score"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .partial_cmp(&a["score"].as_f64().unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                a["api_slug"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(b["api_slug"].as_str().unwrap_or(""))
            })
    });

    let comparison_id = crate::decision_record::record_comparison(
        &state,
        &headers,
        &capability,
        "compare",
        params.requirements().max_price,
        &ranked,
        None,
    );

    let (wp, wl, wr, wi) = params.requirements().weights();
    Ok(Json(json!({
        "comparison_id": comparison_id,
        "capability": capability,
        "interpretation": plan.to_json(),
        "ranked": ranked,
        "excluded": excluded,
        "tradeoffs": tradeoffs(&ranked, &stats),
        "meta": {
            "considered": offers.len(),
            "ranked": ranked.len(),
            "excluded": excluded.len(),
            "optimize_for": params.requirements().optimize_label(),
            "weights": {"price": wp, "latency": wl, "reliability": wr, "inputs": wi},
            "scoring": "0-100, relative to the candidates in THIS comparison: the cheapest scores 1 on price, the dearest 0. Meaningful within the set, meaningless across sets. Every candidate carries its per-dimension contribution so the total can be recomputed.",
            "unmeasured": "Latency and reliability are dropped from the weighting for a candidate Apiosk has never proxied, rather than scored zero. Zero would rank an unmeasured provider below a measurably bad one, which says more about our coverage than about them.",
            "next": "GET /v1/decide with the same parameters returns one selection and the reasoning.",
        },
    }))
    .into_response())
}

/// The comparison in words: what the leader gives up, and to whom.
fn tradeoffs(
    ranked: &[Value],
    stats: &std::collections::HashMap<String, ProviderStats>,
) -> Vec<String> {
    let mut out = Vec::new();
    let Some(top) = ranked.first() else {
        return out;
    };
    let top_slug = top["api_slug"].as_str().unwrap_or("the leader");
    let top_price = top["price_usd"].as_f64().unwrap_or(0.0);

    if let Some(cheaper) = ranked
        .iter()
        .skip(1)
        .filter(|c| c["price_usd"].as_f64().unwrap_or(f64::MAX) < top_price)
        .min_by(|a, b| {
            a["price_usd"]
                .as_f64()
                .unwrap_or(0.0)
                .partial_cmp(&b["price_usd"].as_f64().unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    {
        out.push(format!(
            "{} is cheaper than {top_slug} (${:.6} against ${:.6}) but scores lower overall.",
            cheaper["api_slug"].as_str().unwrap_or("another"),
            cheaper["price_usd"].as_f64().unwrap_or(0.0),
            top_price
        ));
    }

    if let Some(faster) = ranked.iter().skip(1).find(|c| {
        let a = stats.get(c["api_slug"].as_str().unwrap_or(""));
        let b = stats.get(top_slug);
        matches!(
            (a.and_then(|x| x.p50_ms), b.and_then(|y| y.p50_ms)),
            (Some(x), Some(y)) if x < y
        )
    }) {
        out.push(format!(
            "{} answers faster than {top_slug} at the median.",
            faster["api_slug"].as_str().unwrap_or("another")
        ));
    }

    let unmeasured = ranked
        .iter()
        .filter(|c| c["performance"]["measured"] == json!(false))
        .count();
    if unmeasured > 0 {
        out.push(format!(
            "{unmeasured} of {} ranked candidates have no measured latency or reliability, so their score rests on price and input fit alone.",
            ranked.len()
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// 3. decide
// ---------------------------------------------------------------------------

/// `GET /v1/decide` — which one should I use?
pub async fn decide(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<CompareQuery>,
) -> Result<Response, StatusCode> {
    let Gathered {
        offers,
        capability,
        contract,
        plan,
    } = match gather(&state, &headers, &params).await? {
        Some(v) => v,
        None => {
            return Ok(not_found(
                params.q.as_deref().unwrap_or_default(),
                Some(&QueryPlan::verbatim(params.q.as_deref().unwrap_or(""))),
            ))
        }
    };
    // Logged, not swallowed. An empty stats map is indistinguishable from
    // "nothing is measured", so a failure here silently turns every candidate
    // unmeasured and the caller cannot tell.
    let stats = state.cache.peek_provider_stats().await.unwrap_or_default();
    let req = params.requirements();
    let EvaluatedField {
        ranked,
        rejected,
        considered,
        measured_count,
    } = evaluate_offers(&offers, &stats, &contract, &req);

    let comparison_id = crate::decision_record::record_comparison_now(
        &state,
        &headers,
        &capability,
        "decide",
        params.requirements().max_price,
        &considered,
        None,
    )
    .await;

    let Some((top_score, chosen, chosen_offer)) = ranked.first().cloned() else {
        return Ok(Json(json!({
            "comparison_id": comparison_id,
            "capability": capability,
            "selected": Value::Null,
            "reason": "Every candidate failed a hard constraint. Each rejection names the rule, so the binding constraint can be relaxed rather than guessed at.",
            "rejected": rejected,
        }))
        .into_response());
    };

    let reason = reason_for(
        &chosen,
        top_score,
        &ranked,
        &rejected,
        &params.requirements(),
    );
    let decision_id = record_decision(
        &state,
        &headers,
        &capability,
        comparison_id,
        chosen_offer,
        top_score,
        &reason,
        &rejected,
    )
    .await;

    let alternatives: Vec<Value> = ranked
        .iter()
        .skip(1)
        .take(4)
        .map(|(_, v, _)| v.clone())
        .collect();

    // Choosing the endpoint is half of "call the right API for this job". The
    // other half is the parameters, and until they are filled a decision is
    // still a pointer rather than something an agent can send. Opt-in: it is a
    // second model call, and a caller that already knows its arguments should
    // not pay for one. See `crate::request_binding`.
    let bound_request = if params.bind {
        let schemas = crate::request_binding::schemas_for(&state, &[chosen_offer]).await;
        let binding = crate::request_binding::bind(
            &state,
            params.q.as_deref().unwrap_or(""),
            chosen_offer,
            chosen_offer.endpoint_id.and_then(|id| schemas.get(&id)),
            // A GET carries no place to put pre-held values; `/v1/do` is the
            // surface that accepts them.
            &serde_json::Map::new(),
            crate::request_binding::Fill::FromJob,
        )
        .await;
        binding.to_json()
    } else {
        json!({
            "bound": false,
            "how": "Add bind=1 and describe the job in q= to have the winning endpoint's own parameters filled in from your words, returning a request you can send.",
        })
    };

    Ok(Json(json!({
        "decision_id": decision_id,
        "comparison_id": comparison_id,
        "capability": capability,
        "interpretation": plan.to_json(),
        "selected_api": chosen_offer.api_slug,
        "selected": chosen,
        "decision_score": top_score,
        "reason": reason,
        "bound_request": bound_request,
        // What the decision was ACTUALLY made on, as opposed to what it could
        // be made on in principle. A decision that fell back to price because
        // nothing in the field has been proxied enough to time is a weaker
        // claim than one weighed on observed success and p95, and the caller is
        // entitled to know which it got. Without this the two are
        // indistinguishable: both return a confident winner and a score.
        "decision_basis": {
            "measured_candidates": measured_count,
            "total_candidates": offers.len(),
            "dimensions_used": if measured_count > 0 {
                json!(["price", "latency_p95", "reliability", "input_fit"])
            } else {
                json!(["price", "input_fit"])
            },
            "note": if measured_count == 0 {
                "No candidate for this capability has enough proxied calls to report latency or reliability, so this decision rests on price and input compatibility alone. It is a real ranking of what is knowable, not a performance judgement. Latency and success rate appear here once Apiosk has served enough traffic for the provider to be measured."
            } else if measured_count < offers.len() {
                "Some candidates are measured and some are not. Unmeasured dimensions are dropped from those candidates' weighting rather than scored zero, so an unmeasured provider is not punished for Apiosk's blind spot — but neither has it been shown to perform."
            } else {
                "Every candidate in this decision has measured latency and success rate behind it."
            },
        },
        "rejected": rejected,
        "alternatives": alternatives,
        "execution": {
            "call_url": chosen_offer.call_url,
            "method": chosen_offer.endpoint_method,
            "inputs": chosen_offer.input_map,
            "fixed_params": chosen_offer.fixed_params,
            "route": if chosen_offer.hosted_externally { "direct" } else { "managed" },
            "how": if chosen_offer.hosted_externally {
                "Federated: call the provider's own resource and pay it directly with the accepts[] on its 402. Apiosk indexes this listing but does not proxy or settle it."
            } else {
                "Managed: call it through the gateway. A paid endpoint answers 402 with the payment requirements; supply the signed proof and the call forwards."
            },
        },
        "meta": {
            "considered": offers.len(),
            "optimize_for": params.requirements().optimize_label(),
            "outcome_url": decision_id.map(|id| format!("https://gateway.apiosk.com/v1/decisions/{id}/outcome")),
            "feedback": "Report the outcome. Observed price, latency and success feed the next comparison, and nothing else measures whether the choice was right.",
        },
    }))
    .into_response())
}

/// The selection in a sentence: the rule that won it, and what the runners-up
/// lost on.
/// Every candidate scored or rejected against `req`, winner-first.
///
/// Extracted from `decide` so `/v1/do` draws from the SAME evaluation — two
/// endpoints that each rank slightly differently would mean the executed choice
/// is not provably the choice the decision record describes.
pub(super) struct EvaluatedField<'a> {
    pub(super) ranked: Vec<(f64, Value, &'a CapabilityOffer)>,
    pub(super) rejected: Vec<Value>,
    /// Everything shown to the scorer, pre-filter — what the comparison record
    /// stores, so it describes the field rather than the survivors.
    pub(super) considered: Vec<Value>,
    pub(super) measured_count: usize,
}

pub(super) fn evaluate_offers<'a>(
    offers: &'a [CapabilityOffer],
    stats: &std::collections::HashMap<String, ProviderStats>,
    contract: &Value,
    req: &Requirements,
) -> EvaluatedField<'a> {
    let refs: Vec<&CapabilityOffer> = offers.iter().collect();
    let field = Field::of(&refs, stats);
    let measured_count = offers
        .iter()
        .filter(|o| stats.contains_key(&o.api_slug))
        .count();

    let mut ranked: Vec<(f64, Value, &CapabilityOffer)> = Vec::new();
    let mut rejected: Vec<Value> = Vec::new();
    let mut considered: Vec<Value> = Vec::new();

    for o in offers {
        let missing = super::capabilities::missing_inputs(contract, &o.input_map);
        let stat = stats.get(&o.api_slug);
        let base = json!({
            "candidate_id": o.endpoint_id,
            "api_slug": o.api_slug,
            "provider": o.provider_name.clone().unwrap_or_else(|| o.api_slug.clone()),
            "method": o.endpoint_method,
            "path": o.endpoint_path,
            "price_usd": o.price_usd,
            "call_url": o.call_url,
            "settlement": "apiosk-proxied",
            "performance": super::search::performance_json(stat, o.hosted_externally),
            "missing_inputs": missing,
        });
        considered.push(base.clone());

        match excluded_by(o, stat, &missing, req) {
            Some((rule, because)) => {
                let mut r = base;
                r["rejected_by"] = json!(rule);
                r["because"] = json!(because);
                rejected.push(r);
            }
            None => {
                let (total, breakdown, _) = score(o, stat, &missing, &field, req);
                let mut r = base;
                r["score"] = json!(total);
                r["score_breakdown"] = breakdown;
                ranked.push((total, r, o));
            }
        }
    }

    ranked.sort_by(|a, b| {
        let ga =
            ranks_ahead_on_optimised_dimension(req, a.1["performance"]["measured"] == json!(true));
        let gb =
            ranks_ahead_on_optimised_dimension(req, b.1["performance"]["measured"] == json!(true));
        ga.cmp(&gb)
            .then_with(|| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| a.2.api_slug.cmp(&b.2.api_slug))
    });

    EvaluatedField {
        ranked,
        rejected,
        considered,
        measured_count,
    }
}

pub(super) fn reason_for(
    chosen: &Value,
    score: f64,
    ranked: &[(f64, Value, &CapabilityOffer)],
    rejected: &[Value],
    req: &Requirements,
) -> String {
    let slug = chosen["api_slug"].as_str().unwrap_or("it");
    let mut parts = vec![format!(
        "{slug} scores {score:.0} of 100 optimising for {}",
        req.optimize_label()
    )];

    if let Some((_, runner, _)) = ranked.get(1) {
        let name = runner["api_slug"].as_str().unwrap_or("the runner-up");
        let rp = runner["price_usd"].as_f64().unwrap_or(0.0);
        let cp = chosen["price_usd"].as_f64().unwrap_or(0.0);
        if rp < cp {
            parts.push(format!(
                "{name} is cheaper but scores lower once the other dimensions count"
            ));
        } else {
            parts.push(format!("{name} is the closest alternative"));
        }
    }
    if let Some(first) = rejected.first() {
        parts.push(format!(
            "{} was excluded because it {}",
            first["api_slug"].as_str().unwrap_or("one candidate"),
            first["because"].as_str().unwrap_or("failed a constraint")
        ));
    }
    format!("{}.", parts.join("; "))
}

// ---------------------------------------------------------------------------
// Shared plumbing
// ---------------------------------------------------------------------------

/// The wider x402 ecosystem, swept for one capability.
///
/// Lives here rather than inside `discover` because two endpoints owe a caller
/// the same answer: discovery says what could do the job, and `/v1/quote` puts
/// prices in front of a person. A comparison that only ever lists the reviewed
/// shelf reads as "these five are all there is", which is the one claim this
/// product must never make — the catalogue is a subset of the ecosystem, and
/// the user is entitled to see the rest beside it, clearly marked as
/// unreviewed. One implementation, so the two screens cannot disagree about
/// what the ecosystem holds.
///
/// Only the first few phrasings reach each source, so the order below is the
/// whole decision.
///
/// What the agent typed leads, because it is the only phrasing in the set that
/// neither Apiosk nor a model chose — except when it was parsed, which is
/// precisely the case where it is a sentence. These are keyword indexes: a
/// sentence matches them badly and a non-English one matches nothing, while the
/// variants built from our own slug and description search for the words we
/// filed a capability under rather than the words its domain uses.
/// The extension's own words come second, ahead of the slug-derived variants:
/// they were written for how a marketplace files this job, which is closer to
/// how a third-party index describes it than Apiosk's internal slug is.
pub(super) async fn sweep_external(
    state: &Arc<AppState>,
    query: &str,
    plan: &QueryPlan,
    info: &CapabilityInfo,
    enrichment: Option<&crate::enrich::Enrichment>,
    known_hosts: &[String],
) -> Vec<crate::bazaar::ExternalOffer> {
    sweep_external_with_status(
        state,
        query,
        plan,
        info,
        enrichment,
        known_hosts,
        crate::sources::MAX_RETURNED,
    )
    .await
    .offers
}

/// [`sweep_external`] with the per-source status kept, for the responses
/// that publish it (`/v1/discover`'s `meta.sources`).
#[allow(clippy::too_many_arguments)]
pub(super) async fn sweep_external_with_status(
    state: &Arc<AppState>,
    query: &str,
    plan: &QueryPlan,
    info: &CapabilityInfo,
    enrichment: Option<&crate::enrich::Enrichment>,
    known_hosts: &[String],
    limit: usize,
) -> crate::sources::Sweep {
    // The capability's own phrasings ride in the tail: useful widening, but
    // never at the expense of a slot for a need the caller actually stated.
    let capability_phrasings = crate::bazaar::query_variants(
        &info.name,
        &info.description,
        info.category.as_deref(),
        &info.slug,
    );
    let needs = need_terms(query, plan, enrichment, &capability_phrasings);
    crate::sources::sweep_needs_with_status(state, &needs, known_hosts, limit).await
}

/// The parsed job as per-need sweep vocabulary — the shape that lets the
/// sweep ration its HTTP calls per need instead of per job (see
/// [`crate::sources::sweep_needs_with_status`]).
///
/// Per need, the queried lane interleaves the parser's phrasings with the
/// widening's (tags, then extra phrasings): the widening was bought precisely
/// because the caller's own words may be the problem, so it earns slots
/// beside them, not behind them. The indexed lane additionally carries what
/// never merits an HTTP call of its own — the need clause, categories,
/// sectors — because matching a cached catalogue document costs nothing.
///
/// Enriched needs are matched to parsed needs by their text, falling back to
/// position: the enricher is told to copy needs verbatim, and when a model
/// paraphrases anyway, order is the only alignment left.
///
/// `query` and `extra` become the unnamed tail vocabulary: searched
/// everywhere, but never at a named need's expense — a raw sentence matches
/// keyword indexes worst.
pub(super) fn need_terms(
    query: &str,
    plan: &QueryPlan,
    enrichment: Option<&crate::enrich::Enrichment>,
    extra: &[String],
) -> Vec<crate::sources::NeedTerms> {
    let mut out = Vec::new();
    for (position, task) in plan.tasks.iter().enumerate() {
        let widening = enrichment.and_then(|e| {
            e.needs
                .iter()
                .find(|n| n.need.trim().eq_ignore_ascii_case(task.need.trim()))
                .or_else(|| e.needs.get(position))
        });
        let labels: Vec<&String> = widening
            .map(|n| n.tags.iter().chain(&n.extra_terms).collect())
            .unwrap_or_default();
        let mut queried = Vec::new();
        for i in 0..task.keywords.len().max(labels.len()) {
            if let Some(keyword) = task.keywords.get(i) {
                queried.push(keyword.clone());
            }
            if let Some(label) = labels.get(i) {
                queried.push((*label).clone());
            }
        }
        let mut index = vec![task.need.clone()];
        if let Some(widening) = widening {
            index.extend(widening.categories.iter().cloned());
            index.extend(widening.sectors.iter().cloned());
        }
        out.push(crate::sources::NeedTerms {
            need: task.need.clone(),
            queried,
            index,
        });
    }
    let mut tail: Vec<String> = Vec::new();
    if !query.trim().is_empty() {
        tail.push(query.trim().to_string());
    }
    tail.extend(extra.iter().cloned());
    if !tail.is_empty() {
        out.push(crate::sources::NeedTerms {
            need: String::new(),
            queried: tail,
            index: Vec::new(),
        });
    }
    out
}

/// Free text — or an explicit slug — to the capabilities that could serve it.
///
/// Returns the reading alongside the result, because the two are only useful
/// together: candidates without the words they were found under leave a caller
/// unable to tell a bad match from an empty catalogue.
pub(super) async fn resolve_set(
    state: &Arc<AppState>,
    query: &str,
    capability: Option<&str>,
    parse_override: Option<bool>,
) -> Result<(QueryPlan, Vec<CapabilityInfo>), StatusCode> {
    let (plan, infos, _) =
        resolve_set_traced(state, query, capability, parse_override, None).await?;
    Ok((plan, infos))
}

/// [`resolve_set`], but also returning a trace of every substep — the parse
/// gate's decision, each search pass with per-capability scores, and the
/// thresholds applied. `/v1/discover?debug=1` publishes it; every other caller
/// takes the two-tuple wrapper above and pays nothing extra, since the trace is
/// assembled from values the resolution already produced.
pub(super) async fn resolve_set_traced(
    state: &Arc<AppState>,
    query: &str,
    capability: Option<&str>,
    parse_override: Option<bool>,
    supplied: Option<QueryPlan>,
) -> Result<(QueryPlan, Vec<CapabilityInfo>, Value), StatusCode> {
    // A named capability is not a question, so there is nothing to interpret
    // and no reason to spend a model call interpreting it.
    if let Some(slug) = capability.map(str::trim).filter(|s| !s.is_empty()) {
        let info = state
            .db
            .get_capability(slug)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let trace = json!({
            "gate": { "explicit_capability": slug },
            "search_passes": [],
        });
        return Ok((QueryPlan::explicit(), info.into_iter().collect(), trace));
    }
    // Terms the caller worked out themselves. No gate, no model, no retry: a
    // caller who sent vocabulary asked for it to be searched, not second-
    // guessed, and the empty-result retry below exists to rescue a reading this
    // request did not do. An empty answer here is the honest one.
    if let Some(plan) = supplied {
        let (matches, pass) = search_plan(state, &plan).await?;
        let trace = json!({
            "gate": { "supplied_terms": true },
            "search_passes": [json!({
                "phase": "supplied",
                "plan_source": plan.source.as_str(),
                "tasks": pass,
            })],
        });
        return Ok((plan, capability_infos(state, &matches).await?, trace));
    }

    if query.is_empty() {
        let trace = json!({ "gate": { "empty_query": true }, "search_passes": [] });
        return Ok((QueryPlan::verbatim(query), Vec::new(), trace));
    }

    // Parse up front only when the query reads as a sentence. A keyword query
    // goes to full-text search first and reaches the parser only if that finds
    // nothing — which is what keeps the crawler majority of this endpoint's
    // traffic free of model calls entirely.
    let gate = crate::query_plan::should_parse(query);
    let parse_first = parse_override.unwrap_or(gate);

    let mut plan = if parse_first {
        crate::query_plan::plan(state, query).await
    } else {
        QueryPlan::verbatim(query)
    };
    let (mut matches, first_pass) = search_plan(state, &plan).await?;
    let mut passes = vec![json!({
        "phase": if parse_first { "parsed" } else { "literal" },
        "plan_source": plan.source.as_str(),
        "tasks": first_pass,
    })];

    if matches.is_empty() && !parse_first && parse_override != Some(false) {
        // The literal reading found nothing. "Nothing here does that" and "we
        // matched on the wrong words" are the same empty from the outside, so
        // rule the second out before answering.
        let parsed = crate::query_plan::plan(state, query).await;
        if parsed.source == PlanSource::Parsed {
            let (retry_matches, retry_pass) = search_plan(state, &parsed).await?;
            passes.push(json!({
                "phase": "parsed_retry_after_empty",
                "plan_source": parsed.source.as_str(),
                "tasks": retry_pass,
            }));
            matches = retry_matches;
            plan = parsed;
        }
    }

    let trace = json!({
        "gate": {
            "should_parse": gate,
            "parse_override": parse_override,
            "parse_first": parse_first,
        },
        "search_passes": passes,
        "thresholds": {
            "min_rank": crate::db::MIN_RANK,
            "relative_floor": crate::db::RELATIVE_FLOOR,
            "capabilities_per_task": CAPABILITIES_PER_TASK,
            "max_capabilities": MAX_CAPABILITIES,
        },
    });

    Ok((plan, capability_infos(state, &matches).await?, trace))
}

/// Matched slugs to the capabilities themselves, in match order.
///
/// One catalogue read for the whole set: `get_capability` loads every
/// capability in order to find one, so calling it per match would be a full
/// scan per matched need.
async fn capability_infos(
    state: &Arc<AppState>,
    matches: &[String],
) -> Result<Vec<CapabilityInfo>, StatusCode> {
    if matches.is_empty() {
        return Ok(Vec::new());
    }
    let all = state
        .db
        .list_capabilities()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(matches
        .iter()
        .filter_map(|slug| all.iter().find(|c| &c.slug == slug).cloned())
        .collect())
}

/// Every capability any of the plan's needs can reach, best match first —
/// paired with a per-task trace of what was searched and what it scored.
///
/// Needs are searched in order and their hits concatenated, so the first need
/// keeps the top of the list. Ranking within a need is `search_capabilities`'
/// own; nothing here re-scores anything.
async fn search_plan(
    state: &Arc<AppState>,
    plan: &QueryPlan,
) -> Result<(Vec<String>, Vec<Value>), StatusCode> {
    let mut out: Vec<String> = Vec::new();
    let mut trace: Vec<Value> = Vec::new();
    for task in &plan.tasks {
        let search_text = task.search_text();
        let started = std::time::Instant::now();
        let hits = state
            .db
            .search_capabilities(&search_text, CAPABILITIES_PER_TASK)
            .await
            .map_err(|err| {
                tracing::error!("capability search failed: {}", err);
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        trace.push(json!({
            "need": task.need,
            "search_text": search_text,
            "ms": started.elapsed().as_millis() as u64,
            "matches": hits.iter().map(|h| json!({
                "slug": h.slug,
                "score": h.score,
                "matched_via": h.matched_via,
                "offer_count": h.offer_count,
            })).collect::<Vec<_>>(),
        }));
        for hit in hits {
            if !out.contains(&hit.slug) {
                out.push(hit.slug);
            }
        }
        if out.len() >= MAX_CAPABILITIES {
            break;
        }
    }
    out.truncate(MAX_CAPABILITIES);
    Ok((out, trace))
}

/// Takes `limit` offers, round-robin across capabilities in match order.
///
/// Not a global price sort, which is what a plain truncation would give. With
/// two needs matched, the cheaper capability's offers would fill the whole set
/// and the second need would silently go unanswered — an answer that looks
/// complete and is half missing. Round-robin gives every matched need a
/// candidate before any need gets a second one, and within a capability the
/// order the query returned (cheapest first) is preserved.
fn interleave<'a>(
    offers: &'a [CapabilityOffer],
    slugs: &[String],
    limit: usize,
) -> Vec<&'a CapabilityOffer> {
    if slugs.len() <= 1 {
        return offers.iter().take(limit).collect();
    }

    let groups: Vec<Vec<&CapabilityOffer>> = slugs
        .iter()
        .map(|slug| {
            offers
                .iter()
                .filter(|o| &o.capability_slug == slug)
                .collect()
        })
        .collect();

    let mut out = Vec::with_capacity(limit.min(offers.len()));
    let deepest = groups.iter().map(Vec::len).max().unwrap_or(0);
    for rank in 0..deepest {
        for group in &groups {
            if let Some(offer) = group.get(rank) {
                out.push(*offer);
                if out.len() == limit {
                    return out;
                }
            }
        }
    }
    out
}

/// The field a comparison or decision is drawn from, plus how it was arrived at.
struct Gathered {
    offers: Vec<CapabilityOffer>,
    capability: String,
    contract: Value,
    /// How `q` was read. `Explicit` when the caller named ids or a slug and
    /// there was nothing to interpret.
    plan: QueryPlan,
}

async fn gather(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    params: &CompareQuery,
) -> Result<Option<Gathered>, StatusCode> {
    if let Some(raw) = params
        .candidates
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let ids: Vec<Uuid> = raw
            .split(',')
            .filter_map(|s| Uuid::parse_str(s.trim()).ok())
            .collect();
        let offers = state
            .db
            .capability_offers_by_endpoint(&ids)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let Some(first) = offers.first() else {
            return Ok(None);
        };
        let capability = first.capability_slug.clone();
        let contract = state
            .db
            .get_capability(&capability)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .map(|i| i.input_contract)
            .unwrap_or(json!({}));
        return Ok(Some(Gathered {
            offers,
            capability,
            contract,
            plan: QueryPlan::explicit(),
        }));
    }

    // The same reading `/v1/discover` does, rather than a second, blunter one.
    // A caller that skips discover and asks `/v1/decide?q=<job>` outright was
    // matching its whole sentence literally — which for a job description is
    // the case full-text search is worst at, and the case a decision endpoint
    // most needs to get right. Only the best-matching capability is taken:
    // decide returns one winner, so a second capability would be candidates for
    // a different need scored against each other.
    let query = params.q.as_deref().unwrap_or("").trim().to_string();
    // Same till, same budget. `/v1/compare` and `/v1/decide` accept a free-text
    // `q` and read it the way `/v1/discover` does, so a caller throttled on one
    // could otherwise walk to the next door and keep spending. The budget
    // overrides the caller's own `parse` flag rather than deferring to it.
    let parse_override = match params.parse_override() {
        // Settled by the caller; no identity check can change it. See the same
        // short-circuit on `/v1/discover`.
        Some(false) => Some(false),
        given => {
            if crate::parse_budget::allows_parse(state, headers).await {
                given
            } else {
                Some(false)
            }
        }
    };
    let (plan, infos) =
        resolve_set(state, &query, params.capability.as_deref(), parse_override).await?;
    let Some(info) = infos.into_iter().next() else {
        return Ok(None);
    };
    let offers = state
        .db
        .list_capability_offers(&info.slug, &CapabilitySort::Price, None)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Some(Gathered {
        capability: info.slug,
        contract: info.input_contract,
        offers,
        plan,
    }))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn record_decision(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    capability: &str,
    comparison_id: Option<Uuid>,
    chosen: &CapabilityOffer,
    score: f64,
    reason: &str,
    rejected: &[Value],
) -> Option<Uuid> {
    let row = DecisionInsert {
        comparison_id,
        capability_slug: capability.to_string(),
        chosen_api_slug: chosen.api_slug.clone(),
        chosen_method: Some(chosen.endpoint_method.clone()),
        chosen_path: Some(chosen.endpoint_path.clone()),
        chosen_price_usd: Some(chosen.price_usd),
        reason: Some(format!("{reason} (score {score:.0})")),
        policy: Some("apiosk-decide".to_string()),
        rejected: Value::Array(
            rejected
                .iter()
                .map(|r| json!({"api_slug": r["api_slug"], "path": r["path"], "reason": r["because"]}))
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
            tracing::warn!("decision not recorded: {}", err);
            None
        }
    }
}

/// The honest empty.
///
/// From `discover` it carries the reading that produced it, which is the
/// difference between an answer and a dead end: a caller who can see the words
/// that were searched can tell "the catalogue does not do this" from "you read
/// my request wrong", and fix the second themselves. `compare` and `decide`
/// match `q` literally and pass `None` rather than describe a parse that never
/// ran.
fn not_found(query: &str, plan: Option<&QueryPlan>) -> Response {
    let mut body = json!({
        "error": "no_capability",
        "message": "Nothing in the catalogue performs that. Browse GET /v1/capabilities.",
        "query": query,
    });
    if let Some(plan) = plan {
        body["interpretation"] = plan.to_json();
    }
    (StatusCode::NOT_FOUND, Json(body)).into_response()
}

// `not_found_with_debug` lived here. Discovery's empty answer is now built by
// `catalogue_miss`, which carries the same trace under `debug` and, when the
// sweep found anything, is not an empty answer at all.

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn semicolons_separate_needs_and_commas_separate_phrases() {
        assert_eq!(
            term_groups("spot price,ohlcv;crypto news,market news"),
            vec![
                vec!["spot price".to_string(), "ohlcv".to_string()],
                vec!["crypto news".to_string(), "market news".to_string()],
            ]
        );
        // A caller with no need structure to preserve sends a flat list, and
        // it is one need rather than an error.
        assert_eq!(
            term_groups("spot price, ohlcv"),
            vec![vec!["spot price".to_string(), "ohlcv".to_string()]]
        );
        // Separators without content are not needs.
        assert!(term_groups(" ; , ; ").is_empty());
    }

    fn plan_of(tasks: &[(&str, &[&str])]) -> QueryPlan {
        let mut plan = QueryPlan::verbatim("ignored");
        plan.tasks = tasks
            .iter()
            .map(|(need, keywords)| crate::query_plan::Task {
                need: need.to_string(),
                keywords: keywords.iter().map(|k| k.to_string()).collect(),
            })
            .collect();
        plan
    }

    #[test]
    fn need_terms_route_the_parse_and_the_widening_by_lane() {
        let plan = plan_of(&[(
            "get news articles about the Netherlands",
            &["Netherlands news", "Dutch news"],
        )]);
        let enrichment = crate::enrich::Enrichment {
            needs: vec![crate::enrich::EnrichedNeed {
                // Case differs from the parsed need: still matched by text.
                need: "GET NEWS ARTICLES ABOUT THE NETHERLANDS".to_string(),
                tags: vec!["dutch news".to_string(), "headlines".to_string()],
                categories: vec!["news".to_string()],
                sectors: vec!["media".to_string(), "journalism".to_string()],
                extra_terms: vec!["current events Netherlands".to_string()],
            }],
            model: None,
            enrich_ms: None,
            cached: None,
            error: None,
        };
        let needs = need_terms(
            "haal nieuws over Nederland op",
            &plan,
            Some(&enrichment),
            &["news search".to_string()],
        );

        assert_eq!(needs.len(), 2, "one named need plus the tail");
        assert_eq!(needs[0].need, "get news articles about the Netherlands");
        // The parser's phrasings interleave with the widening's.
        assert_eq!(
            needs[0].queried,
            [
                "Netherlands news",
                "dutch news",
                "Dutch news",
                "headlines",
                "current events Netherlands",
            ]
        );
        // The need clause, the shelf labels and the sectors ride the free lane.
        assert_eq!(
            needs[0].index,
            [
                "get news articles about the Netherlands",
                "news",
                "media",
                "journalism",
            ]
        );
        // The tail: the raw sentence first, capability phrasings after,
        // attached to no need.
        assert_eq!(needs[1].need, "");
        assert_eq!(
            needs[1].queried,
            ["haal nieuws over Nederland op", "news search"]
        );
    }

    #[test]
    fn enriched_needs_fall_back_to_position_when_the_model_paraphrased() {
        let plan = plan_of(&[
            ("btc price", &["bitcoin price"]),
            ("btc news", &["bitcoin news"]),
        ]);
        let enrichment = crate::enrich::Enrichment {
            needs: vec![
                crate::enrich::EnrichedNeed {
                    need: "the current bitcoin price".to_string(),
                    tags: vec!["spot price".to_string()],
                    categories: vec![],
                    sectors: vec![],
                    extra_terms: vec![],
                },
                crate::enrich::EnrichedNeed {
                    need: "news coverage of bitcoin".to_string(),
                    tags: vec!["crypto headlines".to_string()],
                    categories: vec![],
                    sectors: vec![],
                    extra_terms: vec![],
                },
            ],
            model: None,
            enrich_ms: None,
            cached: None,
            error: None,
        };
        let needs = need_terms("", &plan, Some(&enrichment), &[]);
        assert_eq!(needs.len(), 2, "no tail without a query or extra phrasings");
        assert_eq!(needs[0].queried, ["bitcoin price", "spot price"]);
        assert_eq!(needs[1].queried, ["bitcoin news", "crypto headlines"]);
    }

    fn offer(slug: &str, price: f64, federated: bool) -> CapabilityOffer {
        CapabilityOffer {
            capability_slug: "c".into(),
            endpoint_id: Some(Uuid::new_v4()),
            api_slug: slug.into(),
            api_name: slug.into(),
            endpoint_method: "GET".into(),
            endpoint_path: "/x".into(),
            endpoint_description: None,
            price_usd: price,
            payment_required: true,
            hosted_externally: federated,
            call_url: Some("https://x.dev/x".into()),
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

    fn stat(slug: &str, p50: i64, rate: f64) -> ProviderStats {
        ProviderStats {
            api_slug: slug.into(),
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
    fn reliability_is_read_in_either_unit() {
        let mut r = Requirements {
            min_reliability: Some(99.0),
            ..Default::default()
        };
        assert_eq!(r.reliability_floor(), Some(0.99));
        r.min_reliability = Some(0.99);
        assert_eq!(r.reliability_floor(), Some(0.99));
    }

    /// An offer belonging to a named capability, for the interleave tests.
    fn offer_for(capability: &str, slug: &str, price: f64) -> CapabilityOffer {
        CapabilityOffer {
            capability_slug: capability.into(),
            ..offer(slug, price, false)
        }
    }

    #[test]
    fn every_matched_need_gets_a_candidate_before_any_need_gets_two() {
        // The failure this exists to prevent: "bitcoin price and news about it"
        // matches two capabilities, the price one is cheaper across the board,
        // and a plain price-sorted truncation returns four price offers and
        // zero news ones — an answer that looks complete and silently drops
        // half the request.
        let offers = vec![
            offer_for("crypto.spot-price", "cheap-a", 0.001),
            offer_for("crypto.spot-price", "cheap-b", 0.002),
            offer_for("crypto.spot-price", "cheap-c", 0.003),
            offer_for("news.search", "news-a", 0.05),
            offer_for("news.search", "news-b", 0.06),
        ];
        let slugs = vec!["crypto.spot-price".to_string(), "news.search".to_string()];

        let picked = interleave(&offers, &slugs, 4);
        let by_capability: Vec<&str> = picked.iter().map(|o| o.capability_slug.as_str()).collect();
        assert_eq!(
            by_capability,
            vec![
                "crypto.spot-price",
                "news.search",
                "crypto.spot-price",
                "news.search"
            ]
        );
        // Within a capability the query's own order (cheapest first) survives.
        assert_eq!(picked[0].api_slug, "cheap-a");
        assert_eq!(picked[2].api_slug, "cheap-b");
    }

    #[test]
    fn a_capability_that_runs_out_does_not_hold_a_slot() {
        let offers = vec![
            offer_for("a", "a1", 0.01),
            offer_for("b", "b1", 0.02),
            offer_for("b", "b2", 0.03),
        ];
        let slugs = vec!["a".to_string(), "b".to_string()];
        let picked = interleave(&offers, &slugs, 5);
        assert_eq!(picked.len(), 3, "no slot is wasted on an exhausted group");
        assert_eq!(picked[2].api_slug, "b2");
    }

    #[test]
    fn one_capability_keeps_the_plain_price_order() {
        // The single-need case is the common one and must be untouched by any
        // of this: cheapest first, straight truncation.
        let offers = vec![
            offer_for("a", "cheapest", 0.001),
            offer_for("a", "middle", 0.01),
            offer_for("a", "dearest", 0.1),
        ];
        let picked = interleave(&offers, &["a".to_string()], 2);
        assert_eq!(picked.len(), 2);
        assert_eq!(picked[0].api_slug, "cheapest");
        assert_eq!(picked[1].api_slug, "middle");
    }

    #[test]
    fn the_parse_flag_is_three_valued() {
        // Absent must not read as "off": the default is to decide from the
        // query, and a bool parameter cannot express that.
        let probe = |value: Option<&str>| {
            DiscoverQuery {
                include_unreliable: false,
                q: None,
                capability: None,
                max_candidates: None,
                max_price: None,
                max_latency_ms: None,
                min_reliability: None,
                settlement: None,
                limit: None,
                require_all_inputs: false,
                optimize_for: None,
                include_external: true,
                parse: value.map(str::to_string),
                enrich: None,
                terms: None,
                debug: false,
            }
            .parse_override()
        };
        assert_eq!(probe(None), None);
        for on in ["1", "true", "yes", "on", "TRUE"] {
            assert_eq!(probe(Some(on)), Some(true), "{on}");
        }
        for off in ["0", "false", "no", "off"] {
            assert_eq!(probe(Some(off)), Some(false), "{off}");
        }
        // Nonsense is not a refusal — fall back to deciding from the query
        // rather than 400ing on a flag nobody has to send.
        assert_eq!(probe(Some("banana")), None);
    }

    #[test]
    fn the_enrich_flag_is_three_valued_too() {
        // Same three states as `parse`, and for a sharper reason: absent means
        // "widen a parsed miss", which is neither on nor off. Read as off, a
        // sentence that matched nothing would go back to being a dead end; read
        // as on, every keyword miss would buy a model call.
        assert_eq!(tri_state(None), None);
        assert_eq!(tri_state(Some("true")), Some(true));
        assert_eq!(tri_state(Some("off")), Some(false));
        assert_eq!(tri_state(Some("banana")), None);
    }

    #[test]
    fn binding_is_opt_in_and_off_by_default() {
        // It is a second model call on top of the parse. A caller that already
        // knows its parameters must not pay for one by default, and a crawler
        // never should.
        let probe = |qs: Value| {
            serde_json::from_value::<CompareQuery>(qs)
                .expect("parses")
                .bind
        };
        assert!(!probe(json!({})), "absent means off");
        for on in ["1", "true", "yes", "on"] {
            assert!(probe(json!({ "bind": on })), "{on}");
        }
        for off in ["0", "false", "no", "banana"] {
            assert!(!probe(json!({ "bind": off })), "{off}");
        }
    }

    #[test]
    fn a_stated_parameter_outranks_a_price_read_out_of_prose() {
        let plan = QueryPlan {
            source: PlanSource::Parsed,
            tasks: Vec::new(),
            max_price_usd: Some(0.05),
            optimize_for: Some("latency".into()),
            recency_days: None,
            model: None,
            parse_ms: None,
            cached: None,
            parser_error: None,
            enrichment: None,
        };

        let inferred = Requirements::default().with_plan_defaults(&plan);
        assert_eq!(inferred.max_price, Some(0.05));
        assert_eq!(inferred.optimize_for.as_deref(), Some("latency"));

        let stated = Requirements {
            max_price: Some(0.01),
            optimize_for: Some("price".into()),
            ..Default::default()
        }
        .with_plan_defaults(&plan);
        assert_eq!(stated.max_price, Some(0.01));
        assert_eq!(stated.optimize_for.as_deref(), Some("price"));
    }

    #[test]
    fn the_cheapest_scores_one_on_price_and_the_dearest_zero() {
        let offers = [offer("cheap", 0.001, false), offer("dear", 0.5, false)];
        let refs: Vec<&CapabilityOffer> = offers.iter().collect();
        let f = Field::of(&refs, &HashMap::new());
        assert_eq!(f.price_score(0.001), 1.0);
        assert_eq!(f.price_score(0.5), 0.0);
    }

    #[test]
    fn one_candidate_is_not_punished_for_being_alone() {
        let offers = [offer("only", 0.42, false)];
        let refs: Vec<&CapabilityOffer> = offers.iter().collect();
        let f = Field::of(&refs, &HashMap::new());
        // With no field to compare against, a lone candidate is best on price
        // by definition rather than worst.
        assert_eq!(f.price_score(0.42), 1.0);
    }

    #[test]
    fn unmeasured_dimensions_are_dropped_not_zeroed() {
        let offers = [offer("a", 0.01, true)];
        let refs: Vec<&CapabilityOffer> = offers.iter().collect();
        let f = Field::of(&refs, &HashMap::new());
        let req = Requirements::default();
        let (total, breakdown, unscored) = score(&offers[0], None, &[], &f, &req);
        assert_eq!(unscored, vec!["latency", "reliability"]);
        // Scoring zero on two of four dimensions would cap it at 50; dropping
        // them lets an unmeasured provider still win on what IS known.
        assert!(total > 90.0, "got {total}");
        assert_eq!(breakdown["not_scored"], json!(["latency", "reliability"]));
    }

    /// A ceiling is a promise about the slow case, so it is judged on p95.
    /// This provider has a fast median (100ms) and a slow tail (300ms): under
    /// the old median rule it passed a 200ms ceiling it would blow one request
    /// in twenty.
    #[test]
    fn a_latency_ceiling_is_judged_on_the_tail_not_the_median() {
        let mut req = Requirements {
            max_latency_ms: Some(200),
            ..Default::default()
        };
        let offer = offer("fast-median-slow-tail", 0.01, false);
        let s = stat("fast-median-slow-tail", 100, 0.99); // p50 100, p95 300

        let excluded = excluded_by(&offer, Some(&s), &[], &req);
        let (rule, why) = excluded.expect("p95 of 300ms must fail a 200ms ceiling");
        assert_eq!(rule, "max_latency_ms");
        assert!(
            why.contains("p95"),
            "the reason must say which statistic: {why}"
        );

        // Raise the ceiling above the tail and it survives.
        req.max_latency_ms = Some(400);
        assert!(excluded_by(&offer, Some(&s), &[], &req).is_none());
    }

    /// The gate that stops a caller being handed a listing Apiosk already
    /// knows is broken. Nobody asked for a reliability floor in this test —
    /// that is the whole point of it.
    #[test]
    fn a_listing_that_fails_more_than_it_works_is_not_offered() {
        // positionstack, as measured 2026-08-30: 35 served of 320 tried, and
        // still active and rankable. Under the old measurement it was not even
        // reportable — 35 successes barely cleared a floor that counted only
        // successes, and a listing at 0% never cleared it at all.
        let req = Requirements::default();
        let o = offer("positionstack", 0.001, false);
        let s = stat("positionstack", 198, 0.11);

        let (rule, why) = excluded_by(&o, Some(&s), &[], &req).expect("11% must be excluded");
        assert_eq!(rule, "reliability");
        assert!(
            why.contains("11%") && why.contains("include_unreliable"),
            "the rejection must say the record and the way back: {why}"
        );

        // A caller who disagrees gets it in one retry. Held back, not deleted.
        let allowed = Requirements {
            include_unreliable: true,
            ..Default::default()
        };
        assert!(excluded_by(&o, Some(&s), &[], &allowed).is_none());

        // And a listing that merely has a bad day is untouched: the floor is
        // evidence as well as ratio.
        let healthy = stat("positionstack", 198, 0.94);
        assert!(excluded_by(&o, Some(&healthy), &[], &req).is_none());
    }

    /// Reliability and speed are measured independently now, so a listing can
    /// be scored on one and not the other.
    #[test]
    fn a_listing_with_a_record_but_no_timing_is_still_scored_on_its_record() {
        let req = Requirements::default();
        let o = offer("few-successes", 0.01, false);
        let mut s = stat("few-successes", 198, 0.8);
        s.p50_ms = None; // twelve tried, nine served: too few to have a p95
        s.p95_ms = None;

        // It is not excluded, and reliability still counts against the field.
        assert!(excluded_by(&o, Some(&s), &[], &req).is_none());
        let field = Field::of(&[&o], &std::collections::HashMap::new());
        let (_, breakdown, unscored) = score(&o, Some(&s), &[], &field, &req);
        assert_eq!(unscored, vec!["latency"], "only latency is unknown");
        let dims = breakdown["dimensions"].as_array().unwrap();
        assert!(
            dims.iter().any(|d| d["dimension"] == "reliability"),
            "a success rate that exists must be scored: {breakdown}"
        );

        // But a latency ceiling it cannot answer still rejects it.
        let timed = Requirements {
            max_latency_ms: Some(500),
            ..Default::default()
        };
        assert_eq!(
            excluded_by(&o, Some(&s), &[], &timed).map(|(r, _)| r),
            Some("measured")
        );
    }

    /// Ranking follows the tail too: between a provider that is usually fast
    /// but occasionally terrible and one that is steadily mediocre, the steady
    /// one is the better bet for an agent holding a timeout.
    #[test]
    fn latency_scoring_ranks_on_p95() {
        let offers = [offer("spiky", 0.01, false), offer("steady", 0.01, false)];
        let refs: Vec<&CapabilityOffer> = offers.iter().collect();
        let mut stats = HashMap::new();
        stats.insert("spiky".to_string(), stat("spiky", 50, 0.99)); // p95 150
        stats.insert("steady".to_string(), stat("steady", 90, 0.99)); // p95 270
        let f = Field::of(&refs, &stats);

        // Field ranges are built from p95 (150..270), so the spiky provider's
        // lower tail wins the dimension despite both being cheap.
        assert_eq!(f.latency_score(150), 1.0);
        assert_eq!(f.latency_score(270), 0.0);
    }

    #[test]
    fn weights_are_published_and_sum_to_the_total() {
        let req = Requirements::default();
        let offers = [offer("a", 0.01, false), offer("b", 0.02, false)];
        let refs: Vec<&CapabilityOffer> = offers.iter().collect();
        let f = Field::of(&refs, &HashMap::new());
        let mut stats = HashMap::new();
        stats.insert("a".to_string(), stat("a", 200, 0.99));
        let (total, breakdown, _) = score(&offers[0], stats.get("a"), &[], &f, &req);
        let sum: f64 = breakdown["dimensions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["contribution"].as_f64().unwrap())
            .sum();
        // The published contributions must add up to the published total.
        assert!((sum - total).abs() < 1.5, "sum {sum} vs total {total}");
    }

    #[test]
    fn optimize_for_moves_the_weight() {
        let mut r = Requirements::default();
        assert_eq!(r.weights().0, W_PRICE);
        r.optimize_for = Some("latency".into());
        let (_, wl, _, _) = r.weights();
        assert!(wl > W_LATENCY, "latency should dominate when asked for");
        r.optimize_for = Some("reliability".into());
        let (_, _, wr, _) = r.weights();
        assert!(wr > W_RELIABILITY);
    }

    #[test]
    fn an_exclusion_says_the_rule_and_the_number() {
        let o = offer("dear", 0.5, false);
        let req = Requirements {
            max_price: Some(0.1),
            ..Default::default()
        };
        let (rule, because) = excluded_by(&o, None, &[], &req).expect("excluded");
        assert_eq!(rule, "max_price");
        // The caller should not have to look up why: the numbers are in the text.
        assert!(because.contains("0.5"));
        assert!(because.contains("0.1"));
    }

    #[test]
    fn a_reliability_floor_excludes_the_unmeasured() {
        let o = offer("unknown", 0.001, true);
        let req = Requirements {
            min_reliability: Some(99.0),
            ..Default::default()
        };
        assert_eq!(
            excluded_by(&o, None, &[], &req).map(|(r, _)| r),
            Some("measured")
        );
    }

    #[test]
    fn the_reason_names_the_runner_up_and_an_exclusion() {
        let a = offer("winner", 0.05, false);
        let b = offer("cheaper", 0.01, false);
        let ranked = vec![
            (94.0, json!({"api_slug": "winner", "price_usd": 0.05}), &a),
            (89.0, json!({"api_slug": "cheaper", "price_usd": 0.01}), &b),
        ];
        let rejected = vec![
            json!({"api_slug": "flaky", "because": "succeeds 42.0% of the time, below the 99.0% floor"}),
        ];
        let why = reason_for(
            &ranked[0].1,
            94.0,
            &ranked,
            &rejected,
            &Requirements::default(),
        );
        assert!(why.contains("winner"));
        assert!(why.contains("94"));
        assert!(why.contains("cheaper is cheaper"));
        assert!(why.contains("flaky"));
    }
}

#[cfg(test)]
mod optimisation_tests {
    use super::*;

    #[test]
    fn optimising_for_latency_prefers_a_candidate_that_was_actually_timed() {
        // Asking for the fastest and receiving a provider nobody has ever timed
        // is not an answer to the question, however well it scores on price.
        let req = Requirements {
            optimize_for: Some("latency".into()),
            ..Default::default()
        };
        assert_eq!(ranks_ahead_on_optimised_dimension(&req, true), 0);
        assert_eq!(ranks_ahead_on_optimised_dimension(&req, false), 1);
    }

    #[test]
    fn optimising_for_price_does_not_care_whether_it_was_timed() {
        // Price is known for everything, so measurement is irrelevant to the
        // ordering and an unmeasured provider must not be demoted for it.
        let req = Requirements::default();
        assert_eq!(ranks_ahead_on_optimised_dimension(&req, false), 0);
        assert_eq!(ranks_ahead_on_optimised_dimension(&req, true), 0);
    }

    #[test]
    fn reliability_behaves_like_latency() {
        let req = Requirements {
            optimize_for: Some("reliability".into()),
            ..Default::default()
        };
        assert_eq!(ranks_ahead_on_optimised_dimension(&req, false), 1);
    }

    /// The external page size a caller may ask for: the sweep's default when
    /// absent, never zero, never past the sweep's own ceiling — an explore
    /// page asking for a thousand gets the hundred the merge actually keeps.
    #[test]
    fn the_external_limit_is_clamped_to_the_sweeps_ceiling() {
        let with_limit = |limit: Option<usize>| {
            DiscoverQuery {
                include_unreliable: false,
                q: None,
                capability: None,
                max_candidates: None,
                max_price: None,
                max_latency_ms: None,
                min_reliability: None,
                settlement: None,
                limit,
                require_all_inputs: false,
                optimize_for: None,
                include_external: true,
                parse: None,
                enrich: None,
                terms: None,
                debug: false,
            }
            .external_limit()
        };
        assert_eq!(with_limit(None), crate::sources::MAX_RETURNED);
        assert_eq!(with_limit(Some(0)), 1);
        assert_eq!(with_limit(Some(40)), 40);
        assert_eq!(with_limit(Some(1_000)), crate::sources::MAX_LIMIT);
        const { assert!(crate::sources::MAX_RETURNED < crate::sources::MAX_LIMIT) };
    }
}
