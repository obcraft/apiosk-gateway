use std::{future::Future, sync::Arc};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, Request, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{json, Value};
use tracing::{error, warn};

use crate::{proxy, AppState};

const FAVICON: &[u8] = include_bytes!("../favicon.ico");
const FAVICON_DARK: &[u8] = include_bytes!("../favicon-dark.ico");
const FAVICON_SVG: &[u8] = include_bytes!("../favicon.svg");
const FAVICON_96: &[u8] = include_bytes!("../favicon-96x96.png");
const FAVICON_DARK_96: &[u8] = include_bytes!("../favicon-dark-96x96.png");
const ICON_MASKABLE: &[u8] = include_bytes!("../icon-maskable-512.png");
const APPLE_TOUCH_ICON: &[u8] = include_bytes!("../apple-touch-icon.png");
const WEB_MANIFEST: &str = include_str!("../site.webmanifest");
const WEB_APP_MANIFEST_192: &[u8] = include_bytes!("../web-app-manifest-192x192.png");
const WEB_APP_MANIFEST_512: &[u8] = include_bytes!("../web-app-manifest-512x512.png");
const LOGO: &[u8] = include_bytes!("../logo.svg");
const LOGO_PNG: &[u8] = include_bytes!("../logo.png");
/// The "Connect with Apiosk" embed script (045). Static, dependency-free; a
/// marketplace loads it with `<script src="…/embed.js" data-apiosk-key="apk_pub_…">`.
const EMBED_JS: &str = include_str!("../embed.js");

/// The WebMCP shim served at `/webmcp.js`. A provider loads it with
/// `<script src="…/webmcp.js" data-apiosk-slug="…">` and their listing's
/// endpoints become tools a browser agent can call.
const WEBMCP_JS: &str = include_str!("../webmcp.js");

/// The payment popup served at `/_apiosk/pay`, opened by `webmcp.js` on a 402.
/// Lives on the gateway origin on purpose: it holds the buyer's session and
/// connect token so the provider's page never can.
const PAY_POPUP_HTML: &str = include_str!("../pay-popup.html");

/// The Apiosk Commerce product script. Static, dependency-free; a webshop loads it
/// with `<script src="…/product.js" data-integration-id="…">` to make a product
/// page machine-buyable via `/v1/commerce/resolve`.
const PRODUCT_JS: &str = include_str!("../product.js");

/// Canonical public origin of the gateway. Hardcoded to match the rest of the
/// codebase (e.g. agent metadata builds gateway URLs from the same base), so
/// discovery manifests advertise stable, absolute URLs regardless of the Host
/// header a crawler happens to use.
const GATEWAY_BASE_URL: &str = "https://gateway.apiosk.com";

/// One canonical answer to "what is this", in one sentence.
///
/// Every discovery surface used to open with "pay-per-call API gateway", which
/// described the settlement half and left the reader to discover the
/// comparison half by following a link. A crawler that reads one document and
/// caches it never follows that link, so the sentence it keeps is the sentence
/// that has to carry the product. Kept short enough for an HTML meta
/// description (under 160 characters) so the same words survive everywhere.
///
/// Used by: the HTML meta description, `/.well-known/ai-plugin.json`,
/// `/.well-known/agent.json`, `/.well-known/mcp.json`, `/.well-known/x402`,
/// `/llms.txt`, `/openapi.json` and the root index. Change it here.
pub(crate) const POSITIONING_SHORT: &str = "The comparison layer for agentic commerce: every provider that can do a job, priced side by side, paid per request over x402.";

/// The same claim with the mechanism and the entry points, for readers that
/// budget more than a sentence: `description_for_model`, the OpenAPI summary
/// and the llms.txt preamble.
pub(crate) const POSITIONING_LONG: &str = "Apiosk is the comparison layer for agentic commerce. Listings are grouped by the job they perform, so an agent that needs a task done gets every provider that can do it in one call, priced side by side, with an input mapping that makes the offers substitutable. Three chained endpoints do the comparing, and none of them charges anything: GET /v1/discover (what can perform this task, with stable candidate ids), GET /v1/compare (how those candidates perform against your requirements, scored 0-100 with the weights and per-dimension contributions that produced the number, so it can be recomputed rather than trusted) and GET /v1/decide (one provider back, the rule that picked it, every rejection with the constraint that caused it, and the runners-up in order). Browse the jobs with GET /v1/capabilities; get the candidates for one, cheapest first, with GET /v1/capabilities/{slug}. The call you pick is paid per request over x402 (HTTP 402 + USDC), with no accounts, API keys or SDKs. Describe a need in plain words at GET /v1/search and get the providers ranked. Price, settlement rail and input compatibility are compared for every offer; measured latency and success rate are published for the listings Apiosk has proxied enough calls of, and explicitly absent for the rest. Result quality is not measured anywhere.";

