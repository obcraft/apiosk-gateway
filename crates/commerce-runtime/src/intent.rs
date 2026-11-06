//! Agent intent survey — "what were you actually looking for?"
//!
//! A free, opt-in collection endpoint: [`submit_intent`] is `POST
//! /_apiosk/intent`, which records the answer in `agent_intents` (migration
//! 049); [`intent_info`] is the self-documenting `GET` describing how to use
//! it. Nothing here is advertised on 402 responses — payment responses are
//! untouched.
//!
//! Everything is non-blocking and optional: a missing/blank answer is a clean
//! 400 (no row written), and a DB failure never affects the payment path.

use std::sync::Arc;

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{
        header::{CONTENT_TYPE, HOST, RETRY_AFTER, USER_AGENT},
        HeaderMap, HeaderName, HeaderValue, StatusCode,
    },
    response::Response,
};
use serde::Deserialize;
use tracing::warn;
use uuid::Uuid;

use crate::{
    traffic::client_ip_from_headers,
    util::truncate_chars,
    rate_limit::{RateLimitDecision, RateLimitPolicy},
    request_context::REQUEST_ID_HEADER,
    AppState,
};

/// The survey question shown to callers.
const SURVEY_QUESTION: &str =
    "What were you trying to do? Tell Apiosk what you need and we'll prioritise building or opening it. Optional — answering never affects payment.";

/// Open, unauthenticated endpoint, so cap per-IP submissions to blunt floods.
const INTENT_RATE_LIMIT: RateLimitPolicy = RateLimitPolicy::per_minute(20);

// Defensive column caps. The `agent_intents` columns are unbounded `text`;
// these keep one abusive caller from writing megabytes.
const MAX_INTENT_CHARS: usize = 2_000;
const MAX_USE_CASE_CHARS: usize = 200;
const MAX_CONTACT_CHARS: usize = 320;
const MAX_AGENT_CHARS: usize = 200;
const MAX_API_SLUG_CHARS: usize = 128;
const MAX_PATH_CHARS: usize = 1_024;
const MAX_METHOD_CHARS: usize = 12;
const MAX_USER_AGENT_CHARS: usize = 512;

/// Caller-supplied intent answer. Accepts both camelCase (what the 402 survey
/// advertises) and snake_case, and is lenient: every field is optional at the
/// serde layer and validated in the handler.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct IntentSubmission {
    #[serde(rename = "requestId", alias = "request_id")]
    request_id: Option<String>,
    intent: Option<String>,
    #[serde(rename = "useCase", alias = "use_case")]
    use_case: Option<String>,
    contact: Option<String>,
    #[serde(alias = "agentName", alias = "agent_name")]
    agent: Option<String>,
    #[serde(rename = "apiSlug", alias = "api_slug")]
    api_slug: Option<String>,
    path: Option<String>,
    method: Option<String>,
}

/// `POST /_apiosk/intent` — record what a caller was looking for.
pub async fn submit_intent(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    let submission: IntentSubmission = if body.is_empty() {
        IntentSubmission::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(parsed) => parsed,
            Err(_) => {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    serde_json::json!({
                        "ok": false,
                        "error": "invalid_json",
                        "message": "Send a JSON body, e.g. {\"intent\": \"...\", \"contact\": \"...\"}.",
                    }),
                );
            }
        }
    };

    let intent = submission.intent.as_deref().map(str::trim).unwrap_or("");
    if intent.is_empty() {
        return json_response(
            StatusCode::BAD_REQUEST,
            serde_json::json!({
                "ok": false,
                "error": "intent_required",
                "message": "Tell us what you were looking for: {\"intent\": \"...\"}. Everything else is optional.",
            }),
        );
    }

    // Client IP, validated to parse as an IP so the `::inet` insert can't fail.
    let ip_address = client_ip_from_headers(&headers);

    // Per-IP flood guard.
    let rl_key = format!("intent:{}", ip_address.as_deref().unwrap_or("unknown"));
    if let RateLimitDecision::Throttled { retry_after } =
        state.rate_limiter.check(&rl_key, INTENT_RATE_LIMIT).await
    {
        let mut response = json_response(
            StatusCode::TOO_MANY_REQUESTS,
            serde_json::json!({
                "ok": false,
                "error": "rate_limited",
                "message": "Thanks — that's a lot of intents. Please try again shortly.",
            }),
        );
        if let Ok(value) = HeaderValue::from_str(&retry_after.as_secs().max(1).to_string()) {
            response.headers_mut().insert(RETRY_AFTER, value);
        }
        return response;
    }

    let intent = truncate_chars(intent, MAX_INTENT_CHARS);
    let use_case = clean(submission.use_case.as_deref(), MAX_USE_CASE_CHARS);
    let contact = clean(submission.contact.as_deref(), MAX_CONTACT_CHARS);
    let agent = clean(submission.agent.as_deref(), MAX_AGENT_CHARS);
    let api_slug = clean(submission.api_slug.as_deref(), MAX_API_SLUG_CHARS);
    let path = clean(submission.path.as_deref(), MAX_PATH_CHARS);
    let method = submission
        .method
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| truncate_chars(&s.to_uppercase(), MAX_METHOD_CHARS));

    // Correlate to the 402: prefer the body's requestId, else the X-Request-Id
    // the traffic middleware stamped on the 402 the caller is answering.
    let request_id = submission
        .request_id
        .as_deref()
        .and_then(|s| Uuid::parse_str(s.trim()).ok())
        .or_else(|| {
            headers
                .get(REQUEST_ID_HEADER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| Uuid::parse_str(s.trim()).ok())
        });

    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| truncate_chars(s, MAX_USER_AGENT_CHARS));

    let insert = crate::db::AgentIntentInsert {
        request_id,
        api_slug: api_slug.as_deref(),
        path: path.as_deref(),
        method: method.as_deref(),
        intent: &intent,
        use_case: use_case.as_deref(),
        contact: contact.as_deref(),
        agent_name: agent.as_deref(),
        user_agent: user_agent.as_deref(),
        ip_address,
        source: "x402_survey",
    };

    match state.db.insert_agent_intent(insert).await {
        Ok(()) => json_response(
            StatusCode::OK,
            serde_json::json!({
                "ok": true,
                "message": "Thanks — logged. Apiosk prioritises building what agents actually ask for.",
            }),
        ),
        Err(err) => {
            warn!("Failed to record agent intent: {}", err);
            json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({
                    "ok": false,
                    "error": "record_failed",
                    "message": "Couldn't record your intent right now. This does not affect payment.",
                }),
            )
        }
    }
}

