//! Classification helpers: request kind, outcome, and a stable internal error
//! taxonomy. These derive controlled-vocabulary values from what the middleware
//! can observe (path, status, disconnect). Deeper handlers MAY override the
//! outcome/failure-stage via a `TelemetryOutcome` response extension (see
//! `mod.rs`); when they don't, these defaults apply.
//!
//! The `err` module holds the stable error codes (§2.10 of the plan). Handlers
//! set these; the taxonomy is intentionally coarse and append-only.

use crate::telemetry::event::{FailureStage, Outcome, RequestKind};

/// Stable, human-stable error codes. Grouped by the stage that produces them.
/// Adding a code here is free; renaming one breaks historical dashboards, so
/// treat these as an append-only vocabulary.
///
/// These are consumed by the deferred payment/upstream handler hooks (§2.8–2.9),
/// which the rollout enables AFTER request telemetry (§2.14 step 6). They are
/// defined now so the taxonomy — and the DB CHECK constraints it maps to — ship
/// with migration 052. `allow(dead_code)` until those hooks are wired.
#[allow(dead_code)]
pub mod err {
    pub const AUTH_TOKEN_MISSING: &str = "auth.token_missing";
    pub const AUTH_TOKEN_UNKNOWN: &str = "auth.token_unknown";
    pub const AUTH_TOKEN_EXPIRED: &str = "auth.token_expired";
    pub const AUTH_TOKEN_REVOKED: &str = "auth.token_revoked";

    pub const PAYMENT_REQUIRED: &str = "payment.required";
    pub const PAYMENT_INVALID_SIGNATURE: &str = "payment.invalid_signature";
    pub const PAYMENT_WRONG_NETWORK: &str = "payment.wrong_network";
    pub const PAYMENT_INSUFFICIENT_FUNDS: &str = "payment.insufficient_funds";
    pub const PAYMENT_LIMIT_EXCEEDED: &str = "payment.limit_exceeded";
    pub const PAYMENT_REPLAY_DETECTED: &str = "payment.replay_detected";
    pub const PAYMENT_FACILITATOR_TIMEOUT: &str = "payment.facilitator_timeout";

    pub const UPSTREAM_CONNECT_TIMEOUT: &str = "upstream.connect_timeout";
    pub const UPSTREAM_READ_TIMEOUT: &str = "upstream.read_timeout";
    pub const UPSTREAM_INVALID_RESPONSE: &str = "upstream.invalid_response";
    pub const UPSTREAM_STATUS_5XX: &str = "upstream.status_5xx";

    pub const GATEWAY_DATABASE_UNAVAILABLE: &str = "gateway.database_unavailable";
    pub const GATEWAY_INTERNAL_ERROR: &str = "gateway.internal_error";

    pub const CLIENT_DISCONNECTED: &str = "client.disconnected";
}

/// The set of paths that are static assets / health, not real API or discovery
/// traffic. Mirrors (a superset of) the middleware's skip list.
pub fn is_asset_path(path: &str) -> bool {
    matches!(
        path,
        "/favicon.ico"
            | "/favicon.svg"
            | "/favicon-dark.ico"
            | "/favicon-dark-96x96.png"
            | "/icon-maskable-512.png"
            | "/favicon-96x96.png"
            | "/apple-touch-icon.png"
            | "/site.webmanifest"
            | "/web-app-manifest-192x192.png"
            | "/web-app-manifest-512x512.png"
            | "/logo.svg"
            | "/logo.png"
    )
}

/// True for the discovery / agent-manifest namespace the middleware used to
/// discard entirely. These now flow into `gateway_discovery_events`.
pub fn is_discovery_path(path: &str) -> bool {
    path == "/.well-known"
        || path.starts_with("/.well-known/")
        || path == "/llms.txt"
        || path == "/openapi.json"
        || path.ends_with("/openapi.json")
        || path.ends_with("/llms.txt")
        || path.ends_with("/.well-known/x402")
}

/// Classify the coarse kind of a request from its path + whether it resolved as
/// an internal gateway API route (`/v1/*`, `/types`, `/health`).
pub fn classify_request_kind(path: &str, is_gateway_api_route: bool) -> RequestKind {
    if path == "/health" {
        return RequestKind::Health;
    }
    if is_asset_path(path) {
        return RequestKind::Asset;
    }
    if is_discovery_path(path) {
        return RequestKind::Discovery;
    }
    if is_gateway_api_route {
        return RequestKind::GatewayApi;
    }
    if path.starts_with("/pay/") || path.starts_with("/connect/") || path == "/checkout" {
        return RequestKind::Checkout;
    }
    RequestKind::Api
}

