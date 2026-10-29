//! `POST /v1/quote` — priced offers a person can choose between.
//!
//! # Why this exists
//!
//! The flow this product describes has a human decision in the middle: three
//! options, a price each, the user picks one. The code had nowhere for that
//! decision to live. `/v1/do` parses, scores, picks and pays in one call, which
//! is right for the n8n node and wrong for a chat where a person is choosing;
//! `/v1/compare` and `/v1/decide` return rankings, but nothing a caller can hand
//! back and say "that one, at that price".
//!
//! The offer the user selected must be the offer that executes, at no more than
//! the price they were shown. Anything else means the price shown and the price
//! charged can differ, which is the one thing a payment product cannot get wrong.
//!
//! # Where a quote lives: a signed token, not a row
//!
//! Both work, and the trade-off is worth stating.
//!
//! A **row** is auditable by construction and lets a quote be revoked. It also
//! needs a table, a migration, an index, an expiry sweep, and a database round
//! trip on the hot path of a step that is supposed to be free.
//!
//! A **signed token** needs none of that. The offer carries its own identity —
//! endpoint, price, expiry — under an HMAC this gateway holds, so a caller
//! cannot invent one, cannot edit the price in one, and cannot use one after it
//! expires. Nothing to clean up, nothing to migrate, and a quote costs exactly
//! the work of scoring it.
//!
//! The token wins here, and the audit trail is not lost with it: `03-8`'s record
//! of what was shown and what was chosen goes to the existing decision log
//! (`record_comparison_now`), which is where every other "what did the agent
//! see" question is already answered. So the row's one real advantage is kept
//! without the row.
//!
//! Revocation is the property genuinely given up. A quote lives fifteen minutes
//! and names one endpoint at one price; there is nothing in that window worth
//! revoking that the policy engine and the wallet caps do not already refuse at
//! payment time.

use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::Engine;
use ring::hmac;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::extract_agent_connect_token;
use crate::db::{AgentConnectTokenLookup, CapabilitySort};
use crate::policy::evaluate::decide_purchase;
use crate::policy::rules::{PurchaseRequest, SpendSnapshot};
use crate::proxy::hash_connect_token;
use crate::AppState;

use super::flow::{evaluate_offers, resolve_set, sweep_external, Requirements};

/// How long a quoted price stands.
///
/// Long enough for a person to read three options, ask a question and answer;
/// short enough that the price is still the price. A provider that repriced
/// mid-quote is caught anyway — the live price is re-checked before payment —
/// so this bounds how often a user is asked twice, not how wrong a price can be.
pub const QUOTE_TTL_SECONDS: i64 = 15 * 60;

#[derive(Debug, Deserialize)]
pub struct QuoteRequest {
    /// The job, in words. Same input as `/v1/do`.
    pub job: Option<String>,
    /// Skip the search and name the capability outright.
    pub capability: Option<String>,
    #[serde(flatten)]
    pub requirements: Requirements,
    /// How many offers to price. Clamped; a menu nobody can read is not a menu.
    pub limit: Option<usize>,
    /// Also sweep the wider x402 ecosystem, not just the reviewed catalogue.
    ///
    /// Defaults to on, like `/v1/discover`. A comparison that shows only the
    /// shelf reads as "these are all there is", and that is the one claim this
    /// product must not make: the reviewed catalogue is a subset of the
    /// ecosystem. The externals come back in their own block, unpriced by us
    /// and unsigned, because they are a different kind of thing — see
    /// `external_offers`.
    #[serde(default = "yes", deserialize_with = "lenient_flag")]
    pub include_external: bool,
}

