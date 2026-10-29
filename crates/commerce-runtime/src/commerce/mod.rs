//! Apiosk Commerce — turn an external webshop product page into an x402 payment
//! endpoint via a single `<script src="…/product.js">` snippet.
//!
//!   * [`resolve`] — public `POST /v1/commerce/resolve`. product.js POSTs the
//!     scraped `{integration_id, url, name, price, currency}`. The gateway checks
//!     the request Origin against the integration's registered domain + the path
//!     against its URL pattern, upserts a product, and returns `{product_id,
//!     charge_url}`.
//!   * [`charge`] — public `GET /commerce/products/:id`. The x402 paywall: no
//!     `X-PAYMENT` header → a priced 402 (phase 1). Settlement + the post-purchase
//!     webhook are phase 2.
//!
//! The gateway is otherwise a byte-forwarding proxy; the product extraction lives
//! client-side in product.js, and the only server-trusted input here is the
//! integration id (resolved server-side to a domain + wallet).

use axum::{
    body::Body,
    http::{header, StatusCode},
    response::Response,
};

pub mod charge;
pub mod resolve;

/// Canonical public origin of the gateway — the base of the returned `charge_url`.
/// Mirrors `gateway_routes::GATEWAY_BASE_URL` and the connect skill charge base.
pub(crate) const GATEWAY_PUBLIC_BASE: &str = "https://gateway.apiosk.com";

// `APIOSK_FEE_BPS` (200, mirroring migration 027) lived here. A commerce
// merchant is a MANAGED provider under the hybrid fee model — Apiosk operates
// their payment endpoint, paywall and settlement — so their split now comes
// from `crate::fees::quote`, which owns both sides of it. A second rate here
// would be a second thing to keep in step.

/// JSON error body in the gateway's `{ "error", "message" }` shape.
pub(crate) fn json_error(status: StatusCode, code: &str, message: &str) -> Response {
    json_response(
        status,
        serde_json::json!({ "error": code, "message": message }),
    )
}

/// Build a JSON response from a serde value. Never panics — falls back to an empty
/// body if the already-serialized value somehow can't be framed.
pub(crate) fn json_response(status: StatusCode, value: serde_json::Value) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(value.to_string()))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

/// The charge URL for a product (`GET …/commerce/products/{id}`, the x402 paywall).
/// Shared with the charge module so the resolve response and the paywall's own
/// `resource` field never drift. Pure.
pub(crate) fn build_charge_url(base: &str, product_id: uuid::Uuid) -> String {
    format!(
        "{}/commerce/products/{}",
        base.trim_end_matches('/'),
        product_id
    )
}

/// Compare a request host against an integration's registered domain: both
/// lowercased and stripped of a leading `www.`, then equal. The registered domain
/// is already normalised (the create RPC strips scheme/path/www); this normalises
/// the incoming host the same way so `www.shop.com` matches `shop.com`. Pure.
pub(crate) fn host_matches(registered_domain: &str, host: &str) -> bool {
    fn norm(s: &str) -> &str {
        s.trim().trim_start_matches("www.")
    }
    let host = host.split(':').next().unwrap_or(host); // drop any :port
    !registered_domain.is_empty()
        && norm(&registered_domain.to_ascii_lowercase()) == norm(&host.to_ascii_lowercase())
}

/// Match a URL path against a glob pattern where `*` matches any run of
/// characters (including `/`). `/products/*` matches `/products/abc` and
/// `/products/a/b`; `/p/*/buy` matches `/p/x/buy`. Anchored at both ends. Pure.
pub(crate) fn path_matches_pattern(pattern: &str, path: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();

    // No wildcard → exact match.
    if parts.len() == 1 {
        return pattern == path;
    }

    let mut cursor = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            // First literal must be a prefix.
            if !path[cursor..].starts_with(part) {
                return false;
            }
            cursor += part.len();
        } else if i == parts.len() - 1 {
            // Last literal must be a suffix, at or after the cursor.
            return path.len() >= cursor && path[cursor..].ends_with(part);
        } else {
            // Middle literal must appear in order.
            match path[cursor..].find(part) {
                Some(pos) => cursor += pos + part.len(),
                None => return false,
            }
        }
    }
    // Pattern ended on a `*` (empty trailing part) → the rest of the path matches.
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_matches_ignores_www_case_and_port() {
        assert!(host_matches("shop.example.com", "shop.example.com"));
        assert!(host_matches("shop.example.com", "www.shop.example.com"));
        assert!(host_matches("shop.example.com", "Shop.Example.com"));
        assert!(host_matches("shop.example.com", "shop.example.com:443"));
        assert!(!host_matches("shop.example.com", "evil.com"));
        assert!(!host_matches("shop.example.com", "sub.shop.example.com"));
        assert!(!host_matches("", "shop.example.com"));
    }

    #[test]
    fn path_pattern_prefix_wildcard() {
        assert!(path_matches_pattern("/products/*", "/products/example"));
        assert!(path_matches_pattern("/products/*", "/products/a/b/c"));
        assert!(path_matches_pattern("/products/*", "/products/"));
        assert!(!path_matches_pattern("/products/*", "/product/example"));
        assert!(!path_matches_pattern("/products/*", "/shop/products/x"));
    }

    #[test]
    fn path_pattern_middle_wildcard() {
        assert!(path_matches_pattern("/p/*/buy", "/p/x/buy"));
        assert!(path_matches_pattern("/p/*/buy", "/p/a/b/buy"));
        assert!(!path_matches_pattern("/p/*/buy", "/p/x/sell"));
        assert!(!path_matches_pattern("/p/*/buy", "/p/buy"));
    }

    #[test]
    fn path_pattern_no_wildcard_is_exact() {
        assert!(path_matches_pattern("/checkout", "/checkout"));
        assert!(!path_matches_pattern("/checkout", "/checkout/now"));
    }

    #[test]
    fn charge_url_points_at_the_product_charge_endpoint() {
        let id = uuid::Uuid::from_u128(0xABC);
        assert_eq!(
            build_charge_url("https://gateway.apiosk.com", id),
            format!("https://gateway.apiosk.com/commerce/products/{id}")
        );
        assert_eq!(
            build_charge_url("https://gateway.apiosk.com/", id),
            format!("https://gateway.apiosk.com/commerce/products/{id}")
        );
    }
}
