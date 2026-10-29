//! `POST /v1/do` — a job in words, a paid answer back.
//!
//! The whole chain in one call: parse the job, find the capability, score the
//! candidates, pick one, fill its parameters in, **pay it from the caller's
//! managed wallet**, and return what the provider said. The n8n node — and any
//! agent that wants the job done rather than a menu — calls this and nothing
//! else.
//!
//! ---------------------------------------------------------------------------
//! Where the money moves, and where it does not
//! ---------------------------------------------------------------------------
//!
//! This module contains no payment code. Execution is a loopback HTTP request
//! to the gateway's own `/{slug}/{path}` proxy route with the caller's connect
//! token attached — the exact request an agent would have made by hand. That
//! path already holds everything that keeps money safe here: per-wallet per-tx
//! and daily caps, idempotency, settlement, the request log. Duplicating any
//! of it would mean two places for a cap check to be wrong; going through the
//! front door means a `/v1/do` call can never spend a cent more than the same
//! call made directly could.
//!
//! Two consequences of that choice, both deliberate:
//!
//!   * A wallet at its cap surfaces here exactly as it would surface directly:
//!     the proxy's own 402 body, passed through under `execution`.
//!   * Only `apiosk-proxied` offers are executable. Federated listings cannot
//!     be settled by Apiosk, so `settlement=apiosk` is forced into the
//!     requirements and federated candidates land in `rejected` with that
//!     stated reason — excluded in the open, not silently skipped.
//!
//! ---------------------------------------------------------------------------
//! Fail-closed, in order
//! ---------------------------------------------------------------------------
//!
//! No token → 401 before anything runs. Nothing matched → 404 with the
//! interpretation, so a misread is distinguishable from an empty catalogue.
//! Every matched capability refuted by the relevance gate → 409 with the
//! verdicts, before any payment (see [`crate::relevance`]). Every candidate
//! rejected → 409 with each rejection named. No wallet able to fund any
//! checked candidate → 402 with the blocker named per wallet — a cap the
//! buyer set versus a wallet that holds no USDC — before the chain runs any
//! further. Required parameters unfilled → 422 with `missing_required`, and
//! **no payment** — a guessed parameter is a paid 400, the one failure that
//! costs money and looks like a provider outage. Only a request that is
//! `ready` reaches the wallet.

use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::flow::{
    evaluate_offers, reason_for, record_decision, resolve_set, EvaluatedField, Requirements,
};
use crate::db::{AgentConnectTokenLookup, CapabilityOffer, CapabilitySort};
use crate::proxy::hash_connect_token;
use crate::rate_limit::{RateLimitDecision, RateLimitPolicy};
use crate::request_binding::{bind, schemas_for, Binding};
use crate::service_auth::{Credential, ServiceAuth};
use crate::AppState;

/// Paid executions per token per minute. Generous for a workflow node, tight
/// enough that a runaway loop burns through a minute of budget, not a wallet.
const DO_RATE_LIMIT: RateLimitPolicy = RateLimitPolicy::per_minute(30);

/// Ceiling on how long the upstream call may take, whatever the caller asks
/// for. The proxy has its own upstream budget; this bounds the loopback leg.
const MAX_UPSTREAM_WAIT_MS: u64 = 120_000;
const DEFAULT_UPSTREAM_WAIT_MS: u64 = 60_000;

/// Cap on the provider response body echoed back. Same figure as the external
/// fetch proxy uses; a workflow that needs more should call the provider
/// directly with the request this endpoint hands back.
const MAX_RESPONSE_BYTES: usize = 512 * 1024;

/// Ranked candidates put through the wallet pre-check before giving up. Each
/// check can cost one on-chain RPC round trip per wallet, so this is bounded;
/// past the first few the field was weak anyway.
const MAX_FUNDING_CHECKED: usize = 3;

/// Whether at least one wallet on the token can carry a call of a given price.
enum Funding {
    Fundable,
    NotFundable,
    /// The on-chain check could not answer (RPC down, settlement disabled).
    /// Proceed and let the paid path — which is authoritative — decide.
    Unknown,
}

/// The App service has already checked and reserved the buyer's balance.
/// Only actual provider payments depend on the treasury's wallets and caps.
async fn funding_for_offer(
    state: &Arc<AppState>,
    credential: &crate::service_auth::Credential,
    token: &crate::db::AgentConnectToken,
    offer: &crate::db::CapabilityOffer,
) -> (Funding, Value) {
    // All callers reach here after service-key/token authentication. The API
    // flag is database-derived, including the exclusion of resell listings.
    if matches!(credential, crate::service_auth::Credential::ServiceKey { .. }) {
        if let Ok(Some(api)) = state.cache.get_api(&state.db, &offer.api_slug).await {
            if api.apiosk_revenue {
                return (Funding::Fundable, json!({
                    "ok": true,
                    "rail": "apiosk_balance",
                    "call_price_usd": offer.price_usd,
                    "wallets": [],
                    "note": "The App authorizes this call against its internal balance; no provider wallet payment is required.",
                }));
            }
        }
    }
    funding_for(state, token, offer.price_usd).await
}

/// The pre-payment funding picture for one call price, per wallet.
///
/// Mirrors `enforce_wallet_caps` + `can_settle` on the proxy's paid path
/// without replacing them: this pair *reports*, the paid path *enforces*. The
/// distinction it exists to surface is caps-versus-money — a buyer who set a
/// €1 cap has configured a limit, not a balance, and "insufficient funds"
/// must say which of the two stopped the call.
async fn funding_for(
    state: &Arc<AppState>,
    token: &crate::db::AgentConnectToken,
    price_usd: f64,
) -> (Funding, Value) {
    if token.wallets.is_empty() {
        return (
            Funding::Unknown,
            json!({
                "ok": Value::Null,
                "call_price_usd": price_usd,
                "wallets": [],
                "note": "This connect token authorizes no wallets; whether another rail can carry the call is decided at payment time.",
            }),
        );
    }

    // Each wallet is a database read; it used to be a database read AND an
    // on-chain balance/allowance call, which is why they run side by side
    // rather than in sequence. Tasks finish in whatever order they finish, so
    // each carries its position and the report is put back in the token's own
    // order.
    let mut set = tokio::task::JoinSet::new();
    for (index, wallet) in token.wallets.iter().cloned().enumerate() {
        let state = Arc::clone(state);
        set.spawn(async move { (index, wallet_funding(&state, &wallet, price_usd).await) });
    }
    let mut checked: Vec<(usize, (Option<bool>, Value))> = Vec::with_capacity(token.wallets.len());
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(entry) => checked.push(entry),
            // A panicked check is an undecided wallet, exactly like an RPC
            // that would not answer: the paid path decides.
            Err(err) => tracing::warn!("wallet funding check failed: {}", err),
        }
    }
    checked.sort_by_key(|(index, _)| *index);

    let any_fundable = checked
        .iter()
        .any(|(_, (fundable, _))| *fundable == Some(true));
    let any_unknown = checked.len() < token.wallets.len()
        || checked.iter().any(|(_, (fundable, _))| fundable.is_none());
    let wallets: Vec<Value> = checked.into_iter().map(|(_, (_, report))| report).collect();

    let (overall, ok) = if any_fundable {
        (Funding::Fundable, json!(true))
    } else if any_unknown {
        (Funding::Unknown, Value::Null)
    } else {
        (Funding::NotFundable, json!(false))
    };
    let note = match overall {
        Funding::Fundable => "At least one wallet can fund this call within its caps.",
        Funding::NotFundable => {
            "No wallet on this connect token can fund this call; every blocker is named per wallet."
        }
        Funding::Unknown => {
            "The on-chain check could not answer; the payment path itself will decide."
        }
    };
    (
        overall,
        json!({
            "ok": ok,
            "call_price_usd": price_usd,
            "wallets": wallets,
            "note": note,
        }),
    )
}

