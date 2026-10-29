//! Generic, reusable direct-purchase payment pointer.
//!
//! A skill-agnostic cashier that sits NEXT TO a provider's own catalogue/MCP,
//! never inside its dataflow. Two plain JSON request/response endpoints.
//!
//! `POST /pay/charge` — phase A issues an x402 challenge for a `(skill_id,
//! price)`; phase B verifies the agent's payment, settles USDC to the provider
//! wallet, and records a durable, idempotent settlement.
//!
//! `POST /pay/verify` — the provider calls this before delivery; it atomically
//! redeems a paid settlement exactly once (one payment = one delivery).
//!
//! Reusability: a "provider" is config DATA (`PAYMENT_PROVIDERS`), not code —
//! no provider is named anywhere in this module. Adding a customer is a
//! config + secret change with zero Rust changes. Every provider shares the
//! durable ledger in [`crate::db`] (`mcp_settlements`) and the gateway's
//! existing, pluggable x402 verifier ([`crate::payment`]); this module never
//! reimplements signature/settlement logic, it orchestrates the gate around it.
//!
//! Default-OFF: with zero providers configured, both endpoints return 404 and
//! the gateway behaves exactly as before. The slug proxy and this module never
//! touch each other's state; an error in a `/pay/*` handler is contained to
//! that request.

mod charge;
mod verify;

pub use charge::charge_handler;
pub use verify::verify_handler;

use std::str::FromStr;

use axum::{body::Body, http::StatusCode, response::Response};
use bigdecimal::BigDecimal;
use serde::Deserialize;
use serde_json::Value;

/// One configured direct-purchase provider. `enabled` defaults OFF: an unset or
/// `false` flag makes both `/pay/*` endpoints 404 for this id, as if the block
/// were never configured.
#[derive(Clone, Debug, Deserialize)]
pub struct PaymentProvider {
    /// Stable provider id, used only as data (config lookup, ledger column,
    /// receipt). Never branched on in handler code.
    pub id: String,

    /// Per-provider feature flag. Default OFF.
    #[serde(default)]
    pub enabled: bool,

    /// Wallet on Base that receives USDC for this provider's purchases.
    #[serde(default)]
    pub settle_wallet: Option<String>,

    #[serde(default = "default_asset")]
    pub asset: String,
    #[serde(default = "default_chain")]
    pub chain: String,
    #[serde(default = "default_mandate")]
    pub mandate: String,

    /// Per-transaction cap as a decimal string; over-cap fails closed.
    #[serde(default)]
    pub cap_per_tx_usdc: Option<String>,
    /// Per-day cap as a decimal string; over-cap fails closed.
    #[serde(default)]
    pub cap_per_day_usdc: Option<String>,

    /// Challenge time-to-live in seconds (default 900 = 15 min). A phase-B
    /// settle after this elapses is rejected.
    #[serde(default = "default_challenge_ttl")]
    pub challenge_ttl_secs: i64,

    /// Phase-2 EUR off-ramp toggle. Inert for the pilot.
    #[serde(default)]
    pub eur_offramp_enabled: bool,

    /// When true, `/pay/verify` additionally binds the redeem to the caller's
    /// `buyer_ref`. Default false (pilot).
    #[serde(default)]
    pub verify_bind_buyer: bool,

    #[serde(default)]
    pub pricing: PricingConfig,
}

impl PaymentProvider {
    /// Parsed per-transaction cap, if configured and valid.
    pub fn cap_per_tx(&self) -> Option<BigDecimal> {
        self.cap_per_tx_usdc.as_deref().and_then(parse_pos_decimal)
    }
    /// Parsed per-day cap, if configured and valid.
    pub fn cap_per_day(&self) -> Option<BigDecimal> {
        self.cap_per_day_usdc.as_deref().and_then(parse_pos_decimal)
    }
}

/// How a skill's price is derived. `trusted_request` (the pilot) takes the
/// price from the charge request and ALWAYS bounds it by `max_usdc_per_skill`
/// (and the per-tx cap). `lookup` (a documented future seam) would resolve the
/// price from a provider price endpoint instead.
#[derive(Clone, Debug, Deserialize)]
pub struct PricingConfig {
    #[serde(default = "default_pricing_source")]
    pub source: String,
    /// Hard upper bound on a single skill's price as a decimal string. The most
    /// important guardrail of the trusted-request model.
    #[serde(default)]
    pub max_usdc_per_skill: Option<String>,
}

impl PricingConfig {
    pub fn max_per_skill(&self) -> Option<BigDecimal> {
        self.max_usdc_per_skill
            .as_deref()
            .and_then(parse_pos_decimal)
    }
}

impl Default for PricingConfig {
    fn default() -> Self {
        Self {
            source: default_pricing_source(),
            max_usdc_per_skill: None,
        }
    }
}

fn default_asset() -> String {
    "USDC".to_string()
}
fn default_chain() -> String {
    "base".to_string()
}
fn default_mandate() -> String {
    "eip1271".to_string()
}
fn default_challenge_ttl() -> i64 {
    900
}
fn default_pricing_source() -> String {
    "trusted_request".to_string()
}

/// Parse a non-negative decimal string; reject negatives and garbage.
fn parse_pos_decimal(s: &str) -> Option<BigDecimal> {
    BigDecimal::from_str(s.trim())
        .ok()
        .filter(|d| d >= &BigDecimal::from(0))
}