pub async fn root(State(state): State<Arc<AppState>>, headers: axum::http::HeaderMap) -> Response {
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let is_browser =
        accept.contains("text/html") || (accept.contains("*/*") && user_agent.contains("Mozilla"));

    // Humans get the gateway landing page: what the gateway does, then the
    // four ways into it (APIs, providers, types, docs). Agents keep the JSON
    // discovery document below, unchanged.
    if is_browser {
        // Counts only. The landing page is the most-linked URL on the gateway
        // and says nothing that depends on the database, so a directory query
        // that fails costs it its stat line — never its availability.
        let providers = crate::v1_routes::provider_directory(&state, None)
            .await
            .unwrap_or_else(|status| {
                error!("Home directory counts unavailable: {}", status);
                Vec::new()
            });

        let html = crate::v1_routes::render_home_html(&providers);
        return Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/html; charset=utf-8")
            .body(Body::from(html))
            .unwrap();
    }

    let json_body = json!({
        "name": "Apiosk Gateway",
        "status": "ok",
        "message": "Apiosk gateway is up and running. See docs below to explore and publish APIs.",
        "docs": {
            "overview": POSITIONING_LONG,
            "agent_native": true,
            "mcp_native": true,
            "protocol": "x402",
            "base_url": "https://gateway.apiosk.com",
            "capabilities": [
                "Compare every provider that can perform a job, priced side by side (GET /v1/capabilities)",
                "Record which candidate you chose and why, so the alternatives you rejected are not lost (POST /v1/decisions)",
                "Discover verified APIs and their pricing",
                "Read MCP-style, agent-native metadata for any API",
                "Call any API via a uniform /execute contract or a transparent proxy path",
                "Settle payment per request via x402 (USDC on Base)",
                "Publish and manage your own APIs with wallet-signed requests"
            ],
            "payment": {
                "protocol": "x402",
                "challenge_status": 402,
                "settlement_asset": "USDC",
                "settlement_network": "Base",
                "proof_header": "PAYMENT-SIGNATURE",
                "proof_header_legacy": "X-Payment",
                "x402Version": 2,
                "challenge_headers": ["PAYMENT-REQUIRED", "X-Payment-Amount", "X-Payment-Currency", "X-Payment-Protocol"],
                "rails": ["x402"],
                "notes": "Paid endpoints return HTTP 402 with BOTH dialects: the base64-encoded PAYMENT-REQUIRED header carries x402 v2 accepts[] (amount in atomic units, CAIP-2 network), while the JSON body carries x402 v1 accepts[] (maxAmountRequired, bare network names like 'base'/'polygon') for x402-fetch SDK clients. Supply the signed payment via the PAYMENT-SIGNATURE header (the legacy X-Payment header is still accepted for x402 v1 clients). Free endpoints forward immediately."
            },
            "discovery": {
                "well_known_index": "/.well-known/",
                "x402_discovery": "/.well-known/x402",
                "ai_plugin_manifest": "/.well-known/ai-plugin.json",
                "agent_card": "/.well-known/agent.json",
                "mcp_discovery": "/.well-known/mcp.json",
                "llms_txt": "/llms.txt",
                "openapi": "/openapi.json",
                "api_catalog": "/v1/apis",
                "api_detail": "/v1/apis/<slug>",
                "discover": "/v1/discover?q=<what you need, in words> — step 1: what can perform this task, in the reviewed catalogue and across the wider x402 ecosystem",
                "compare": "/v1/compare?candidates=<ids from discover> — step 2: how they perform against your requirements, scored with the weights shown",
                "decide": "/v1/decide?candidates=<ids from discover> — step 3: one provider, the rule that picked it, and every rejection",
                "search": "/v1/search?q=<what you need, in words>",
                "network": "/v1/network — who is on the network: every name and mark, the reviewed catalogue and the external storefronts as one list. Identity only, no prices",
                "sources": "/v1/sources — the external x402 indexes swept, and which can be listed whole",
                "source_apis": "/v1/sources/<id>/apis — the providers one index publishes",
                "source_api_endpoints": "/v1/sources/<id>/apis/<host> — every endpoint of one provider, with its declared price, rail, input schema and whether Apiosk can settle it",
                "select": "/v1/select?q=<what you need>&max_price=&prefer=cheapest|fastest|most_reliable",
                "capabilities": "/v1/capabilities",
                "capability_candidates": "/v1/capabilities/<capability-slug>",
                "report_decision": "/v1/decisions",
                "report_outcome": "/v1/decisions/<decision-id>/outcome",
                "provider_directory": "/v1/providers",
                "provider_listings": "/v1/providers/<username>",
                "browse_by_type": "/types",
                "api_metadata": "/<api-slug>/metadata",
                "uniform_execution": "/<api-slug>/execute"
            },
            // Every listing runs through the gateway. This block used to split
            // listings into managed and federated and send federated callers to
            // the provider's own host — a documented route around the gateway.
            // There is one call path now, and it is the same for both.
            "where_a_listing_runs": {
                "field": "gateway_url (on every /v1/apis item and /v1/apis/<slug>)",
                "call": "gateway_url — https://gateway.apiosk.com/<api-slug>/<path>, or POST /<api-slug>/execute",
                "payment": "Pay the gateway's 402 challenge; the gateway verifies, forwards and settles to the provider.",
                "note": "hosted_externally is false on every listing and external_resources is empty. Provider origins and payment addresses are not published."
            },
            "quickstart": [
                "GET /llms.txt",
                "GET /.well-known/x402",
                "GET /.well-known/ai-plugin.json",
                "GET /types",
                "GET /v1/discover?q=<what you need>",
                "GET /v1/compare?candidates=<ids from discover>",
                "GET /v1/decide?candidates=<ids from discover>&max_price=0.05",
                "GET /v1/apis?search=weather&sort=price&order=asc&limit=10",
                "GET /v1/apis/<slug>",
                "GET /v1/providers",
                "GET /v1/providers/<username>",
                "GET /<api-slug>/metadata",
                "POST /<api-slug>/execute",
                "GET /<api-slug>/<path>"
            ],
            "explore_endpoints": {
                "routes": [
                    {"method": "GET", "path": "/types", "purpose": "List supported groups you can browse."},
                    {"method": "GET", "path": "/types/:listing_type/v1", "purpose": "Browse APIs in one group."},
                    {"method": "GET", "path": "/v1/apis", "purpose": "Browse all active verified APIs."},
                    {"method": "GET", "path": "/v1/discover", "purpose": "Step 1 of the comparison layer: what can perform this task? Returns a candidate set with stable candidate_id values to carry into /v1/compare, so the set you compare is provably the set you discovered."},
                    {"method": "GET", "path": "/v1/compare", "purpose": "Step 2: how do those candidates perform against your requirements? Price, measured latency, measured success rate and input fit side by side, each scored 0-100 with the weights and per-dimension contributions that produced the number, so it can be recomputed. Unmeasured dimensions are dropped from the weighting and named, never scored zero."},
                    {"method": "GET", "path": "/v1/decide", "purpose": "Step 3: which one should you use? One provider back with the rule that won it, every rejected candidate and the exact constraint that removed it, the runners-up in order, how to execute it, and an outcome URL to report back to."},
                    {"method": "GET", "path": "/v1/network", "purpose": "Who is on the network, as a name and a mark. The reviewed Apiosk providers and the external x402 sources that are themselves one seller, deduplicated into one list. Identity only — no price, no endpoint count, no ranking. Aggregator indexes publish many sellers and so appear nowhere here; see /v1/sources for those."},
                    {"method": "GET", "path": "/v1/sources", "purpose": "Browse the ecosystem instead of searching it: every external x402 index the gateway sweeps, with whether it can be listed whole or only answers questions. The reviewed Apiosk catalogue is not one of these — it is always searched, and is browsable at /v1/apis."},
                    {"method": "GET", "path": "/v1/sources/:id/apis", "purpose": "The providers one index publishes, grouped from its resource URLs by host, with endpoint count, price range, rails and how many publish a callable input schema. Queried indexes need ?q=."},
                    {"method": "GET", "path": "/v1/sources/:id/apis/:host", "purpose": "Every endpoint one provider publishes in that index, with the full metadata it declared and what Apiosk would do about settling each one. Unreviewed and unmeasured, and each entry says so."},
                    {"method": "GET", "path": "/v1/select", "purpose": "Hand over the constraints and get one provider back with the rule that picked it, every rejection and the reason it was rejected, and the runners-up in order. Constraints: max_price, max_latency_ms, min_success_rate, settlement=apiosk|direct, require_all_inputs, include_unreliable. Preference: prefer. Listings that fail more often than they work are held back by default and named in the rejections; include_unreliable=true considers them anyway. The choice is recorded as a decision you can report an outcome against."},
                    {"method": "GET", "path": "/v1/search", "purpose": "Describe a need in words and get the providers that can serve it, ranked. Start here when you do not know the capability slug. Accepts prefer=cheapest|fastest|most_reliable, max_price and include_external=1."},
                    {"method": "GET", "path": "/v1/capabilities", "purpose": "Browse the catalog by the task to be done rather than by seller: every capability, how many providers offer it and the price range."},
                    {"method": "POST", "path": "/v1/decisions", "purpose": "Report which candidate you chose, what you passed over and why. Free, unauthenticated, never affects payment. Nothing else records the alternatives you rejected."},
                    {"method": "POST", "path": "/v1/decisions/:id/outcome", "purpose": "Report whether the provider you chose actually delivered: status, latency and whether the result was usable. The only result-quality signal in the system."},
                    {"method": "GET", "path": "/v1/capabilities/:slug", "purpose": "Every provider that can perform one task, with price, call target and how to map the capability's inputs onto that provider's parameters. Compare before you buy."},
                    {"method": "GET", "path": "/v1/providers", "purpose": "Browse every provider with their managed and federated listing counts."},
                    {"method": "GET", "path": "/v1/providers/:username", "purpose": "List one provider's listings."},
                    {"method": "GET", "path": "/:api_slug/metadata", "purpose": "Get the agent-native and MCP-style metadata manifest for one API."},
                    {"method": "POST", "path": "/:api_slug/execute", "purpose": "Call the API through the uniform agent execution contract."},
                    {"method": "ANY", "path": "/:api_slug/*path", "purpose": "Call a provider API through the gateway."}
                ],
                "listing_types": [
                    {"type": "api", "description": "All verified active APIs"},
                    {"type": "dataset", "description": "Datasets and queryable data products"},
                    {"type": "service", "description": "Webhook-backed services and async actions"},
                    {"type": "connector", "description": "Paid relays into third-party platforms"},
                    {"type": "skill", "description": "Agent-native tools and MCP-style skills"},
                    {"type": "product", "description": "Digital products and downloadable assets"}
                ]
            },
            "search_and_filter": {
                "supported_on": ["/v1/apis", "/types/:listing_type/v1", "/v1/providers", "/v1/providers/:username"],
                "query_params": {
                    "search": "Full text match on name/description/category (on /v1/providers it also matches the provider's name, username and bio). A MATCH, not a lookup: it reaches into endpoint paths and descriptions too, so a listing that merely mentions the name comes back with it. Use slug when you already know which listing you want",
                    "slug": "Exactly one listing, by slug (/v1/apis only). Narrows on top of the visibility rules, never around them: a listing that is not public returns nothing here either",
                    "category": "Filter by category string (/v1/apis only)",
                    "tag": "Filter by listing tag (/v1/apis only)",
                    "listing_type": "api | dataset | service | connector | skill | product | federated (/v1/apis only)",
                    "min_price": "Minimum price in USD, inclusive (/v1/apis only)",
                    "max_price": "Maximum price in USD, inclusive (/v1/apis only)",
                    "created_after": "RFC3339 timestamp or YYYY-MM-DD, inclusive (/v1/apis only)",
                    "created_before": "RFC3339 timestamp or YYYY-MM-DD, inclusive (/v1/apis only)",
                    "include_test": "true to include sandbox/smoke listings, which are excluded by default (/v1/apis only)",
                    "sort": "name | price | newest (default: name)",
                    "order": "asc | desc (default: asc)",
                    "limit": "1..500 (default: 500)",
                    "offset": ">= 0 (default: 0)"
                },
                "examples": [
                    "GET /v1/apis?search=weather",
                    "GET /v1/apis?slug=tavily",
                    "GET /v1/apis?category=data&sort=price&order=asc&limit=20",
                    "GET /v1/apis?max_price=0.01&listing_type=federated",
                    "GET /v1/apis?created_after=2026-01-01&sort=newest&order=desc",
                    "GET /v1/apis/<slug>",
                    "GET /v1/providers",
                    "GET /v1/providers/<username>",
                    "GET /types/dataset/v1?search=crypto&sort=newest&order=desc",
                    "GET /weather/metadata",
                    "POST /weather/execute"
                ],
                "response_depth": "Catalog items are already complete: each carries provider identity, pricing, listing_metadata, operations[], endpoints[] (method, path, full_url, price, payment_required, request/response schemas and examples, query_params, headers, per-parameter pricing_rules, documentation_markdown, external docs and OpenAPI links). No extra call is needed to learn how to call an endpoint. Every listing is called at its full_url on gateway.apiosk.com; provider origins and payment addresses are not published."
            },
            "publish_and_manage": {
                "steps": [
                    "1) POST /v1/apis/register to publish a new API",
                    "2) GET /v1/apis/mine?wallet=<0x...> to list your APIs",
                    "3) POST /v1/apis/:slug to update endpoint/price/description/active",
                    "4) DELETE /v1/apis/:slug?wallet=<0x...> to remove an API"
                ],
                "register_required_fields": {
                    "name": "string",
                    "slug": "lowercase alphanumeric + hyphens",
                    "endpoint_url": "must start with https://",
                    "price_usd": "0.0001 to 10.00",
                    "description": "string",
                    "owner_wallet": "0x-prefixed EVM address (42 chars)",
                    "category": "optional string (default: data)"
                },
                "wallet_auth_headers": [
                    "x-wallet-address",
                    "x-wallet-signature",
                    "x-wallet-timestamp",
                    "x-wallet-nonce"
                ],
                "wallet_auth_message_format": "Apiosk auth\\naction:<action>\\nwallet:<lowercase_wallet>\\nresource:<resource>\\ntimestamp:<unix_seconds>\\nnonce:<nonce>",
                "auth_notes": [
                    "Timestamp must be within ±300 seconds",
                    "Signatures are replay-protected",
                    "Wallet in header must match payload owner_wallet"
                ]
            },
            "agent_navigation_hints": [
                "Start at /types or /v1/apis",
                "Use /:api_slug/metadata for MCP-style tool discovery",
                "Use /:api_slug/execute for a uniform agent contract across APIs",
                "Use limit/offset pagination for large result sets",
                "Use search first, then narrow by category/sort/order",
                "Call provider APIs through /:api_slug/*path"
            ]
        }
    });

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(json_body.to_string()))
        .unwrap()
}