/// One wallet's verdict for one call price: `Some(true)` can fund it,
/// `Some(false)` cannot and the report says why, `None` could not be decided.
async fn wallet_funding(
    state: &Arc<AppState>,
    wallet: &crate::db::AgentConnectTokenWallet,
    price_usd: f64,
) -> (Option<bool>, Value) {
    let per_tx = wallet
        .per_tx_limit_usdc
        .to_string()
        .parse::<f64>()
        .unwrap_or(0.0);
    let per_day = wallet
        .daily_limit_usdc
        .to_string()
        .parse::<f64>()
        .unwrap_or(0.0);
    let spent_today = state
        .db
        .get_wallet_paid_spend_today(&wallet.wallet_address)
        .await
        .map(|v| v.to_string().parse::<f64>().unwrap_or(0.0))
        .unwrap_or(0.0);

    let mut blockers: Vec<String> = Vec::new();
    if wallet.wallet_status != "active" {
        blockers.push(format!(
            "wallet status is '{}', not active",
            wallet.wallet_status
        ));
    }
    if price_usd > per_tx {
        blockers.push(format!(
            "call costs ${price_usd} but the per-transaction cap set on this connection is ${per_tx}"
        ));
    }
    if spent_today + price_usd > per_day {
        blockers.push(format!(
            "daily cap ${per_day} has ${:.6} left today and the call costs ${price_usd}",
            (per_day - spent_today).max(0.0)
        ));
    }

    // `fundable` used to be answered by the chain: `SettlementClient::can_settle`
    // checked the wallet's USDC balance and its allowance to the settlement
    // contract, and `usdc_balance_of` filled in `balance_usdc` so the blocker
    // could say "empty wallet" rather than "allowance too low". A buyer wallet
    // on a chain is not how Apiosk is paid any more — the buyer's Apiosk balance
    // is debited in the App's Supabase and the treasury pays the provider — so
    // the caps above are the whole verdict this preview can give. `null`
    // (rather than `true`) is deliberate: it means unknown, which is what the
    // gateway reported whenever the settlement RPC was unavailable, and callers
    // already handle it.
    let balance_usdc: Option<f64> = None;
    let fundable: Option<bool> = if blockers.is_empty() {
        None
    } else {
        Some(false)
    };

    (
        fundable,
        json!({
            "address": wallet.wallet_address,
            "status": wallet.wallet_status,
            "cap_per_tx_usdc": per_tx,
            "cap_per_day_usdc": per_day,
            "spent_today_usdc": spent_today,
            "balance_usdc": balance_usdc,
            "fundable": fundable,
            "blockers": blockers,
        }),
    )
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct DoRequest {
    /// Gateway v2 supplies a fully bound named endpoint. This opt-in can only
    /// tighten execution: no intent model and no additional resolver purchase.
    pub deterministic: bool,
    /// The job, in words. The only required field.
    pub job: Option<String>,
    /// Skip the search and name the capability outright.
    pub capability: Option<String>,
    pub max_price: Option<f64>,
    pub optimize_for: Option<String>,
    pub max_latency_ms: Option<i64>,
    pub min_reliability: Option<f64>,

    /// Run a listing Apiosk has quarantined for failing more often than it
    /// works. Off by default: this endpoint spends money on the answer, so
    /// the one place a known-broken candidate must never be chosen silently
    /// is here.
    #[serde(default)]
    pub include_unreliable: bool,
    /// `true` forces the query parser, `false` forbids it; absent lets the
    /// query decide, same as `/v1/discover`.
    pub parse: Option<bool>,
    /// The endpoint to run, by `api_endpoints.id`.
    ///
    /// Two meanings, and which one applies is decided by `capability`:
    ///
    /// * **With `capability`** — a pin on that capability's decision. The full
    ///   field is still scored, so every constraint below still applies and a
    ///   pinned candidate that fails one is refused with the rule named:
    ///   pinning is not a way around `max_price`, the settlement rule, or a
    ///   wallet cap. What it buys is determinism, so a retry carrying `inputs`
    ///   binds them to the endpoint those values were written for.
    ///
    /// * **Alone** — run this endpoint. There is no decision to narrow: the
    ///   caller read this endpoint's own description, schema and price and
    ///   chose it, which is the case every Explore and catalogue surface is in.
    ///   Routing that through the capability matcher does not check the choice,
    ///   it discards it — most of the catalogue is mapped to no capability at
    ///   all, so the job text matches something else and the pin then empties
    ///   the field. See `run_named_candidate`; every money rule is the same.
    pub candidate_id: Option<Uuid>,
    /// Parameter values the caller already holds, by the names the previous
    /// answer listed in `request.missing_required`.
    ///
    /// Applied over anything read out of the job, and validated exactly as model
    /// output is: a name the endpoint does not document, or a value outside a
    /// documented `enum`, is reported in `request.ignored_inputs` rather than
    /// sent. Credential-shaped names can never match.
    pub inputs: Option<serde_json::Map<String, Value>>,
    /// Everything except the payment: decide, bind, and report what WOULD have
    /// been sent. The n8n node's "plan" switch.
    pub dry_run: bool,
    /// Upstream wait in milliseconds, clamped to [1, 120000].
    pub timeout_ms: Option<u64>,
    /// The offer the USER chose, from `POST /v1/quote`.
    ///
    /// Opaque and signed, so it cannot be invented or edited. When present the
    /// field is still scored, and then narrowed to this one endpoint — the same
    /// discipline `candidate_id` follows, and for the same reason: pinning buys
    /// determinism, never a way around a constraint.
    ///
    /// Absent — which is every call the n8n node makes — behaves exactly as it
    /// does today.
    pub offer_id: Option<String>,
    /// Carried for the audit trail. The offer_id already names its own quote,
    /// so this is not what makes the pin work.
    pub quote_id: Option<Uuid>,
    /// The price ceiling the user actually saw and confirmed.
    ///
    /// The call is REFUSED rather than paid if the real price is above it. This
    /// is the checkpoint that makes "the price shown is the price charged" a
    /// property of the system rather than a hope about timing.
    pub max_price_usdc: Option<f64>,
    /// Resume a purchase the buyer's rules held, once they have approved it.
    ///
    /// From a previous 402 carrying `status: "approval_required"`. It is
    /// permission for ONE purchase on the terms the buyer saw, not a bypass of
    /// the other rules: the policy is evaluated again, the price is re-checked
    /// against the approved amount, and the approval is spent exactly once.
    ///
    /// Absent — which is every call that predates this — behaves exactly as it
    /// does today.
    pub approval_id: Option<Uuid>,
}

impl DoRequest {
    fn requirements(&self) -> Requirements {
        Requirements {
            max_price: self.max_price,
            max_latency_ms: self.max_latency_ms,
            min_reliability: self.min_reliability,
            // Forced, not defaulted: this endpoint settles through Apiosk, so a
            // federated candidate is not something it could pay even if chosen.
            // The exclusion happens inside the scorer, where it is named in
            // `rejected` instead of silently narrowing the field.
            settlement: Some("apiosk".to_string()),
            require_all_inputs: false,
            optimize_for: self.optimize_for.clone(),
            include_unreliable: self.include_unreliable,
        }
    }
}

/// `GET /v1/do` — self-documenting, like the intent endpoint: an agent or a
/// human poking at the URL learns how to use it without leaving it.
pub async fn do_job_info() -> Response {
    Json(json!({
        "ok": true,
        "what": "POST a job in plain words; Apiosk picks the right paid API, fills its parameters in from your words, pays it from your connected wallet, and returns the provider's response.",
        "auth": "X-Apiosk-Connect-Token: <token from the buyer portal>. The token carries the wallets and the spending caps you set there; this endpoint can never exceed them.",
        "body": {
            "job": "Get Nvidia earnings news from the last 7 days",
            "max_price": "optional price ceiling in USD",
            "optimize_for": "optional: price | latency | reliability | balanced",
            "dry_run": "optional: true = decide and bind but do not pay or call",
            "approval_id": "optional: resume a purchase the buyer's rules held, from a previous 402's approval_id, once they have approved it in the portal. Permission for one purchase on the terms they saw — the rules are still checked and the price is still re-checked.",
            "candidate_id": "optional: run one named endpoint, by the id GET /v1/discover, GET /v1/apis and GET /v1/providers/{username} publish. WITH `capability` it narrows that capability's decision and never bypasses it — a pinned candidate still has to pass every constraint. WITHOUT `capability` there is no decision to narrow: the named endpoint runs, bound against its own published schema. The price you confirmed, the wallet check, the fail-closed on a missing parameter and the settlement are identical either way.",
            "inputs": "optional: parameter values you already hold, by the names a previous 422 listed in request.missing_required. Applied over anything read out of the job, and validated the same way.",
        },
        "retrying_a_422": "A 422 carries a `retry_with` object: the same job, the capability, the candidate_id, and an `inputs` map with one null per missing parameter. Fill the nulls and POST it back. With capability and candidate_id set, the retry makes no model call at all — the capability is looked up rather than parsed, and supplied parameters are not re-read.",
        "flow": "parse -> search -> relevance gate -> compare -> decide -> wallet pre-check -> bind -> pay -> respond. Each step's output is in the response, so the answer always says which API answered, why it won, what was sent, and what it cost.",
        "refusals": "401 without a valid token; 404 when nothing serves the job; 409 when no matched capability actually answers the job (each verdict named) or every candidate fails your constraints; 402 when no wallet can fund any viable candidate (the blocker named per wallet — a cap you set, or the wallet's own USDC balance); 422 when the provider requires parameters your job did not state — nothing is paid on any of those.",
    }))
    .into_response()
}

/// `POST /v1/do`
pub async fn do_job(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap<HeaderValue>,
    body: axum::body::Bytes,
) -> Result<Response, StatusCode> {
    // ---- 1. Identity first. Nothing runs, and nothing is spent — model calls
    // included — for a caller who could never pay anyway.
    let credential = match Credential::from_headers(&headers) {
        Some(c) => c,
        None => {
            return Ok(error(
                StatusCode::UNAUTHORIZED,
                "connect_token_missing",
                "Send the connect token from the buyer portal via X-Apiosk-Connect-Token or Authorization: Bearer <token>. It is created once — connect a wallet, set your limits, copy the token.",
            ))
        }
    };
    // A service key is Apiosk's own surface spending from the treasury; the
    // gateway resolves it to the treasury's connect token itself, and from
    // here on the two kinds of caller are the same thing.
    let lookup = match &credential {
        Credential::ServiceKey { .. } => match crate::service_auth::resolve(&state, &headers).await
        {
            ServiceAuth::Resolved(lookup) => Ok(lookup),
            ServiceAuth::Rejected(code, message) => {
                return Ok(error(StatusCode::UNAUTHORIZED, code, message))
            }
            ServiceAuth::NotPresented => Ok(AgentConnectTokenLookup::Unknown),
        },
        Credential::ConnectToken(raw_token) => {
            state
                .db
                .lookup_agent_connect_token_by_hash(&hash_connect_token(raw_token))
                .await
        }
    };
    let token = match lookup {
        Ok(AgentConnectTokenLookup::Active(token)) => token,
        Ok(AgentConnectTokenLookup::Unknown) => {
            return Ok(error(
                StatusCode::UNAUTHORIZED,
                "connect_token_unknown",
                "This connect token is not recognized.",
            ))
        }
        Ok(AgentConnectTokenLookup::Revoked { token_prefix }) => {
            return Ok(error(
                StatusCode::UNAUTHORIZED,
                "connect_token_revoked",
                &format!("Connect token {token_prefix}… has been revoked."),
            ))
        }
        Ok(AgentConnectTokenLookup::Expired { token_prefix, .. }) => {
            return Ok(error(
                StatusCode::UNAUTHORIZED,
                "connect_token_expired",
                &format!("Connect token {token_prefix}… has expired."),
            ))
        }
        Err(err) => {
            tracing::error!("connect token lookup failed for /v1/do: {}", err);
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    // Per-token, because the token is the spender. An IP limit would let one
    // token spread across IPs and would lump NAT'd callers together.
    if let RateLimitDecision::Throttled { retry_after } = state
        .rate_limiter
        .check(&format!("do:{}", token.token_id), DO_RATE_LIMIT)
        .await
    {
        let mut response = error(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Too many /v1/do calls on this token. Wait and retry.",
        );
        if let Ok(v) = HeaderValue::from_str(&retry_after.as_secs().max(1).to_string()) {
            response.headers_mut().insert("retry-after", v);
        }
        return Ok(response);
    }

    // ---- 2. The job.
    let request: DoRequest = if body.is_empty() {
        DoRequest::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(err) => {
                return Ok(error(
                    StatusCode::BAD_REQUEST,
                    "invalid_json",
                    &format!("Body must be JSON, e.g. {{\"job\": \"...\"}} — {err}"),
                ))
            }
        }
    };
    if request.deterministic
        && (request.candidate_id.is_none()
            || request
                .capability
                .as_deref()
                .is_some_and(|v| !v.trim().is_empty()))
    {
        return Ok(error(
            StatusCode::BAD_REQUEST,
            "deterministic_candidate_required",
            "Deterministic execution requires candidate_id and no capability selection.",
        ));
    }
    let job = request.job.as_deref().unwrap_or("").trim().to_string();
    if job.is_empty()
        && request
            .capability
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
    {
        return Ok(error(
            StatusCode::BAD_REQUEST,
            "job_required",
            "Describe the job: {\"job\": \"Get Nvidia earnings news from the last 7 days\"}.",
        ));
    }

    // ---- 2b. A candidate the caller NAMED, with no capability to decide.
    //
    // `candidate_id` alone means "run exactly this endpoint". There is nothing
    // left to decide: a person read the endpoint's own description, schema and
    // price on a page and pressed the button on that row. The capability layer
    // exists to answer "what performs this job" from words, and it only knows
    // the endpoints a human has mapped — a few dozen of the catalogue's
    // fifteen hundred — so routing a named endpoint through it does not check
    // the choice, it discards it: the job text gets matched to some other
    // capability and the pin then empties the field, which is the
    // `candidate_unavailable` every Explore run would have ended in.
    //
    // `capability` + `candidate_id` keeps the old meaning exactly — the field
    // is scored and then narrowed to this one endpoint — because there the
    // caller asked for a decision and named a preference within it.
    //
    // Nothing about money is relaxed here. The price the user confirmed, the
    // funding pre-check, the binding, the loopback through the proxy and the
    // fee arithmetic are the same code the decided path runs; see
    // `run_named_candidate`.
    if let (Some(candidate_id), true) = (
        request.candidate_id,
        request
            .capability
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty(),
    ) {
        return Ok(run_named_candidate(
            &state,
            &headers,
            &credential,
            &token,
            &request,
            &job,
            candidate_id,
        )
        .await);
    }

    // ---- 3. Parse + search: the same reading and retrieval discover uses.
    let (plan, infos) =
        resolve_set(&state, &job, request.capability.as_deref(), request.parse).await?;
    let Some(first_info) = infos.first().cloned() else {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(json!({
                "did": false,
                "error": "no_capability",
                "message": "Nothing in the catalogue performs this job. The interpretation below shows how it was read — pass capability=<slug> to name one outright, or browse GET /v1/capabilities.",
                "job": job,
                "interpretation": plan.to_json(),
            })),
        )
            .into_response());
    };

    // ---- 3b. Relevance gate. Full-text search is lexical: a job about
    // "Nvidia earnings news" can rank equity.quote above news.search because
    // "earnings" is all over quote endpoints. Everything downstream of that
    // mistake is correct and still buys the wrong thing, so on this paid path
    // the matched capabilities are judged against the job before any of them
    // is committed to. A refuted first match falls through to the next
    // confirmed one; all refuted blocks the request before any payment.
    // Skipped when the caller named the capability or pinned a candidate —
    // both mean the choice was already theirs — and fail-open when the model
    // cannot be asked, so the gate can only ever remove wrong purchases.
    let explicit_capability = request
        .capability
        .as_deref()
        .map(str::trim)
        .is_some_and(|s| !s.is_empty());
    let (info, relevance) = if explicit_capability || request.candidate_id.is_some() {
        (first_info, Value::Null)
    } else {
        let candidates: Vec<crate::relevance::Candidate<'_>> = infos
            .iter()
            .take(crate::relevance::MAX_CHECKED)
            .map(|i| crate::relevance::Candidate {
                slug: &i.slug,
                name: &i.name,
                description: &i.description,
            })
            .collect();
        let outcome = crate::relevance::check(&state, &job, &candidates).await;
        if outcome.is_checked() {
            match outcome.first_confirmed().map(str::to_string) {
                Some(slug) => {
                    let chosen = infos
                        .iter()
                        .find(|i| i.slug == slug)
                        .cloned()
                        .unwrap_or(first_info);
                    (chosen, outcome.to_json())
                }
                None => {
                    return Ok((
                        StatusCode::CONFLICT,
                        Json(json!({
                            "did": false,
                            "error": "no_relevant_capability",
                            "message": "The catalogue matched capabilities for these words, but none of them actually answers this job — each verdict below says why. Nothing was paid. Name the capability outright with `capability` if a verdict is wrong, or rephrase the job.",
                            "job": job,
                            "interpretation": plan.to_json(),
                            "relevance": outcome.to_json(),
                            "matched_capabilities": infos.iter()
                                .map(|i| json!({ "slug": i.slug, "name": i.name }))
                                .collect::<Vec<_>>(),
                        })),
                    )
                        .into_response());
                }
            }
        } else {
            (first_info, outcome.to_json())
        }
    };

    // ---- 4. Compare + decide: the same evaluation `/v1/decide` runs.
    let requirements = request.requirements().with_plan_defaults(&plan);
    let offers = state
        .db
        .list_capability_offers(&info.slug, &CapabilitySort::Price, requirements.max_price)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let stats = state.cache.peek_provider_stats().await.unwrap_or_default();
    let EvaluatedField {
        ranked,
        rejected,
        considered,
        measured_count,
    } = evaluate_offers(&offers, &stats, &info.input_contract, &requirements);

    let comparison_id = crate::decision_record::record_comparison_now(
        &state,
        &headers,
        &info.slug,
        "do",
        requirements.max_price,
        &considered,
        None,
    )
    .await;

    // A chosen offer pins the field to one endpoint, and pins its PRICE.
    //
    // Same discipline as `candidate_id` below: applied after scoring, never
    // instead of it, so a chosen offer that a constraint rejected is refused
    // with that rejection rather than executed because the user picked it.
    // What an offer_id adds is the price it was quoted at, which is what makes
    // "the price shown is the price charged" a property rather than a hope.
    let pinned_endpoint = match request.offer_id.as_deref() {
        None => None,
        Some(raw) => {
            let Some(verified) = crate::v1_routes::verify_offer_id(&state, raw) else {
                return Ok((
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "did": false,
                        "error": "offer_invalid",
                        "message": "That offer_id was not issued by this gateway, or it has been altered. Call POST /v1/quote again and use an offer from the answer.",
                    })),
                )
                    .into_response());
            };
            if verified.expired {
                return Ok((
                    StatusCode::CONFLICT,
                    Json(json!({
                        "did": false,
                        "error": "quote_expired",
                        "message": "That quote has expired, so the price on it is no longer one anybody promised. Call POST /v1/quote again, show the user the fresh prices, and use an offer from that answer.",
                        "quote_id": verified.quote_id,
                        "api_slug": verified.api_slug,
                    })),
                )
                    .into_response());
            }
            Some(verified)
        }
    };

    // A pin narrows the field to one candidate. Applied *after* scoring rather
    // than instead of it: a pinned candidate that a constraint rejected has to be
    // refused with that rejection, not executed because the caller named it.
    // Otherwise `candidate_id` would be a way around `max_price` and the
    // settlement rule, and the guarantee at the top of this module — that /v1/do
    // can never spend more than the same call made directly — would stop holding.
    let ranked = match pinned_endpoint
        .as_ref()
        .map(|offer| offer.endpoint_id)
        .or(request.candidate_id)
    {
        Some(pinned) => ranked
            .into_iter()
            .filter(|(_, _, offer)| offer.endpoint_id == Some(pinned))
            .collect::<Vec<_>>(),
        None => ranked,
    };

    if let (Some(offer), true) = (pinned_endpoint.as_ref(), ranked.is_empty()) {
        return Ok((
            StatusCode::CONFLICT,
            Json(json!({
                "did": false,
                "error": "offer_unavailable",
                "message": "The offer the user chose is no longer viable: it was scored with the rest of the field and a constraint removed it, so it cannot be executed just because it was chosen. `rejected` below names the rule. Quote again and show the user what is actually available.",
                "job": job,
                "capability": info.slug,
                "offer_id": request.offer_id,
                "quote_id": offer.quote_id,
                "rejected": rejected,
            })),
        )
            .into_response());
    }

    if let (Some(pinned), true) = (request.candidate_id, ranked.is_empty()) {
        return Ok((
            StatusCode::CONFLICT,
            Json(json!({
                "did": false,
                "error": "candidate_unavailable",
                "message": "The pinned candidate_id is not among the viable candidates for this job. It was scored with the rest of the field rather than skipped, so if it appears in `rejected` below then that rejection is the reason and pinning cannot override it. A pin also only holds within one capability: retry with the job text and `capability` from the answer that produced this id, or drop candidate_id to let Apiosk choose again.",
                "job": job,
                "capability": info.slug,
                "candidate_id": pinned,
                "interpretation": plan.to_json(),
                "comparison_id": comparison_id,
                "rejected": rejected,
            })),
        )
            .into_response());
    }

    if ranked.is_empty() {
        return Ok((
            StatusCode::CONFLICT,
            Json(json!({
                "did": false,
                "error": "no_viable_candidate",
                "message": "Every candidate failed a constraint. Each rejection names the rule, so relax the binding one rather than guessing.",
                "job": job,
                "capability": info.slug,
                "interpretation": plan.to_json(),
                "comparison_id": comparison_id,
                "rejected": rejected,
            })),
        )
            .into_response());
    }

    // ---- 4a. The price the user confirmed.
    //
    // Checked here, before binding and long before payment, against the offer
    // that actually won. Two numbers can disagree by now: the price quoted
    // minutes ago, and the ceiling the user was shown and agreed to. Either
    // being below the live price is a refusal, not a rounding decision — a
    // payment product may charge less than it said and may never charge more.
    if let Some((_, _, offer)) = ranked.first() {
        let live = offer.price_usd;
        let ceiling = request
            .max_price_usdc
            .into_iter()
            .chain(pinned_endpoint.as_ref().map(|o| o.quoted_price_usd))
            .fold(f64::INFINITY, f64::min);

        // A hundredth of a cent of tolerance, for the same reason the policy
        // evaluator carries an epsilon: these are doubles holding six-decimal
        // USDC, and refusing a purchase for representation error is worse than
        // the error.
        if ceiling.is_finite() && live - ceiling > 1e-9 {
            return Ok((
                StatusCode::CONFLICT,
                Json(json!({
                    "did": false,
                    "error": "price_moved",
                    "message": format!(
                        "{} now costs ${live:.6}, above the ${ceiling:.6} the user confirmed. Nothing was paid. Show them the new price and ask again.",
                        offer.api_slug
                    ),
                    "job": job,
                    "capability": info.slug,
                    "api_slug": offer.api_slug,
                    "quoted_price_usdc": pinned_endpoint.as_ref().map(|o| o.quoted_price_usd),
                    "confirmed_max_price_usdc": request.max_price_usdc,
                    "live_price_usdc": live,
                    "quote_id": pinned_endpoint.as_ref().map(|o| o.quote_id),
                })),
            )
                .into_response());
        }
    }

    // ---- 4b. Wallet pre-check, before any binding or payment. The proxy
    // enforces the same rules again at payment time (that check stays
    // authoritative); this one exists so an unfundable call is refused with
    // the exact blocker named — a cap the buyer set versus a wallet that
    // simply holds no USDC — instead of surfacing as a bare 402 after the
    // whole chain ran. It is also the pre-payment fallback: a candidate no
    // wallet can fund is skipped in the open and the next-ranked one is
    // tried, because "the winner is unaffordable" should cost the caller a
    // cheaper candidate, not the whole request. Fail-open on an unreachable
    // RPC: Unknown proceeds and the paid path decides.
    let mut skipped_unfundable: Vec<Value> = Vec::new();
    let mut selection = None;
    // The top candidate's verdict, kept for the dry run below so it is not
    // asked of the database and the chain a second time.
    let mut first_funding_json: Option<Value> = None;
    for (score, candidate_json, offer) in ranked.iter().take(MAX_FUNDING_CHECKED).cloned() {
        let (funding, funding_json) = funding_for_offer(&state, &credential, &token, &offer).await;
        if first_funding_json.is_none() {
            first_funding_json = Some(funding_json.clone());
        }
        match funding {
            Funding::Fundable | Funding::Unknown => {
                selection = Some((score, candidate_json, offer, funding_json));
                break;
            }
            Funding::NotFundable => skipped_unfundable.push(json!({
                "api_slug": offer.api_slug,
                "price_usd": offer.price_usd,
                "because": "no wallet on this connect token can fund this call",
                "funding": funding_json,
            })),
        }
    }

    // A dry run is the "plan only" path: it must still show the full decision
    // and bound request, with `funding.ok: false` saying payment would be
    // refused. Only a real execution turns an unfundable field into a 402.
    if selection.is_none() && request.dry_run {
        if let Some((score, candidate_json, offer)) = ranked.first().cloned() {
            let funding_json = match first_funding_json.take() {
                Some(report) => report,
                None => funding_for_offer(&state, &credential, &token, &offer).await.1,
            };
            selection = Some((score, candidate_json, offer, funding_json));
        }
    }

    let Some((top_score, chosen_json, chosen_offer, funding_json)) = selection else {
        return Ok((
            StatusCode::PAYMENT_REQUIRED,
            Json(json!({
                "did": false,
                "error": "agent_wallet_cannot_fund",
                "message": "Candidates were checked against your wallets before paying, and none of the checked ones can be funded. Each entry below names the exact blocker. Note that the caps you set on a connect token are limits, not money: the wallet itself must hold USDC (and allowance) on Base. Nothing was paid.",
                "job": job,
                "capability": info.slug,
                "interpretation": plan.to_json(),
                "comparison_id": comparison_id,
                "relevance": relevance,
                "unfundable": skipped_unfundable,
                "rejected": rejected,
            })),
        )
            .into_response());
    };

    // ---- 5. Bind: the winner's own documented parameters, filled from the job.
    let schema_map = schemas_for(&state, &[chosen_offer]).await;
    let supplied = request.inputs.clone().unwrap_or_default();
    let binding = bind(
        &state,
        &job,
        chosen_offer,
        chosen_offer.endpoint_id.and_then(|id| schema_map.get(&id)),
        &supplied,
        crate::request_binding::Fill::FromJob,
    )
    .await;

    // ---- 5b. The automatic resolver.
    //
    // A required identifier the request cannot carry (a KVK profile's
    // `kvkNummer` when the caller only named a company) is resolved by
    // running a declared resolver endpoint FIRST — paid and settled through
    // the same loopback as any other call — and re-binding with its answer.
    // Nothing here runs unless some `param_resolvers` row applies, so a job
    // that binds ready never pays for a lookup it did not need.
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .unwrap_or_else(|| format!("do-{}", Uuid::new_v4()));
    let timeout = std::time::Duration::from_millis(
        request
            .timeout_ms
            .unwrap_or(DEFAULT_UPSTREAM_WAIT_MS)
            .clamp(1, MAX_UPSTREAM_WAIT_MS),
    );

    let mut binding = binding;
    let mut steps: Vec<Value> = Vec::new();
    let mut resolution: Vec<Value> = Vec::new();
    // Which declarations apply: see `resolver_applies`. NOT "is the binding
    // short of a parameter", which is the same question only for the half of
    // the cases where nothing filled the identifier in at all. A form whose
    // one box holds a company name binds READY — with a name where a
    // registration number belongs — and gating the lookup on readiness sent
    // that name upstream as an identifier for a guaranteed paid refusal,
    // under a panel that had already quoted the lookup.
    let resolvers = if request.dry_run {
        // A dry run pays for nothing, so it resolves nothing; the pre-step
        // line below says what WOULD run.
        Vec::new()
    } else {
        applicable_resolvers(&state, chosen_offer, Some(&info.slug), &binding).await
    };
    if !resolvers.is_empty() {
        // Same ceiling as step 4a above, PLUS `max_price` — that one was
        // missing here. Step 4a's own omission is harmless there because the
        // SQL candidate filter already bounds the primary to `max_price`; the
        // resolver leg has no such filter (`direct_offer` loads it by id
        // alone), so this fold is the only guard standing between an
        // unbounded second call and a caller who set `max_price` but not
        // `max_price_usdc`.
        let ceiling = request
            .max_price_usdc
            .into_iter()
            .chain(pinned_endpoint.as_ref().map(|o| o.quoted_price_usd))
            .chain(request.max_price)
            .fold(f64::INFINITY, f64::min);
        match auto_resolve_inputs(
            &state,
            &job,
            binding,
            chosen_offer,
            resolvers,
            Some(&info.slug),
            &credential,
            &idempotency_key,
            timeout,
            ceiling,
            &supplied,
            request.approval_id,
            crate::request_binding::Fill::FromJob,
        )
        .await
        {
            AutoResolve::NotApplicable(unchanged) => binding = unchanged,
            AutoResolve::Refusal(response) => return Ok(*response),
            AutoResolve::Resolved {
                binding: rebound,
                steps: ran,
                resolution: resolved,
            } => {
                binding = rebound;
                steps = ran;
                resolution = resolved;
            }
        }
    }

    let reason = reason_for(&chosen_json, top_score, &ranked, &rejected, &requirements);
    let decision_id = record_decision(
        &state,
        &headers,
        &info.slug,
        comparison_id,
        chosen_offer,
        top_score,
        &reason,
        &rejected,
    )
    .await;

    let decision = json!({
        "decision_id": decision_id,
        "comparison_id": comparison_id,
        "capability": info.slug,
        "selected_api": chosen_offer.api_slug,
        "selected": chosen_json,
        "score": top_score,
        "reason": reason,
        "candidates_considered": offers.len(),
        "candidates_measured": measured_count,
        "rejected": rejected,
        // The two pre-payment gates, published so a skipped capability or an
        // unaffordable candidate is always visible with its reason.
        "relevance": relevance,
        "funding": funding_json,
        "skipped_unfundable": skipped_unfundable,
    });

    // A missing value that a declared pre-step will produce is not a user
    // clarification. Dry runs do not spend, so project the request past those
    // steps and return the same successful preview the direct-candidate path
    // returns. This keeps capability callers and pinned-candidate callers on
    // one contract.
    if request.dry_run && !binding.ready() {
        let pre_steps = planned_pre_steps(&state, chosen_offer, Some(&info.slug), &binding).await;
        let resolved = pre_step_parameters(&pre_steps);
        let projected = binding.to_json_after_resolving(&resolved);
        if projected.get("ready").and_then(Value::as_bool) == Some(true) {
            return Ok(Json(json!({
                "did": false,
                "dry_run": true,
                "message": "Decision and request only — nothing was paid or sent. Declared lookup steps will fill the unresolved provider parameters before execution.",
                "job": job,
                "interpretation": plan.to_json(),
                "decision": decision,
                "request": projected,
                "pre_steps": if pre_steps.is_empty() { Value::Null } else { Value::Array(pre_steps) },
            }))
            .into_response());
        }
    }

    // ---- 6. Fail closed before any money moves.
    if !binding.ready() {
        // A dry run never pays, so it never runs a resolver either — it says
        // which lookup WOULD run, and at what price, instead.
        let pre_steps = if request.dry_run {
            planned_pre_steps(&state, chosen_offer, Some(&info.slug), &binding).await
        } else {
            Vec::new()
        };
        // Two different failures reach here and they are not the caller's fault
        // in the same way. One is "your job does not say what this endpoint
        // needs"; the other is "we could not read your job at all", which is
        // ours. Reporting the second as the first sends a caller off filling in
        // parameters that were never the problem.
        let binder_failed = binding.source == crate::request_binding::Source::Unavailable;
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "did": false,
                "error": if binder_failed { "request_not_built" } else { "missing_parameters" },
                // "Nothing was paid" is only true when no resolver step ran
                // first — a KVK lookup that resolved a kvkNummer but left a
                // SECOND required parameter unfilled still spent the buyer's
                // money, and `steps` right below says exactly how much. An LLM
                // deciding whether a retry costs money reads this sentence,
                // not the array.
                "message": if binder_failed {
                    format!(
                        "Apiosk could not turn this job into a request: the step that reads the job and fills the endpoint's parameters did not answer, so the call would have gone out with no parameters at all and bought the provider's error. {} Try again — that step is a live call and fails transiently — or name the values yourself in `inputs`, with `candidate_id` from `retry_with` below. What this endpoint accepts is listed at GET /v1/apis/{}.",
                        if steps.is_empty() { "Nothing was paid." } else { "A lookup step that ran first was still paid — see `steps`." },
                        chosen_offer.api_slug
                    )
                } else {
                    format!(
                        "The chosen endpoint requires parameters this job does not state. {} Retry with those values in `inputs`, carrying `capability` and `candidate_id` from `retry_with` below so they bind to this same endpoint — or add the values to the job text and let them be read out again.",
                        if steps.is_empty() {
                            "Nothing was paid: sending the call anyway would cost the request price and return an error."
                        } else {
                            "A lookup step that ran first was still paid — see `steps` — even though this call itself was not."
                        }
                    )
                },
                "job": job,
                // Everything the retry needs, pre-assembled: the two fields that
                // pin the decision, and the parameters to fill, by name. A caller
                // reading this — an LLM in a workflow's blocked branch, usually —
                // should not have to know which of the fields above matter.
                "retry_with": {
                    "endpoint": "POST /v1/do",
                    "job": job,
                    "capability": info.slug,
                    "candidate_id": chosen_offer.endpoint_id,
                    "inputs": binding.missing_required.iter()
                        .map(|p| (p.name.clone(), Value::Null))
                        .collect::<serde_json::Map<_, _>>(),
                    "how": "Fill each null in `inputs` with a value for that parameter, then POST this object. Sending `capability` and `candidate_id` makes the retry deterministic and costs no model call: the same endpoint is chosen, and supplied parameters are not re-read out of the job.",
                },
                "interpretation": plan.to_json(),
                "decision": decision,
                "request": binding.to_json(),
                // Present only when a lookup ran (and was charged) before this
                // refusal, or — on a dry run — when one would have.
                "steps": if steps.is_empty() { Value::Null } else { Value::Array(steps.clone()) },
                "pre_steps": if pre_steps.is_empty() { Value::Null } else { Value::Array(pre_steps) },
            })),
        )
            .into_response());
    }

    if request.dry_run {
        return Ok(Json(json!({
            "did": false,
            "dry_run": true,
            "message": "Decision and request only — nothing was paid or sent. POST again without dry_run to execute.",
            "job": job,
            "interpretation": plan.to_json(),
            "decision": decision,
            "request": binding.to_json(),
        }))
        .into_response());
    }

    // ---- 7. Execute: through the gateway's own front door, as the caller.
    let execution = execute_native(
        &state,
        chosen_offer,
        &binding,
        &credential,
        &idempotency_key,
        timeout,
        request.approval_id,
    )
    .await;

    // Audit line for the paid path: who, which key, what it cost — and never
    // the token itself.
    let field_str = |name: &str| {
        execution
            .get(name)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let field_bool = |name: &str| {
        execution
            .get(name)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    tracing::info!(
        token_id = %token.token_id,
        idempotency_key = %idempotency_key,
        request_id = %field_str("request_id"),
        api_slug = %chosen_offer.api_slug,
        status = execution
            .get("status")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        paid = field_bool("paid"),
        replayed = field_bool("replayed"),
        list_price_usdc = %field_str("list_price_usdc"),
        charged_usdc = %field_str("charged_usdc"),
        "/v1/do execution"
    );

    // The gateway's own idempotency verdicts surface as the same 409 they are
    // on the proxy route, so a caller holding the key knows to wait (and keep
    // the key) rather than read a provider error out of `execution.body`.
    if let Some(response) = pending_payment_response(&execution) {
        return Ok(response);
    }
    if let Some(code @ ("in_progress" | "idempotency_key_conflict")) =
        execution.get("error").and_then(Value::as_str)
    {
        return Ok((
            StatusCode::CONFLICT,
            Json(json!({
                "did": false,
                "error": code,
                "retryable": code == "in_progress",
                "message": if code == "in_progress" {
                    "A call with this Idempotency-Key is still running. Nothing was paid twice. Retry with the same key shortly to receive its outcome."
                } else {
                    "This Idempotency-Key was already used for a different call. Use a unique key per job."
                },
                "job": job,
                "decision": decision,
                "request": binding.to_json(),
                "execution": execution,
            })),
        )
            .into_response());
    }

    // When a lookup ran first, the answer has to account for BOTH calls: what
    // each one was, what each cost, and what the job was charged in total.
    // Absent entirely when nothing was resolved, so a plain call's response is
    // byte-identical to what it has always been.
    let (steps_json, combined_json, resolution_json) = if steps.is_empty() {
        (Value::Null, Value::Null, Value::Null)
    } else {
        let mut all = steps;
        all.push(step_json(
            "primary",
            &chosen_offer.api_name,
            chosen_offer,
            &execution,
        ));
        let combined = combined_figures(&all);
        (Value::Array(all), combined, Value::Array(resolution))
    };

    Ok(Json(json!({
        // A paid call went out and the provider answered. Check
        // execution.status for what it answered WITH — a provider 4xx is still
        // an answer, and it is the caller's to read.
        "did": execution.get("paid").and_then(Value::as_bool).unwrap_or(false),
        "job": job,
        "interpretation": plan.to_json(),
        "decision": decision,
        "request": binding.to_json(),
        "execution": execution,
        "steps": steps_json,
        "combined": combined_json,
        "resolution": resolution_json,
    }))
    .into_response())
}

/// Run the endpoint the caller named, with no capability decision in front of
/// it (`candidate_id` without `capability`).
///
/// The same six gates the decided path applies after it has chosen, in the same
/// order and out of the same functions: the price the user confirmed, the
/// funding pre-check, the binding against the endpoint's own published schema,
/// fail-closed on a missing parameter, and the loopback through the proxy that
/// actually settles. What is missing is only the part there is no question
/// about — which endpoint.
///
/// `decision` is still published, because a caller reading the answer should
/// not have to know which path produced it. It says `direct`, names no
/// alternatives and rejects nobody, which is the honest description of a
/// choice that was made before the request arrived.
#[allow(clippy::too_many_arguments)]
async fn run_named_candidate(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    credential: &Credential,
    token: &crate::db::AgentConnectToken,
    request: &DoRequest,
    job: &str,
    candidate_id: Uuid,
) -> Response {
    let offer = match state.db.direct_offer(candidate_id).await {
        Ok(Some(offer)) => offer,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "did": false,
                    "error": "candidate_unknown",
                    "message": "No endpoint in the catalogue has that candidate_id, or its listing is no longer published. Nothing was paid. Ids come from GET /v1/discover, GET /v1/apis and GET /v1/providers/{username}; re-read the listing and use the id it carries now.",
                    "candidate_id": candidate_id,
                })),
            )
                .into_response()
        }
        Err(err) => {
            tracing::error!("direct candidate lookup failed: {}", err);
            // A BODY, not a bare 500, and the reason is money.
            //
            // This failure happens before anything is bound, priced or paid —
            // the catalogue read itself broke. The caller cannot know that from
            // a status code alone: `use-selection` reads a 5xx it cannot
            // classify as `unresolved`, which HOLDS the buyer's reservation for
            // reconciliation rather than giving it back. So a gateway bug that
            // costs nothing to run was charging users for it.
            //
            // `gateway_error` is in the caller's unpaid set: nothing was paid,
            // the reservation goes back, and the sentence says whose fault it
            // is instead of blaming the provider that was never reached.
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "did": false,
                    "error": "gateway_error",
                    "retryable": true,
                    "message": "The gateway could not read this endpoint's listing. Nothing was paid.",
                    "candidate_id": candidate_id,
                })),
            )
                .into_response();
        }
    };

    // The price the user confirmed, checked before anything is bound and long
    // before anything is paid. Two ceilings can apply — the one the user agreed
    // to and a `max_price` constraint — and the lower wins. Same epsilon as the
    // decided path: these are doubles holding six-decimal USDC.
    let ceiling = request
        .max_price_usdc
        .into_iter()
        .chain(request.max_price)
        .fold(f64::INFINITY, f64::min);
    if ceiling.is_finite() && offer.price_usd - ceiling > 1e-9 {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "did": false,
                "error": "price_moved",
                "message": format!(
                    "{} now costs ${:.6}, above the ${ceiling:.6} the user confirmed. Nothing was paid. Show them the new price and ask again.",
                    offer.api_slug, offer.price_usd
                ),
                "job": job,
                "api_slug": offer.api_slug,
                "candidate_id": candidate_id,
                "confirmed_max_price_usdc": request.max_price_usdc,
                "live_price_usdc": offer.price_usd,
            })),
        )
            .into_response();
    }

    // Reports, never enforces — the proxy checks the same rules again at
    // payment time. There is no next-ranked candidate to fall through to here,
    // so an unfundable one is a refusal with the blocker named rather than a
    // bare 402 after the whole chain ran.
    let (funding, funding_json) = funding_for_offer(state, credential, token, &offer).await;
    if matches!(funding, Funding::NotFundable) && !request.dry_run {
        return (
            StatusCode::PAYMENT_REQUIRED,
            Json(json!({
                "did": false,
                "error": "agent_wallet_cannot_fund",
                "message": "This call was checked against your wallets before paying and none of them can fund it. The entry below names the exact blocker. Note that the caps you set on a connect token are limits, not money: the wallet itself must hold USDC (and allowance) on Base. Nothing was paid.",
                "job": job,
                "api_slug": offer.api_slug,
                "candidate_id": candidate_id,
                "funding": funding_json,
            })),
        )
            .into_response();
    }

    // Bound against the endpoint's OWN published parameters. A caller looking
    // at this endpoint's schema sends its names and they are used as they are;
    // a surface rendering the capability's form sends the capability's names,
    // and the offer's `input_map` translates those onto the endpoint's — see
    // `map_capability_inputs`. Endpoint names win either way.
    let schema_map = schemas_for(state, &[&offer]).await;
    let supplied = request.inputs.clone().unwrap_or_default();

    // Values on the request mean someone already answered the question a model
    // would be asked. Fill from them alone, and leave the job-reading path
    // exactly as it was for a caller that sent none — an agent handing over a
    // job in words still gets its parameters read out of it.
    let fill = if supplied.is_empty() && !request.deterministic {
        crate::request_binding::Fill::FromJob
    } else {
        crate::request_binding::Fill::SuppliedOnly
    };
    let binding = bind(
        state,
        job,
        &offer,
        offer.endpoint_id.and_then(|id| schema_map.get(&id)),
        &supplied,
        fill,
    )
    .await;

    let decision = json!({
        "capability": Value::Null,
        "selected_api": offer.api_slug,
        "selected": {
            "candidate_id": offer.endpoint_id,
            "api_slug": offer.api_slug,
            "api_name": offer.api_name,
            "method": offer.endpoint_method,
            "path": offer.endpoint_path,
            "price_usd": offer.price_usd,
            "hosted_externally": offer.hosted_externally,
            "provider": offer.provider_name,
        },
        "reason": "direct: the caller named this endpoint by candidate_id, so no capability was matched and no alternatives were scored. Every price, funding and settlement rule below still applied.",
        "candidates_considered": 1,
        "rejected": [],
        "funding": funding_json,
    });

    if request.dry_run {
        // Never pays, so never runs a resolver: it names the lookup that
        // would run, and what it would cost.
        let pre_steps = planned_pre_steps(state, &offer, None, &binding).await;
        let resolved = pre_step_parameters(&pre_steps);
        return Json(json!({
            "did": false,
            "dry_run": true,
            "message": "Request only — nothing was paid or sent. POST again without dry_run to execute.",
            "job": job,
            "decision": decision,
            // Show what remains missing AFTER the declared pre-steps. The
            // pre-step itself stays visible and priced below; asking the user
            // for a value this plan will resolve is a false blocker.
            "request": binding.to_json_after_resolving(&resolved),
            "pre_steps": if pre_steps.is_empty() { Value::Null } else { Value::Array(pre_steps) },
        }))
        .into_response();
    }

    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .unwrap_or_else(|| format!("do-{}", Uuid::new_v4()));
    let timeout = std::time::Duration::from_millis(
        request
            .timeout_ms
            .unwrap_or(DEFAULT_UPSTREAM_WAIT_MS)
            .clamp(1, MAX_UPSTREAM_WAIT_MS),
    );

    // The same automatic resolver the decided path runs, on the endpoint the
    // caller named, under the same gate — see the note there, and
    // `resolver_applies`. This is the path the app's own runs take, so it is
    // the one where a company name typed into an identifier's box arrives.
    // `fill` follows this path's own rule: a caller that sent values already
    // answered the question a model would be asked.
    let mut binding = binding;
    let mut steps: Vec<Value> = Vec::new();
    let mut resolution: Vec<Value> = Vec::new();
    let resolvers = if request.deterministic {
        Vec::new()
    } else {
        applicable_resolvers(state, &offer, None, &binding).await
    };
    if !resolvers.is_empty() {
        match auto_resolve_inputs(
            state,
            job,
            binding,
            &offer,
            resolvers,
            None,
            credential,
            &idempotency_key,
            timeout,
            ceiling,
            &supplied,
            request.approval_id,
            fill,
        )
        .await
        {
            AutoResolve::NotApplicable(unchanged) => binding = unchanged,
            AutoResolve::Refusal(response) => return *response,
            AutoResolve::Resolved {
                binding: rebound,
                steps: ran,
                resolution: resolved,
            } => {
                binding = rebound;
                steps = ran;
                resolution = resolved;
            }
        }
    }

    if !binding.ready() {
        // Same split as the capability path: a job that does not state what the
        // endpoint needs, versus a binder that never read the job. Only the
        // first is something the caller can fix by filling parameters in.
        let binder_failed = binding.source == crate::request_binding::Source::Unavailable;
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "did": false,
                "error": if binder_failed { "request_not_built" } else { "missing_parameters" },
                // Same distinction as the decided path: a resolver step run
                // ahead of this refusal was still charged, whatever this call
                // itself was.
                "message": if binder_failed {
                    format!(
                        "Apiosk could not turn this job into a request: the step that reads the job and fills the endpoint's parameters did not answer, so the call would have gone out with no parameters at all and bought the provider's error. {} Try again — that step is a live call and fails transiently — or name the values yourself in `inputs`, with the same `candidate_id`. What this endpoint accepts is listed at GET /v1/apis/{}.",
                        if steps.is_empty() { "Nothing was paid." } else { "A lookup step that ran first was still paid — see `steps`." },
                        offer.api_slug
                    )
                } else {
                    format!(
                        "This endpoint requires parameters the request does not carry. {} Retry with those values in `inputs`, carrying the same `candidate_id`.",
                        if steps.is_empty() {
                            "Nothing was paid: sending the call anyway would cost the request price and return an error."
                        } else {
                            "A lookup step that ran first was still paid — see `steps` — even though this call itself was not."
                        }
                    )
                },
                "job": job,
                "retry_with": {
                    "endpoint": "POST /v1/do",
                    "job": job,
                    "candidate_id": offer.endpoint_id,
                    "inputs": binding.missing_required.iter()
                        .map(|p| (p.name.clone(), Value::Null))
                        .collect::<serde_json::Map<_, _>>(),
                    "how": "Fill each null in `inputs` with a value for that parameter, then POST this object. With candidate_id set the retry makes no model call at all: the same endpoint runs and supplied parameters are not re-read out of the job.",
                },
                "decision": decision,
                "request": binding.to_json(),
                "steps": if steps.is_empty() { Value::Null } else { Value::Array(steps.clone()) },
            })),
        )
            .into_response();
    }

    let execution = execute_native(
        state,
        &offer,
        &binding,
        credential,
        &idempotency_key,
        timeout,
        request.approval_id,
    )
    .await;

    let paid = execution
        .get("paid")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    tracing::info!(
        token_id = %token.token_id,
        idempotency_key = %idempotency_key,
        api_slug = %offer.api_slug,
        candidate_id = %candidate_id,
        paid,
        "/v1/do direct execution"
    );

    if let Some(response) = pending_payment_response(&execution) {
        return response;
    }
    if let Some(code @ ("in_progress" | "idempotency_key_conflict")) =
        execution.get("error").and_then(Value::as_str)
    {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "did": false,
                "error": code,
                "retryable": code == "in_progress",
                "message": if code == "in_progress" {
                    "A call with this Idempotency-Key is still running. Nothing was paid twice. Retry with the same key shortly to receive its outcome."
                } else {
                    "This Idempotency-Key was already used for a different call. Use a unique key per job."
                },
                "job": job,
                "decision": decision,
                "request": binding.to_json(),
                "execution": execution,
            })),
        )
            .into_response();
    }

    let (steps_json, combined_json, resolution_json) = if steps.is_empty() {
        (Value::Null, Value::Null, Value::Null)
    } else {
        let mut all = steps;
        all.push(step_json("primary", &offer.api_name, &offer, &execution));
        let combined = combined_figures(&all);
        (Value::Array(all), combined, Value::Array(resolution))
    };

    Json(json!({
        "did": paid,
        "job": job,
        "decision": decision,
        "request": binding.to_json(),
        "execution": execution,
        "steps": steps_json,
        "combined": combined_json,
        "resolution": resolution_json,
    }))
    .into_response()
}