/// Load the provider list from the environment.
///
/// Reads `PAYMENT_PROVIDERS` (a JSON array) or, failing that, the file named by
/// `PAYMENT_PROVIDERS_FILE`. Returns an empty list when neither is present or
/// when parsing fails (fail-closed: a broken config disables the pay endpoints
/// rather than taking down the slug proxy). A parse error is logged WITHOUT
/// echoing the config body, which may reference wallets/secrets.
pub fn parse_payment_providers() -> Vec<PaymentProvider> {
    let raw = std::env::var("PAYMENT_PROVIDERS").ok().or_else(|| {
        std::env::var("PAYMENT_PROVIDERS_FILE")
            .ok()
            .and_then(|path| std::fs::read_to_string(path).ok())
    });

    let Some(raw) = raw else {
        return Vec::new();
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    match serde_json::from_str::<Vec<PaymentProvider>>(trimmed) {
        Ok(list) => list,
        Err(err) => {
            tracing::error!("Failed to parse PAYMENT_PROVIDERS config: {}", err);
            Vec::new()
        }
    }
}

/// Resolve an ENABLED provider by id. A disabled or unknown id yields `None`,
/// which both handlers turn into a 404 (the endpoint is inert).
pub fn find_enabled<'a>(providers: &'a [PaymentProvider], id: &str) -> Option<&'a PaymentProvider> {
    providers.iter().find(|p| p.enabled && p.id == id)
}

// ---------------------------------------------------------------------------
// Shared response helpers
// ---------------------------------------------------------------------------

/// `{ "error": { "code": ..., "message": ... } }` with the given status.
pub(crate) fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    json_response(
        status,
        serde_json::json!({ "error": { "code": code, "message": message } }),
    )
}

pub(crate) fn json_response(status: StatusCode, body: Value) -> Response {
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| {
            let mut resp = Response::new(Body::from(r#"{"error":{"code":"internal"}}"#));
            *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            resp
        })
}

/// Disabled/unknown provider → 404, identical for both endpoints so a probe
/// can't distinguish "no such provider" from "feature off".
pub(crate) fn not_found() -> Response {
    error_response(
        StatusCode::NOT_FOUND,
        "not_found",
        "No such payment provider, or the endpoint is not enabled.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn empty_config_yields_no_provider() {
        // The migration guarantee: zero configured providers => /pay/* is inert.
        assert!(find_enabled(&[], "anything").is_none());
    }

    #[test]
    fn find_enabled_skips_disabled_and_unknown() {
        let json = r#"[
            {"id":"a","enabled":true,"settle_wallet":"0xA"},
            {"id":"b","enabled":false,"settle_wallet":"0xB"}
        ]"#;
        let providers: Vec<PaymentProvider> = serde_json::from_str(json).unwrap();
        assert!(find_enabled(&providers, "a").is_some());
        assert!(
            find_enabled(&providers, "b").is_none(),
            "disabled => None (404)"
        );
        assert!(
            find_enabled(&providers, "c").is_none(),
            "unknown => None (404)"
        );
    }

    #[test]
    fn minimal_provider_applies_defaults() {
        let providers: Vec<PaymentProvider> = serde_json::from_str(r#"[{"id":"x"}]"#).unwrap();
        let p = &providers[0];
        assert!(!p.enabled, "enabled defaults OFF");
        assert_eq!(p.asset, "USDC");
        assert_eq!(p.chain, "base");
        assert_eq!(p.mandate, "eip1271");
        assert_eq!(p.challenge_ttl_secs, 900);
        assert!(!p.verify_bind_buyer);
        assert_eq!(p.pricing.source, "trusted_request");
        assert!(p.pricing.max_per_skill().is_none());
        assert!(p.cap_per_tx().is_none());
        assert!(p.cap_per_day().is_none());
    }

    #[test]
    fn full_provider_parses_caps_and_pricing() {
        let json = r#"[{
            "id":"x","enabled":true,"settle_wallet":"0xWALLET",
            "cap_per_tx_usdc":"15","cap_per_day_usdc":"100",
            "challenge_ttl_secs":600,"verify_bind_buyer":true,
            "pricing":{"source":"trusted_request","max_usdc_per_skill":"50"}
        }]"#;
        let p = &serde_json::from_str::<Vec<PaymentProvider>>(json).unwrap()[0];
        assert!(p.enabled);
        assert_eq!(p.challenge_ttl_secs, 600);
        assert!(p.verify_bind_buyer);
        assert_eq!(p.cap_per_tx(), Some(BigDecimal::from(15)));
        assert_eq!(p.cap_per_day(), Some(BigDecimal::from(100)));
        assert_eq!(p.pricing.max_per_skill(), Some(BigDecimal::from(50)));
    }

    #[test]
    fn negative_and_garbage_decimals_are_rejected() {
        assert!(parse_pos_decimal("-5").is_none(), "negatives rejected");
        assert!(parse_pos_decimal("abc").is_none(), "garbage rejected");
        assert_eq!(parse_pos_decimal("0"), Some(BigDecimal::from(0)));
        assert_eq!(
            parse_pos_decimal(" 12.50 "),
            BigDecimal::from_str("12.50").ok()
        );
    }
}