pub async fn health() -> &'static str {
    "ok"
}

/// Public status endpoint. Returns a JSON snapshot of gateway health: build
/// info, region, the result of a trivial DB ping (latency + ok flag), and the
/// count of active+verified APIs. Cached for 30 seconds at the edge so a noisy
/// uptime checker can not turn this into a load source. Intentionally exposes
/// no traffic metrics.
pub async fn status(State(state): State<Arc<AppState>>) -> Response {
    use std::time::Instant;

    let started = Instant::now();
    let db_check = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.db.pool)
        .await;
    let db_latency_ms = started.elapsed().as_millis() as u64;
    let db_ok = db_check.is_ok();
    if let Err(ref err) = db_check {
        warn!("status DB ping failed: {}", err);
    }

    let active_apis: i64 = sqlx::query_scalar(
        // Counts what the catalog serves, so it carries no payout-wallet gate
        // either — see CATALOG_VISIBILITY in db/discovery.rs.
        // `discoverable` hoort erbij: sinds de catalogus tot zes bronnen is
        // teruggebracht telde deze regel listings mee die nergens meer te zien
        // zijn, en dan zegt /status iets anders dan /v1/apis.
        r#"SELECT count(*)::bigint FROM apis
           WHERE status = 'active' AND is_verified = TRUE
             AND COALESCE(discoverable, TRUE) = TRUE"#,
    )
    .fetch_one(&state.db.pool)
    .await
    .unwrap_or(0);

    let (policy_allowed, policy_denied, policy_held) =
        crate::telemetry::policy_counters::snapshot();

    let body = json!({
        "service": "apiosk-gateway",
        "version": env!("CARGO_PKG_VERSION"),
        "region": std::env::var("FLY_REGION").ok(),
        "machine_id": std::env::var("FLY_MACHINE_ID").ok(),
        "facilitator": state.config.facilitator_url,
        "db": {
            "ok": db_ok,
            "latency_ms": db_latency_ms,
        },
        "active_apis": active_apis,
        // Purchase policy verdicts since this process started. A rule that
        // suddenly refuses everything looks, from every other angle here, like
        // a quiet day: no errors, no 500s, no latency, just agents that stopped
        // buying. These three numbers are how that becomes visible in minutes.
        "policy_verdicts": {
            "allowed": policy_allowed,
            "denied": policy_denied,
            "held": policy_held,
        },
        "ts": chrono::Utc::now().to_rfc3339(),
    });

    let status = if db_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .header(
            "Cache-Control",
            "public, max-age=30, stale-while-revalidate=60",
        )
        .header("X-Apiosk-Gateway", "true")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| Response::new(Body::from(r#"{"error":"status_build_error"}"#)))
}

pub async fn favicon() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/x-icon")], FAVICON)
}

pub async fn favicon_svg() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/svg+xml")], FAVICON_SVG)
}

pub async fn favicon_dark() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/x-icon")], FAVICON_DARK)
}

pub async fn favicon_dark_96() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png")], FAVICON_DARK_96)
}

pub async fn icon_maskable() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png")], ICON_MASKABLE)
}

pub async fn favicon_96() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png")], FAVICON_96)
}

pub async fn apple_touch_icon() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png")], APPLE_TOUCH_ICON)
}

pub async fn site_webmanifest() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/manifest+json")],
        WEB_MANIFEST,
    )
}

pub async fn web_app_manifest_192() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png")], WEB_APP_MANIFEST_192)
}

pub async fn web_app_manifest_512() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png")], WEB_APP_MANIFEST_512)
}

pub async fn logo_svg() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/svg+xml")], LOGO)
}

pub async fn logo_png() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png")], LOGO_PNG)
}

/// `GET /embed.js` — the "Connect with Apiosk" embed script. Public, cacheable,
/// and served cross-origin (the global CORS layer already allows any origin); a
/// `<script src>` load isn't CORS-gated anyway. Cache for an hour so updates roll
/// out without being pinned forever.
pub async fn embed_js() -> impl IntoResponse {
    (
        [
            (
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            ),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        EMBED_JS,
    )
}

/// `GET /webmcp.js` — the WebMCP shim a provider drops onto their own site to
/// expose their Apiosk listing's endpoints as browser-agent tools. Same public,
/// cross-origin, hour-cached policy as `embed.js`; it reads `/v1/apis/{slug}` at
/// runtime, so a listing change needs no redeploy of this file.
pub async fn webmcp_js() -> impl IntoResponse {
    (
        [
            (
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            ),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        WEBMCP_JS,
    )
}

/// `GET /_apiosk/pay` — the payment confirmation popup `webmcp.js` opens when a
/// tool call comes back 402.
///
/// Deliberately NOT cached: it is a consent surface, and a stale copy is a
/// consent surface showing the wrong price. `X-Frame-Options: DENY` because it
/// must be a real window with a visible apiosk.com in the URL bar — framed
/// inside a provider's page it would be trivial to dress up as something else.
pub async fn pay_popup() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_FRAME_OPTIONS, "DENY"),
        ],
        PAY_POPUP_HTML,
    )
}

