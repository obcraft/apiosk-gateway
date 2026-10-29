use axum::{
    body::Body,
    extract::{Json, State},
    http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use bigdecimal::BigDecimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::auth::{
    extract_agent_connect_token, extract_provider_api_key, should_strip_forwarded_header,
};
use crate::cache::CacheKey;
use crate::db::PresenceSessionUpsert;
use crate::handlers;
use crate::listing::{DispatchResult, Listing, ListingType};
use crate::payment::{extract_payment_proof, settle_payment, PaymentRequirement};
use crate::request_context::RequestContext;
use crate::AppState;

/// Internal marker set only when the Bright Data adapter returns before buyer
/// settlement. HTTP headers and upstream response bodies cannot forge it.
#[derive(Clone)]
struct BrightDataUnsettled;

mod agent_execute;
mod agent_surface;
mod apimarket;
mod crunchbase;
mod bazaar_guard;
mod browser;
mod brightdata;
mod dispatch;
pub(crate) mod domain_policy;
pub(crate) mod external_fetch;
mod forward;
mod handle_flow;
mod iban_check;
mod manifest_page;
mod payment_flow;
mod payment_page;
mod preflight;
mod presence;
mod public_sources;
mod overheid;
#[cfg(test)]
pub(crate) use public_sources::prepare as prepare_public_source_for_test;
#[allow(dead_code)] // some helpers are used only in tests / by the owner runbook
pub(crate) mod resell;
mod resources;
mod settlement;
mod siwx;
#[cfg(test)]
mod tests;
mod wallet;

use agent_execute::*;
use agent_surface::*;
use browser::*;
// The policy layer refuses purchases and must say so in the same shape every
// other refusal on this path uses. One helper, one envelope.
pub(crate) use browser::json_error_response as policy_error_response;
use dispatch::*;
use forward::*;
use manifest_page::*;
use payment_flow::*;
use payment_page::*;
use preflight::*;
use settlement::*;
use wallet::*;

pub use handle_flow::handle;
pub use presence::{presence_connect, presence_disconnect, presence_heartbeat};
pub use resources::{serve_dataset_file, serve_document_file, serve_media_file};
pub use wallet::hash_connect_token;

/// Run a payment preflight against `slug` without touching the upstream
/// API or settling any payment. Used by `POST /v1/apis/{slug}/preflight`.
///
/// `operation` is the optional endpoint path the agent wants to call (e.g.
/// `/current`); if omitted we use the API's first endpoint as the default.
/// `method` defaults to GET when omitted.
pub async fn preflight_for_slug(
    state: &Arc<AppState>,
    slug: &str,
    operation: Option<&str>,
    method: Option<&str>,
    headers: &HeaderMap<HeaderValue>,
) -> Result<Response<Body>, Response<Body>> {
    let api = state
        .cache
        .get_api(&state.db, slug)
        .await
        .map_err(|e| {
            error!("preflight: API lookup failed for {}: {}", slug, e);
            json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database_error",
                "Failed to resolve API.",
            )
        })?
        .ok_or_else(|| {
            json_error_response(
                StatusCode::NOT_FOUND,
                "api_not_found",
                &format!("API '{}' not found", slug),
            )
        })?;

    if api.status != "active" {
        return Err(json_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "api_inactive",
            &format!("API '{}' is not active", slug),
        ));
    }

    let endpoints = state.db.list_endpoints_for_api(api.id).await.map_err(|e| {
        error!("preflight: endpoint lookup failed for {}: {}", slug, e);
        json_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database_error",
            "Failed to load endpoints.",
        )
    })?;

    // Pick the endpoint the agent is asking about. Same matching rules as
    // the runtime path: case-insensitive method, leading slash optional on
    // the path. Falls back to the first endpoint when no operation given.
    let want_method = method.unwrap_or("GET").to_ascii_uppercase();
    let want_path = operation.map(|op| {
        let trimmed = op.trim();
        if trimmed.starts_with('/') {
            trimmed.to_string()
        } else {
            format!("/{}", trimmed)
        }
    });
    let endpoint = match want_path.as_deref() {
        Some(p) => endpoints
            .iter()
            .find(|e| e.method.eq_ignore_ascii_case(&want_method) && e.path.eq_ignore_ascii_case(p))
            .or_else(|| endpoints.iter().find(|e| e.path.eq_ignore_ascii_case(p)))
            .cloned(),
        None => endpoints.first().cloned(),
    };

    let resolved_method = endpoint
        .as_ref()
        .map(|e| Method::from_bytes(e.method.as_bytes()).unwrap_or(Method::GET))
        .unwrap_or(Method::GET);
    let resolved_path = endpoint
        .as_ref()
        .map(|e| e.path.clone())
        .unwrap_or_else(|| "/".to_string());

    // Build a listing context so AllowedApiPaymentRails / credits-fallback
    // decisions mirror the real call. List lookup is best-effort: if the
    // listing isn't materialized for this API the rail decision falls back
    // to "all rails allowed", which is the same default the live path uses.
    let listing = state.db.get_listing_by_slug(slug).await.ok().flatten();
    let listing_type = ListingType::Api;

    let resolved = ResolvedProxyRequest {
        api,
        listing,
        listing_type,
        preset_auth_headers: Vec::new(),
        preset_auth_query: Vec::new(),
        endpoint,
        virtual_execute_request: None,
        resolved_method,
        resolved_path,
        resell: None, // preflight/dry-run: never pays an upstream
    };

    let verdict = preflight::build_preflight_verdict(state, headers, &resolved).await;
    Ok(preflight::preflight_response(&verdict))
}