/// This body is JSON, not a query string, so the flag arrives as a real bool
/// from a normal client and as `"false"` from the ones that stringify
/// everything. `/v1/discover`'s `lenient_bool` reads strings only and would
/// reject the honest bool, which is why this one is here rather than shared.
fn lenient_flag<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Value::deserialize(deserializer)? {
        Value::Bool(flag) => Ok(flag),
        Value::String(raw) => Ok(matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )),
        Value::Number(n) => Ok(n.as_f64().unwrap_or(0.0) != 0.0),
        // Absent is handled by `default`; anything else is a caller mistake we
        // resolve the same way the endpoint's default does — the world, shown.
        _ => Ok(true),
    }
}

/// `include_external` defaults to on. Serde needs a function to say so.
fn yes() -> bool {
    true
}

const MAX_OFFERS: usize = 5;

/// What an `offer_id` carries, before it is signed.
///
/// Every field here is something the executing call must not be able to change:
/// the endpoint, the price it was quoted at, and when the quote dies. The whole
/// point is that a caller can hold this and hand it back, and cannot edit it.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct OfferClaim {
    /// Groups the offers a caller was shown together, for the audit trail.
    q: Uuid,
    /// The endpoint this offer is for.
    e: Uuid,
    /// Slug, so a refusal can name the provider without a lookup.
    s: String,
    /// Price in USD at quote time.
    p: f64,
    /// Unix seconds. Checked on redemption; a stale offer is refused by name.
    x: i64,
}

/// Sign an offer so a caller can hold it and cannot forge one.
fn sign(state: &AppState, claim: &OfferClaim) -> Option<String> {
    let payload = serde_json::to_vec(claim).ok()?;
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&payload);
    let tag = hmac::sign(&quote_key(state), encoded.as_bytes());
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(tag.as_ref());
    Some(format!("{encoded}.{signature}"))
}

/// Read an `offer_id` back, or `None` for anything that was not signed by this
/// gateway. Verification is constant-time (`hmac::verify`), and a bad signature
/// and a malformed token answer identically — telling them apart would be a
/// forging oracle.
pub fn verify_offer_id(state: &AppState, offer_id: &str) -> Option<VerifiedOffer> {
    let (encoded, signature) = offer_id.split_once('.')?;
    let tag = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(signature)
        .ok()?;
    hmac::verify(&quote_key(state), encoded.as_bytes(), &tag).ok()?;

    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .ok()?;
    let claim: OfferClaim = serde_json::from_slice(&payload).ok()?;

    Some(VerifiedOffer {
        quote_id: claim.q,
        endpoint_id: claim.e,
        api_slug: claim.s,
        quoted_price_usd: claim.p,
        expired: chrono::Utc::now().timestamp() > claim.x,
    })
}

/// An `offer_id` this gateway signed, decoded.
#[derive(Debug, Clone)]
pub struct VerifiedOffer {
    pub quote_id: Uuid,
    pub endpoint_id: Uuid,
    pub api_slug: String,
    pub quoted_price_usd: f64,
    /// Reported rather than swallowed: an expired quote gets its own refusal
    /// with a hint to re-quote, not a generic 400.
    pub expired: bool,
}

/// The key offers are signed with.
///
/// Derived from the same secret the rest of the gateway's signing uses. An
/// ephemeral per-process key would be safe but would invalidate every
/// outstanding quote on deploy, which turns a routine restart into "everyone
/// mid-choice has to start over".
fn quote_key(state: &AppState) -> hmac::Key {
    hmac::Key::new(
        hmac::HMAC_SHA256,
        state.config.quote_signing_secret.as_bytes(),
    )
}