/// `GET /product.js` — the Apiosk Commerce product script. Public, cacheable, and
/// served cross-origin (the global CORS layer already allows any origin); a
/// `<script src>` load isn't CORS-gated anyway. Same cache policy as `embed.js`.
pub async fn product_js() -> impl IntoResponse {
    (
        [
            (
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            ),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        PRODUCT_JS,
    )
}

fn json_manifest_response(body: Value) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("Cache-Control", "public, max-age=3600")
        .header("X-Apiosk-Gateway", "true")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| Response::new(Body::from(r#"{"error":"manifest_build_error"}"#)))
}

/// Agent-discovery manifest (ChatGPT-plugin shape) served from
/// `/.well-known/ai-plugin.json`. Points agents at the live OpenAPI contract
/// and describes the x402 payment model. The path is fully static, so axum's
/// router matches it ahead of the `/:api_slug/*path` proxy fallback.
pub async fn well_known_ai_plugin() -> Response {
    let body = json!({
        "schema_version": "v1",
        "name_for_human": "Apiosk Gateway",
        "name_for_model": "apiosk_gateway",
        "description_for_human": POSITIONING_SHORT,
        "description_for_model": ([POSITIONING_LONG, " Also available: browse every listing with GET /v1/apis (supports search, category, sort, order, limit, offset). Read a per-API agent/MCP manifest with GET /{api_slug}/metadata. Call any API with POST /{api_slug}/execute using a JSON body {operation, input}, or call it transparently with ANY /{api_slug}/{path}. Paid endpoints return HTTP 402 (x402 v2) with a base64 PAYMENT-REQUIRED header and JSON body until a valid payment proof is supplied via the PAYMENT-SIGNATURE header (the legacy X-Payment header is also accepted for x402 v1); settlement is USDC on Base. No accounts, API keys, or SDKs."].concat()),
        "auth": { "type": "none" },
        "api": {
            "type": "openapi",
            "url": format!("{GATEWAY_BASE_URL}/openapi.json"),
            "is_user_authenticated": false
        },
        "logo_url": format!("{GATEWAY_BASE_URL}/logo.png"),
        "contact_email": "support@apiosk.com",
        "legal_info_url": "https://apiosk.com"
    });
    json_manifest_response(body)
}

/// Plain-text agent/LLM discovery guide served from `/llms.txt`. Describes how
/// agents discover, call, and pay for APIs through the gateway. Static path, so
/// it is matched ahead of the `/:api_slug` proxy fallback.
/// `GET /robots.txt` — allow crawlers everywhere and point them at the sitemap.
/// Without this, a crawler's request for `/robots.txt` falls through to the
/// `/:api_slug` proxy and 404s, which some crawlers treat as "site disallows
/// crawling". The API surfaces that only make sense to agents (`/execute`,
/// `/metadata`, the raw proxy paths) are left crawlable but carry no inbound
/// links, so they cost nothing; the sitemap is what actually gets indexed.
pub async fn robots_txt() -> Response {
    let body = format!("User-agent: *\nAllow: /\n\nSitemap: {GATEWAY_BASE_URL}/sitemap.xml\n");
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/plain; charset=utf-8")
        .header("Cache-Control", "public, max-age=3600")
        .body(Body::from(body))
        .unwrap_or_else(|_| Response::new(Body::from("User-agent: *\nAllow: /\n")))
}

/// Token replaced with [`POSITIONING_LONG`] when `/llms.txt` is served.
const PLACEHOLDER: &str = "{{POSITIONING}}";

/// `/llms.txt`, verbatim except for [`PLACEHOLDER`].
///
/// A raw string rather than a format string: the document is full of
/// literal braces (`{api_slug}`, `{path}`), so substituting one token beats
/// escaping every brace in it.
const LLMS_TXT_TEMPLATE: &str = r#"# Apiosk Gateway

> {{POSITIONING}}

The gateway is a transparent reverse proxy. Every listed API is reachable at https://gateway.apiosk.com/{api_slug}/{path}. Free endpoints forward immediately; paid endpoints return `402 Payment Required` (x402 v2) with a base64 `PAYMENT-REQUIRED` header plus a JSON body until a valid payment proof is supplied.

## The comparison layer: discover, compare, decide

A provider's own API can tell you its price. It cannot tell you how it compares to the four alternatives, and it has no reason to. These three endpoints answer that, they chain, and none of them charges anything:

1. `GET https://gateway.apiosk.com/v1/discover?q=read+a+web+page` - what can perform this task, anywhere? Returns a candidate set, each with a stable `candidate_id`, AND a separate `external_candidates` block: the wider x402 ecosystem searched live under several phrasings of the same need. The external hits are unreviewed, unmeasured and not settleable here, so they carry no `candidate_id` and cannot be compared — they are returned anyway, because an agent that needs the job done would rather hear "nobody reviewed this, here it is" than "no results". `include_external=false` returns the reviewed catalogue alone.
2. `GET https://gateway.apiosk.com/v1/compare?candidates=<id>,<id>` - how do they perform against your requirements? Price, measured latency, measured success rate and input compatibility side by side, each scored 0-100.
3. `GET https://gateway.apiosk.com/v1/decide?candidates=<id>,<id>` - which one should you use? One provider back, plus the rule that picked it, every rejected candidate with the exact constraint that removed it, and the runners-up in order so you can overrule it in one read.

State your requirements once and pass the same ones down the chain: `max_price`, `max_latency_ms`, `min_reliability` (0..1 or 0..100), `settlement=apiosk|direct`, `require_all_inputs`, `optimize_for=price|latency|reliability|balanced`. Any step also accepts `q=` or `capability=` directly if you want to skip ahead.

Carrying the candidate ids forward is what makes the set you compared provably the set you discovered. Without them each step re-derives the match, and the decision cannot be attributed to a menu.

On the score: it is 0-100 relative to the candidates in THAT comparison - the cheapest scores 1 on price, the dearest 0 - so it is meaningful within a set and meaningless across sets. It is never returned alone. Every response carries the weights that produced it and each candidate's contribution per dimension, because a score you can recompute is an argument and one you cannot is an advertisement. Dimensions Apiosk has not measured for a candidate are dropped from the weighting and named in `not_scored`, never scored zero: zero would rank a provider Apiosk has never proxied below one it has measured badly, which says more about our coverage than about them. Asking for `optimize_for=latency` or `reliability` sorts measured candidates above unmeasured ones; setting a hard `max_latency_ms` or `min_reliability` rejects unmeasured ones outright, because a floor cannot be met by a claim.

The decision is recorded end to end - discovered, considered, filtered, ranked, selected, rejected - and the response carries an outcome URL to report back to. A vendor sees only the transactions it wins. This sees the choice.

## Discovery

- API catalog (JSON): https://gateway.apiosk.com/v1/apis - every active, verified listing. Each item is already complete: provider identity, category, pricing, listing_metadata, operations[] and endpoints[] (method, path, full_url, price_usd, payment_required, request/response schemas and examples, query_params, headers, per-parameter pricing_rules, documentation_markdown, external docs + OpenAPI links).
- Catalog filters: search, category, tag, listing_type, min_price, max_price, created_after, created_before, include_test, sort (name|price|newest), order (asc|desc), limit (1..500, default 500), offset.
- One API in full: https://gateway.apiosk.com/v1/apis/{slug} - the same shape plus long_description, example_usage, authentication notes and, for federated listings, the provider's own x402 resources.
- Compare before you buy: https://gateway.apiosk.com/v1/discover -> /v1/compare -> /v1/decide, the comparison layer described above. Use this when more than one provider can do the job, which is most of the time.
- Let Apiosk choose: https://gateway.apiosk.com/v1/select?q=read+a+web+page&max_price=0.05 - one provider back, with the rule that picked it, every rejected candidate and the constraint that removed it, and the runners-up in order so you can overrule it in one read. Hard constraints: max_price, max_latency_ms, min_success_rate, settlement=apiosk|direct, require_all_inputs, include_unreliable. A latency or reliability floor rejects unmeasured providers rather than assuming they meet it, and a listing measured below 50% success is held back whether or not you set a floor - named in the rejections, and reachable with include_unreliable=true.
- Start here if you have a need but no slug: https://gateway.apiosk.com/v1/search?q=financial+investor+news - describe the job in plain words and get the matching capabilities with their providers ranked. prefer=cheapest (default), fastest or most_reliable; max_price binds a budget; include_external=1 also searches the wider x402 ecosystem. Latency and success rate are real measurements where Apiosk has proxied enough calls, and explicitly absent where it has not.
- Capabilities (JSON): https://gateway.apiosk.com/v1/capabilities - the same catalog grouped by the task to be done instead of by seller. One capability lists every provider that can perform it, with price, call target and an input mapping that makes the offers substitutable. Compared: price, settlement rail, input compatibility. NOT measured yet: latency, reliability, result quality, provider terms.
- Candidates for one task: https://gateway.apiosk.com/v1/capabilities/{capability_slug} - ordered by price ascending by default (no managed-first preference on this surface); pass sort=managed_first for the catalog's ordering, max_price to bind a budget.
- Report a decision: POST https://gateway.apiosk.com/v1/decisions with {"comparison_id", "capability", "chosen_api_slug", optional "reason", "policy", "rejected"}. Free and unauthenticated. The comparison_id comes from the capability response; quoting it ties your choice to the menu it was made from.
- Report an outcome: POST https://gateway.apiosk.com/v1/decisions/{decision_id}/outcome with {"status": "success|error|timeout|abandoned", optional "latency_ms", "http_status", "satisfied"}. This is the only place any result-quality signal exists.
- Providers: https://gateway.apiosk.com/v1/providers - everyone publishing here, most listings first (name, username, avatar, bio, sector, api_count). Supports search.
- One provider's listings: https://gateway.apiosk.com/v1/providers/{username} - supports search, sort, order, limit, offset.
- Browse by type: https://gateway.apiosk.com/types - api, dataset, service, connector, skill, product. One type: https://gateway.apiosk.com/types/{listing_type}/v1
- Per-API metadata: https://gateway.apiosk.com/{api_slug}/metadata - agent-native, MCP-style manifest for one API (operations, input/output schemas, pricing, tool annotations).
- Gateway health: https://gateway.apiosk.com/health

## Where a listing runs (read this before calling)

Every listing runs through the gateway. Call it at its `gateway_url`, i.e.
https://gateway.apiosk.com/{api_slug}/{path}, or POST /{api_slug}/execute. Pay
the gateway's 402; the gateway verifies, forwards and settles to the provider.

There is no second path. `hosted_externally` is false on every listing and
`external_resources` is empty — provider origins and payment addresses are not
published.

## Calling an API

- Uniform execution: POST https://gateway.apiosk.com/{api_slug}/execute with body {"operation": "<id | /path | METHOD /path>", "input": { ... }}. Returns {status, result, cost, latency, operation, api, upstream_status}.
- Transparent proxy: ANY https://gateway.apiosk.com/{api_slug}/{path} - forwards your request to the upstream API.

## Payment (x402)

- Protocol: x402 over HTTP 402 (v1 body + v2 header). Settlement asset: USDC on Base.
- A paid request without payment returns 402 with BOTH dialects: a base64 PAYMENT-REQUIRED header (decoded: {x402Version:2, resource, accepts:[{scheme, network (CAIP-2 e.g. eip155:8453), amount (atomic units), asset, payTo, maxTimeoutSeconds, extra}]}) AND a JSON body carrying x402 v1 accepts[] ({scheme, network (bare name e.g. "base"), maxAmountRequired, resource, description, mimeType, payTo, maxTimeoutSeconds, asset, extra}) for x402-fetch SDK clients, plus X-Payment-Amount, X-Payment-Currency: USDC, X-Payment-Protocol: x402.
- Supply the signed payment proof via the PAYMENT-SIGNATURE header (the legacy X-Payment header is still accepted for x402 v1 clients).
- Check spend for a wallet: https://gateway.apiosk.com/v1/balance?address=0x...

## Publishing an API

- Register: POST https://gateway.apiosk.com/v1/apis/register (wallet-signed). See the root discovery document for required fields and the wallet-auth header format.

## Machine-readable manifests

- Well-known index: https://gateway.apiosk.com/.well-known/
- x402 discovery document (all payable resources + payment requirements): https://gateway.apiosk.com/.well-known/x402
- Plugin manifest: https://gateway.apiosk.com/.well-known/ai-plugin.json
- A2A agent card: https://gateway.apiosk.com/.well-known/agent.json
- MCP discovery: https://gateway.apiosk.com/.well-known/mcp.json
- OpenAPI: https://gateway.apiosk.com/openapi.json
- Root discovery JSON: https://gateway.apiosk.com/ (send `Accept: application/json`)
"#;

pub async fn llms_txt() -> Response {
    // A raw string, not a format string: the body is full of literal braces
    // (`{api_slug}`, `{path}`), so the positioning line is substituted below
    // rather than interpolated. Keeps one source of truth without escaping
    // every brace in the document.
    let body = LLMS_TXT_TEMPLATE.replace(PLACEHOLDER, POSITIONING_LONG);

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/plain; charset=utf-8")
        .header("Cache-Control", "public, max-age=3600")
        .header("X-Apiosk-Gateway", "true")
        .body(Body::from(body))
        .unwrap_or_else(|_| Response::new(Body::from("Apiosk Gateway")))
}

/// One `/{slug}/execute` OpenAPI path per active, paid API listing.
///
/// x402-scanner tools probe literal paths from the OpenAPI document — they
/// don't resolve `{api_slug}` path templates, so a single generic
/// `/{api_slug}/execute` entry gets probed as the literal string
/// `/{api_slug}/execute`, which `is_valid_api_slug` rejects with 400 before
/// the 402 challenge ever runs. Emitting one concrete path per real slug
/// (mirroring what `/.well-known/x402` already does) gives scanners a
/// resource they can actually probe.
fn per_api_execute_paths(apis: &[crate::db::ApiInfo]) -> serde_json::Map<String, Value> {
    let mut paths = serde_json::Map::new();
    for api in apis {
        if api.listing_type != "api" {
            continue;
        }
        let path = format!("/{}/execute", api.slug);
        let operation_id = format!("execute_{}", api.slug.replace('-', "_"));
        let summary = format!("Execute the {} API.", api.name);
        let response_schema = json!({
            "type": "object",
            "properties": {
                "status": {"type": "string"},
                "result": {"description": "Upstream response body."},
                "cost": {"type": "string"},
                "latency": {"type": "number"},
                "operation": {"type": "string"},
                "api": {"type": "string"},
                "upstream_status": {"type": "integer"}
            },
            "required": ["status", "result"]
        });
        let request_body = json!({
            "required": false,
            "content": {"application/json": {
                "schema": {
                    "type": "object",
                    "properties": {
                        "operation": {"type": "string", "description": "Operation id, /path, or METHOD /path selector."},
                        "input": {"description": "JSON payload forwarded to the selected operation."}
                    }
                },
                "example": {}
            }}
        });

        let mut operation = json!({
            "operationId": operation_id,
            "summary": summary,
            "requestBody": request_body,
            "responses": {
                "200": {
                    "description": "Execution result.",
                    "content": {"application/json": {"schema": response_schema}}
                }
            }
        });
        let operation_obj = operation.as_object_mut().expect("operation is an object");

        if api.price_usd > 0 {
            operation_obj.insert(
                "x-payment-info".to_string(),
                json!({
                    "price": {"mode": "fixed", "currency": "USD", "amount": api.price_usd.to_string()},
                    "protocols": [{"x402": {}}]
                }),
            );
            operation_obj["responses"]["402"] = json!({
                "description": "Payment required (x402). See the response body's accepts[] and X-Payment-* headers."
            });
        } else {
            operation_obj.insert("security".to_string(), json!([]));
        }

        paths.insert(path, json!({ "post": operation }));
    }
    paths
}

/// OpenAPI 3.1 contract served from `/openapi.json`: the gateway's own
/// discovery/execution meta-endpoints, plus one concrete `/{slug}/execute`
/// path per active API listing (see [`per_api_execute_paths`]). Per-API
/// operation/schema detail beyond `execute` lives at `/{api_slug}/metadata`.
pub async fn openapi_json(State(state): State<Arc<AppState>>) -> Response {
    // Identical parameters to the discovery catalog, so it shares that single
    // cached entry outright rather than warming a second copy of the same rows.
    let per_api_paths = match state.cache.get_discovery_catalog(&state.db).await {
        Ok(apis) => per_api_execute_paths(&apis),
        Err(err) => {
            error!("openapi.json: listing APIs failed: {}", err);
            serde_json::Map::new()
        }
    };

    let mut body = json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Apiosk Gateway",
            "version": env!("CARGO_PKG_VERSION"),
            "description": ([POSITIONING_LONG, " This document covers the gateway's discovery and execution meta-endpoints. Individual provider APIs are reachable at /{api_slug}/{path} and described by GET /{api_slug}/metadata. Browse the catalog with GET /v1/apis, one listing with GET /v1/apis/{slug}, who publishes it with GET /v1/providers, and by kind with GET /types."].concat()),
            "x-guidance": "Discover APIs with GET /v1/apis (free) — every item already carries its endpoints, schemas, examples and pricing rules, so no extra call is needed to learn how to call one. Read one API's operations and tool annotations with GET /{api_slug}/metadata (free). Every listing is called the same way, through the gateway: POST /{api_slug}/execute with {operation, input}, or the transparent proxy at /{api_slug}/{path}. Paid gateway calls return 402 with x402 payment requirements (accepts[]) until a valid PAYMENT-SIGNATURE proof is supplied; see the runtime 402 body for the exact amount. Full payable-resource list: GET /.well-known/x402.",
            "contact": {
                "email": "olivier@apiosk.com"
            }
        },
        "servers": [{ "url": GATEWAY_BASE_URL }],
        "paths": {
            "/v1/apis": {
                "get": {
                    "operationId": "listApis",
                    "summary": "List active, verified APIs with pricing.",
                    "description": "Each item carries provider identity, pricing, listing_metadata, operations[] and endpoints[] (schemas, examples, per-parameter pricing rules), plus gateway_url, which is how every listing is called.",
                    "security": [],
                    "parameters": [
                        {"name": "search", "in": "query", "schema": {"type": "string"}, "description": "A match, not a lookup: it reaches into endpoint paths and descriptions, so listings that merely mention the term come back too. Use slug for one known listing."},
                        {"name": "slug", "in": "query", "schema": {"type": "string"}, "description": "Exactly one listing, by slug. Narrows on top of the visibility rules, never around them."},
                        {"name": "category", "in": "query", "schema": {"type": "string"}},
                        {"name": "tag", "in": "query", "schema": {"type": "string"}},
                        {"name": "listing_type", "in": "query", "schema": {"type": "string", "enum": ["api", "dataset", "service", "connector", "skill", "product"]}},
                        {"name": "min_price", "in": "query", "schema": {"type": "number", "minimum": 0}},
                        {"name": "max_price", "in": "query", "schema": {"type": "number", "minimum": 0}},
                        {"name": "created_after", "in": "query", "schema": {"type": "string"}, "description": "RFC3339 timestamp or YYYY-MM-DD, inclusive."},
                        {"name": "created_before", "in": "query", "schema": {"type": "string"}, "description": "RFC3339 timestamp or YYYY-MM-DD, inclusive."},
                        {"name": "include_test", "in": "query", "schema": {"type": "boolean", "default": false}, "description": "Include sandbox/smoke listings, excluded by default."},
                        {"name": "sort", "in": "query", "schema": {"type": "string", "enum": ["name", "price", "newest"]}},
                        {"name": "order", "in": "query", "schema": {"type": "string", "enum": ["asc", "desc"]}},
                        {"name": "limit", "in": "query", "schema": {"type": "integer", "minimum": 1, "maximum": 500, "default": 500}},
                        {"name": "offset", "in": "query", "schema": {"type": "integer", "minimum": 0}}
                    ],
                    "responses": {"200": {"description": "A list of APIs with pricing."}}
                }
            },
            "/v1/discover": {
                "get": {
                    "operationId": "discoverCandidates",
                    "summary": "Step 1 of 3 — what can perform this task, anywhere?",
                    "description": "Returns a candidate set for one capability, each with a stable candidate_id, indicative price, settlement rail and whether Apiosk has measured it. Pass those ids to /v1/compare. The ids are the point: without them the next step has to redo the match, so the set an agent compared is not provably the set it discovered. A candidate set, not the catalogue — the ceiling is 25. By default it also sweeps the wider x402 ecosystem live under several phrasings of the capability and returns the hits in a separate external_candidates block: unreviewed, unmeasured, not settleable through Apiosk and therefore carrying no candidate_id, but returned anyway, because an agent that needs the job done would rather hear 'nobody reviewed this, here it is' than 'no results'. Pass include_external=false for the reviewed catalogue alone.",
                    "security": [],
                    "parameters": [
                        {"name": "q", "in": "query", "schema": {"type": "string"}, "description": "The need in words. Either this or capability."},
                        {"name": "capability", "in": "query", "schema": {"type": "string"}, "description": "A capability slug, when you already know it."},
                        {"name": "terms", "in": "query", "schema": {"type": "string"}, "description": "Search terms you worked out yourself, in place of this endpoint's own reading of q. Commas separate the phrases within one need, semicolons separate the needs: terms=spot price,ohlcv;crypto news. For a caller that owns its own parsing step and wants the rest of the pipeline unchanged. No model runs when it is present, and the retrieval, ordering and candidate set are the same code every other request goes through."},
                        {"name": "max_candidates", "in": "query", "schema": {"type": "integer", "minimum": 1, "maximum": 25, "default": 8}},
                        {"name": "include_external", "in": "query", "schema": {"type": "boolean", "default": true}, "description": "Sweep the wider x402 ecosystem as well as the reviewed catalogue. On by default. Results land in external_candidates and never in candidates."},
                        {"name": "max_price", "in": "query", "schema": {"type": "number", "minimum": 0}},
                        {"name": "max_latency_ms", "in": "query", "schema": {"type": "integer", "minimum": 0}},
                        {"name": "min_reliability", "in": "query", "schema": {"type": "number", "minimum": 0}, "description": "0..1, or 0..100 for callers that think in percent. Both are accepted."},
                        {"name": "settlement", "in": "query", "schema": {"type": "string", "enum": ["apiosk", "direct"]}},
                        {"name": "require_all_inputs", "in": "query", "schema": {"type": "boolean", "default": false}},
                        {"name": "optimize_for", "in": "query", "schema": {"type": "string", "enum": ["price", "latency", "reliability", "balanced"], "default": "price"}}
                    ],
                    "responses": {
                        "200": {"description": "A discovery_id and the candidate set, with candidate_id values for /v1/compare."},
                        "404": {"description": "Nothing in the catalogue matches."}
                    }
                }
            },
            "/v1/compare": {
                "get": {
                    "operationId": "compareCandidates",
                    "summary": "Step 2 of 3 — how do these perform against my requirements?",
                    "description": "Turns a candidate set into evidence: price, measured latency, measured success rate and input compatibility side by side, each scored 0-100. The score is never returned alone — every response carries the weights that produced it and each candidate's contribution per dimension, so it can be recomputed rather than trusted. Scores are relative to the candidates in THIS comparison (cheapest scores 1 on price, dearest 0), which makes them meaningful within a set and meaningless across sets. Dimensions Apiosk has not measured for a candidate are dropped from the weighting and named in not_scored, never scored zero: zero would rank an unmeasured provider below a measurably bad one, which says more about Apiosk's coverage than about the provider.",
                    "security": [],
                    "parameters": [
                        {"name": "candidates", "in": "query", "schema": {"type": "string"}, "description": "Comma-separated candidate_id values from /v1/discover."},
                        {"name": "capability", "in": "query", "schema": {"type": "string"}, "description": "Or compare every provider of one capability directly."},
                        {"name": "q", "in": "query", "schema": {"type": "string"}, "description": "Or describe the need in words."},
                        {"name": "max_price", "in": "query", "schema": {"type": "number", "minimum": 0}},
                        {"name": "max_latency_ms", "in": "query", "schema": {"type": "integer", "minimum": 0}},
                        {"name": "min_reliability", "in": "query", "schema": {"type": "number", "minimum": 0}, "description": "0..1, or 0..100. Rejects unmeasured candidates rather than assuming they pass."},
                        {"name": "settlement", "in": "query", "schema": {"type": "string", "enum": ["apiosk", "direct"]}},
                        {"name": "require_all_inputs", "in": "query", "schema": {"type": "boolean", "default": false}},
                        {"name": "optimize_for", "in": "query", "schema": {"type": "string", "enum": ["price", "latency", "reliability", "balanced"], "default": "price"}}
                    ],
                    "responses": {
                        "200": {"description": "Ranked candidates with per-dimension score breakdowns, the weights used, the exclusions and the tradeoffs in words."},
                        "404": {"description": "Nothing in the catalogue matches."}
                    }
                }
            },
            "/v1/decide": {
                "get": {
                    "operationId": "decideProvider",
                    "summary": "Step 3 of 3 — which one should I use, and why that one?",
                    "description": "Collapses the comparison into one purchase. Takes the same parameters as /v1/compare and returns the selection, the rule that won it, every rejected candidate with the exact constraint that removed it, and the runners-up in order so the caller can overrule it in one read. Carries an execution block saying how to call the winner through the gateway, and an outcome_url to report back to. The decision is recorded: discovered, considered, filtered, ranked, selected, rejected. A vendor sees only the transactions it wins; this sees the choice.",
                    "security": [],
                    "parameters": [
                        {"name": "candidates", "in": "query", "schema": {"type": "string"}, "description": "Comma-separated candidate_id values from /v1/discover."},
                        {"name": "capability", "in": "query", "schema": {"type": "string"}},
                        {"name": "q", "in": "query", "schema": {"type": "string"}},
                        {"name": "max_price", "in": "query", "schema": {"type": "number", "minimum": 0}},
                        {"name": "max_latency_ms", "in": "query", "schema": {"type": "integer", "minimum": 0}},
                        {"name": "min_reliability", "in": "query", "schema": {"type": "number", "minimum": 0}},
                        {"name": "settlement", "in": "query", "schema": {"type": "string", "enum": ["apiosk", "direct"]}},
                        {"name": "require_all_inputs", "in": "query", "schema": {"type": "boolean", "default": false}},
                        {"name": "optimize_for", "in": "query", "schema": {"type": "string", "enum": ["price", "latency", "reliability", "balanced"], "default": "price"}}
                    ],
                    "responses": {
                        "200": {"description": "One selection with its reasoning, the rejections and the runners-up, or selected=null when every candidate failed a hard constraint."},
                        "404": {"description": "Nothing in the catalogue matches."}
                    }
                }
            },
            "/v1/select": {
                "get": {
                    "operationId": "selectProvider",
                    "summary": "Apply constraints and get one provider, with the reasoning.",
                    "description": "Hard constraints remove candidates (max_price, max_latency_ms, min_success_rate, settlement, require_all_inputs); prefer decides between the survivors. Apiosk applies one floor of its own: a listing measured below 50% success over 20 or more calls is not offered by default, because handing over a listing already known to be broken is worse than returning a shorter list. It is named in the rejections like any other exclusion, stays callable by name, returns on its own once it works, and include_unreliable=true considers it now. The response names the rule that selected the winner, lists every rejection with the constraint that caused it, and returns the runners-up in order, so the caller can disagree without a second request. A latency or reliability floor rejects unmeasured candidates: a floor cannot be met by a claim. The selection is written to the decision record and the response carries the outcome_url to report back to.",
                    "security": [],
                    "parameters": [
                        {"name": "q", "in": "query", "schema": {"type": "string"}, "description": "The need in words. Either this or capability."},
                        {"name": "capability", "in": "query", "schema": {"type": "string"}},
                        {"name": "max_price", "in": "query", "schema": {"type": "number", "minimum": 0}},
                        {"name": "max_latency_ms", "in": "query", "schema": {"type": "integer", "minimum": 0}},
                        {"name": "min_success_rate", "in": "query", "schema": {"type": "number", "minimum": 0, "maximum": 1}},
                        {"name": "include_unreliable", "in": "query", "schema": {"type": "boolean"}, "description": "Consider listings Apiosk holds back for failing more often than they work. Default false."},
                        {"name": "settlement", "in": "query", "schema": {"type": "string", "enum": ["apiosk", "direct"]}},
                        {"name": "require_all_inputs", "in": "query", "schema": {"type": "boolean", "default": false}},
                        {"name": "prefer", "in": "query", "schema": {"type": "string", "enum": ["cheapest", "fastest", "most_reliable"], "default": "cheapest"}}
                    ],
                    "responses": {
                        "200": {"description": "The chosen provider with reasoning, or chosen=null when every candidate failed a constraint."},
                        "400": {"description": "Neither q nor capability supplied."},
                        "404": {"description": "Nothing in the catalogue matches."}
                    }
                }
            },
            "/v1/search": {
                "get": {
                    "operationId": "searchByNeed",
                    "summary": "Describe a need in words, get ranked providers.",
                    "description": "Matches free text against capability names and descriptions AND against the endpoint text of the offers assigned to them, so domain words that appear only in a provider's own docs still find the right capability. Returns each matching capability with its offers, carrying price, measured latency and success rate where a sample exists, and explicit absence where it does not. Ranking is the caller's choice via prefer; there is no composite quality score.",
                    "security": [],
                    "parameters": [
                        {"name": "q", "in": "query", "required": true, "schema": {"type": "string"}, "description": "The need, in plain words."},
                        {"name": "prefer", "in": "query", "schema": {"type": "string", "enum": ["cheapest", "fastest", "most_reliable"], "default": "cheapest"}},
                        {"name": "max_price", "in": "query", "schema": {"type": "number", "minimum": 0}},
                        {"name": "include_external", "in": "query", "schema": {"type": "boolean", "default": false}}
                    ],
                    "responses": {"200": {"description": "Matching capabilities with ranked offers."}, "400": {"description": "q missing."}}
                }
            },
            "/v1/capabilities": {
                "get": {
                    "operationId": "listCapabilities",
                    "summary": "List every task the catalog can perform, with offer counts and price range.",
                    "description": "The catalog grouped by the job to be done rather than by seller. Each capability carries an input_contract, an output_contract and how many listings currently offer it. Price and settlement are compared; latency, reliability, result quality and provider terms are not measured yet and every response says so.",
                    "security": [],
                    "parameters": [],
                    "responses": {"200": {"description": "Capabilities with offer counts and price ranges."}}
                }
            },
            "/v1/capabilities/{slug}": {
                "get": {
                    "operationId": "getCapabilityCandidates",
                    "summary": "List every provider that can perform one task.",
                    "description": "Candidates for one capability: price, settlement rail, call target and the mapping from the capability's inputs onto each provider's own parameter names. missing_inputs names any capability input an offer cannot accept. Federated candidates carry the provider's own resource URL, never a gateway URL.",
                    "security": [],
                    "parameters": [
                        {"name": "slug", "in": "path", "required": true, "schema": {"type": "string"}},
                        {"name": "sort", "in": "query", "schema": {"type": "string", "enum": ["price", "managed_first"], "default": "price"}, "description": "Default price ascending. Unlike /v1/apis this surface applies no managed-first preference unless asked."},
                        {"name": "max_price", "in": "query", "schema": {"type": "number", "minimum": 0}, "description": "Drop candidates above this listed price."}
                    ],
                    "responses": {"200": {"description": "Ranked candidates for the capability."}, "404": {"description": "Unknown capability."}}
                }
            },
            "/v1/decisions": {
                "post": {
                    "operationId": "reportDecision",
                    "summary": "Report which candidate you chose for a capability, and why.",
                    "description": "Free and unauthenticated; rate-limited per IP and never on a payment path. Only capability and chosen_api_slug are required. Quote the comparison_id from GET /v1/capabilities/{slug} to tie the choice to the candidate set it was made from. rejected accepts either bare slugs or {api_slug, path, reason} objects. Returns decision_id and the outcome_url to report back to.",
                    "security": [],
                    "responses": {
                        "201": {"description": "Decision recorded."},
                        "400": {"description": "capability or chosen_api_slug missing, or the body is not JSON."},
                        "404": {"description": "Unknown capability."},
                        "429": {"description": "Rate limited."}
                    }
                }
            },
            "/v1/decisions/{id}/outcome": {
                "post": {
                    "operationId": "reportOutcome",
                    "summary": "Report whether the chosen provider delivered.",
                    "description": "status is one of success, error, timeout, abandoned. latency_ms, http_status, satisfied and notes are optional. Several outcomes per decision are legal — a retry or a fallback each get their own attempt number, derived server-side so a retry cannot overwrite the first verdict.",
                    "security": [],
                    "parameters": [{"name": "id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}],
                    "responses": {
                        "201": {"description": "Outcome recorded."},
                        "400": {"description": "Invalid decision id or status."},
                        "404": {"description": "Unknown decision."},
                        "429": {"description": "Rate limited."}
                    }
                }
            },
            "/v1/apis/{slug}": {
                "get": {
                    "operationId": "getApi",
                    "summary": "Get details and endpoints for one API.",
                    "security": [],
                    "parameters": [{"name": "slug", "in": "path", "required": true, "schema": {"type": "string"}}],
                    "responses": {"200": {"description": "API detail."}}
                }
            },
            "/v1/providers": {
                "get": {
                    "operationId": "listProviders",
                    "summary": "List every provider publishing on the gateway, most listings first.",
                    "security": [],
                    "parameters": [
                        {"name": "search", "in": "query", "schema": {"type": "string"}, "description": "Matches the provider's name, username and bio, and the names/descriptions/categories of their listings."}
                    ],
                    "responses": {"200": {"description": "Providers with name, username, avatar, bio, sector and listing counts."}}
                }
            },
            "/v1/providers/{username}": {
                "get": {
                    "operationId": "getProviderListings",
                    "summary": "List one provider's active listings.",
                    "security": [],
                    "parameters": [
                        {"name": "username", "in": "path", "required": true, "schema": {"type": "string"}},
                        {"name": "search", "in": "query", "schema": {"type": "string"}},
                        {"name": "sort", "in": "query", "schema": {"type": "string", "enum": ["name", "price", "newest"]}},
                        {"name": "order", "in": "query", "schema": {"type": "string", "enum": ["asc", "desc"]}},
                        {"name": "limit", "in": "query", "schema": {"type": "integer", "minimum": 1, "maximum": 500}},
                        {"name": "offset", "in": "query", "schema": {"type": "integer", "minimum": 0}}
                    ],
                    "responses": {"200": {"description": "The provider and their listings."}, "404": {"description": "No provider with visible listings under that username."}}
                }
            },
            "/types": {
                "get": {
                    "operationId": "listTypes",
                    "summary": "List the listing types you can browse.",
                    "security": [],
                    "responses": {"200": {"description": "Supported listing types and their browse endpoints."}}
                }
            },
            "/types/{listing_type}/v1": {
                "get": {
                    "operationId": "listByType",
                    "summary": "Browse listings of one type.",
                    "security": [],
                    "parameters": [
                        {"name": "listing_type", "in": "path", "required": true, "schema": {"type": "string", "enum": ["api", "dataset", "service", "connector", "skill", "product"]}},
                        {"name": "search", "in": "query", "schema": {"type": "string"}},
                        {"name": "sort", "in": "query", "schema": {"type": "string", "enum": ["name", "price", "newest"]}},
                        {"name": "order", "in": "query", "schema": {"type": "string", "enum": ["asc", "desc"]}},
                        {"name": "limit", "in": "query", "schema": {"type": "integer", "minimum": 1, "maximum": 500}},
                        {"name": "offset", "in": "query", "schema": {"type": "integer", "minimum": 0}}
                    ],
                    "responses": {"200": {"description": "Listings of that type."}}
                }
            },
            "/.well-known/x402": {
                "get": {
                    "operationId": "getX402Discovery",
                    "summary": "Every payable resource with its full x402 payment requirements.",
                    "security": [],
                    "responses": {"200": {"description": "x402 discovery document (x402Version + items[])."}}
                }
            },
            "/health": {
                "get": {
                    "operationId": "getHealth",
                    "summary": "Gateway liveness.",
                    "security": [],
                    "responses": {"200": {"description": "Gateway status."}}
                }
            },
            "/{api_slug}/metadata": {
                "get": {
                    "operationId": "getApiMetadata",
                    "summary": "Agent-native, MCP-style metadata manifest for one API.",
                    "security": [],
                    "parameters": [{"name": "api_slug", "in": "path", "required": true, "schema": {"type": "string"}}],
                    "responses": {"200": {"description": "MCP-style metadata for the API."}}
                }
            },
            "/v1/balance": {
                "get": {
                    "operationId": "getBalance",
                    "summary": "USDC spend summary for a wallet address.",
                    "security": [],
                    "parameters": [{"name": "address", "in": "query", "required": true, "schema": {"type": "string"}}],
                    "responses": {"200": {"description": "Spend summary for the wallet."}}
                }
            }
        }
    });
    if let Some(paths) = body.get_mut("paths").and_then(Value::as_object_mut) {
        paths.extend(per_api_paths);
    }
    json_manifest_response(body)
}

pub async fn invalidate_cache(State(state): State<Arc<AppState>>) -> &'static str {
    state.cache.invalidate_all().await;
    "cache invalidated"
}