/// Pay for and perform the bound request by calling the gateway's own proxy
/// route in-process, with the caller's connect token.
///
/// Everything that governs money — wallet selection, caps, idempotency,
/// settlement, logging — runs in that path exactly as if the agent had made
/// this request itself, because it effectively has.
async fn execute_native(
    state: &Arc<AppState>,
    offer: &CapabilityOffer,
    binding: &Binding,
    credential: &Credential,
    idempotency_key: &str,
    timeout: std::time::Duration,
    approval_id: Option<Uuid>,
) -> Value {
    let base = format!("http://commerce.internal/{}{}", offer.api_slug, offer.endpoint_path);
    // Path-segment parameters ride between the stored endpoint path and the
    // query string (`/kvk/api/v1/basisprofielen/68750110?...`); the proxy's
    // longest-prefix match resolves them back onto the stored row.
    let base = crate::request_binding::append_path_segments(&base, &binding.path);
    let url = crate::request_binding::with_query(&base, &binding.query);

    let method =
        reqwest::Method::from_bytes(binding.method.as_bytes()).unwrap_or(reqwest::Method::GET);
    let mut call = state
        .http_client
        .request(method, &url)
        .header(credential.header().0, credential.header().1)
        .header("idempotency-key", idempotency_key)
        .header("accept", "application/json")
        .timeout(timeout);
    // Carried to the proxy, where the policy gate lives — the same gate the
    // agent would have hit calling the proxy directly. Redemption belongs beside
    // the caps it has to satisfy, not up here.
    if let Some(approval_id) = approval_id {
        call = call.header("x-apiosk-approval-id", approval_id.to_string());
    }
    // A workflow run the App priced at list + 20% says so again on internal dispatch,
    // beside the same service key; the proxy re-verifies the key before it
    // honours the markup, and a connect-token caller never forwards one.
    if let Some((name, value)) = credential.markup_header() {
        call = call.header(name, value);
    }
    if !binding.body.is_empty() {
        call = call.json(&Value::Object(binding.body.clone()));
    }

    let response = match crate::in_process::send(state, call).await {
        Ok(r) => r,
        Err(err) => {
            // No response means no forward and no settlement — the prepaid
            // debit happens on the paid path itself, which never ran to
            // completion. Reported as unpaid; the wallet page is authoritative.
            tracing::warn!("/v1/do upstream call failed before a response: {}", err);
            return json!({
                "paid": false,
                "error": "upstream_unreachable",
                "message": format!("The call could not be completed: {err}. Nothing settled; check your wallet activity in the buyer portal if in doubt."),
                "idempotency_key": idempotency_key,
            });
        }
    };

    let status = response.status().as_u16();
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let content_type = header("content-type");
    let request_id = header("x-request-id");
    let payment_rail = header("x-apiosk-payment-rail");
    let wallet_used = header("x-apiosk-wallet-used");
    let tx_hash = header("x-apiosk-tx-hash");
    let replayed = header("x-apiosk-idempotent-replay").as_deref() == Some("true");
    // The list price the proxy actually settled, when it settled one. It can
    // differ from the offer's price: a parameter pricing rule on the endpoint
    // re-prices the call at the proxy, and that is the number the wallet was
    // debited for, so it is the number the figures below must start from.
    let settled_list_usd = header("x-apiosk-price-listed");

    let bytes = response.bytes().await.unwrap_or_default();
    let truncated = bytes.len() > MAX_RESPONSE_BYTES;
    let slice = &bytes[..bytes.len().min(MAX_RESPONSE_BYTES)];
    let body: Value = match serde_json::from_slice(slice) {
        Ok(v) => v,
        Err(_) => Value::String(String::from_utf8_lossy(slice).into_owned()),
    };

    // The gateway's own verdicts that never reach a provider. A 409 or 429
    // from the paid path is the gateway refusing to run the call — a key in
    // flight, a key reused, a token over its limit — and a provider's own 409
    // is told apart by its body: the gateway's carries one of these codes.
    let gateway_error = body
        .get("error")
        .and_then(Value::as_str)
        .filter(|code| {
            matches!(
                *code,
                "in_progress" | "idempotency_key_conflict" | "rate_limited"
            ) && (status == 409 || status == 429)
        })
        .map(str::to_string);

    // `paid` must answer exactly one question: did the proxy settle? So it is
    // computed on the proxy's own settlement condition and nothing else —
    // `proxy::wallet::should_settle_deferred_api_payment`, which is
    // `status.is_success()`. Every rail charges on 2xx and only on 2xx.
    //
    // This used to be `status != 402 && status < 500`, which called a call paid
    // whenever the provider answered 4xx. The proxy had already declined to
    // settle those — that gate was tightened for exactly this reason, so a
    // 400/401/403/404 from a misconfigured upstream would stop taking the
    // buyer's money for an error — but /v1/do went on reporting `paid: true`,
    // and the caller that trusts this field (app/supabase/functions/_shared/
    // outcome.ts) therefore skipped its refund branch and left the buyer's
    // balance debited for a call nobody was paid for. A KVK search rejected
    // with IPD1998 stranded EUR 0.02 that way, under a screen reading
    // "Nothing has been charged".
    //
    // A 402 and a 5xx are still unpaid, and still are: neither is 2xx.
    let paid = (200..300).contains(&status) && gateway_error.is_none();

    // The figures a caller settles against (contract C4): the provider's list
    // price, the 15% markup (20% on a service-key workflow run) rounded up to
    // the atomic unit, and their sum — the
    // one number that actually left the wallet. All three come from
    // `fees::quote`, the single source of that arithmetic, with the offer's
    // provider class. `charged_usdc` is zero unless the provider was paid.
    let figures = execution_figures(
        offer,
        settled_list_usd.as_deref(),
        paid,
        credential.buyer_markup_bps(),
    );

    json!({
        "paid": paid,
        "replayed": replayed,
        "error": gateway_error,
        "status": status,
        "content_type": content_type,
        "body": body,
        "truncated": truncated,
        "settlement": "apiosk-proxied",
        "price_usd": offer.price_usd,
        "list_price_usdc": figures.list_price_usdc,
        "fee_usdc": figures.fee_usdc,
        "charged_usdc": figures.charged_usdc,
        "payment_rail": payment_rail,
        "wallet_used": wallet_used,
        "tx_hash": tx_hash,
        "request_id": request_id,
        "idempotency_key": idempotency_key,
        // The note has to follow `paid`, not the status class. It used to be
        // keyed on the status alone, so every 5xx claimed "the provider was
        // paid" while the `paid` field one line above said false — and a caller
        // deciding whether a retry costs money twice reads the sentence, not the
        // flag. A gateway-side 503 (a rail that is not configured) spends
        // nothing and is safe to retry; saying otherwise sends a recoverable
        // failure to a human.
        "note": if body.get("error").and_then(Value::as_str) == Some("payment_unresolved") {
            "Provider payment is unconfirmed. Keep the existing execution for reconciliation; do not retry with a new key."
        } else if status == 402 {
            "The gateway declined to pay: the wallet is at a cap you set, unfunded, or this rail is unavailable. The body carries the exact reason. Your limits did their job — nothing was spent."
        } else if !paid {
            "The gateway could not carry this call, so nothing was paid. The body carries the exact reason. Retrying costs nothing extra."
        } else if !(200..300).contains(&(status as u32)) {
            "The provider was paid and answered with an error status. The body is the provider's own answer."
        } else {
            "Paid and answered. The body is the provider's response, unmodified."
        },
    })
}