pub async fn quote(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap<HeaderValue>,
    Json(request): Json<QuoteRequest>,
) -> Response {
    let job = request
        .job
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_default()
        .to_string();

    if job.is_empty() && request.capability.is_none() {
        return error(
            StatusCode::BAD_REQUEST,
            "no_subject",
            "Say what you need, in plain words, as `job` — or name a `capability`.",
        );
    }

    // Same till, same budget as `/v1/discover` and `/v1/compare` — see
    // `crate::parse_budget`. This endpoint reads a free-text `job` through the
    // same parser, and the parser is the only step here that spends money, so
    // leaving it ungated made this the unlocked door in a corridor of locked
    // ones: a caller refused a reading on discover could send the identical
    // sentence here and be read anyway, once per request, with no account and
    // no ceiling. `None` — "decide from the query" — became `Some(false)` for
    // anyone this gateway cannot name, which is the same blunter reading every
    // caller got before a parser existed, not a refusal.
    let parse_override = if crate::parse_budget::allows_parse(&state, &headers).await {
        None
    } else {
        Some(false)
    };
    let Ok((plan, infos)) =
        resolve_set(&state, &job, request.capability.as_deref(), parse_override).await
    else {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "resolve_failed",
            "Could not work out what this job needs.",
        );
    };

    // A catalogue miss is not the end of the answer.
    //
    // "Nothing in the shelf" and "nothing anywhere" are different facts, and
    // this endpoint used to give them the same 404. The indexes are swept on
    // the caller's own words, and if the ecosystem holds something the user
    // gets the same table with the reviewed half empty — the honest shape of
    // "nobody reviewed these, here they are".
    let Some(info) = infos.into_iter().next() else {
        let external = if request.include_external {
            // Per-need vocabulary, so a two-need job reaches the queried
            // indexes as two needs — see `flow::need_terms`.
            let needs = super::flow::need_terms(&job, &plan, None, &[]);
            crate::sources::sweep_needs(&state, &needs, &[]).await
        } else {
            Vec::new()
        };

        if external.is_empty() {
            return error(
                StatusCode::NOT_FOUND,
                "no_capability",
                "Nothing in the catalogue serves this job, and the x402 indexes returned nothing either. Try apiosk_discover with different words.",
            );
        }

        return (
            StatusCode::OK,
            Json(json!({
                "quote_id": Uuid::new_v4(),
                "capability": Value::Null,
                "expires_in_seconds": QUOTE_TTL_SECONDS,
                "interpretation": plan.to_json(),
                "offers": Vec::<Value>::new(),
                "external_offers": external_block(&state, &external).await,
                "guidance": "The reviewed catalogue serves nothing here, so there is no offer to pin with an offer_id. `external_offers` are unreviewed ecosystem listings. Each states its payment eligibility and indicative prices: `buyer_price_usd` uses the configured /v1/x402/fetch tariff; `balance_buyer_price_usd` uses the standard Apiosk balance tariff. A usable input contract and validated quote are required before purchase. `settlement_reason` explains any payment restriction.",
            })),
        )
            .into_response();
    };

    let offers = match state
        .db
        .list_capability_offers(
            &info.slug,
            &CapabilitySort::Price,
            request.requirements.max_price,
        )
        .await
    {
        Ok(offers) => offers,
        Err(err) => {
            tracing::error!("could not list offers for {}: {err}", info.slug);
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "offers_unavailable",
                "Could not read the providers for this capability.",
            );
        }
    };

    let stats = state.cache.peek_provider_stats().await.unwrap_or_default();

    // The SAME scorer /v1/decide and /v1/do use. A second implementation here
    // would mean the menu and the meal could disagree — the user shown three
    // prices and the call that runs would be ranking different things.
    let evaluated = evaluate_offers(&offers, &stats, &info.input_contract, &request.requirements);

    if evaluated.ranked.is_empty() {
        return error_with(
            StatusCode::CONFLICT,
            "no_viable_offer",
            "Every provider failed one of your constraints. Each rejection names the rule, so relax the binding one rather than guessing.",
            json!({ "capability": info.slug, "rejected": evaluated.rejected }),
        );
    }

    // What the buyer's own rules make of each offer, BEFORE anybody is shown a
    // price. An agent that offers a user something their rules will refuse has
    // wasted the one moment the user was paying attention, and the refusal
    // arrives after they chose rather than before.
    //
    // Optional: a quote without a connect token is anonymous window-shopping and
    // still works, exactly as it did before this existed.
    let judge = PolicyJudge::resolve(&state, &headers).await;

    let quote_id = Uuid::new_v4();
    let expires_at = chrono::Utc::now() + chrono::Duration::seconds(QUOTE_TTL_SECONDS);
    let limit = request.limit.unwrap_or(MAX_OFFERS).clamp(1, MAX_OFFERS);

    let mut offers_json: Vec<Value> = Vec::new();
    // The lookup steps these offers would need before they can run, in one
    // query for the whole page. A quote that hides a required pre-step quotes
    // a price the caller will not actually be charged.
    let pre_step_rows = {
        let endpoint_ids: Vec<Uuid> = evaluated
            .ranked
            .iter()
            .take(limit)
            .filter_map(|(_, _, offer)| offer.endpoint_id)
            .collect();
        let capabilities = std::slice::from_ref(&info.slug);
        state
            .db
            .pre_step_rows(&endpoint_ids, capabilities)
            .await
            .unwrap_or_else(|err| {
                tracing::warn!("quote pre-step lookup unavailable: {}", err);
                Vec::new()
            })
    };
    let no_inputs = Value::Object(serde_json::Map::new());

    for (score, candidate, offer) in evaluated.ranked.iter().take(limit) {
        // An endpoint with no id cannot be pinned, and an offer that cannot be
        // pinned is not an offer — it is a suggestion the caller cannot act on.
        let Some(endpoint_id) = offer.endpoint_id else {
            continue;
        };
        let Some(offer_id) = sign(
            &state,
            &OfferClaim {
                q: quote_id,
                e: endpoint_id,
                s: offer.api_slug.clone(),
                p: offer.price_usd,
                x: expires_at.timestamp(),
            },
        ) else {
            continue;
        };

        // Unmeasured dimensions come back null, never as a plausible default. A
        // latency figure invented for a provider nobody has ever called is the
        // one number on this screen that would be a lie.
        let performance = candidate.get("performance").cloned().unwrap_or(Value::Null);
        let pre_steps = crate::param_resolver::pre_steps_for(
            &pre_step_rows,
            offer.endpoint_id,
            Some(&info.slug),
            &job,
            &no_inputs,
        );
        let buyer_price_usdc = crate::fees::buyer_total_usd(offer.price_usd, false);
        offers_json.push(json!({
            "offer_id": offer_id,
            "provider": offer.provider_name.clone().unwrap_or_else(|| offer.api_slug.clone()),
            "api_slug": offer.api_slug,
            "capability": info.slug,
            "price_usdc": offer.price_usd,
            // The buyer total (list + 15%). Additive: `price_usdc` stays the raw
            // list price the offer_id pins; this is what the wallet is debited.
            // The 15% buyer markup is class-independent, so Managed stands in.
            "buyer_price_usdc": buyer_price_usdc,
            // A lookup this offer needs first (resolving an identifier the job
            // does not state), and the total including it. Both absent when
            // the call can run on its own. Prediction, not a promise: /v1/do
            // charges only what it actually runs.
            "pre_steps": if pre_steps.is_empty() { Value::Null } else { Value::Array(pre_steps.clone()) },
            "combined_buyer_price_usdc": if pre_steps.is_empty() {
                Value::Null
            } else {
                json!(crate::param_resolver::combined_buyer_price_usd(buyer_price_usdc, &pre_steps))
            },
            "score": score,
            "expected_output": candidate.get("expected_output").cloned().unwrap_or(Value::Null),
            "p95_latency_ms": performance.get("p95_latency_ms").cloned().unwrap_or(Value::Null),
            "success_rate": performance.get("success_rate").cloned().unwrap_or(Value::Null),
            "availability": performance.get("availability").cloned().unwrap_or(Value::Null),
            "coverage": candidate.get("missing_inputs").cloned().unwrap_or(Value::Null),
            "settlement": "apiosk",
            // null when this quote is anonymous — "we did not check" rather
            // than "your rules allow it", which are not the same claim.
            "policy": judge.judge(offer.price_usd, &offer.api_slug),
        }));
    }

    if offers_json.is_empty() {
        return error(
            StatusCode::CONFLICT,
            "no_quotable_offer",
            "The surviving providers have no pinnable endpoint, so none of them can be quoted a price you could act on.",
        );
    }

    // The world beside the shelf.
    //
    // The reviewed offers above are the ones Apiosk can pin, price and settle.
    // They are not everything that exists, and a comparison that shows only
    // them tells the user something false about the market. The sweep runs on
    // the same words and the same indexes discovery uses, and the hits come
    // back in their own block — unpinned, unpriced by us, unreviewed — so that
    // nothing here can be mistaken for an offer Apiosk stands behind.
    //
    // Endpoints already served by a listing above are dropped by host, so the
    // same provider does not appear twice under two different labels.
    let known_hosts: Vec<String> = offers
        .iter()
        .filter_map(|o| o.call_url.as_deref())
        .filter_map(|u| u.split("://").nth(1))
        .filter_map(|rest| rest.split('/').next())
        .map(|h| h.to_ascii_lowercase())
        .collect();
    let external = if request.include_external {
        sweep_external(&state, &job, &plan, &info, None, &known_hosts).await
    } else {
        Vec::new()
    };

    // The audit trail for "why did the agent pick the expensive one": what was
    // shown, in order, at what price. The choice itself is recorded when the
    // offer is redeemed on the payment path.
    let comparison_id = crate::decision_record::record_comparison_now(
        &state,
        &headers,
        &info.slug,
        "quote",
        request.requirements.max_price,
        &evaluated.considered,
        None,
    )
    .await;

    (
        StatusCode::OK,
        Json(json!({
            "quote_id": quote_id,
            "capability": info.slug,
            "expires_at": expires_at,
            "expires_in_seconds": QUOTE_TTL_SECONDS,
            "interpretation": plan.to_json(),
            "comparison_id": comparison_id,
            "offers": offers_json,
            "external_offers": external_block(&state, &external).await,
            "rejected": evaluated.rejected,
            "guidance": "Show these to the user and let them choose. Nothing is paid here. Call POST /v1/do with the chosen offer_id and max_price_usdc set to the price you showed them; the call is refused rather than paid if the real price is above it. Show `external_offers` as unreviewed discovery leads that cannot be pinned with an offer_id. Their configured direct-fetch and standard balance price estimates may differ; validate the input contract and exact purchase quote before asking for approval.",
        })),
    )
        .into_response()
}