pub async fn handle_request_no_path(
    State(state): State<Arc<AppState>>,
    Path(api_slug): Path<String>,
    request: Request<Body>,
) -> Result<Response<Body>, StatusCode> {
    proxy::handle(state, &api_slug, "/", request).await
}

pub async fn handle_request_with_path(
    State(state): State<Arc<AppState>>,
    Path((api_slug, path)): Path<(String, String)>,
    request: Request<Body>,
) -> Result<Response<Body>, StatusCode> {
    proxy::handle(state, &api_slug, &format!("/{}", path), request).await
}

pub async fn proxy_mcp(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
) -> Result<Response<Body>, StatusCode> {
    // Transparent passthrough to the apiosk MCP upstream(s). Generic per-tool
    // payment proxying was removed in favour of the pointer-based purchase flow
    // (`/pay/charge` + `/pay/verify`, see `crate::pay`), which keeps payment
    // out of the MCP dataflow entirely.
    let path_and_query = request
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| "/mcp".to_string());
    let method = request.method().clone();
    let filtered_headers = request
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            let name_str = name.as_str().to_lowercase();
            if matches!(
                name_str.as_str(),
                "host" | "connection" | "transfer-encoding" | "keep-alive"
            ) {
                None
            } else {
                Some((name.clone(), value.clone()))
            }
        })
        .collect::<Vec<_>>();

    let body_bytes = axum::body::to_bytes(request.into_body(), 10 * 1024 * 1024)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    proxy_mcp_request(
        &state.http_client,
        &state.config.mcp_proxy_upstream_urls,
        &path_and_query,
        method,
        &filtered_headers,
        body_bytes,
    )
    .await
}