/// Preserve the proxy's uncertain payment verdict at the HTTP boundary. A
/// wrapped HTTP 200 + paid:false would otherwise become a refund/new purchase
/// in the App. Its existing 5xx handling retains the reservation instead.
fn pending_payment_response(execution: &Value) -> Option<Response> {
    (execution.get("body")?.get("error")?.as_str()? == "payment_unresolved").then(|| {
        (StatusCode::GATEWAY_TIMEOUT, Json(json!({
            "did": false, "error": "payment_unresolved",
            "message": "Provider payment confirmation is pending. Keep this execution for reconciliation; do not make a new purchase.",
            "execution": execution,
        }))).into_response()
    })
}

/// Outcome of the automatic input resolver.
///
/// The mechanism (declarations in `param_resolvers`, pure helpers in
/// `crate::param_resolver`): when the bound request cannot carry a required
/// identifier a declared resolver endpoint can produce — a KVK profile call
/// with no `kvkNummer`, or with a company NAME sitting in it — the resolver
/// runs FIRST, paid and settled through the same loopback as any other call,
/// the identifier is extracted from its response, and the primary request is
/// re-bound with the value supplied. Every refusal is fully assembled here so
/// both `/v1/do` paths (decided and named-candidate) speak one shape.
enum AutoResolve {
    /// No declared resolver applies — the caller sees exactly today's 422.
    NotApplicable(Binding),
    /// A refusal to return as-is: combined price over the ceiling (nothing
    /// paid), resolver failure, zero hits, or an ambiguous hit list (search
    /// leg paid, named in `steps`).
    Refusal(Box<Response>),
    /// The identifier was resolved and the primary request re-bound.
    Resolved {
        binding: Binding,
        /// One executed resolver step per resolved parameter, in run order.
        steps: Vec<Value>,
        /// What was chosen and what else matched, per parameter.
        resolution: Vec<Value>,
    },
}

