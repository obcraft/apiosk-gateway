//! Request and response helpers shared by the handlers: one JSON body
//! builder, the `{"ok": false, …}` refusal shape, one per-IP throttle and one
//! way to read the caller's address and headers.

use axum::{
    body::Body,
    http::{
        header::{CACHE_CONTROL, CONTENT_TYPE, RETRY_AFTER},
        HeaderMap, HeaderValue, StatusCode,
    },
    response::Response,
};
use serde_json::{json, Value};

use crate::{
    rate_limit::{RateLimitDecision, RateLimitPolicy},
    AppState,
};

/// A JSON body with `status`. Never panics: the builder cannot fail for these
/// inputs, and if it ever did the answer degrades to an empty body.
pub(crate) fn json(status: StatusCode, body: Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

/// [`json`] with a public cache policy, for documents that change on a timer
/// rather than per request.
pub(crate) fn json_cached(status: StatusCode, body: Value, cache_control: &'static str) -> Response<Body> {
    let mut response = json(status, body);
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static(cache_control));
    response
}

/// The refusal the discovery, sources and decisions routes answer with.
pub(crate) fn refusal(status: StatusCode, code: &str, message: &str) -> Response<Body> {
    json(status, json!({ "ok": false, "error": code, "message": message }))
}

/// A trimmed, non-empty header value.
pub(crate) fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// The caller's address from the trusted forwarding headers (Fly's first,
/// then the forwarded chain), or `unknown` when none parses as an IP — so a
/// malformed header keys one shared bucket instead of its own.
pub(crate) fn client_ip(headers: &HeaderMap) -> String {
    crate::traffic::client_ip_from_headers(headers).unwrap_or_else(|| "unknown".to_string())
}

/// Per-IP fixed-window throttle for an open route. `Some(429)` carrying
/// `Retry-After` when `scope:ip` is over `policy`; `None` to proceed.
pub(crate) async fn throttled_by_ip(
    state: &AppState,
    headers: &HeaderMap,
    scope: &str,
    policy: RateLimitPolicy,
    message: &str,
) -> Option<Response<Body>> {
    let ip = client_ip(headers);
    match state.rate_limiter.check(&format!("{scope}:{ip}"), policy).await {
        RateLimitDecision::Allowed { .. } => None,
        RateLimitDecision::Throttled { retry_after } => {
            let mut response = refusal(StatusCode::TOO_MANY_REQUESTS, "rate_limited", message);
            if let Ok(value) = HeaderValue::from_str(&retry_after.as_secs().max(1).to_string()) {
                response.headers_mut().insert(RETRY_AFTER, value);
            }
            Some(response)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderName;

    #[test]
    fn client_ip_prefers_the_fly_proxy_header_and_falls_back_to_unknown() {
        let mut headers = HeaderMap::new();
        headers.insert(HeaderName::from_static("fly-client-ip"), HeaderValue::from_static("203.0.113.9"));
        headers.insert(HeaderName::from_static("x-forwarded-for"), HeaderValue::from_static("198.51.100.4, 10.0.0.1"));
        assert_eq!(client_ip(&headers), "203.0.113.9");
        let mut forwarded = HeaderMap::new();
        forwarded.insert(HeaderName::from_static("x-forwarded-for"), HeaderValue::from_static("198.51.100.4, 10.0.0.1"));
        assert_eq!(client_ip(&forwarded), "198.51.100.4");
        let mut garbage = HeaderMap::new();
        garbage.insert(HeaderName::from_static("x-forwarded-for"), HeaderValue::from_static("not-an-ip"));
        assert_eq!(client_ip(&garbage), "unknown");
        assert_eq!(client_ip(&HeaderMap::new()), "unknown");
    }

    #[test]
    fn refusals_carry_the_shared_shape() {
        let response = refusal(StatusCode::BAD_REQUEST, "bad", "say why");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
        let cached = json_cached(StatusCode::OK, json!({}), "public, max-age=60");
        assert_eq!(cached.headers()[CACHE_CONTROL], "public, max-age=60");
    }
}