fn build_mcp_target_url(upstream_base: &str, path_and_query: &str) -> String {
    let suffix = path_and_query
        .strip_prefix("/mcp")
        .unwrap_or(path_and_query);
    if suffix.is_empty() || suffix == "/" {
        upstream_base.to_string()
    } else {
        format!("{upstream_base}{suffix}")
    }
}

fn is_retriable_mcp_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE | StatusCode::GATEWAY_TIMEOUT
    )
}

async fn proxy_mcp_request(
    http_client: &reqwest::Client,
    upstreams: &[String],
    path_and_query: &str,
    method: axum::http::Method,
    filtered_headers: &[(axum::http::HeaderName, axum::http::HeaderValue)],
    body_bytes: axum::body::Bytes,
) -> Result<Response<Body>, StatusCode> {
    proxy_mcp_request_with_transport(
        upstreams,
        path_and_query,
        method,
        filtered_headers.to_vec(),
        body_bytes,
        |target_url, method, filtered_headers, body_bytes| async move {
            let mut upstream_req = http_client
                .request(method, &target_url)
                .timeout(std::time::Duration::from_secs(20));

            for (name, value) in filtered_headers {
                upstream_req = upstream_req.header(name.clone(), value.clone());
            }

            if !body_bytes.is_empty() {
                upstream_req = upstream_req.body(body_bytes);
            }

            let upstream_res = upstream_req.send().await.map_err(|err| err.to_string())?;
            let status = upstream_res.status();
            let headers = upstream_res
                .headers()
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect::<Vec<_>>();
            let body = upstream_res.bytes().await.map_err(|err| err.to_string())?;

            Ok(McpUpstreamResponse {
                status,
                headers,
                body,
            })
        },
    )
    .await
}