/// Parse a fixed 6-decimal USDC string back into atomic units. The inverse of
/// `usdc_6dp`, for summing step figures without floats.
fn atomic_from_6dp(s: &str) -> i128 {
    let mut parts = s.trim().splitn(2, '.');
    let whole: i128 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let frac_raw = parts.next().unwrap_or("0");
    let frac: i128 = format!("{frac_raw:0<6}")
        .chars()
        .take(6)
        .collect::<String>()
        .parse()
        .unwrap_or(0);
    whole * crate::fees::USDC_SCALE + frac
}

/// One step entry for the `steps` array, from an execution's own figures.
fn step_json(role: &str, label: &str, offer: &CapabilityOffer, execution: &Value) -> Value {
    let s = |name: &str| {
        execution
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    json!({
        "role": role,
        "label": label,
        "api_slug": offer.api_slug,
        "endpoint": format!("{} {}", offer.endpoint_method, offer.endpoint_path),
        "list_price_usdc": s("list_price_usdc"),
        "fee_usdc": s("fee_usdc"),
        "charged_usdc": s("charged_usdc"),
        "status": execution.get("status").cloned().unwrap_or(Value::Null),
        "paid": execution.get("paid").cloned().unwrap_or(Value::Bool(false)),
        "request_id": execution.get("request_id").cloned().unwrap_or(Value::Null),
        "executed": true,
    })
}

/// The combined money figures over every executed step, summed in atomic
/// units, never floats.
fn combined_figures(steps: &[Value]) -> Value {
    let sum = |field: &str| {
        steps
            .iter()
            .filter_map(|s| s.get(field).and_then(Value::as_str))
            .map(atomic_from_6dp)
            .sum::<i128>()
    };
    let list = sum("list_price_usdc");
    let fee = sum("fee_usdc");
    let charged = sum("charged_usdc");
    json!({
        "buyer_total_usdc": usdc_6dp(list + fee),
        "charged_usdc": usdc_6dp(charged),
        // The two legs the total is made of, published for the same reason a
        // single execution publishes them: whatever settles this job has to
        // split what was charged into what the provider got and what Apiosk
        // kept, and summing them here is the only place both are known.
        "list_price_usdc": usdc_6dp(list),
        "fee_usdc": usdc_6dp(fee),
    })
}

/// The resolver declarations that apply to this binding: active rows for the
/// endpoint or capability whose `target_param` the binder reported missing,
/// each joined with its (catalogue-visible) resolver offer.
async fn applicable_resolvers(
    state: &Arc<AppState>,
    offer: &CapabilityOffer,
    capability: Option<&str>,
    binding: &Binding,
) -> Vec<(crate::db::ParamResolver, CapabilityOffer)> {
    let rows = match state
        .db
        .resolvers_for(offer.endpoint_id, capability.filter(|c| !c.is_empty()))
        .await
    {
        Ok(rows) => rows,
        Err(err) => {
            // Fail open to today's behaviour: the caller gets the plain 422.
            tracing::warn!("param resolver lookup failed: {err}");
            return Vec::new();
        }
    };
    let mut joined = Vec::new();
    for row in rows {
        if !resolver_applies(&row, binding) {
            continue;
        }
        match state.db.direct_offer(row.resolver_endpoint_id).await {
            Ok(Some(resolver_offer)) => joined.push((row, resolver_offer)),
            Ok(None) => {
                // The resolver endpoint is not (or no longer) publishable —
                // a declaration pointing at it must not run it.
                tracing::warn!(
                    "param resolver {} points at a non-visible endpoint {}",
                    row.id,
                    row.resolver_endpoint_id
                );
            }
            Err(err) => tracing::warn!("param resolver offer lookup failed: {err}"),
        }
    }
    joined
}

/// Whether one declaration applies to this binding.
///
/// Two ways, and the second is why a KVK job used to fail with the provider's
/// own error:
///
///   * The binder reported the parameter MISSING. The original trigger, and
///     the common one — the job names a company and nothing filled the number.
///
///   * Something bound a value the declaration says cannot BE the identifier.
///     "de kvk informatie van Mollie BV" has one candidate value in it and one
///     field to put it in, so the company name lands in `kvkNummer`; a model
///     that will not guess writes the literal `<UNKNOWN>` there instead
///     (`sanitise` now drops that one, but the general case remains). Neither
///     is a registration number, and sending either upstream buys a
///     guaranteed provider refusal — IPD0004, "Het KVK-nummer Mollie BV is
///     niet valide". It is, however, exactly the search term the resolver
///     wants, and `auto_resolve_inputs` moves it there (see
///     `unusable_bound_value`) rather than letting it travel as a malformed
///     identifier.
///
/// Both call sites gate the resolver on THIS function rather than on
/// `Binding::ready`, because the second way binds ready. That was the bug: the
/// quote promised a lookup, the run skipped it, and the buyer read the
/// provider's Dutch complaint about a number nobody typed.
///
/// Only ever on a DECLARED shape: with `target_shape` null this is the old
/// missing-only rule, unchanged.
fn resolver_applies(row: &crate::db::ParamResolver, binding: &Binding) -> bool {
    if binding
        .missing_required
        .iter()
        .any(|p| p.name == row.target_param)
    {
        return true;
    }
    binding
        .bound(&row.target_param)
        .is_some_and(|value| !crate::param_resolver::shape_fits(row.target_shape.as_deref(), value))
}

/// The value sitting in the identifier's place that cannot BE that identifier
/// — "Mollie BV" in `kvkNummer`.
///
/// Not an answer, and not noise either: it is the thing somebody wanted looked
/// up, written in the only box the form had. It is kept out of both requests
/// (see [`unusable_bound_params`]) and handed to the resolver as its search
/// term instead.
fn unusable_bound_value<'a>(
    row: &crate::db::ParamResolver,
    binding: &'a Binding,
) -> Option<&'a Value> {
    binding
        .bound(&row.target_param)
        .filter(|value| !crate::param_resolver::shape_fits(row.target_shape.as_deref(), value))
}