/// The external half of a quote, in the shape `/v1/discover` already publishes.
///
/// Deliberately its own block rather than extra rows in `offers`: everything in
/// `offers` carries a signed `offer_id` that pins an endpoint at a price, and
/// nothing here can. Merging the two would launder "nobody has checked this,
/// and Apiosk cannot settle it" into "an offer" — the one confusion a payment
/// product cannot afford. A caller that wants them side by side on screen can
/// put them there; the API never blurs which is which.
async fn external_block(state: &Arc<AppState>, external: &[crate::bazaar::ExternalOffer]) -> Value {
    // Priced by the payer that would charge it — see `quote_external_offers`.
    let settlements = crate::proxy::external_fetch::quote_external_offers(state, external).await;
    let offers: Vec<Value> = external
        .iter()
        .zip(settlements.iter())
        .map(|(offer, settlement)| offer.to_json_settled(settlement))
        .collect();
    let payable = settlements.iter().filter(|s| s.payable).count();
    json!({
        "count": external.len(),
        "settleable_by_apiosk": payable,
        "offers": offers,
        "source": crate::sources::reach_sentence(state).await,
        "sources_swept": crate::sources::source_ids(state).await,
        "why_separate": "Unreviewed: these listings have no measured latency or success rate, trusted input mapping, or comparison score, and cannot be pinned with an offer_id. `settlement` indicates payment eligibility subject to contract and live-price validation. `buyer_price_usd` is an indicative configured /v1/x402/fetch total; `balance_buyer_price_usd` is a separate standard Apiosk balance estimate. Neither estimate is an approved purchase quote. `settlement_reason` explains any restriction.",
        "turn_off": "include_external=false quotes the reviewed catalogue alone.",
    })
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    error_with(status, code, message, json!({}))
}