#[derive(Debug, Clone)]
struct AgentWalletSession {
    token_id: Uuid,
    wallet_id: Uuid,
    wallet_address: String,
}

#[derive(Debug, Clone, Default)]
struct TrackingContext {
    request_id: Option<Uuid>,
    session_id: Option<Uuid>,
    managed_wallet_id: Option<Uuid>,
    client_id: Option<String>,
    agent_connect_token_id: Option<Uuid>,
    /// The buyer markup this request was priced at, as resolved by
    /// `crate::service_auth::buyer_markup_bps` at the proxy's entry (only a
    /// valid app service key can ask for anything but the default). Read it
    /// through [`TrackingContext::buyer_markup_bps`]: the raw field defaults to
    /// 0, which normalises to the standard 15%.
    buyer_markup_bps: i128,
}

impl TrackingContext {
    /// The normalised buyer markup (1500 or 2000 bps) every quote on this
    /// request's paid path — price check, receipt, settlement — must use.
    fn buyer_markup_bps(&self) -> i128 {
        crate::fees::allowed_buyer_markup_bps(self.buyer_markup_bps)
    }

    fn from_request(context: Option<&RequestContext>) -> Self {
        let mut tracking = Self::default();
        if let Some(context) = context {
            tracking.request_id = Some(context.request_id);
            if let Some(ref presence) = context.presence {
                tracking.session_id = Some(presence.session_id);
                tracking.managed_wallet_id = presence.managed_wallet_id;
                tracking.client_id = Some(presence.client_id.clone());
            }
        }
        tracking
    }

    fn with_managed_wallet_id(&self, wallet_id: Uuid) -> Self {
        let mut tracking = self.clone();
        tracking.managed_wallet_id = Some(wallet_id);
        tracking
    }

    fn with_connect_token_id(&self, token_id: Uuid) -> Self {
        let mut tracking = self.clone();
        tracking.agent_connect_token_id = Some(token_id);
        tracking
    }
}

#[allow(clippy::result_large_err)]
fn take_request_or_error(
    request: &mut Option<Request<Body>>,
    api_slug: &str,
    path: &str,
    method: &Method,
    browser_mode: bool,
) -> Result<Request<Body>, Response<Body>> {
    request.take().ok_or_else(|| {
        error!(
            "Gateway request stream missing before dispatch: api_slug={}, path={}, method={}",
            api_slug, path, method
        );

        if browser_mode {
            branded_status_page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Gateway request unavailable",
                "The gateway could not keep the incoming request available for this step.",
                api_slug,
                path,
                method,
                &[],
                &[("Reason", "request_state_missing".to_string())],
                None,
            )
        } else {
            json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "request_state_missing",
                "The gateway request stream was no longer available.",
            )
        }
    })
}

