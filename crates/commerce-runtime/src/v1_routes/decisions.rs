//! `POST /v1/decisions` and `POST /v1/decisions/{id}/outcome` — the half of the
//! decision record only the agent can supply.
//!
//! `/v1/capabilities/{slug}` records what was on the table (see
//! `crate::decision_record`). What it cannot know is which candidate the agent
//! took, which ones it deliberately passed over, on what rule, and whether the
//! result was any good. These two endpoints are where the agent says so.
//!
//! Both are free, unauthenticated and rate-limited per IP, on the same
//! reasoning as the 402 intent survey (`crate::intent`): the data is worth more
//! than the friction of an auth handshake, and an agent that cannot report
//! cheaply will not report at all. Nothing here is on a payment path.
//!
//! Everything except the choice itself is optional. An agent that sends only
//! `{capability, chosen_api_slug}` still contributes the single most valuable
//! fact in the system, and demanding more would mean getting less.

use std::sync::Arc;

use axum::{
    body::{Body, Bytes},
    extract::{Path, State},
    http::{header::RETRY_AFTER, HeaderMap, HeaderValue, StatusCode},
    response::Response,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::db::{DecisionInsert, OutcomeInsert};
use crate::rate_limit::{RateLimitDecision, RateLimitPolicy};
use crate::AppState;

/// Open and unauthenticated, so cap per-IP submissions to blunt floods. Higher
/// than the intent survey's: a working agent legitimately decides many times a
/// minute, where it answers a survey once.
const DECISION_RATE_LIMIT: RateLimitPolicy = RateLimitPolicy::per_minute(120);

const MAX_REASON_CHARS: usize = 2_000;
const MAX_POLICY_CHARS: usize = 120;
const MAX_SLUG_CHARS: usize = 128;
const MAX_PATH_CHARS: usize = 1_024;
const MAX_METHOD_CHARS: usize = 12;
const MAX_NOTES_CHARS: usize = 2_000;
/// Rejections are agent-supplied and unbounded by construction.
const MAX_REJECTED_ENTRIES: usize = 100;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct DecisionSubmission {
    #[serde(rename = "comparisonId", alias = "comparison_id")]
    comparison_id: Option<String>,
    #[serde(alias = "capability_slug")]
    capability: Option<String>,
    #[serde(rename = "chosenApiSlug", alias = "chosen_api_slug", alias = "chosen")]
    chosen_api_slug: Option<String>,
    #[serde(rename = "chosenMethod", alias = "chosen_method", alias = "method")]
    chosen_method: Option<String>,
    #[serde(rename = "chosenPath", alias = "chosen_path", alias = "path")]
    chosen_path: Option<String>,
    #[serde(
        rename = "chosenPriceUsd",
        alias = "chosen_price_usd",
        alias = "price_usd"
    )]
    chosen_price_usd: Option<f64>,
    reason: Option<String>,
    policy: Option<String>,
    rejected: Option<Value>,
    supersedes: Option<String>,
    #[serde(rename = "agentName", alias = "agent_name", alias = "agent")]
    agent_name: Option<String>,
    #[serde(rename = "walletAddress", alias = "wallet_address")]
    wallet_address: Option<String>,
    #[serde(rename = "requestId", alias = "request_id")]
    request_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct OutcomeSubmission {
    status: Option<String>,
    #[serde(rename = "httpStatus", alias = "http_status")]
    http_status: Option<i32>,
    #[serde(rename = "latencyMs", alias = "latency_ms")]
    latency_ms: Option<i64>,
    satisfied: Option<bool>,
    notes: Option<String>,
}

