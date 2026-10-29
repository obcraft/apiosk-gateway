//! `POST /v1/commerce/resolve` — the public ingest behind product.js.
//!
//! product.js POSTs the scraped `{integration_id, url, name, price, currency}`. We
//! resolve the (public) integration id to its domain + wallet, then apply the two
//! anti-abuse guards for a public, unauthenticated ingest:
//!
//!   1. the request **Origin** host must match the integration's registered domain
//!      (the browser sets Origin on a cross-origin fetch and the page can't forge
//!      it — a stronger guard than the client-supplied URL), and
//!   2. the posted **path** must match the integration's URL pattern.
//!
//! On success we upsert a product (wallet snapshotted server-side) and return the
//! `charge_url` — the x402 paywall the page is now buyable through.

use std::sync::Arc;

use axum::{
    extract::{Json, State},
    http::{header, HeaderMap, StatusCode},
    response::Response,
};
use bigdecimal::{BigDecimal, ToPrimitive};
use serde::Deserialize;
use serde_json::json;
use std::str::FromStr;
use tracing::warn;
use uuid::Uuid;

use super::{
    build_charge_url, host_matches, json_error, json_response, path_matches_pattern,
    GATEWAY_PUBLIC_BASE,
};
use crate::AppState;

/// Upper bound on a resolved price: 1,000,000 USDC in minor units. A scrape that
/// yields something larger is far likelier to be a parse error than a real price.
const MAX_PRICE_MINOR_UNITS: i64 = 1_000_000 * 1_000_000;

const MAX_NAME_LEN: usize = 200;
const MAX_URL_LEN: usize = 2048;

#[derive(Deserialize)]
pub struct ResolveRequest {
    integration_id: Uuid,
    /// The product page path, e.g. `/products/example` (product.js posts
    /// `location.pathname`).
    url: String,
    name: String,
    /// Scraped price as a decimal string, e.g. `"19.95"`.
    price: String,
    /// Scraped currency (informational in v1 — always charged 1:1 as USDC).
    #[serde(default)]
    #[allow(dead_code)]
    currency: Option<String>,
}

pub async fn resolve_product(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Json<ResolveRequest>,
) -> Response {
    let req = body.0;

    // Resolve the (public) integration id → domain + wallet. 404 covers both
    // unknown and disabled, so a caller can't probe which integrations exist.
    let integration = match state
        .db
        .lookup_webshop_integration(req.integration_id)
        .await
    {
        Ok(Some(i)) if i.status == "active" => i,
        Ok(_) => {
            return json_error(
                StatusCode::NOT_FOUND,
                "integration_not_found",
                "No active integration for this id.",
            )
        }
        Err(err) => {
            warn!("commerce integration lookup failed: {err}");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "lookup_failed",
                "Could not load the integration.",
            );
        }
    };

    // Guard 1: the browser Origin host must be the registered domain. Fall back to
    // Referer if Origin is absent; if neither is present or usable, refuse — a
    // real product.js load always carries one.
    let Some(host) = request_host(&headers) else {
        return json_error(
            StatusCode::FORBIDDEN,
            "origin_required",
            "Missing Origin/Referer; product.js must load on your registered domain.",
        );
    };
    if !host_matches(&integration.domain, &host) {
        return json_error(
            StatusCode::FORBIDDEN,
            "domain_mismatch",
            "This request did not come from the integration's registered domain.",
        );
    }

    // Guard 2: the path must match the integration's product-URL pattern.
    let url = req.url.trim();
    if url.len() > MAX_URL_LEN || !url.starts_with('/') {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_url",
            "url must be an absolute path, e.g. /products/example.",
        );
    }
    if !path_matches_pattern(&integration.product_url_pattern, url) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "url_not_matched",
            "This path does not match the integration's product URL pattern.",
        );
    }

    // Validate + bound the scraped product fields.
    let name = req.name.trim();
    if name.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "missing_name",
            "Could not read a product name from the page.",
        );
    }
    let name: String = name.chars().take(MAX_NAME_LEN).collect();

    let Some(price_amount) = price_to_minor_units(&req.price) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_price",
            "Could not read a positive price from the page.",
        );
    };

    // Upsert (idempotent on integration_id + url). Wallet is snapshotted from the
    // integration, never taken from the payload.
    let product_id = match state
        .db
        .upsert_commerce_product(
            integration.id,
            integration.owner_id,
            url,
            &name,
            price_amount,
            &integration.wallet_address,
        )
        .await
    {
        Ok(id) => id,
        Err(err) => {
            warn!("commerce product upsert failed: {err}");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "product_persist_failed",
                "Could not create the product.",
            );
        }
    };

    json_response(
        StatusCode::OK,
        json!({
            "product_id": product_id,
            "charge_url": build_charge_url(GATEWAY_PUBLIC_BASE, product_id),
            "price_amount": price_amount,
            "price_currency": "USDC",
        }),
    )
}

