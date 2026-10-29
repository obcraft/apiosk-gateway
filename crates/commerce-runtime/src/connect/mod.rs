//! Apiosk Connect track — gateway middleware, helpers + owner-facing handlers for
//! the marketplace Connect surface. Each submodule is self-contained per task:
//!   * [`api_key_auth`]  — 00-08, marketplace API-key auth middleware.
//!   * [`idempotency`]   — 00-07, idempotency middleware over the 022 store.
//!   * [`audit`]         — 00-09, append-only audit-log helper.
//!   * [`owner_auth`]    — resolve the portal (Supabase) user behind a request.
//!   * [`marketplaces`]  — 01-06, owner endpoint to mint a marketplace API key.

use axum::{
    body::Body,
    http::{header, StatusCode},
    response::Response,
};

pub mod api_key_auth;
pub mod audit;
// 04-02 skill charge paywall (GET /v1/skills/:id/charge → x402 402 with split).
pub mod charge;
pub mod embed;
// 04-01 fee split — pure; consumed by the 402 builder (04-02) + settle (04-04).
#[allow(dead_code)]
pub mod fee;
// The idempotency middleware entrypoint is enforced inline by handlers (see
// `marketplaces`/01-06) rather than as a tower layer, so some of its items are
// unused from non-test code — allow it here rather than scatter per-item attrs.
#[allow(dead_code)]
pub mod idempotency;
pub mod marketplaces;
pub mod merchant_onboarding;
// 077 — authorization-code handoff of a connect token to n8n / Make / Zapier.
pub mod oauth;
pub mod owner_auth;
// 04-07 purpose-note sanitizer — central helper; wired into storage on the
// settle path (04-04), so unused from non-test code until then.
#[allow(dead_code)]
pub mod purpose;
pub mod session;
pub mod siwe;
pub mod skills;
pub mod tokens;

/// JSON error body in the gateway's `{ "error", "message" }` shape.
pub(crate) fn json_error(status: StatusCode, code: &str, message: &str) -> Response {
    json_response(
        status,
        serde_json::json!({ "error": code, "message": message }),
    )
}

/// Build a JSON response from a serde value. Falls back to an empty body if the
/// (already-serialized) value somehow can't be framed — never panics.
pub(crate) fn json_response(status: StatusCode, value: serde_json::Value) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(value.to_string()))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}
