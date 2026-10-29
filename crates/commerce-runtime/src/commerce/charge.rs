//! `GET /commerce/products/:id` — the commerce x402 paywall.
//!
//! Public entry a paying agent hits for a webshop product. With no `X-PAYMENT`
//! proof it returns an x402 `402` whose `payTo` is the PLATFORM wallet and whose
//! body carries the hybrid split — the same settlement model as the connect
//! skill charge (04-02).
//!
//! A commerce merchant is a MANAGED provider, so the buyer is charged
//! `list + 15%` and the merchant receives `list - 10%`. The gross quoted in the
//! 402 is therefore the BUYER TOTAL, not the product's list price.
//!
//! Phase 1 stops at the challenge: a request that DOES carry `X-PAYMENT` gets a
//! clear "not available yet" 503 rather than a misleading second 402. Verifying +
//! settling the payment and firing the post-purchase webhook to the webshop are
//! phase 2 (the product listing's delivery path still has open gaps).

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::Response,
};
use serde_json::{json, Value};
use tracing::warn;
use uuid::Uuid;

use super::{build_charge_url, json_error, json_response, GATEWAY_PUBLIC_BASE};
use crate::connect::fee::Split;
use crate::payment::PAYMENT_HEADER;
use crate::AppState;

pub async fn charge_product(
    State(state): State<Arc<AppState>>,
    Path(product_id): Path<Uuid>,
    headers: HeaderMap,
) -> Response {
    // Settle model (mirrors connect 04-02): the gross is paid to the PLATFORM
    // wallet via the existing facilitator; the merchant/Apiosk split is disbursed
    // at payout. `payTo` is therefore the platform wallet (always configured); we
    // only need the settlement asset (USDC), which must be enabled on the gateway.
    let pay_to = state.config.platform_wallet.as_str();
    let Some(asset) = state.config.connect_settlement_asset.as_deref() else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "settlement_not_configured",
            "Commerce charging is not enabled on this gateway yet.",
        );
    };

    let product = match state.db.get_commerce_product_charge(product_id).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            return json_error(
                StatusCode::NOT_FOUND,
                "product_not_found",
                "Product not found.",
            )
        }
        Err(err) => {
            warn!("commerce product charge lookup failed: {err}");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "lookup_failed",
                "Could not load the product.",
            );
        }
    };

    // The hybrid model, in the shape `compute_split` can express: gross up the
    // list price by the buyer's 15% first, then take a fee out of that gross
    // large enough to leave the merchant `list - 10%`.
    //
    // `compute_split` alone cannot express "buyer +15% AND seller -10%" — it
    // only takes fees out of a single gross — so the gross-up happens here and
    // the split stays the one place the on-chain division is computed.
    // `fees::quote` is the source of both numbers; the fee passed to
    // `compute_split` is derived from it rather than from a fixed rate, because
    // the Apiosk share of gross (25/115 = 2173.91 bps) is not a whole number of
    // basis points.
    let quote = crate::fees::quote(
        product.price_amount as i128,
        crate::fees::ProviderClass::Managed,
    );
    let gross = quote.buyer_total_atomic as i64;
    let split = Split {
        seller_amount: quote.provider_payout_atomic as i64,
        marketplace_fee: 0,
        apiosk_fee: quote.apiosk_take_atomic as i64,
    };

    // Phase 2: a submitted payment would be verified + settled + trigger the
    // webhook here. Until that lands, say so plainly instead of a second 402.
    if headers.contains_key(PAYMENT_HEADER) {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "purchase_not_available",
            "This product is buyable but purchase settlement is not enabled yet.",
        );
    }

    // The paywall: a priced 402 the agent (or an x402-aware client) can pay.
    let resource = build_charge_url(GATEWAY_PUBLIC_BASE, product_id);
    let body = build_charge_402_body(&Charge402 {
        product_id,
        gross,
        network: &state.config.connect_network,
        asset,
        pay_to,
        resource: &resource,
        seller_wallet: &product.destination_wallet,
        split,
    });
    payment_required(body, gross)
}