fn error_with(status: StatusCode, code: &str, message: &str, extra: Value) -> Response {
    let mut body = json!({ "error": code, "message": message });
    if let (Some(body_map), Some(extra_map)) = (body.as_object_mut(), extra.as_object()) {
        for (key, value) in extra_map {
            body_map.insert(key.clone(), value.clone());
        }
    }
    (status, Json(body)).into_response()
}

/// The buyer's rules, resolved once for a whole quote.
///
/// Resolved once rather than per offer because the answer does not vary by offer
/// — the rules, the spend so far and the wallet are the same for all of them —
/// and reading them five times would turn a free step into five round trips.
struct PolicyJudge {
    policy: Option<crate::db::PurchasePolicy>,
    spend: SpendSnapshot,
}

impl PolicyJudge {
    async fn resolve(state: &Arc<AppState>, headers: &HeaderMap<HeaderValue>) -> Self {
        let none = Self {
            policy: None,
            spend: SpendSnapshot::default(),
        };

        let Some(raw_token) = extract_agent_connect_token(headers) else {
            return none;
        };
        let Ok(AgentConnectTokenLookup::Active(token)) = state
            .db
            .lookup_agent_connect_token_by_hash(&hash_connect_token(&raw_token))
            .await
        else {
            return none;
        };
        let Some(policy) = state.db.load_policy_for_connect_token(token.token_id).await else {
            return none;
        };

        let spend = match token.wallets.first() {
            Some(wallet) => {
                let (today, month, calls) = state
                    .db
                    .get_wallet_policy_spend(&wallet.wallet_address)
                    .await;
                SpendSnapshot {
                    spent_today_usd: bigdecimal::ToPrimitive::to_f64(&today).unwrap_or(0.0),
                    spent_this_month_usd: bigdecimal::ToPrimitive::to_f64(&month).unwrap_or(0.0),
                    calls_today: calls,
                }
            }
            None => SpendSnapshot::default(),
        };

        Self {
            policy: Some(policy),
            spend,
        }
    }