struct McpUpstreamResponse {
    status: StatusCode,
    headers: Vec<(axum::http::HeaderName, axum::http::HeaderValue)>,
    body: axum::body::Bytes,
}

async fn proxy_mcp_request_with_transport<F, Fut>(
    upstreams: &[String],
    path_and_query: &str,
    method: axum::http::Method,
    filtered_headers: Vec<(axum::http::HeaderName, axum::http::HeaderValue)>,
    body_bytes: axum::body::Bytes,
    mut send: F,
) -> Result<Response<Body>, StatusCode>
where
    F: FnMut(
        String,
        axum::http::Method,
        Vec<(axum::http::HeaderName, axum::http::HeaderValue)>,
        axum::body::Bytes,
    ) -> Fut,
    Fut: Future<Output = Result<McpUpstreamResponse, String>>,
{
    let upstreams = if upstreams.is_empty() {
        vec!["https://mcp.apiosk.com/mcp".to_string()]
    } else {
        upstreams.to_vec()
    };

    let mut last_err = None;

    for (index, upstream_base) in upstreams.iter().enumerate() {
        let target_url = build_mcp_target_url(upstream_base, path_and_query);
        match send(
            target_url.clone(),
            method.clone(),
            filtered_headers.clone(),
            body_bytes.clone(),
        )
        .await
        {
            Ok(upstream_res) => {
                let status = upstream_res.status;
                if index + 1 < upstreams.len() && is_retriable_mcp_status(status) {
                    warn!(
                        "MCP upstream {} returned {}, trying fallback",
                        target_url, status
                    );
                    continue;
                }

                return proxy_mcp_response(upstream_res);
            }
            Err(err) => {
                warn!("MCP upstream {} failed: {}", target_url, err);
                last_err = Some(err.to_string());
            }
        }
    }

    if let Some(err) = last_err {
        error!("All MCP upstreams failed: {}", err);
    }

    Err(StatusCode::BAD_GATEWAY)
}