/// Inputs for the pure 402-body builder. Borrowed strings so the handler passes
/// config/db slices without cloning.
struct Charge402<'a> {
    product_id: Uuid,
    gross: i64,
    network: &'a str,
    asset: &'a str,
    pay_to: &'a str,
    resource: &'a str,
    seller_wallet: &'a str,
    split: Split,
}

/// Build the x402 `402` body: a spec-shaped `accepts` requirement (pay `gross`
/// USDC to the platform wallet) plus an `apiosk` block with the informational
/// split. Pure + unit-tested; the split comes solely from [`compute_split`].
fn build_charge_402_body(p: &Charge402) -> Value {
    json!({
        "x402Version": 1,
        "accepts": [{
            "scheme": "exact",
            "network": p.network,
            "maxAmountRequired": p.gross.to_string(),
            "resource": p.resource,
            "description": format!("Apiosk Commerce product {}", p.product_id),
            "mimeType": "application/json",
            "payTo": p.pay_to,
            "maxTimeoutSeconds": 300,
            "asset": p.asset,
            // EIP-712 domain for USDC (name() = "USD Coin", version "2").
            "extra": { "name": "USD Coin", "version": "2" }
        }],
        "apiosk": {
            "product_id": p.product_id,
            "split": {
                "seller": p.seller_wallet,
                "seller_amount": p.split.seller_amount,
                "apiosk_fee": p.split.apiosk_fee
            }
        }
    })
}

/// Frame the body as a `402 Payment Required` with the gateway's x402 headers
/// (see gateway/CLAUDE.md). Never panics — falls back to static header values.
fn payment_required(body: Value, amount: i64) -> Response {
    let mut resp = json_response(StatusCode::PAYMENT_REQUIRED, body);
    let h = resp.headers_mut();
    h.insert(
        "X-Payment-Amount",
        HeaderValue::from_str(&amount.to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    h.insert("X-Payment-Currency", HeaderValue::from_static("USDC"));
    h.insert("X-Payment-Protocol", HeaderValue::from_static("x402"));
    h.insert(
        "X-Request-Id",
        HeaderValue::from_str(&Uuid::new_v4().to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("commerce-charge")),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A $100 product under the hybrid model: the buyer pays $110, the merchant
    /// receives $90, Apiosk keeps $20.
    fn sample() -> Value {
        let quote = crate::fees::quote(100_000000, crate::fees::ProviderClass::Managed);
        build_charge_402_body(&Charge402 {
            product_id: Uuid::from_u128(0x5111),
            gross: quote.buyer_total_atomic as i64,
            network: "base",
            asset: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
            pay_to: "0xPLATFORM",
            resource: "https://gateway.apiosk.com/commerce/products/x",
            seller_wallet: "0xSELLER",
            split: Split {
                seller_amount: quote.provider_payout_atomic as i64,
                marketplace_fee: 0,
                apiosk_fee: quote.apiosk_take_atomic as i64,
            },
        })
    }

    #[test]
    fn body_advertises_gross_payto_and_asset() {
        let b = sample();
        let acc = &b["accepts"][0];
        assert_eq!(acc["scheme"], "exact");
        assert_eq!(acc["network"], "base");
        // The BUYER TOTAL — list + 15% — not the product's list price. Quoting
        // the list price here would leave the Apiosk fee unfunded.
        assert_eq!(acc["maxAmountRequired"], "115000000"); // string, minor units
        assert_eq!(acc["payTo"], "0xPLATFORM");
        assert_eq!(acc["asset"], "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
        assert_eq!(b["x402Version"], 1);
    }

    #[test]
    fn split_is_the_hybrid_managed_split() {
        let b = sample();
        let s = &b["apiosk"]["split"];
        assert_eq!(s["seller"], "0xSELLER");
        // Was 98/2 out of a $100 gross. Now $115 gross: merchant keeps
        // list - 10%, Apiosk keeps the buyer's 15% plus the merchant's 10%.
        assert_eq!(s["seller_amount"].as_i64().unwrap(), 90_000000);
        assert_eq!(s["apiosk_fee"].as_i64().unwrap(), 25_000000);
        // The legs must re-sum to what the buyer is charged.
        assert_eq!(
            s["seller_amount"].as_i64().unwrap() + s["apiosk_fee"].as_i64().unwrap(),
            115_000000
        );
    }
}