/// `GET /_apiosk/intent` — self-documenting: describe how to submit an intent
/// without writing a row. Handy for a human or agent poking at the URL.
pub async fn intent_info(headers: HeaderMap) -> Response<Body> {
    let submit_url = build_submit_url(&headers);
    json_response(
        StatusCode::OK,
        serde_json::json!({
            "ok": true,
            "survey": survey_value(None, &submit_url),
            "message": "POST here with a JSON body to tell Apiosk what you were looking for.",
        }),
    )
}

/// Build the absolute submit URL from the request's forwarded scheme + host,
/// falling back to a relative path when there's no Host header.
pub fn build_submit_url(headers: &HeaderMap) -> String {
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("https");
    match headers
        .get(HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(host) => format!("{scheme}://{host}/_apiosk/intent"),
        None => "/_apiosk/intent".to_string(),
    }
}

/// The `apioskIntentSurvey` object embedded in 402 JSON bodies (and returned by
/// the GET info route). `request_id` is echoed so the answer joins back to the
/// prompting 402.
fn survey_value(request_id: Option<Uuid>, submit_url: &str) -> serde_json::Value {
    serde_json::json!({
        "question": SURVEY_QUESTION,
        "optional": true,
        "requestId": request_id,
        "submit": {
            "method": "POST",
            "url": submit_url,
            "contentType": "application/json",
            "body": {
                "requestId": request_id,
                "intent": "<what you were looking for>",
                "contact": "<optional: email / wallet / handle>"
            }
        }
    })
}

/// Build a JSON response with the gateway's standard headers.
fn json_response(status: StatusCode, value: serde_json::Value) -> Response<Body> {
    let body = serde_json::to_vec(&value).unwrap_or_else(|_| b"{\"ok\":false}".to_vec());
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response.headers_mut().insert(
        HeaderName::from_static("x-apiosk-gateway"),
        HeaderValue::from_static("true"),
    );
    response
}

/// Trim, drop-if-empty, and cap an optional caller-supplied string.
fn clean(value: Option<&str>, max: usize) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| truncate_chars(s, max))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_with(host: Option<&str>, proto: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(host) = host {
            headers.insert(HOST, HeaderValue::from_str(host).unwrap());
        }
        if let Some(proto) = proto {
            headers.insert(
                HeaderName::from_static("x-forwarded-proto"),
                HeaderValue::from_str(proto).unwrap(),
            );
        }
        headers
    }

    #[test]
    fn build_submit_url_uses_forwarded_scheme_and_host() {
        let headers = headers_with(Some("gateway.apiosk.com"), Some("https"));
        assert_eq!(
            build_submit_url(&headers),
            "https://gateway.apiosk.com/_apiosk/intent"
        );
    }

    #[test]
    fn build_submit_url_defaults_scheme_then_falls_back_to_relative() {
        let headers = headers_with(Some("example.test"), None);
        assert_eq!(
            build_submit_url(&headers),
            "https://example.test/_apiosk/intent"
        );
        assert_eq!(build_submit_url(&HeaderMap::new()), "/_apiosk/intent");
    }

    #[test]
    fn client_ip_only_accepts_parseable_ips() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-forwarded-for"),
            HeaderValue::from_static("203.0.113.7, 10.0.0.1"),
        );
        assert_eq!(client_ip_from_headers(&headers), Some("203.0.113.7".to_string()));

        let mut junk = HeaderMap::new();
        junk.insert(
            HeaderName::from_static("x-forwarded-for"),
            HeaderValue::from_static("not-an-ip"),
        );
        assert_eq!(client_ip_from_headers(&junk), None);
    }

    #[test]
    fn client_ip_prefers_fly_proxy_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("fly-client-ip"),
            HeaderValue::from_static("203.0.113.9"),
        );
        headers.insert(
            HeaderName::from_static("x-forwarded-for"),
            HeaderValue::from_static("198.51.100.4, 10.0.0.1"),
        );
        assert_eq!(client_ip_from_headers(&headers), Some("203.0.113.9".to_string()));
    }
}