/// `POST /v1/decisions` — "I chose this one, over these, for this reason."
pub async fn submit_decision(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    let Some(submission) = parse_body::<DecisionSubmission>(&body) else {
        return invalid_json();
    };

    if let Some(limited) = rate_limited(&state, &headers, "decision").await {
        return limited;
    }

    let Some(capability) = clean(submission.capability.as_deref(), MAX_SLUG_CHARS) else {
        return error(
            StatusCode::BAD_REQUEST,
            "capability_required",
            "Which capability was this decision for? Send {\"capability\": \"web.page-to-text\", \"chosen_api_slug\": \"...\"}.",
        );
    };
    let Some(chosen) = clean(submission.chosen_api_slug.as_deref(), MAX_SLUG_CHARS) else {
        return error(
            StatusCode::BAD_REQUEST,
            "chosen_api_slug_required",
            "Which candidate did you take? Send {\"chosen_api_slug\": \"tavily\"}. Everything else is optional.",
        );
    };

    // An unknown capability is the caller's mistake, not a 500 from a foreign
    // key. Checked before the insert so the message can say what is wrong.
    match state.db.get_capability(&capability).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return error(
                StatusCode::NOT_FOUND,
                "unknown_capability",
                "No such capability. List them at GET /v1/capabilities.",
            )
        }
        Err(err) => {
            tracing::error!("capability lookup failed: {}", err);
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "lookup_failed",
                "Could not verify the capability. Please retry.",
            );
        }
    }

    // A stale or invented comparison id drops to NULL rather than failing the
    // write: the decision is the valuable half and must still land.
    let comparison_id = match parse_uuid(submission.comparison_id.as_deref()) {
        Some(id) => match state.db.comparison_exists(id).await {
            Ok(true) => Some(id),
            _ => None,
        },
        None => None,
    };
    let supersedes = match parse_uuid(submission.supersedes.as_deref()) {
        Some(id) => match state.db.decision_exists(id).await {
            Ok(true) => Some(id),
            _ => None,
        },
        None => None,
    };

    let row = DecisionInsert {
        comparison_id,
        capability_slug: capability,
        chosen_api_slug: chosen,
        chosen_method: clean(submission.chosen_method.as_deref(), MAX_METHOD_CHARS)
            .map(|m| m.to_uppercase()),
        chosen_path: clean(submission.chosen_path.as_deref(), MAX_PATH_CHARS),
        chosen_price_usd: submission.chosen_price_usd.filter(|p| p.is_finite()),
        reason: clean(submission.reason.as_deref(), MAX_REASON_CHARS),
        policy: clean(submission.policy.as_deref(), MAX_POLICY_CHARS),
        rejected: normalize_rejected(submission.rejected.as_ref()),
        supersedes,
        agent_name: clean(submission.agent_name.as_deref(), 200)
            .or_else(|| header_value(&headers, "x-agent-name", 200)),
        wallet_address: clean(submission.wallet_address.as_deref(), 128)
            .map(|w| w.to_lowercase())
            .or_else(|| header_value(&headers, "x-wallet-address", 128).map(|w| w.to_lowercase())),
        request_id: parse_uuid(submission.request_id.as_deref()),
    };

    match state.db.insert_decision(&row).await {
        Ok(id) => json_response(
            StatusCode::CREATED,
            json!({
                "ok": true,
                "decision_id": id,
                "comparison_id": comparison_id,
                "outcome_url": format!("https://gateway.apiosk.com/v1/decisions/{id}/outcome"),
                "note": "Report back with POST to outcome_url once you know whether it worked. Nothing else measures that.",
            }),
        ),
        Err(err) => {
            tracing::error!("decision not recorded: {}", err);
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "not_recorded",
                "Could not record the decision. Please retry.",
            )
        }
    }
}

/// `POST /v1/decisions/{id}/outcome` — "and here is whether it worked."
pub async fn submit_outcome(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Response<Body> {
    let Some(submission) = parse_body::<OutcomeSubmission>(&body) else {
        return invalid_json();
    };

    if let Some(limited) = rate_limited(&state, &headers, "outcome").await {
        return limited;
    }

    let Some(decision_id) = parse_uuid(Some(&id)) else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_decision_id",
            "The path must carry the decision_id returned by POST /v1/decisions.",
        );
    };

    let status = submission
        .status
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !matches!(
        status.as_str(),
        "success" | "error" | "timeout" | "abandoned"
    ) {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_status",
            "status must be one of: success, error, timeout, abandoned.",
        );
    }

    match state.db.decision_exists(decision_id).await {
        Ok(true) => {}
        Ok(false) => {
            return error(
                StatusCode::NOT_FOUND,
                "unknown_decision",
                "No such decision. Use the decision_id returned by POST /v1/decisions.",
            )
        }
        Err(err) => {
            tracing::error!("decision lookup failed: {}", err);
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "lookup_failed",
                "Could not verify the decision. Please retry.",
            );
        }
    }

    let row = OutcomeInsert {
        decision_id,
        status,
        http_status: submission.http_status.filter(|s| (100..=599).contains(s)),
        latency_ms: submission.latency_ms.filter(|ms| *ms >= 0),
        satisfied: submission.satisfied,
        notes: clean(submission.notes.as_deref(), MAX_NOTES_CHARS),
    };

    match state.db.insert_outcome(&row).await {
        Ok((outcome_id, attempt)) => json_response(
            StatusCode::CREATED,
            json!({"ok": true, "outcome_id": outcome_id, "decision_id": decision_id, "attempt": attempt}),
        ),
        Err(err) => {
            tracing::error!("outcome not recorded: {}", err);
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "not_recorded",
                "Could not record the outcome. Please retry.",
            )
        }
    }
}

// --- helpers ---------------------------------------------------------------

/// `None` when the body is present but is not JSON. The error response is built
/// by the caller rather than carried in an `Err`: a `Response` is a large
/// value, and putting one in the error arm makes every success path pay for it.
fn parse_body<T: Default + serde::de::DeserializeOwned>(body: &Bytes) -> Option<T> {
    if body.is_empty() {
        return Some(T::default());
    }
    serde_json::from_slice(body).ok()
}

fn invalid_json() -> Response<Body> {
    error(StatusCode::BAD_REQUEST, "invalid_json", "Send a JSON body.")
}