/// The requesting host: prefer the `Origin` header (a bare origin the page can't
/// forge on a cross-origin fetch), fall back to `Referer`. Returns the host only
/// (no scheme, path or port). Pure.
fn request_host(headers: &HeaderMap) -> Option<String> {
    let raw = headers
        .get(header::ORIGIN)
        .or_else(|| headers.get(header::REFERER))
        .and_then(|v| v.to_str().ok())?;
    host_from_url(raw)
}

/// Extract the host from an origin/URL string: `https://shop.com:443/x` → `shop.com`.
/// Pure; `None` when there's no `scheme://host`.
fn host_from_url(raw: &str) -> Option<String> {
    let after_scheme = raw.split("://").nth(1)?;
    let authority = after_scheme.split('/').next().unwrap_or(after_scheme);
    let host = authority.split(':').next().unwrap_or(authority).trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// Parse a scraped decimal price string to USDC minor units (6 decimals), charged
/// 1:1 with the scraped fiat amount. Rejects non-positive, unparseable, or
/// absurdly large values. Truncates precision beyond 6 decimals. Pure.
fn price_to_minor_units(s: &str) -> Option<i64> {
    let decimal = BigDecimal::from_str(s.trim()).ok()?;
    // Checking positivity on the integer minor units (rather than the BigDecimal)
    // covers negative AND zero prices in one guard: both floor to `minor <= 0`.
    let minor = (&decimal * BigDecimal::from(1_000_000)).to_i64()?;
    if minor <= 0 || minor > MAX_PRICE_MINOR_UNITS {
        return None;
    }
    Some(minor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_from_url_variants() {
        assert_eq!(
            host_from_url("https://shop.com").as_deref(),
            Some("shop.com")
        );
        assert_eq!(
            host_from_url("https://shop.com:443/products/x").as_deref(),
            Some("shop.com")
        );
        assert_eq!(
            host_from_url("http://www.shop.com/").as_deref(),
            Some("www.shop.com")
        );
        assert_eq!(host_from_url("not-a-url"), None);
        assert_eq!(host_from_url("https://"), None);
    }

    #[test]
    fn price_parses_1to1_to_usdc_minor_units() {
        assert_eq!(price_to_minor_units("19.95"), Some(19_950_000));
        assert_eq!(price_to_minor_units("1"), Some(1_000_000));
        assert_eq!(price_to_minor_units(" 0.01 "), Some(10_000));
        // Precision beyond 6 dp is truncated, not rounded up.
        assert_eq!(price_to_minor_units("1.2345678"), Some(1_234_567));
    }

    #[test]
    fn price_rejects_nonpositive_garbage_and_absurd() {
        assert_eq!(price_to_minor_units("0"), None);
        assert_eq!(price_to_minor_units("-5"), None);
        assert_eq!(price_to_minor_units(""), None);
        assert_eq!(price_to_minor_units("free"), None);
        assert_eq!(price_to_minor_units("2000000"), None); // > 1,000,000 USDC cap
    }
}