fn proxy_mcp_response(upstream_res: McpUpstreamResponse) -> Result<Response<Body>, StatusCode> {
    let mut response_builder = Response::builder().status(upstream_res.status);

    for (name, value) in upstream_res.headers {
        response_builder = response_builder.header(name.clone(), value.clone());
    }

    response_builder
        .body(Body::from(upstream_res.body))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bigdecimal::BigDecimal;
    use std::collections::VecDeque;
    use std::str::FromStr;
    use std::sync::{Arc, Mutex};

    fn test_api(slug: &str, price_usd: BigDecimal, listing_type: &str) -> crate::db::ApiInfo {
        crate::db::ApiInfo {
            id: slug.to_string(),
            slug: slug.to_string(),
            name: slug.to_string(),
            description: format!("{slug} description"),
            price_usd,
            category: "data".to_string(),
            implementation_type: "direct".to_string(),
            model_backed: false,
            listing_type: listing_type.to_string(),
            docs_url: format!("https://apiosk.com/docs/{slug}"),
            origin_url: "https://upstream.example".to_string(),
            gateway_url: format!("https://gateway.apiosk.com/{slug}"),
            active: true,
            verified: true,
            endpoint_count: 1,
            endpoint_docs: json!([]),
            listing_metadata: json!({}),
            is_external: false,
            external_x402: json!([]),
            provider_name: None,
            provider_username: None,
            provider_avatar_url: None,
            provider_bio: None,
            provider_sector: None,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn per_api_execute_paths_emits_concrete_paid_path_with_x_payment_info() {
        let apis = [test_api(
            "weather-api",
            BigDecimal::from_str("0.02").unwrap(),
            "api",
        )];
        let paths = per_api_execute_paths(&apis);

        let op = &paths["/weather-api/execute"]["post"];
        assert_eq!(op["x-payment-info"]["price"]["mode"], "fixed");
        assert_eq!(op["x-payment-info"]["price"]["amount"], "0.02");
        assert!(op["responses"]["402"]["description"].is_string());
        assert!(op.get("security").is_none());
    }

    #[test]
    fn per_api_execute_paths_marks_zero_price_as_free() {
        let apis = [test_api("free-api", BigDecimal::from(0), "api")];
        let paths = per_api_execute_paths(&apis);

        let op = &paths["/free-api/execute"]["post"];
        assert_eq!(op["security"], json!([]));
        assert!(op.get("x-payment-info").is_none());
    }

    #[test]
    fn per_api_execute_paths_skips_non_api_listings() {
        let apis = [test_api(
            "some-dataset",
            BigDecimal::from_str("1.0").unwrap(),
            "dataset",
        )];
        assert!(per_api_execute_paths(&apis).is_empty());
    }

    #[test]
    fn mcp_target_url_uses_base_for_root_endpoint() {
        assert_eq!(
            build_mcp_target_url("https://apiosk-mcp.fly.dev/mcp", "/mcp"),
            "https://apiosk-mcp.fly.dev/mcp"
        );
        assert_eq!(
            build_mcp_target_url("https://apiosk-mcp.fly.dev/mcp", "/mcp?foo=bar"),
            "https://apiosk-mcp.fly.dev/mcp?foo=bar"
        );
    }

    #[test]
    fn mcp_target_url_preserves_nested_path_and_query() {
        assert_eq!(
            build_mcp_target_url("https://apiosk-mcp.fly.dev/mcp", "/mcp/tools/list?limit=5"),
            "https://apiosk-mcp.fly.dev/mcp/tools/list?limit=5"
        );
    }

    #[tokio::test]
    async fn proxy_mcp_retries_on_503_and_returns_fallback_response() {
        let attempts = Arc::new(Mutex::new(VecDeque::from([
            Ok(McpUpstreamResponse {
                status: StatusCode::SERVICE_UNAVAILABLE,
                headers: vec![(
                    header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static("application/json"),
                )],
                body: axum::body::Bytes::from_static(br#"{"error":"dns cache overflow"}"#),
            }),
            Ok(McpUpstreamResponse {
                status: StatusCode::OK,
                headers: vec![(
                    header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static("application/json"),
                )],
                body: axum::body::Bytes::from_static(br#"{"ok":true}"#),
            }),
        ])));

        let response =
            proxy_mcp_request_with_transport(
                &[
                    "https://primary.example/mcp".to_string(),
                    "https://fallback.example/mcp".to_string(),
                ],
                "/mcp",
                axum::http::Method::GET,
                Vec::new(),
                axum::body::Bytes::new(),
                {
                    let attempts = Arc::clone(&attempts);
                    move |_target_url, _method, _headers, _body| {
                        let attempts = Arc::clone(&attempts);
                        async move {
                            attempts.lock().unwrap().pop_front().unwrap_or_else(|| {
                                Err("unexpected extra proxy attempt".to_string())
                            })
                        }
                    }
                },
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(body.as_ref(), br#"{"ok":true}"#);
        assert!(attempts.lock().unwrap().is_empty());
    }
}

#[cfg(test)]
mod positioning_tests {
    use super::*;

    #[test]
    fn llms_txt_substitutes_the_positioning() {
        // The template is a raw string, so a missed substitution ships the
        // literal token to every crawler that reads /llms.txt.
        assert!(LLMS_TXT_TEMPLATE.contains(PLACEHOLDER));
        let served = LLMS_TXT_TEMPLATE.replace(PLACEHOLDER, POSITIONING_LONG);
        assert!(!served.contains(PLACEHOLDER));
        assert!(served.contains("comparison layer for agentic commerce"));
        // The braces the template needs kept must survive untouched.
        assert!(served.contains("/{api_slug}/{path}"));
    }

    #[test]
    fn the_short_line_fits_a_meta_description() {
        assert!(
            POSITIONING_SHORT.len() <= 160,
            "meta descriptions are truncated past ~160 chars, got {}",
            POSITIONING_SHORT.len()
        );
    }

    #[test]
    fn both_lines_lead_with_the_comparison_claim() {
        for line in [POSITIONING_SHORT, POSITIONING_LONG] {
            assert!(line.contains("comparison layer for agentic commerce"));
        }
        // The old positioning must not creep back into the canonical strings.
        assert!(!POSITIONING_SHORT.contains("Pay-per-call API gateway"));
        assert!(!POSITIONING_LONG.contains("Pay-per-call API gateway"));
    }
}