/// Every parameter a declaration would resolve whose currently bound value
/// cannot be that identifier — the values that must not survive into either
/// the resolver's request or the primary's.
fn unusable_bound_params(
    resolvers: &[(crate::db::ParamResolver, CapabilityOffer)],
    binding: &Binding,
) -> Vec<String> {
    resolvers
        .iter()
        .filter(|(row, _)| unusable_bound_value(row, binding).is_some())
        .map(|(row, _)| row.target_param.clone())
        .collect()
}

/// Planned (unexecuted) pre-steps for a dry run: which resolvers WOULD run,
/// at what buyer price, without paying anything.
async fn planned_pre_steps(
    state: &Arc<AppState>,
    offer: &CapabilityOffer,
    capability: Option<&str>,
    binding: &Binding,
) -> Vec<Value> {
    applicable_resolvers(state, offer, capability, binding)
        .await
        .into_iter()
        .map(|(row, resolver_offer)| {
            json!({
                "role": "resolver",
                "label": row.label,
                "parameter": row.target_param,
                // See `param_resolver::pre_steps_for`: the confirm surface
                // tests what the user types against this.
                "shape": row.target_shape,
                // See `param_resolver::pre_steps_for` — the upstream leg,
                // published because a ceiling is compared in list prices.
                "price_usd": resolver_offer.price_usd,
                "api_slug": resolver_offer.api_slug,
                "endpoint": format!("{} {}", resolver_offer.endpoint_method, resolver_offer.endpoint_path),
                "buyer_price_usd": crate::fees::buyer_total_usd(
                    resolver_offer.price_usd,
                    resolver_offer.hosted_externally
                ),
                "executed": false,
            })
        })
        .collect()
}

