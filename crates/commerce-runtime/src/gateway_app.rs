use std::sync::Arc;

use axum::{
    extract::Request,
    http::Method,
    middleware,
    routing::{any, get, post},
    Router,
};
use tower::ServiceBuilder;
use tower_http::{
    cors::{Any, CorsLayer},
    trace::{DefaultOnFailure, TraceLayer},
};
use tracing::Level;

use sentry::integrations::tower::{NewSentryLayer, SentryHttpLayer};

use crate::{api_management, proxy, traffic, v1_routes, AppState};

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(crate::gateway_routes::root))
        .route("/favicon.ico", get(crate::gateway_routes::favicon))
        .route("/favicon-dark.ico", get(crate::gateway_routes::favicon_dark))
        .route("/favicon.svg", get(crate::gateway_routes::favicon_svg))
        .route("/favicon-96x96.png", get(crate::gateway_routes::favicon_96))
        .route("/favicon-dark-96x96.png", get(crate::gateway_routes::favicon_dark_96))
        .route("/icon-maskable-512.png", get(crate::gateway_routes::icon_maskable))
        .route(
            "/apple-touch-icon.png",
            get(crate::gateway_routes::apple_touch_icon),
        )
        .route(
            "/site.webmanifest",
            get(crate::gateway_routes::site_webmanifest),
        )
        .route(
            "/web-app-manifest-192x192.png",
            get(crate::gateway_routes::web_app_manifest_192),
        )
        .route(
            "/web-app-manifest-512x512.png",
            get(crate::gateway_routes::web_app_manifest_512),
        )
        .route("/logo.svg", get(crate::gateway_routes::logo_svg))
        .route("/logo.png", get(crate::gateway_routes::logo_png))
        .route("/embed.js", get(crate::gateway_routes::embed_js))
        .route("/product.js", get(crate::gateway_routes::product_js))
        .route("/webmcp.js", get(crate::gateway_routes::webmcp_js))
        .route("/llms.txt", get(crate::gateway_routes::llms_txt))
        // Static SEO surfaces, so they match ahead of the `/:api_slug` proxy
        // fallback — otherwise a crawler's `/robots.txt` becomes an "unknown
        // API slug" and the site reads as uncrawlable / has no sitemap.
        .route("/robots.txt", get(crate::gateway_routes::robots_txt))
        .route("/sitemap.xml", get(crate::v1_routes::sitemap_xml))
        .route("/openapi.json", get(crate::gateway_routes::openapi_json))
        .route(
            "/.well-known/ai-plugin.json",
            get(crate::gateway_routes::well_known_ai_plugin),
        )
        // Agent-discovery namespace. All static paths, so they match ahead of
        // the `/:api_slug` proxy fallback (which used to turn a crawler's
        // `GET /.well-known/x402` into a 400 "unknown API slug").
        .route("/.well-known", get(crate::well_known::well_known_index))
        .route("/.well-known/", get(crate::well_known::well_known_index))
        .route("/.well-known/x402", get(crate::well_known::well_known_x402))
        .route(
            "/.well-known/x402.json",
            get(crate::well_known::well_known_x402),
        )
        .route(
            "/.well-known/agent.json",
            get(crate::well_known::well_known_agent_card),
        )
        .route(
            "/.well-known/agent-card.json",
            get(crate::well_known::well_known_agent_card),
        )
        .route(
            "/.well-known/mcp.json",
            get(crate::well_known::well_known_mcp),
        )
        .route(
            "/.well-known/security.txt",
            get(crate::well_known::well_known_security_txt),
        )
        .route("/health", any(crate::gateway_routes::health))
        .route("/status", any(crate::gateway_routes::status))
        .route(
            "/cache/invalidate",
            any(crate::gateway_routes::invalidate_cache),
        )
        .route("/types", get(v1_routes::list_types))
        .route("/types/:listing_type/v1", get(v1_routes::list_by_type))
        .route("/v1/apis", get(v1_routes::list_apis))
        .route("/v1/discover", get(v1_routes::discover))
        // The shape a caller should turn a question into, with this
        // catalogue's own capability vocabulary in it. Published so an agent
        // that has already read the question can search in our words instead
        // of paying for a second reading of its own sentence. Free, public,
        // and additive: it changes nothing about /v1/discover above.
        .route("/v1/discover/schema", get(v1_routes::discover_schema))
        // Question in, five callable endpoints out. A separate surface from
        // /v1/discover on purpose: that one serves the compare/decide/do
        // pipeline with its lanes and contracts, this one answers as fast as it
        // can and explains each pick. Neither changes for the other.
        .route("/v1/ask", get(v1_routes::ask))
        .route("/v1/answer", post(v1_routes::answer))
        .route(
            "/v1/engine/structured",
            post(v1_routes::structured_inference),
        )
        .route("/v1/parse", get(v1_routes::parse_only))
        .route("/v1/enrich", get(v1_routes::enrich_only))
        .route("/v1/find", get(v1_routes::find_all))
        .route("/v1/match", get(v1_routes::match_terms))
        // The whole chain in one paid call: parse -> decide -> bind -> pay ->
        // respond. POST does the job; GET explains itself to whoever pokes it.
        .route(
            "/v1/do",
            get(v1_routes::do_job_info).post(v1_routes::do_job),
        )
        .route("/v1/compare", get(v1_routes::compare))
        .route("/v1/decide", get(v1_routes::decide))
        .route("/v1/search", get(v1_routes::search))
        .route("/v1/select", get(v1_routes::select))
        // Browsing the ecosystem instead of searching it: index -> provider ->
        // endpoint. Free and unauthenticated like the rest of discovery; the
        // offers come out of the same settled path /v1/discover uses.
        .route("/v1/network", get(v1_routes::list_network))
        .route("/v1/sources", get(v1_routes::list_sources))
        // Before the parameterised route: axum matches in order, and
        // `/v1/sources/apis` would otherwise be read as source id "apis".
        .route("/v1/sources/apis", get(v1_routes::list_all_source_apis))
        .route("/v1/sources/:id/apis", get(v1_routes::list_source_apis))
        .route("/v1/sources/:id/apis/:host", get(v1_routes::get_source_api))
        .route("/v1/capabilities", get(v1_routes::list_capabilities))
        .route("/v1/capabilities/:slug", get(v1_routes::get_capability))
        .route("/v1/decisions", post(v1_routes::submit_decision))
        .route("/v1/decisions/:id/outcome", post(v1_routes::submit_outcome))
        // Admin-only: run the sweep's own parser against a candidate source
        // URL, so a shape can be checked before it is saved and a dead source
        // can be told apart from a merely slow one. Portal-session auth, then
        // an active-platform-admin check against the same table Postgres uses.
        .route(
            "/v1/admin/discovery-sources/probe",
            post(v1_routes::probe_source),
        )
        // Admin-only: snapshot one indexed source's sellers into
        // source_providers now, instead of on the crawler's next timer tick.
        // Same auth gate as the probe.
        .route(
            "/v1/admin/sources/:id/providers/refresh",
            post(v1_routes::refresh_source_providers),
        )
        .route("/v1/providers", get(v1_routes::list_providers))
        .route("/v1/providers/:username", get(v1_routes::get_provider_apis))
        .route("/v1/apis/:slug", get(v1_routes::get_api_detail))
        .route(
            "/v1/apis/:slug/preflight",
            get(v1_routes::preflight_get).post(v1_routes::preflight),
        )
        // The state of a held purchase. Registered here, well ahead of the
        // `/:api_slug` proxy fallback, or an agent polling an approval would be
        // proxied to a listing called "v1".
        // Priced offers a person can choose between. Registered here, ahead of
        // the `/:api_slug` proxy fallback, like every other /v1 route.
        // Doel -> plan -> bedrag. Read-only: no provider is called, no
        // resolver fires and nothing is reserved. See src/v1_routes/plans.rs.
        .route("/v1/plans", post(v1_routes::create_plan))
        .route("/v1/plans/:id", get(v1_routes::get_plan))
        // De job-API. POST antwoordt 202: het werk overleeft het verzoek.
        .route("/v1/jobs", post(v1_routes::start_job))
        .route("/v1/jobs/:id", get(v1_routes::get_job))
        .route("/v1/jobs/:id/events", get(v1_routes::job_events))
        .route("/v1/jobs/:id/facts", get(v1_routes::job_facts))
        .route("/v1/jobs/:id/resolve", post(v1_routes::resolve_job))
        .route("/v1/jobs/:id/cancel", post(v1_routes::cancel_job))
        .route("/v1/quote", axum::routing::post(v1_routes::quote))
        .route("/v1/approvals/:id", get(v1_routes::get_approval))
        .route("/v1/me", get(v1_routes::me))
        // Five routes used to live here and are gone for good:
        // `/v1/me/sepa`, `/v1/payout/link`, `/v1/payout/verify`,
        // `/v1/offramp/trigger` and `/v1/offramp/demo-fund`. They were the EUR
        // side of the business — a provider linking an IBAN through Monerium,
        // and the keeper that redeemed their USDC to euro over SEPA. Apiosk now
        // keeps exactly one payment rail: the buyer's Apiosk balance is debited
        // in the App's Supabase and the treasury pays the provider over x402.
        // There is no fiat leg left for these to drive, so they 404.
        // Connect (marketplace owner) — mint a marketplace API key. Owner-auth via
        // the portal session JWT; enforces the Idempotency-Key contract inline.
        .route(
            "/v1/marketplaces/:id/api-keys",
            axum::routing::post(crate::connect::marketplaces::generate_marketplace_api_key),
        )
        // Connect (marketplace owner) — mint the PUBLISHABLE embed key (apk_pub_).
        // Owner-auth via the portal session JWT, same idempotency contract.
        .route(
            "/v1/marketplaces/:id/publishable-keys",
            axum::routing::post(crate::connect::marketplaces::generate_marketplace_publishable_key),
        )
        // Connect (self-serve embed, 045) — the "Connect with Apiosk" snippet
        // starts onboarding with the marketplace's PUBLISHABLE key. Public: no
        // marketplace-secret auth; the apk_pub_ key only starts a draft connect.
        .route(
            "/v1/connect/embed/start",
            axum::routing::post(crate::connect::embed::start_embed_onboarding),
        )
        // Connect (skill onboarding, package 03) — a marketplace registers/updates a
        // billable skill for one of its merchants. Marketplace API-key auth (00-08).
        .route(
            "/v1/skills",
            axum::routing::post(crate::connect::skills::create_skill).route_layer(
                middleware::from_fn_with_state(
                    state.clone(),
                    crate::connect::api_key_auth::require_marketplace_api_key,
                ),
            ),
        )
        .route(
            "/v1/skills/:id",
            axum::routing::put(crate::connect::skills::update_skill).route_layer(
                middleware::from_fn_with_state(
                    state.clone(),
                    crate::connect::api_key_auth::require_marketplace_api_key,
                ),
            ),
        )
        // 04-02: skill charge paywall. PUBLIC (the paying agent has no marketplace
        // key) — returns an x402 402 with the fee split; no auth middleware.
        .route(
            "/v1/skills/:id/charge",
            get(crate::connect::charge::charge_skill),
        )
        // Connect (merchant onboarding, package 02).
        // 02-01: marketplace mints a signed onboarding link — marketplace API-key
        // auth (00-08 middleware) applied to just this route.
        .route(
            "/v1/merchants/onboarding-link",
            axum::routing::post(crate::connect::merchant_onboarding::generate_onboarding_link)
                .route_layer(middleware::from_fn_with_state(
                    state.clone(),
                    crate::connect::api_key_auth::require_marketplace_api_key,
                )),
        )
        // 02-02..02-07: token / merchant-session authenticated (no marketplace key).
        .route(
            "/v1/merchants/onboarding/verify",
            axum::routing::post(crate::connect::merchant_onboarding::verify_onboarding),
        )
        .route(
            "/v1/merchants/profile",
            axum::routing::post(crate::connect::merchant_onboarding::update_merchant_profile),
        )
        .route(
            "/v1/merchants/activate",
            axum::routing::post(crate::connect::merchant_onboarding::activate_merchant),
        )
        // Connect-token handoff (077). The buyer arrives from n8n/Make/Zapier,
        // finishes setup here, and the platform collects the token by code
        // exchange rather than by copy-paste — so a live spending credential
        // never travels through the browser. See `connect::oauth`.
        //
        // `park` is called by the buyer portal with the buyer's session. `token`
        // is called by the platform's SERVER and is public by design: the code
        // plus its PKCE verifier are the credential.
        .route(
            "/v1/connect/oauth/park",
            axum::routing::post(crate::connect::oauth::park_connect_token),
        )
        .route(
            "/v1/connect/oauth/token",
            axum::routing::post(crate::connect::oauth::exchange_code),
        )
        .route(
            "/v1/connect/oauth/clients/:client_id",
            get(crate::connect::oauth::describe_client),
        )
        .route(
            "/v1/siwe/nonce",
            axum::routing::post(crate::connect::siwe::issue_nonce),
        )
        .route(
            "/v1/siwe/verify",
            axum::routing::post(crate::connect::siwe::verify_wallet),
        )
        .route("/v1/balance", get(v1_routes::check_balance))
        .route("/v1/usage", get(v1_routes::usage_stats))
        // Gateway-as-payer: pay an EXTERNAL x402 endpoint from a connected
        // buyer's managed wallet. Registered before the /:api_slug catch-all so
        // it is not shadowed. 403s (feature_disabled) unless EXTERNAL_FETCH_ENABLED.
        .route(
            "/v1/x402/fetch",
            axum::routing::post(crate::proxy::external_fetch::external_x402_fetch),
        )
        .route(
            "/v1/presence/connect",
            axum::routing::post(proxy::presence_connect),
        )
        .route(
            "/v1/presence/heartbeat",
            axum::routing::post(proxy::presence_heartbeat),
        )
        .route(
            "/v1/presence/disconnect",
            axum::routing::post(proxy::presence_disconnect),
        )
        .route(
            "/v1/apis/register",
            axum::routing::post(api_management::register_api),
        )
        .route("/v1/apis/mine", get(api_management::my_apis))
        .route(
            "/v1/apis/:slug",
            axum::routing::post(api_management::update_api).delete(api_management::delete_api),
        )
        .route(
            "/v1/apis/:slug/endpoints/docs",
            axum::routing::put(api_management::update_endpoint_docs),
        )
        .route(
            "/datasets/:collection_slug/files/:file_slug",
            get(proxy::serve_dataset_file).head(proxy::serve_dataset_file),
        )
        .route(
            "/documents/:collection_slug/files/:file_slug",
            get(proxy::serve_document_file).head(proxy::serve_document_file),
        )
        .route(
            "/media/:collection_slug/:route_group/:file_slug",
            get(proxy::serve_media_file).head(proxy::serve_media_file),
        )
        .route("/mcp", any(crate::gateway_routes::proxy_mcp))
        .route("/mcp/*path", any(crate::gateway_routes::proxy_mcp))
        // Generic, reusable direct-purchase payment pointer (default-OFF; both
        // 404 unless PAYMENT_PROVIDERS configures an enabled provider). Static
        // paths, so they match ahead of the `/:api_slug` proxy fallback.
        .route(
            "/pay/charge",
            axum::routing::post(crate::pay::charge_handler),
        )
        .route(
            "/pay/verify",
            axum::routing::post(crate::pay::verify_handler),
        )
        // Optional agent intent survey. Every 402 advertises this (an
        // `apioskIntentSurvey` field + `X-Apiosk-Intent-*` headers, injected in
        // the traffic middleware); callers POST what they were looking for.
        // Free + unauthenticated (rate-limited per IP). GET is self-documenting.
        // Static path, so it matches ahead of the `/:api_slug` proxy fallback.
        .route(
            "/_apiosk/intent",
            axum::routing::post(crate::intent::submit_intent).get(crate::intent::intent_info),
        )
        // Where webmcp.js reports what it injected: how many browser-agent tools
        // it registered, for which listing, on which site. PUBLIC (it is called
        // cross-origin from provider pages) and per-IP rate limited; the site is
        // read from the browser-set Origin header, never from the body. Static
        // path, so it matches ahead of the `/:api_slug` proxy fallback.
        .route(
            "/_apiosk/webmcp",
            axum::routing::post(crate::webmcp_traffic::report_injection),
        )
        // The payment popup webmcp.js opens on a 402. It runs on THIS origin so
        // the buyer's session + connect token never reach the provider's page;
        // it makes the paid call itself and posts back only the response body.
        .route("/_apiosk/pay", get(crate::gateway_routes::pay_popup))
        // Commerce (webshop product script). PUBLIC: product.js POSTs scraped
        // {name, price, currency, url} + the integration id; resolve validates the
        // integration's domain + URL pattern and returns a { product_id, charge_url }.
        // The charge endpoint is the x402 paywall. Static paths, so they match ahead
        // of the `/:api_slug` proxy fallback.
        .route(
            "/v1/commerce/resolve",
            axum::routing::post(crate::commerce::resolve::resolve_product),
        )
        .route(
            "/commerce/products/:id",
            get(crate::commerce::charge::charge_product),
        )
        // Public, no-account self-serve intake for FEDERATED x402 listings. A
        // provider pastes their /.well-known/x402 URL; the gateway validates it
        // (including a live 402 probe) and shows a preview; on submit it lands as
        // a status='pending' federated row in the existing admin review queue.
        // Gated behind FEDERATED_SELF_SERVE_ENABLED (the handlers 404 when off).
        // Static prefix, so these match ahead of the `/:api_slug` proxy fallback.
        .route(
            "/federated/new",
            get(crate::federated_intake::federated_new),
        )
        .route(
            "/federated/inspect",
            axum::routing::post(crate::federated_intake::federated_inspect),
        )
        .route(
            "/federated/submit",
            axum::routing::post(crate::federated_intake::federated_submit),
        )
        // The tracked outbound hop every federated listing page links to
        // ("Open provider", and "Open" per published resource). Static prefix,
        // so it matches ahead of the `/:api_slug` proxy fallback — without this
        // route those links resolve to the proxy, which 404s with
        // "API 'f' not found".
        .route(
            "/f/:slug",
            get(crate::federated_traffic::federated_redirect),
        )
        .route(
            "/:api_slug",
            any(crate::gateway_routes::handle_request_no_path),
        )
        .route(
            "/:api_slug/",
            any(crate::gateway_routes::handle_request_no_path),
        )
        .route(
            "/:api_slug/*path",
            any(crate::gateway_routes::handle_request_with_path),
        )
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([
                    Method::GET,
                    Method::POST,
                    Method::PUT,
                    Method::DELETE,
                    Method::OPTIONS,
                ])
                .allow_headers(Any),
        )
        // The gateway is a reverse proxy: when an upstream API returns a non-2xx
        // (a 4xx from a bad client request, a 429 rate limit, a transient 5xx),
        // we faithfully pass that status through. tower-http's TraceLayer
        // classifies those passed-through responses as "failures" and, by
        // default, logs them at ERROR level — which the Sentry tracing
        // integration then captures as a new issue ("response failed"). That
        // pages us for upstream behaviour we neither caused nor can fix.
        //
        // Downgrade the failure log to WARN so it stays in the logs for
        // debugging but no longer escalates to a Sentry error. Genuine gateway
        // faults still surface through the explicit error!/panic paths and the
        // Sentry panic + reqwest integrations.
        .layer(TraceLayer::new_for_http().on_failure(DefaultOnFailure::new().level(Level::WARN)))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            traffic::track_request,
        ))
        // Sentry must be the OUTERMOST layer so each request gets its own hub and
        // performance transaction, capturing anything inner layers emit. A single
        // ServiceBuilder fixes the order sentry-tower requires (NewSentryLayer
        // outer, then SentryHttpLayer) and avoids the "wrong order => memory leak"
        // footgun of chaining the two layers directly. Listed LAST because axum
        // runs chained `.layer()` calls bottom-to-top (last listed = outermost).
        .layer(
            ServiceBuilder::new()
                .layer(NewSentryLayer::<Request>::new_from_top())
                .layer(SentryHttpLayer::new().enable_transaction()),
        )
        .with_state(state)
}