#[derive(Debug, Clone, Default)]
struct EndpointPaymentDecision {
    deferred_payment: Option<DeferredApiPayment>,
    pending_credits_charge: Option<PendingCreditsCharge>,
    // `pending_fiat_charge: Option<PendingFiatCharge>` used to sit here — the
    // euro leg, settled through Mollie SEPA after the upstream call returned.
    // Apiosk keeps one payment rail now: the buyer's Apiosk balance is debited
    // in the App's Supabase and the treasury pays the provider over x402.
    /// Connect token used to authorize this call, if any. Plumbed through to
    /// `gateway_usage.agent_connect_token_id` so per-connection limits can be
    /// reconstructed on later requests.
    agent_connect_token_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
struct PendingCreditsCharge {
    user_id: Uuid,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
enum DeferredApiPayment {
    /// Approved App balance; this source has no per-call provider payout.
    AppBalance,
    AgentWalletPrepaid { session: AgentWalletSession },
}

/// Receipt produced after a paid call settles. Surfaced to the client as
/// response headers (and in agent_execute's JSON body) so the agent can see
/// *what actually happened* — which wallet paid, which on-chain tx settled,
/// and how much the gateway debited — instead of having to infer from its
/// own mental model.
#[derive(Debug, Clone)]
struct SettledPaymentReceipt {
    /// Buyer total including the fee, in USD (legacy field name).
    price_paid_usdc: f64,
    /// Listed price before the fee, in USD (legacy field name).
    price_listed_usdc: f64,
    /// On-chain hash for x402, or `apiosk_revenue_…` / `aw_internal_…`
    /// for an internal ledger reference without an on-chain transfer.
    tx_hash: String,
    /// Lower-cased 0x address of the wallet that actually paid (chosen by the
    /// gateway from the connect-token's wallet list).
    wallet_used: Option<String>,
}

impl SettledPaymentReceipt {
    fn payment_rail(&self) -> &'static str {
        if self.tx_hash.starts_with("apiosk_revenue_") {
            "apiosk_balance"
        } else {
            "x402"
        }
    }

    fn onchain_tx_hash(&self) -> Option<&str> {
        (!self.tx_hash.is_empty()
            && self.tx_hash != "unknown"
            && !self.tx_hash.starts_with("aw_internal_")
            && !self.tx_hash.starts_with("apiosk_revenue_"))
            .then_some(self.tx_hash.as_str())
    }

    fn price_headers(
        &self,
        builder: axum::http::response::Builder,
    ) -> axum::http::response::Builder {
        builder
            .header(
                "X-Apiosk-Price-Paid",
                format!("{:.6}", self.price_paid_usdc),
            )
            .header(
                "X-Apiosk-Price-Listed",
                format!("{:.6}", self.price_listed_usdc),
            )
    }
}

#[derive(Debug, Clone, Serialize)]
struct AgentOperation {
    id: String,
    key: String,
    method: String,
    path: String,
    gateway_url: String,
    execute_url: String,
    payment_required: bool,
    price_usd: f64,
    description: Option<String>,
    request_body: Option<serde_json::Value>,
    response_body: Option<serde_json::Value>,
    request_example: Option<serde_json::Value>,
    response_example: Option<serde_json::Value>,
    headers: Option<serde_json::Value>,
    query_params: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
struct AgentSurfaceContext {
    api_detail: crate::db::ApiDetail,
    listing_metadata: serde_json::Value,
    operations: Vec<AgentOperation>,
    default_operation: Option<AgentOperation>,
}

#[derive(Debug, Clone)]
struct VirtualExecuteRequest {
    operation: AgentOperation,
    upstream_method: Method,
    upstream_path: String,
    upstream_query: Option<String>,
    upstream_body_bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
struct ResolvedProxyRequest {
    api: crate::db::Api,
    listing: Option<Listing>,
    listing_type: ListingType,
    preset_auth_headers: Vec<(String, String)>,
    preset_auth_query: Vec<(String, String)>,
    endpoint: Option<crate::db::Endpoint>,
    virtual_execute_request: Option<VirtualExecuteRequest>,
    resolved_method: Method,
    resolved_path: String,
    /// Set for a resell listing (`listing_metadata.resell.enabled`): the agent-
    /// facing listing price in atomic USDC (6-dp) = the spread ceiling the
    /// gateway will pay the upstream. `None` for ordinary listings — the resell
    /// leg only fires when this is `Some`.
    resell: Option<u128>,
}

impl ResolvedProxyRequest {
    fn uses_app_balance(&self) -> bool {
        self.api.apiosk_revenue
            && matches!(self.listing_type, ListingType::Api)
            && self.resell.is_none()
    }
}

#[cfg(test)]
mod app_balance_tests;