/// Target parameters a dry-run pre-step will supply.
fn pre_step_parameters(pre_steps: &[Value]) -> Vec<String> {
    pre_steps
        .iter()
        .filter_map(|step| step.get("parameter").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

fn resolver_plan_is_bounded(
    resolvers: &[(crate::db::ParamResolver, CapabilityOffer)],
    ceiling: f64,
) -> bool {
    ceiling.is_finite()
        && ceiling > 0.0
        && resolvers.len() < crate::plan::compile::Limits::default().max_paid_nodes
        && resolvers.iter().all(|(r, o)| {
            o.price_usd.is_finite()
                && o.price_usd > 0.0
                && !r.target_param.trim().is_empty()
                && !r.resolver_input_param.trim().is_empty()
                && !r.extraction_path.trim().is_empty()
        })
}

/// Run the automatic resolver for the declarations that apply to this
/// binding. See [`AutoResolve`] for the contract; the caller decides whether
/// anything applies (via [`applicable_resolvers`]) and passes the list in, so
/// the gate and the run can never disagree about it. The primary call is
/// never made from here.
#[allow(clippy::too_many_arguments)]
async fn auto_resolve_inputs(
    state: &Arc<AppState>,
    job: &str,
    binding: Binding,
    offer: &CapabilityOffer,
    resolvers: Vec<(crate::db::ParamResolver, CapabilityOffer)>,
    capability: Option<&str>,
    credential: &Credential,
    idempotency_key: &str,
    timeout: std::time::Duration,
    ceiling: f64,
    supplied: &serde_json::Map<String, Value>,
    approval_id: Option<Uuid>,
    fill: crate::request_binding::Fill,
) -> AutoResolve {
    if resolvers.is_empty() {
        return AutoResolve::NotApplicable(binding);
    }

    if !resolver_plan_is_bounded(&resolvers, ceiling) {
        return AutoResolve::Refusal(Box::new((StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"did":false,"error":"resolver_plan_invalid","message":"The dependency calls need explicit mappings, verifiable prices and a finite combined ceiling before execution. Request a research plan or supply the required identifiers."}))).into_response()));
    }

    // ---- Combined ceiling, before anything is paid.
    //
    // Compared in LIST prices, because that is what a `max_price` /
    // `max_price_usdc` on this endpoint has always meant (see the single-call
    // check at step 4a, and `buildGatewayRequest` in the app, which sends the
    // selection's provider leg). Reading the same field in a different basis
    // here would refuse a call the caller did agree to pay for — a 0.09 buyer
    // total against a 0.078 list ceiling is the same purchase, priced twice.
    //
    // What matters is that the ceiling bounds the WHOLE job rather than only
    // its last step: a lookup the gateway decided to run is spend the caller
    // did not separately confirm, so it counts against the ceiling like any
    // other. The buyer figures are reported alongside so the refusal can be
    // read in the currency the user was actually shown.
    let primary_list = offer.price_usd;
    let resolver_list: f64 = resolvers.iter().map(|(_, o)| o.price_usd).sum();
    let combined_list = primary_list + resolver_list;
    let markup_bps = credential.buyer_markup_bps();
    let combined_buyer =
        crate::fees::buyer_total_usd_with_markup(primary_list, offer.hosted_externally, markup_bps)
            + resolvers
                .iter()
                .map(|(_, o)| {
                    crate::fees::buyer_total_usd_with_markup(
                        o.price_usd,
                        o.hosted_externally,
                        markup_bps,
                    )
                })
                .sum::<f64>();
    if ceiling.is_finite() && combined_list - ceiling > 1e-9 {
        return AutoResolve::Refusal(Box::new(
            (
                StatusCode::CONFLICT,
                Json(json!({
                    "did": false,
                    "error": "combined_price_above_ceiling",
                    "message": format!(
                        "This job needs a lookup step before the main call ({}), and together they are above the ${ceiling:.6} ceiling this request set. Nothing was paid. The combined price is ${combined_buyer:.6} all in; confirm it and retry with the ceiling raised, or supply the identifier yourself in `inputs` so no lookup is needed.",
                        resolvers
                            .iter()
                            .map(|(r, o)| format!("{} ${:.6}", r.label, crate::fees::buyer_total_usd_with_markup(o.price_usd, o.hosted_externally, markup_bps)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    "job": job,
                    "api_slug": offer.api_slug,
                    // Both bases, named for what they are: the ceiling is
                    // compared against the list figures, and the buyer figures
                    // are what the user was shown.
                    "combined_price_usdc": combined_list,
                    "combined_buyer_price_usdc": combined_buyer,
                    "confirmed_max_price_usdc": if ceiling.is_finite() { Value::from(ceiling) } else { Value::Null },
                })),
            )
                .into_response(),
        ));
    }

    let mut supplied = supplied.clone();
    // A value that cannot be the identifier is removed before anything binds
    // against it. It would otherwise travel twice, both times as a
    // malformed identifier: into the resolver's own request (KVK Zoeken takes
    // a `kvkNummer` of its own, so "Mollie BV" would land there and be
    // refused) and, through `apply_supplied` winning over the model, back into
    // the primary the moment the resolver produced the real value. The words
    // themselves are not lost: they are handed to the resolver as its search
    // term a few lines below.
    for name in unusable_bound_params(&resolvers, &binding) {
        supplied.remove(&name);
    }
    let mut steps: Vec<Value> = Vec::new();
    let mut resolution: Vec<Value> = Vec::new();
    let mut prepared = Vec::new();

    for (row, resolver_offer) in &resolvers {
        // What to search for, when something already said it.
        //
        // The value stripped above is the search term, and it is a better one
        // than the job: it is what the person typed into the box the form put
        // in front of them, and on a form whose field was edited after the job
        // was written it is the only place the intended company appears. It
        // rides in as a supplied input, which wins inside `bind`; everything
        // else the resolver documents is still read out of the job.
        let mut resolver_supplied = supplied.clone();
        if let Some(term) = unusable_bound_value(row, &binding) {
            resolver_supplied.insert(row.resolver_input_param.clone(), term.clone());
        }

        // Bind the resolver endpoint from the job (or the caller's own
        // inputs, which win inside `bind` exactly as they do on the primary).
        let schema_map = schemas_for(state, &[resolver_offer]).await;
        let resolver_binding = bind(
            state,
            job,
            resolver_offer,
            resolver_offer
                .endpoint_id
                .and_then(|id| schema_map.get(&id)),
            &resolver_supplied,
            crate::request_binding::Fill::RequiredFromJob,
        )
        .await;
        if !resolver_binding.ready() {
            // The job names neither the identifier nor anything the resolver
            // can search by. Fall through to the plain 422, which lists the
            // identifier itself as missing.
            return AutoResolve::Refusal(Box::new((StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"did":false,"error":"resolver_inputs_missing","message":"A dependency is missing required inputs. Nothing was paid."}))).into_response()));
        }
        prepared.push((row, resolver_offer, resolver_binding));
    }
    for (row, resolver_offer, resolver_binding) in prepared {
        let searched = resolver_binding
            .query
            .get(&row.resolver_input_param)
            .or_else(|| resolver_binding.body.get(&row.resolver_input_param))
            .or_else(|| {
                resolver_binding
                    .path
                    .iter()
                    .find(|(name, _)| name == &row.resolver_input_param)
                    .map(|(_, v)| v)
            })
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        // Paid first, through the normal proxy path. The derived idempotency
        // key makes a caller's retry replay this leg instead of paying twice.
        let resolver_key = format!("{idempotency_key}-resolve-{}", row.target_param);
        let execution = execute_native(
            state,
            resolver_offer,
            &resolver_binding,
            credential,
            &resolver_key,
            timeout,
            approval_id,
        )
        .await;
        let paid = execution
            .get("paid")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let status = execution.get("status").and_then(Value::as_u64).unwrap_or(0);
        let step = step_json("resolver", &row.label, resolver_offer, &execution);

        // KVK answers an unknown value with 404/IPD5200 — that is "zero
        // hits", not a failure. Everything else non-2xx (or unpaid) is.
        let zero_hits = status == 404;
        if !paid || (!(200..300).contains(&status) && !zero_hits) {
            steps.push(step);
            return AutoResolve::Refusal(Box::new(
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({
                        "did": false,
                        "error": "resolver_failed",
                        "message": format!(
                            "The {} step this job needs did not complete, so the main call was not made. `steps` carries exactly what ran and what was charged{}. Retry, or supply `{}` yourself in `inputs`.",
                            row.label,
                            if paid { "" } else { " (nothing was paid)" },
                            row.target_param
                        ),
                        "job": job,
                        "steps": steps,
                        "retry_with": retry_with_inputs(job, capability, offer, &[(row.target_param.clone(), Value::Null)]),
                    })),
                )
                    .into_response(),
            ));
        }

        let body = execution.get("body").cloned().unwrap_or(Value::Null);
        let (value, chosen, alternatives, candidates) = if let Some(list_path) =
            row.alternatives_path.as_deref()
        {
            let hits = if zero_hits {
                Vec::new()
            } else {
                crate::param_resolver::extract(list_path, &body)
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            };
            let hits = crate::param_resolver::hits_that_carry(hits, &row.extraction_path);
            match crate::param_resolver::choose_hit(&hits, &searched, row.match_fields.as_ref()) {
                crate::param_resolver::HitChoice::Chosen(index) => {
                    let hit = &hits[index];
                    let value = crate::param_resolver::extract(&row.extraction_path, hit).cloned();
                    let chosen_name = row
                        .match_fields
                        .as_ref()
                        .and_then(|f| f.get("name"))
                        .and_then(Value::as_str)
                        .and_then(|field| hit.get(field))
                        .cloned()
                        .unwrap_or(Value::Null);
                    (value, chosen_name, hits.len().saturating_sub(1), Vec::new())
                }
                crate::param_resolver::HitChoice::Empty => (None, Value::Null, 0, Vec::new()),
                crate::param_resolver::HitChoice::Ambiguous => {
                    let candidates = crate::param_resolver::candidate_summaries(
                        &hits,
                        &row.extraction_path,
                        row.match_fields.as_ref(),
                    );
                    (None, Value::Null, hits.len(), candidates)
                }
            }
        } else if zero_hits {
            (None, Value::Null, 0, Vec::new())
        } else {
            (
                crate::param_resolver::extract(&row.extraction_path, &body).cloned(),
                Value::Null,
                0,
                Vec::new(),
            )
        };

        if !candidates.is_empty() {
            // More than one defensible identity. The search leg IS paid —
            // shown honestly in `steps` — and the caller picks; guessing a
            // registry identity wrong is worse than one confirmation round.
            steps.push(step);
            let choose: Vec<Value> = candidates
                .iter()
                .filter_map(|c| c.get("value").cloned())
                .collect();
            return AutoResolve::Refusal(Box::new(
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({
                        "did": false,
                        "error": "resolver_ambiguous",
                        "message": format!(
                            "The {} step found {alternatives} registrations matching \"{searched}\" and none is a clear winner. The lookup ran and was charged (see steps). Pick the intended one from `candidates` and retry with its value in `inputs.{}`.",
                            row.label, row.target_param
                        ),
                        "job": job,
                        // The term itself, beside the sentence that embeds it:
                        // a surface writing its own copy should not have to
                        // parse this message to name what was searched for.
                        "searched": searched,
                        "steps": steps,
                        "candidates": candidates,
                        "retry_with": retry_with_inputs(job, capability, offer, &[(
                            row.target_param.clone(),
                            json!({
                                "choose_one_of": choose,
                                "hint": format!("the {} of the intended registration, from `candidates`", row.target_param),
                            }),
                        )]),
                    })),
                )
                    .into_response(),
            ));
        }

        let Some(value) = value else {
            steps.push(step);
            return AutoResolve::Refusal(Box::new(
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({
                        "did": false,
                        "error": "resolver_no_match",
                        "message": format!(
                            "The {} step found nothing for \"{searched}\", so the main call was not made. The lookup itself ran and was charged (see steps). Check the name, or supply `{}` yourself in `inputs`.",
                            row.label, row.target_param
                        ),
                        "job": job,
                        // See the ambiguous refusal above: named, not embedded.
                        "searched": searched,
                        "steps": steps,
                        "retry_with": retry_with_inputs(job, capability, offer, &[(row.target_param.clone(), Value::Null)]),
                    })),
                )
                    .into_response(),
            ));
        };

        resolution.push(json!({
            "parameter": row.target_param,
            "value": value,
            "chosen_name": chosen,
            "alternatives": alternatives,
            "resolver": { "api_slug": resolver_offer.api_slug, "label": row.label },
        }));
        steps.push(step);
        supplied.insert(row.target_param.clone(), value);
    }

    if steps.is_empty() {
        // Every applicable resolver failed to bind an input of its own — the
        // job says nothing searchable and the binder is unavailable, since the
        // value that triggered this is handed over as the search term above.
        // Today's behaviour is the fallback, deliberately: this is the one
        // remaining case where a malformed identifier still reaches the
        // provider, and a refusal here would be this module deciding, without
        // a lookup, that a value the endpoint might well accept cannot work.
        return AutoResolve::NotApplicable(binding);
    }

    // Re-bind the primary with the resolved value(s) supplied. The supplied
    // map wins inside `bind`, and everything else is read from the job again
    // by the same machinery as the first pass.
    let schema_map = schemas_for(state, &[offer]).await;
    let binding = bind(
        state,
        job,
        offer,
        offer.endpoint_id.and_then(|id| schema_map.get(&id)),
        &supplied,
        fill,
    )
    .await;

    AutoResolve::Resolved {
        binding,
        steps,
        resolution,
    }
}

/// The `retry_with` envelope every resolver refusal carries — same shape as
/// the plain missing-parameters 422, with the inputs pre-named.
fn retry_with_inputs(
    job: &str,
    capability: Option<&str>,
    offer: &CapabilityOffer,
    inputs: &[(String, Value)],
) -> Value {
    let mut envelope = serde_json::Map::new();
    envelope.insert("endpoint".into(), json!("POST /v1/do"));
    envelope.insert("job".into(), json!(job));
    if let Some(capability) = capability.filter(|c| !c.is_empty()) {
        envelope.insert("capability".into(), json!(capability));
    }
    envelope.insert("candidate_id".into(), json!(offer.endpoint_id));
    envelope.insert(
        "inputs".into(),
        Value::Object(inputs.iter().cloned().collect()),
    );
    envelope.insert(
        "how".into(),
        json!("Fill `inputs` with real values, then POST this object. With candidate_id set the same endpoint runs and supplied parameters are not re-read out of the job."),
    );
    Value::Object(envelope)
}

/// The three money figures on an execution, as 6-decimal USDC strings.
struct ExecutionFigures {
    list_price_usdc: String,
    fee_usdc: String,
    charged_usdc: String,
}

/// Price the executed call through [`crate::fees::quote`].
///
/// `settled_list_usd` is the proxy's `X-Apiosk-Price-Listed` header — the list
/// price it actually settled — and wins over the offer's price when present;
/// the offer's price is the fallback for a call that never reached settlement.
/// The class follows the offer: a federated listing pays its provider the full
/// list, a managed one pays list minus the provider fee, and the buyer side is
/// list + 15% either way — or list + 20% when `buyer_markup_bps` is the
/// workflow markup a verified service key asked for.
fn execution_figures(
    offer: &CapabilityOffer,
    settled_list_usd: Option<&str>,
    paid: bool,
    buyer_markup_bps: i128,
) -> ExecutionFigures {
    use std::str::FromStr as _;
    let list = settled_list_usd
        .and_then(|s| bigdecimal::BigDecimal::from_str(s.trim()).ok())
        .or_else(|| bigdecimal::BigDecimal::try_from(offer.price_usd).ok())
        .unwrap_or_default();
    let class = crate::fees::ProviderClass::from_listing_type(Some(if offer.hosted_externally {
        "federated"
    } else {
        "managed"
    }));
    let quote = crate::fees::quote_usdc_with_markup(&list, class, buyer_markup_bps);
    ExecutionFigures {
        list_price_usdc: usdc_6dp(quote.list_atomic),
        fee_usdc: usdc_6dp(quote.buyer_markup_atomic),
        charged_usdc: usdc_6dp(if paid { quote.buyer_total_atomic } else { 0 }),
    }
}