/// Derive an outcome from the final status + observed disconnect. Handlers may
/// override via the `TelemetryOutcome` response extension; this is the fallback.
pub fn classify_outcome(status: u16, kind: RequestKind, client_disconnected: bool) -> Outcome {
    if client_disconnected {
        return Outcome::ClientDisconnected;
    }
    if kind == RequestKind::Discovery {
        return Outcome::Discovery;
    }
    match status {
        200..=399 => Outcome::Success,
        401 | 403 => Outcome::AuthFailed,
        402 => Outcome::PaymentRequired,
        429 => Outcome::RateLimited,
        400 | 404..=428 | 430..=499 => Outcome::ClientError,
        502..=504 => Outcome::UpstreamError,
        500..=599 => Outcome::ServerError,
        _ => Outcome::ClientError,
    }
}

/// Best-effort failure stage for a status when a handler didn't set one. Kept
/// deliberately conservative — only maps stages we can infer from status alone.
pub fn failure_stage_for_status(status: u16) -> Option<FailureStage> {
    match status {
        401 | 403 => Some(FailureStage::Authentication),
        402 => Some(FailureStage::PaymentChallenge),
        429 => Some(FailureStage::RateLimit),
        502..=504 => Some(FailureStage::Upstream),
        500 | 501 | 505..=599 => Some(FailureStage::Internal),
        _ => None,
    }
}

/// Coarse crawler / agent-family classification for discovery traffic, derived
/// from the User-Agent. Used only for grouping — never for access decisions.
pub fn classify_crawler(user_agent: Option<&str>) -> Option<String> {
    let ua = user_agent?.to_ascii_lowercase();
    let family = if ua.contains("gptbot") || ua.contains("openai") || ua.contains("chatgpt") {
        "openai"
    } else if ua.contains("claude") || ua.contains("anthropic") {
        "anthropic"
    } else if ua.contains("perplexity") {
        "perplexity"
    } else if ua.contains("googlebot") || ua.contains("google-extended") {
        "google"
    } else if ua.contains("bingbot") {
        "bing"
    } else if ua.contains("agent402") || ua.contains("x402") {
        "x402-crawler"
    } else if ua.contains("bazaar") || ua.contains("cdp") {
        "cdp-bazaar"
    } else if ua.contains("bot") || ua.contains("crawler") || ua.contains("spider") {
        "generic-bot"
    } else if ua.is_empty() {
        return None;
    } else {
        "other"
    };
    Some(family.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_paths_classify_as_discovery() {
        assert!(is_discovery_path("/.well-known/x402"));
        assert!(is_discovery_path("/.well-known/agent.json"));
        assert!(is_discovery_path("/llms.txt"));
        assert!(is_discovery_path("/openapi.json"));
        assert!(is_discovery_path("/weather-api/openapi.json"));
        assert!(!is_discovery_path("/weather-api/v1/forecast"));
        assert_eq!(
            classify_request_kind("/.well-known/x402", false),
            RequestKind::Discovery
        );
    }

    #[test]
    fn asset_and_health_kinds() {
        assert_eq!(classify_request_kind("/health", false), RequestKind::Health);
        assert_eq!(
            classify_request_kind("/favicon.ico", false),
            RequestKind::Asset
        );
        assert_eq!(
            classify_request_kind("/weather-api/v1/x", false),
            RequestKind::Api
        );
        assert_eq!(
            classify_request_kind("/v1/apis", true),
            RequestKind::GatewayApi
        );
        assert_eq!(
            classify_request_kind("/pay/charge", false),
            RequestKind::Checkout
        );
    }

    #[test]
    fn outcome_from_status() {
        assert_eq!(
            classify_outcome(200, RequestKind::Api, false),
            Outcome::Success
        );
        assert_eq!(
            classify_outcome(402, RequestKind::Api, false),
            Outcome::PaymentRequired
        );
        assert_eq!(
            classify_outcome(403, RequestKind::Api, false),
            Outcome::AuthFailed
        );
        assert_eq!(
            classify_outcome(429, RequestKind::Api, false),
            Outcome::RateLimited
        );
        assert_eq!(
            classify_outcome(404, RequestKind::Api, false),
            Outcome::ClientError
        );
        assert_eq!(
            classify_outcome(502, RequestKind::Api, false),
            Outcome::UpstreamError
        );
        assert_eq!(
            classify_outcome(500, RequestKind::Api, false),
            Outcome::ServerError
        );
        // Disconnect wins over status.
        assert_eq!(
            classify_outcome(200, RequestKind::Api, true),
            Outcome::ClientDisconnected
        );
        // Discovery kind maps to the discovery outcome regardless of 2xx/4xx.
        assert_eq!(
            classify_outcome(404, RequestKind::Discovery, false),
            Outcome::Discovery
        );
    }

    #[test]
    fn crawler_families() {
        assert_eq!(
            classify_crawler(Some("GPTBot/1.0")).as_deref(),
            Some("openai")
        );
        assert_eq!(
            classify_crawler(Some("ClaudeBot/1.0")).as_deref(),
            Some("anthropic")
        );
        assert_eq!(classify_crawler(Some("curl/8.0")).as_deref(), Some("other"));
        assert_eq!(classify_crawler(None), None);
    }
}