async fn rate_limited(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    scope: &str,
) -> Option<Response<Body>> {
    let ip = headers
        .get("fly-client-ip")
        .or_else(|| headers.get("x-forwarded-for"))
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(',').next().unwrap_or(v).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    match state
        .rate_limiter
        .check(&format!("{scope}:{ip}"), DECISION_RATE_LIMIT)
        .await
    {
        RateLimitDecision::Allowed { .. } => None,
        RateLimitDecision::Throttled { retry_after } => {
            let mut response = error(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "That is a lot of decisions. Please slow down and retry.",
            );
            if let Ok(value) = HeaderValue::from_str(&retry_after.as_secs().max(1).to_string()) {
                response.headers_mut().insert(RETRY_AFTER, value);
            }
            Some(response)
        }
    }
}

/// Keeps only what the record can use: a bounded array of
/// `{api_slug, path, reason}`. An agent that sends bare slugs, or a wrapped
/// object, still gets its rejections recorded rather than dropped.
fn normalize_rejected(value: Option<&Value>) -> Value {
    let Some(value) = value else {
        return json!([]);
    };
    let items: Vec<&Value> = match value {
        Value::Array(items) => items.iter().collect(),
        other => vec![other],
    };

    Value::Array(
        items
            .into_iter()
            .take(MAX_REJECTED_ENTRIES)
            .filter_map(|item| match item {
                Value::String(slug) => Some(json!({"api_slug": truncate(slug, MAX_SLUG_CHARS)})),
                Value::Object(_) => {
                    let slug = item
                        .get("api_slug")
                        .or_else(|| item.get("apiSlug"))
                        .and_then(Value::as_str)
                        .map(|s| truncate(s, MAX_SLUG_CHARS))?;
                    let mut out = json!({"api_slug": slug});
                    if let Some(path) = item.get("path").and_then(Value::as_str) {
                        out["path"] = json!(truncate(path, MAX_PATH_CHARS));
                    }
                    if let Some(reason) = item.get("reason").and_then(Value::as_str) {
                        out["reason"] = json!(truncate(reason, MAX_REASON_CHARS));
                    }
                    Some(out)
                }
                _ => None,
            })
            .collect(),
    )
}

fn clean(value: Option<&str>, max: usize) -> Option<String> {
    let trimmed = value?.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(truncate(trimmed, max))
}

fn truncate(value: &str, max: usize) -> String {
    match value.char_indices().nth(max) {
        Some((idx, _)) => value[..idx].to_string(),
        None => value.to_string(),
    }
}

fn header_value(headers: &HeaderMap, name: &str, max: usize) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| clean(Some(v), max))
}

fn parse_uuid(value: Option<&str>) -> Option<Uuid> {
    Uuid::parse_str(value?.trim()).ok()
}

fn json_response(status: StatusCode, body: Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn error(status: StatusCode, code: &str, message: &str) -> Response<Body> {
    json_response(
        status,
        json!({"ok": false, "error": code, "message": message}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_slugs_are_accepted_as_rejections() {
        // An agent that sends ["exa", "firecrawl"] means something useful; a
        // stricter schema would just lose it.
        let out = normalize_rejected(Some(&json!(["exa", "firecrawl"])));
        assert_eq!(out.as_array().unwrap().len(), 2);
        assert_eq!(out[0]["api_slug"], "exa");
    }

    #[test]
    fn a_rejection_keeps_its_reason() {
        let out = normalize_rejected(Some(&json!([
            {"api_slug": "exa", "path": "/contents", "reason": "over budget"}
        ])));
        assert_eq!(out[0]["api_slug"], "exa");
        assert_eq!(out[0]["path"], "/contents");
        assert_eq!(out[0]["reason"], "over budget");
    }

    #[test]
    fn a_rejection_without_a_slug_is_dropped_not_stored_half() {
        let out = normalize_rejected(Some(&json!([{"reason": "too slow"}, {"api_slug": "exa"}])));
        assert_eq!(out.as_array().unwrap().len(), 1);
        assert_eq!(out[0]["api_slug"], "exa");
    }

    #[test]
    fn a_single_object_is_treated_as_one_rejection() {
        let out = normalize_rejected(Some(&json!({"api_slug": "exa"})));
        assert_eq!(out.as_array().unwrap().len(), 1);
    }

    #[test]
    fn missing_rejections_are_an_empty_list_never_null() {
        assert_eq!(normalize_rejected(None), json!([]));
        assert_eq!(normalize_rejected(Some(&json!(null))), json!([]));
    }

    #[test]
    fn rejections_are_bounded() {
        let many: Vec<Value> = (0..500).map(|i| json!(format!("api-{i}"))).collect();
        let out = normalize_rejected(Some(&Value::Array(many)));
        assert_eq!(out.as_array().unwrap().len(), MAX_REJECTED_ENTRIES);
    }

    #[test]
    fn free_text_is_capped_and_trimmed() {
        assert_eq!(clean(Some("  tavily  "), 128).as_deref(), Some("tavily"));
        assert_eq!(clean(Some("   "), 128), None);
        assert_eq!(clean(None, 128), None);
        assert_eq!(
            clean(Some(&"x".repeat(9_000)), MAX_REASON_CHARS)
                .unwrap()
                .len(),
            2_000
        );
    }

    #[test]
    fn multibyte_text_is_truncated_on_a_char_boundary() {
        // A byte-slice truncation here would panic on production input.
        let value = "é".repeat(50);
        assert_eq!(truncate(&value, 10).chars().count(), 10);
    }
}