/// Atomic USDC as a fixed 6-decimal string — the wire shape a settlement
/// reads with a decimal parser, never a float.
fn usdc_6dp(atomic: i128) -> String {
    let atomic = atomic.max(0);
    format!(
        "{}.{:06}",
        atomic / crate::fees::USDC_SCALE,
        atomic % crate::fees::USDC_SCALE
    )
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({ "did": false, "error": code, "message": message })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn uncertain_provider_payment_remains_a_reconciliation_response() {
        let execution = serde_json::json!({"paid":false,"status":504,"request_id":"request-one","body":{"error":"payment_unresolved"}});
        let response = super::pending_payment_response(&execution).unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::GATEWAY_TIMEOUT);
        let bytes = axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], "payment_unresolved");
        assert_eq!(body["execution"], execution);
        assert!(super::pending_payment_response(
            &serde_json::json!({"body":{"error":"upstream_declined"}})
        )
        .is_none());
    }

    use super::*;
    use crate::request_binding::{Location, Param, Source};

    /// The KVK declaration, as `param_resolvers` holds it: the profile call
    /// wants an eight-digit `kvkNummer`, and the search leg takes a `naam`.
    fn kvk_declaration() -> crate::db::ParamResolver {
        crate::db::ParamResolver {
            id: Uuid::nil(),
            target_param: "kvkNummer".into(),
            resolver_endpoint_id: Uuid::nil(),
            resolver_input_param: "naam".into(),
            resolver_input_hint: None,
            extraction_path: "kvkNummer".into(),
            alternatives_path: Some("resultaten".into()),
            match_fields: None,
            target_shape: Some("digits:8".into()),
            label: "KVK company lookup".into(),
            endpoint_level: true,
        }
    }

    fn empty_binding() -> Binding {
        Binding {
            method: "GET".into(),
            url: Some("https://api.kvk.nl/api/v1/basisprofielen".into()),
            query: serde_json::Map::new(),
            body: serde_json::Map::new(),
            path: Vec::new(),
            fixed: serde_json::Map::new(),
            missing_required: Vec::new(),
            not_bindable: Vec::new(),
            supplied: Vec::new(),
            ignored_inputs: Vec::new(),
            source: Source::Bound,
            model: None,
        }
    }

    /// A binding that carries `kvkNummer` — as the app's own form produces it.
    fn binding_carrying(value: Value) -> Binding {
        let mut binding = empty_binding();
        binding.path.push(("kvkNummer".into(), value));
        binding
    }

    fn binding_missing(name: &str) -> Binding {
        let mut binding = empty_binding();
        binding.missing_required.push(Param {
            name: name.into(),
            location: Location::Path,
            ty: "string",
            required: true,
            required_one_of: None,
            description: None,
            enum_values: None,
            format: None,
            pattern: None,
        });
        binding
    }

    #[test]
    fn a_declaration_applies_when_nothing_filled_the_identifier_in() {
        assert!(resolver_applies(
            &kvk_declaration(),
            &binding_missing("kvkNummer")
        ));
    }

    #[test]
    fn a_declaration_applies_to_a_company_name_in_the_identifiers_place() {
        // THE SCREENSHOT. One box on the form, a company name typed into it,
        // a binding that is `ready` — and a lookup the quote had already
        // promised. Gating on readiness sent "Mollie BV" to KVK as a
        // registration number and bought its refusal.
        let binding = binding_carrying(json!("Mollie BV"));
        assert!(
            binding.ready(),
            "the case only bites when the binding binds"
        );
        assert!(resolver_applies(&kvk_declaration(), &binding));
    }

    #[test]
    fn a_real_registration_number_needs_no_lookup() {
        let binding = binding_carrying(json!("30204462"));
        assert!(!resolver_applies(&kvk_declaration(), &binding));
        // Unquoted, and from a declaration that names no shape, likewise.
        assert!(!resolver_applies(
            &kvk_declaration(),
            &binding_carrying(json!(30204462))
        ));
        let mut undeclared = kvk_declaration();
        undeclared.target_shape = None;
        assert!(!resolver_applies(&undeclared, &binding));
    }

    #[test]
    fn the_name_in_the_identifiers_place_is_the_search_term() {
        // Both halves of what happens to that value: it is kept out of the
        // primary's request, and it is what the lookup searches for.
        let binding = binding_carrying(json!("Mollie BV"));
        let row = kvk_declaration();
        assert_eq!(
            unusable_bound_value(&row, &binding),
            Some(&json!("Mollie BV"))
        );
        assert_eq!(
            unusable_bound_value(&row, &binding_carrying(json!("30204462"))),
            None
        );
    }

    #[test]
    fn buyer_totals_are_the_price_the_confirm_surface_shows() {
        // KVK's list prices are chosen so the buyer pays exact round cents.
        // If this drifts, the confirm button and the wallet disagree.
        assert!((crate::fees::buyer_total_usd(0.017391, false) - 0.02).abs() < 1e-9);
        assert!((crate::fees::buyer_total_usd(0.060869, false) - 0.07).abs() < 1e-9);
        assert!((crate::fees::buyer_total_usd(0.043478, false) - 0.05).abs() < 1e-9);
    }

    #[test]
    fn six_dp_usdc_round_trips_through_atomic_units() {
        for s in ["0.020000", "0.070000", "1.234567", "0.000001", "12.000000"] {
            assert_eq!(usdc_6dp(atomic_from_6dp(s)), s, "round trip for {s}");
        }
        // Short and absent fractions are read as written, not as garbage.
        assert_eq!(atomic_from_6dp("0.02"), 20_000);
        assert_eq!(atomic_from_6dp("3"), 3_000_000);
        assert_eq!(atomic_from_6dp(""), 0);
    }

    #[test]
    fn combined_figures_sum_every_step_in_atomic_units() {
        // The KVK case the confirm surface renders: 0.02 lookup + 0.07 profile
        // = 0.09 on the button, summed as integers rather than floats.
        let steps = vec![
            json!({"list_price_usdc": "0.017391", "fee_usdc": "0.002609", "charged_usdc": "0.020000"}),
            json!({"list_price_usdc": "0.060869", "fee_usdc": "0.009131", "charged_usdc": "0.070000"}),
        ];
        let combined = combined_figures(&steps);
        assert_eq!(combined["buyer_total_usdc"], json!("0.090000"));
        assert_eq!(combined["charged_usdc"], json!("0.090000"));
    }

    #[test]
    fn an_unpaid_step_is_counted_as_charging_nothing() {
        // A resolver that ran but was not paid must not inflate the total the
        // caller is told they were charged.
        let steps = vec![
            json!({"list_price_usdc": "0.017391", "fee_usdc": "0.002609", "charged_usdc": "0.000000"}),
        ];
        assert_eq!(combined_figures(&steps)["charged_usdc"], json!("0.000000"));
    }

    #[test]
    fn the_combined_ceiling_is_compared_in_the_same_basis_as_a_single_call() {
        // The ceiling on this endpoint has always been a LIST figure: step 4a
        // compares `offer.price_usd` to it, and the app sends the selection's
        // provider leg. The combined check has to read it the same way, or a
        // KVK profile behind a lookup (0.078261 list, 0.09 buyer) would be
        // refused against the 0.078261 the caller actually agreed to.
        let primary_list = 0.060869_f64;
        let resolver_list = 0.017391_f64;
        let combined_list = primary_list + resolver_list;

        // What the caller confirmed for the primary alone no longer covers the
        // job once a lookup is needed: refused, and nothing paid.
        let ceiling = primary_list;
        assert!(combined_list - ceiling > 1e-9);

        // The ceiling that covers both legs lets it through.
        let ceiling = combined_list;
        assert!(combined_list - ceiling <= 1e-9);

        // And the figure reported back is the buyer total, which is what the
        // user was shown: 0.02 + 0.07.
        let combined_buyer = crate::fees::buyer_total_usd(primary_list, false)
            + crate::fees::buyer_total_usd(resolver_list, false);
        assert!((combined_buyer - 0.09).abs() < 1e-9);
    }

    #[test]
    fn a_resolver_retry_envelope_names_the_parameter_and_the_endpoint() {
        let offer = crate::db::CapabilityOffer {
            capability_slug: "company.registry-profile".into(),
            endpoint_id: Some(Uuid::nil()),
            api_slug: "kvk".into(),
            api_name: "KVK".into(),
            endpoint_method: "GET".into(),
            endpoint_path: "/api/v1/basisprofielen".into(),
            endpoint_description: None,
            price_usd: 0.060869,
            payment_required: true,
            hosted_externally: false,
            call_url: None,
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
        };
        let envelope = retry_with_inputs(
            "profiel van Mollie",
            Some("company.registry-profile"),
            &offer,
            &[(
                "kvkNummer".to_string(),
                json!({"choose_one_of": ["30204462", "87654321"]}),
            )],
        );
        assert_eq!(envelope["capability"], json!("company.registry-profile"));
        assert_eq!(envelope["candidate_id"], json!(Uuid::nil()));
        // The machine-readable hint carries real choices, never a bare null.
        assert_eq!(
            envelope["inputs"]["kvkNummer"]["choose_one_of"],
            json!(["30204462", "87654321"])
        );
    }

    #[test]
    fn the_requirements_always_force_apiosk_settlement() {
        // The one invariant that makes /v1/do safe to point at a wallet: it can
        // only choose what it can settle. A federated winner would mean paying
        // nothing and calling a paid endpoint unpaid — or worse, "succeeding"
        // without the provider being paid at all.
        let request: DoRequest = serde_json::from_value(json!({
            "job": "x",
            "settlement": "direct"
        }))
        .unwrap();
        assert_eq!(request.requirements().settlement.as_deref(), Some("apiosk"));
    }

    #[test]
    fn dry_run_and_options_parse_from_json() {
        let request: DoRequest = serde_json::from_value(json!({
            "job": "Get Nvidia earnings news",
            "max_price": 0.05,
            "optimize_for": "latency",
            "dry_run": true,
            "timeout_ms": 5000,
        }))
        .unwrap();
        assert_eq!(request.job.as_deref(), Some("Get Nvidia earnings news"));
        assert!(request.dry_run);
        assert_eq!(request.timeout_ms, Some(5000));
        let requirements = request.requirements();
        assert_eq!(requirements.max_price, Some(0.05));
        assert_eq!(requirements.optimize_for.as_deref(), Some("latency"));
    }

    #[test]
    fn an_empty_body_is_a_job_error_not_a_parse_error() {
        let request: DoRequest = DoRequest::default();
        assert!(request.job.is_none());
        assert!(!request.dry_run);
    }

    #[test]
    fn a_retry_carries_the_pin_and_the_values_back() {
        // The shape a blocked answer's `retry_with` is posted back as, verbatim.
        let request: DoRequest = serde_json::from_value(json!({
            "job": "Get Nvidia earnings news from the last 7 days",
            "capability": "news.search",
            "candidate_id": "d2ba3fb9-c457-46cf-b819-eb636ee25736",
            "inputs": { "tickers": "NVDA", "time_from": "2026-08-10" },
        }))
        .unwrap();

        assert_eq!(
            request.candidate_id.map(|id| id.to_string()).as_deref(),
            Some("d2ba3fb9-c457-46cf-b819-eb636ee25736")
        );
        assert_eq!(request.capability.as_deref(), Some("news.search"));
        let inputs = request.inputs.expect("inputs parsed");
        assert_eq!(inputs.get("tickers"), Some(&json!("NVDA")));
        assert_eq!(inputs.len(), 2);
    }

    #[test]
    fn a_pin_does_not_relax_the_settlement_rule() {
        // Pinning narrows the field the scorer ranks; it does not replace the
        // scorer. If `candidate_id` could carry its own requirements, naming a
        // federated candidate would be a way to "pay" for something Apiosk
        // cannot settle — so requirements are built the same way either way.
        let pinned: DoRequest = serde_json::from_value(json!({
            "job": "x",
            "candidate_id": "d2ba3fb9-c457-46cf-b819-eb636ee25736",
            "settlement": "direct",
            "max_price": 0.01,
        }))
        .unwrap();
        let requirements = pinned.requirements();
        assert_eq!(requirements.settlement.as_deref(), Some("apiosk"));
        assert_eq!(requirements.max_price, Some(0.01));
    }

    fn offer(price_usd: f64, hosted_externally: bool) -> CapabilityOffer {
        CapabilityOffer {
            capability_slug: "news.search".into(),
            endpoint_id: None,
            api_slug: "newsapi".into(),
            api_name: "News".into(),
            endpoint_method: "GET".into(),
            endpoint_path: "/v1/search".into(),
            endpoint_description: None,
            price_usd,
            payment_required: true,
            hosted_externally,
            call_url: None,
            input_map: json!({}),
            fixed_params: json!({}),
            request_body: json!({}),
            query_params: json!({}),
            notes: None,
            source: "catalog".into(),
            confidence: 1.0,
            provider_name: None,
            provider_username: None,
            search_text: None,
        }
    }

    /// Contract C4: the figures are 6-decimal strings from `fees::quote`, the
    /// markup rounds up, and `charged_usdc` is list + fee only when paid.
    #[test]
    fn execution_figures_come_from_the_fee_module_and_round_up() {
        let f = execution_figures(&offer(0.10, false), None, true, 1_500);
        assert_eq!(f.list_price_usdc, "0.100000");
        assert_eq!(f.fee_usdc, "0.015000");
        assert_eq!(f.charged_usdc, "0.115000");

        // $0.000015: 15% is 2.25 units — charged 3, never 2.
        let odd = execution_figures(&offer(0.000015, false), None, true, 1_500);
        assert_eq!(odd.fee_usdc, "0.000003");
        assert_eq!(odd.charged_usdc, "0.000018");

        // Unpaid: the quote is still shown, nothing was charged.
        let unpaid = execution_figures(&offer(0.10, false), None, false, 1_500);
        assert_eq!(unpaid.list_price_usdc, "0.100000");
        assert_eq!(unpaid.charged_usdc, "0.000000");
    }

    /// The proxy's settled list price wins over the offer's: a pricing rule
    /// can re-price the call at the proxy, and the wallet paid that number.
    #[test]
    fn execution_figures_prefer_the_price_the_proxy_settled() {
        let f = execution_figures(&offer(0.10, false), Some("0.250000"), true, 1_500);
        assert_eq!(f.list_price_usdc, "0.250000");
        assert_eq!(f.fee_usdc, "0.037500");
        assert_eq!(f.charged_usdc, "0.287500");
        // A malformed header falls back to the offer rather than to zero.
        let fallback = execution_figures(&offer(0.10, false), Some("n/a"), true, 1_500);
        assert_eq!(fallback.charged_usdc, "0.115000");
    }

    /// A service-key workflow run reports list + 20%; the provider list is
    /// unchanged, and an unrecognised markup reports the standard 15%.
    #[test]
    fn execution_figures_follow_the_workflow_markup() {
        let wf = execution_figures(&offer(0.10, false), None, true, 2_000);
        assert_eq!(wf.list_price_usdc, "0.100000");
        assert_eq!(wf.fee_usdc, "0.020000");
        assert_eq!(wf.charged_usdc, "0.120000");
        let settled = execution_figures(&offer(0.10, true), Some("0.250000"), true, 2_000);
        assert_eq!(settled.fee_usdc, "0.050000");
        assert_eq!(settled.charged_usdc, "0.300000");
        let bogus = execution_figures(&offer(0.10, false), None, true, 2_500);
        assert_eq!(bogus.charged_usdc, "0.115000");
    }

    #[test]
    fn usdc_6dp_is_fixed_width_and_never_negative() {
        assert_eq!(usdc_6dp(0), "0.000000");
        assert_eq!(usdc_6dp(1), "0.000001");
        assert_eq!(usdc_6dp(1_100_000), "1.100000");
        assert_eq!(usdc_6dp(123_456_789), "123.456789");
        assert_eq!(usdc_6dp(-5), "0.000000");
    }

    #[test]
    fn a_malformed_candidate_id_is_a_parse_error_not_a_silent_unpinned_run() {
        // Dropping it quietly would turn a retry meant for one endpoint into a
        // fresh decision that could pick another, and bind the caller's values
        // to a schema they were not written against.
        let parsed: Result<DoRequest, _> =
            serde_json::from_value(json!({ "job": "x", "candidate_id": "not-a-uuid" }));
        assert!(parsed.is_err());
    }
}