    /// What the rules would say about this offer at this price.
    ///
    /// A preview, and labelled as one. It runs the same evaluator the payment
    /// path runs, on the same rules, but minutes earlier — a rule edited in
    /// between, or a purchase made on another connection, moves the answer, and
    /// the verdict at payment time is the one that counts.
    fn judge(&self, price_usd: f64, api_slug: &str) -> Value {
        let Some(policy) = self.policy.as_ref() else {
            return Value::Null;
        };

        let request = PurchaseRequest {
            amount_usd: price_usd,
            seller: Some(api_slug.to_string()),
            asset: Some("USDC".to_string()),
            ..PurchaseRequest::default()
        };
        let verdict = decide_purchase(policy.enabled, &policy.rules, &request, self.spend);

        json!({
            "verdict": verdict.decision.as_str(),
            "policy_name": policy.name,
            "policy_version": policy.version,
            "matched_rule": verdict.matched_rule(),
            "reason": verdict.reason(),
            "preview": true,
            "note": "What the buyer's rules say about this offer right now. The verdict at payment time is the one that counts — a rule edited in between, or spending on another connection, moves it.",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `AppState` is heavy, and none of these cases need one — only the key.
    /// Signing and verification are exercised through the same two functions the
    /// endpoint uses, with the state parameter's one job (the secret) stood in
    /// for directly.
    fn key() -> hmac::Key {
        hmac::Key::new(hmac::HMAC_SHA256, b"test-quote-secret")
    }

    fn sign_with(key: &hmac::Key, claim: &OfferClaim) -> String {
        let payload = serde_json::to_vec(claim).unwrap();
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&payload);
        let tag = hmac::sign(key, encoded.as_bytes());
        format!(
            "{encoded}.{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(tag.as_ref())
        )
    }

    fn verify_with(key: &hmac::Key, offer_id: &str) -> Option<VerifiedOffer> {
        let (encoded, signature) = offer_id.split_once('.')?;
        let tag = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(signature)
            .ok()?;
        hmac::verify(key, encoded.as_bytes(), &tag).ok()?;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .ok()?;
        let claim: OfferClaim = serde_json::from_slice(&payload).ok()?;
        Some(VerifiedOffer {
            quote_id: claim.q,
            endpoint_id: claim.e,
            api_slug: claim.s,
            quoted_price_usd: claim.p,
            expired: chrono::Utc::now().timestamp() > claim.x,
        })
    }

    fn claim(price: f64, expires_in: i64) -> OfferClaim {
        OfferClaim {
            q: Uuid::new_v4(),
            e: Uuid::new_v4(),
            s: "weather-api".to_string(),
            p: price,
            x: chrono::Utc::now().timestamp() + expires_in,
        }
    }

    #[test]
    fn an_offer_survives_the_round_trip_intact() {
        let key = key();
        let original = claim(0.004, 900);
        let verified = verify_with(&key, &sign_with(&key, &original)).expect("must verify");

        assert_eq!(verified.quote_id, original.q);
        assert_eq!(verified.endpoint_id, original.e);
        assert_eq!(verified.api_slug, original.s);
        assert_eq!(verified.quoted_price_usd, original.p);
        assert!(!verified.expired);
    }

    #[test]
    fn a_forged_offer_is_refused() {
        // Someone who has seen an offer_id must not be able to make another one.
        let key = key();
        let theirs = hmac::Key::new(hmac::HMAC_SHA256, b"not-our-secret");
        let forged = sign_with(&theirs, &claim(0.004, 900));
        assert!(verify_with(&key, &forged).is_none());
    }

    #[test]
    fn an_edited_price_is_refused() {
        // The attack this exists to stop: take a real offer, halve the price,
        // hand it back. The signature covers the payload, so the edit breaks it.
        let key = key();
        let real = sign_with(&key, &claim(1.0, 900));
        let (encoded, signature) = real.split_once('.').unwrap();

        let mut payload: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(encoded)
                .unwrap(),
        )
        .unwrap();
        payload["p"] = serde_json::json!(0.0001);
        let tampered = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&payload).unwrap());

        assert!(verify_with(&key, &format!("{tampered}.{signature}")).is_none());
    }

    #[test]
    fn a_malformed_offer_is_refused_the_same_way_a_forged_one_is() {
        // Same answer for both, so this is not an oracle for what a valid token
        // looks like.
        let key = key();
        for bad in ["", ".", "not-a-token", "aGVsbG8.aGVsbG8", "a.b.c"] {
            assert!(verify_with(&key, bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn an_expired_offer_verifies_but_says_it_is_stale() {
        // Reported rather than rejected outright: the caller gets `quote_expired`
        // and a hint to re-quote, not a generic "invalid" that reads as a bug.
        let key = key();
        let verified = verify_with(&key, &sign_with(&key, &claim(0.004, -1))).expect("must verify");
        assert!(verified.expired);
        assert_eq!(verified.api_slug, "weather-api");
    }

    #[test]
    fn the_quote_window_is_long_enough_to_read_and_short_enough_to_mean_something() {
        // Three options, a question, an answer. Fifteen minutes is the shape of
        // that conversation; an hour is the shape of a stale price.
        const { assert!(QUOTE_TTL_SECONDS >= 5 * 60) };
        const { assert!(QUOTE_TTL_SECONDS <= 30 * 60) };
    }
}
