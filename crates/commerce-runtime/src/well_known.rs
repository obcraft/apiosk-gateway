//! `/.well-known/*` agent-discovery endpoints.
//!
//! x402 crawlers (Coinbase Bazaar, Agent402, agent registries) probe a
//! gateway's `/.well-known/` namespace to find payable resources without a
//! human reading docs. Before this module only `ai-plugin.json` existed as a
//! static route, so `GET /.well-known/x402` fell through to the `/:api_slug`
//! proxy fallback and produced a 400 — the crawler learned nothing.
//!
//! All paths here are fully static, so axum's router matches them ahead of
//! the `/:api_slug/*path` proxy fallback.

use std::{borrow::Borrow, collections::HashMap, str::FromStr, sync::Arc};

use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use bigdecimal::BigDecimal;
use serde_json::{json, Value};
use tracing::error;

use crate::{
    db::{ApiInfo, NetworkToken},
    endpoint_docs::EndpointDoc,
    facilitator_caps::SupportedNetworks,
    payment::PaymentRequirement,
    AppState,
};
use uuid::Uuid;

const GATEWAY_BASE_URL: &str = "https://gateway.apiosk.com";
const MCP_BASE_URL: &str = "https://mcp.apiosk.com";

/// Resolves which network/asset/payTo a discovery item advertises, mirroring
/// the live 402 path in `payment_flow`: an endpoint's explicit network token
/// is honoured only when it is on the v1 allowlist AND the facilitator
/// supports it — anything else falls back to the Base USDC default. Keeping the discovery doc and
/// the live 402 challenge consistent is what stops a crawler from caching a
/// requirement the gateway would never actually issue.
struct X402Offers {
    default_token: NetworkToken,
    tokens_by_id: HashMap<Uuid, NetworkToken>,
    caps: Arc<SupportedNetworks>,
    platform_wallet: String,
}

impl X402Offers {
    /// (token, payTo) for one endpoint.
    fn resolve(&self, network_token_id: Option<Uuid>) -> (&NetworkToken, &str) {
        let fallback = (&self.default_token, self.platform_wallet.as_str());
        let Some(nt) = network_token_id.and_then(|id| self.tokens_by_id.get(&id)) else {
            return fallback;
        };
        if nt.network_name == self.default_token.network_name {
            return fallback;
        }
        match self.caps.support_for(&nt.network_name) {
            Some(_) => (nt, self.platform_wallet.as_str()),
            None => fallback,
        }
    }

    /// Global fallback offers every endpoint can currently advertise. Base is
    /// intentionally the only global fallback. Alternate networks are exposed
    /// only when the endpoint/API explicitly selects them;
    /// otherwise older buyers/indexers that assume a single EVM accept can stop
    /// before payment.
    fn global_fallback_offers(&self) -> Vec<(&NetworkToken, &str)> {
        vec![(&self.default_token, self.platform_wallet.as_str())]
    }

    /// Endpoint-specific accepts list: primary endpoint token first when it is
    /// currently usable, followed by every other globally enabled network.
    fn resolve_all(&self, network_token_id: Option<Uuid>) -> Vec<(&NetworkToken, &str)> {
        let mut offers = Vec::new();
        let primary = self.resolve(network_token_id);
        offers.push(primary);
        for offer in self.global_fallback_offers() {
            if !offers
                .iter()
                .any(|(existing, _)| existing.network_name == offer.0.network_name)
            {
                offers.push(offer);
            }
        }
        offers
    }

    /// Distinct network names this gateway currently advertises.
    fn enabled_networks(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .global_fallback_offers()
            .into_iter()
            .map(|(nt, _)| nt.network_name.clone())
            .collect();
        names.sort_unstable();
        names
    }
}

fn json_response(body: Value) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header(
            header::CACHE_CONTROL,
            "public, max-age=300, stale-while-revalidate=600",
        )
        .header("X-Apiosk-Gateway", "true")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| Response::new(Body::from(r#"{"error":"well_known_build_error"}"#)))
}

fn service_unavailable(code: &str) -> Response {
    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "error": code }).to_string()))
        .unwrap_or_else(|_| Response::new(Body::from(r#"{"error":"well_known_build_error"}"#)))
}

/// `GET /.well-known` and `GET /.well-known/` — machine-readable index of
/// every discovery document the gateway serves, so an agent that lands on
/// the namespace root can enumerate instead of guessing filenames.
pub async fn well_known_index() -> Response {
    json_response(json!({
        "gateway": "apiosk-gateway",
        "base_url": GATEWAY_BASE_URL,
        "well_known": [
            {
                "path": "/.well-known/x402",
                "description": "x402 discovery document: payable resources with their full x402 payment requirements (scheme, network, price in atomic USDC units, payTo). Paginated, best-documented first; walk links.next or narrow with ?page=, ?perPage=, ?category=, ?q=, ?documented=true."
            },
            {
                "path": "/.well-known/x402.json",
                "description": "Alias of /.well-known/x402."
            },
            {
                "path": "/.well-known/ai-plugin.json",
                "description": "AI plugin manifest (ChatGPT-plugin shape) pointing at the OpenAPI contract."
            },
            {
                "path": "/.well-known/agent.json",
                "description": "A2A-style agent card describing the gateway's skills, transport, and x402 payment model."
            },
            {
                "path": "/.well-known/agent-card.json",
                "description": "Alias of /.well-known/agent.json."
            },
            {
                "path": "/.well-known/mcp.json",
                "description": "MCP server discovery: the gateway's Model Context Protocol endpoint."
            },
            {
                "path": "/.well-known/security.txt",
                "description": "Security contact (RFC 9116)."
            }
        ],
        "other_discovery": {
            "llms_txt": format!("{GATEWAY_BASE_URL}/llms.txt"),
            "openapi": format!("{GATEWAY_BASE_URL}/openapi.json"),
            "api_catalog": format!("{GATEWAY_BASE_URL}/v1/apis"),
            "api_detail": format!("{GATEWAY_BASE_URL}/v1/apis/{{slug}}"),
            "capabilities": format!("{GATEWAY_BASE_URL}/v1/capabilities"),
            "capability_candidates": format!("{GATEWAY_BASE_URL}/v1/capabilities/{{capability_slug}}"),
            "discover": format!("{GATEWAY_BASE_URL}/v1/discover"),
            "compare": format!("{GATEWAY_BASE_URL}/v1/compare"),
            "decide": format!("{GATEWAY_BASE_URL}/v1/decide"),
            "do": format!("{GATEWAY_BASE_URL}/v1/do (POST, connect token: the whole chain plus payment in one call)"),
            "provider_directory": format!("{GATEWAY_BASE_URL}/v1/providers"),
            "provider_listings": format!("{GATEWAY_BASE_URL}/v1/providers/{{username}}"),
            "browse_by_type": format!("{GATEWAY_BASE_URL}/types"),
            "per_api_metadata": format!("{GATEWAY_BASE_URL}/{{api_slug}}/metadata"),
            "root_discovery": format!("{GATEWAY_BASE_URL}/ (Accept: application/json)")
        }
    }))
}

/// `GET /.well-known/x402` — the x402 discovery document.
///
/// Lists payable resources on the gateway with full, spec-shaped `accepts`
/// entries (the same `PaymentRequirement`s a live 402 challenge carries: exact
/// scheme, atomic USDC amount, platform payTo per network). Shape follows the
/// Bazaar/facilitator discovery-list convention (`x402Version` + `items[]`), so
/// a crawler can ingest it without probing each route for a 402 first.
///
/// **Paginated.** The document once emitted the entire catalog in one response
/// and had grown to 7.3 MB — roughly 1.8M tokens, past every context window and
/// past what most crawlers will read at all. A catalog that cannot be loaded is
/// not discoverable, so the default response is now a slim first page: the
/// best-documented resources first, plus the counts, category facets and
/// `links.next` an agent needs to walk the rest. Filters (`?page=`, `?perPage=`,
/// `?category=`, `?q=`, `?documented=true`) narrow it further.
///
/// The envelope is unchanged — `x402Version` + `items[]` at the top level — so
/// indexers that only understand the old shape keep working; they now read one
/// page instead of failing on all of it.
pub async fn well_known_x402(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
    headers: axum::http::HeaderMap,
) -> Response {
    let network_token = match state.db.get_default_network_token().await {
        Ok(Some(nt)) => nt,
        Ok(None) => {
            error!("well-known x402: default network token not configured");
            return service_unavailable("default_network_token_not_found");
        }
        Err(err) => {
            error!("well-known x402: network token fetch failed: {}", err);
            return service_unavailable("default_network_token_fetch_failed");
        }
    };

    // Cached for 60s (see `EndpointCache::get_discovery_catalog`). This is the
    // gateway's most expensive query, and paginating the document turned one
    // read of it per crawl into one per page — enough, under a crawler burst, to
    // start hitting the statement timeout and 503 the document agents read.
    let apis = match state.cache.get_discovery_catalog(&state.db).await {
        Ok(apis) => apis,
        Err(err) => {
            error!("well-known x402: listing APIs failed: {}", err);
            return service_unavailable("api_list_fetch_failed");
        }
    };

    let tokens = match state.db.list_x402_usdc_network_tokens().await {
        Ok(tokens) => tokens,
        Err(err) => {
            // Non-fatal: the discovery doc degrades to all-Base rather than 503.
            error!("well-known x402: network token list failed: {}", err);
            Vec::new()
        }
    };
    let caps = state
        .facilitator_caps
        .get(&state.http_client, &state.config, &state.facilitator_url)
        .await;
    let offers = X402Offers {
        default_token: network_token.clone(),
        tokens_by_id: tokens.into_iter().map(|nt| (nt.id, nt)).collect(),
        caps,
        platform_wallet: state.config.platform_wallet.clone(),
    };

    let query = DiscoveryQuery::from_params(&params);

    // Built once per catalog TTL, not once per request. Building the items is
    // what actually costs memory — every paid resource with its schemas and
    // examples — and a page of 15 used to allocate all ~1.5k of them and throw
    // the rest away. Under concurrent crawler reads that multiplied until the
    // process was OOM-killed. Shared read-only behind an `Arc`; filtering and
    // paging below copy only the handful of items that go out.
    let items = state
        .cache
        .get_discovery_items(|| {
            let mut items = build_x402_items(&apis, &offers);
            stamp_contract_tiers(&mut items);
            items
        })
        .await;

    // What the document leaves out, stated rather than silently dropped: an
    // agent that finds a federated resource elsewhere must be able to tell that
    // we index it on purpose, not that we lost it.
    let indexed_resources = items.len();
    // Borrowed views over the shared build from here down: filtering and
    // ranking move pointers, and only the items that actually go out are cloned.
    let published: Vec<&Value> = items
        .iter()
        .filter(|item| query.include.admits(item))
        .collect();
    let paid_resources = published.len();
    let withheld = indexed_resources - paid_resources;
    // Coverage, tiers and facets describe the WHOLE catalog, not the page in
    // hand: an agent reading page 1 still needs to know how much is out there
    // and where the parts it can act on are.
    let coverage = DocumentationCoverage::of(&published);
    let contracts = contract_counts(&published);
    let categories = category_facets(&published);

    let mut matched: Vec<&Value> = published
        .into_iter()
        .filter(|item| query.matches(item))
        .collect();
    // The resources an agent can act on lead every response, so a crawler that
    // reads only the first page — most of them do — gets the usable part of the
    // catalog rather than whichever listing happened to sort first. Stable, so
    // `db::RANKING_NOTE` order still decides ties.
    rank_documented_first(&mut matched);
    // A search asks a different question than a browse: the best answer is the
    // closest match, not the best-documented resource in the catalog.
    if !query.search_terms.is_empty() {
        rank_by_relevance(&mut matched, &query.search_terms);
    }
    let page = Page::new(&query, matched.len());
    let page_items: Vec<Value> = matched
        .into_iter()
        .skip(page.offset())
        .take(page.per_page)
        .cloned()
        .collect();

    // Both counters below are credited for THIS PAGE only, not for the whole
    // catalog. A listing on page 7 was not put in front of an agent that read
    // page 1, and counting it as if it were is how a discovery funnel comes to
    // report impressions nobody had.
    //
    // The whole /.well-known namespace is excluded from gateway_requests_v2
    // (traffic::track_request) because it is crawler-dominated, so this
    // aggregated counter is the only visibility we have into the top of the
    // federated funnel. Detached and fail-open — it cannot delay the response.
    let published = published_listings(&page_items);
    let federated_slugs: Vec<String> = published_listings(
        &page_items
            .iter()
            .filter(|item| {
                item.get("metadata")
                    .and_then(|metadata| metadata.get("listingType"))
                    .and_then(Value::as_str)
                    == Some("federated")
            })
            .cloned()
            .collect::<Vec<Value>>(),
    )
    .into_iter()
    .map(|(slug, _)| slug)
    .collect();
    crate::federated_traffic::bump(
        &state,
        federated_slugs,
        crate::federated_traffic::FederatedEvent::Discovery,
        crate::federated_traffic::ua_class(&headers),
    );

    // The same count for EVERY listing on the page, federated or not (mig 068),
    // tagged with the contract tier it went out with (mig 073) so "do documented
    // listings convert better?" is answerable from the funnel rather than from
    // belief. A first-party listing's calls are visible once someone pays; this
    // is the step before that — how often the listing was put in front of an
    // agent at all. Taken from the built items rather than the catalog, so a
    // listing that ended up with no payable resource is not credited with an
    // impression it never got. Buffered in-process: one map update per slug.
    state
        .discovery
        .record_impressions(&published, "/.well-known/x402", &headers);

    json_response(json!({
        "x402Version": 2,
        "provider": {
            "name": "Apiosk Gateway",
            "url": GATEWAY_BASE_URL,
            "description": crate::gateway_routes::POSITIONING_LONG,
        },
        "updatedAt": chrono::Utc::now().to_rfc3339(),
        "items": page_items,
        "pagination": page.to_value(&query),
        "meta": {
            "apis": apis.len(),
            "paidResources": paid_resources,
            // What this document publishes and what it deliberately does not.
            // Stated rather than silently omitted: an agent that meets a
            // federated Apiosk resource elsewhere should be able to see that we
            // index it on purpose, and where to ask for it.
            "publishes": {
                "include": query.include.as_str(),
                "resources": paid_resources,
                "withheldResources": withheld,
                "note": "Managed listings — hosted, priced and settled through Apiosk — are published here by default. Federated listings (external x402 endpoints Apiosk indexes but does not proxy) are withheld: they carry the provider's own payment terms and almost no request documentation. Ask for them with ?include=federated, both kinds with ?include=all, or browse them at /v1/apis?listing_type=federated.",
                "options": ["managed", "federated", "all"]
            },
            // How much of the catalog an agent can act on, and how to ask for
            // just that (`?contract=documented`). Every item carries its own
            // tier in metadata.contract.
            "contracts": contracts,
            // Where the callable parts of the catalog are. Each entry carries a
            // ready-made URL, so narrowing by category is one link-follow rather
            // than a parameter an agent has to infer.
            "categories": categories,
            "settlement": {
                "protocol": "x402",
                "scheme": "exact",
                // Default network; per-resource networks are in each item's
                // accepts[0].network (providers choose base/polygon/arbitrum).
                "network": network_token.network_name,
                "networks": offers.enabled_networks(),
                "asset": network_token.contract_address,
                "assetSymbol": network_token.token_symbol,
                "assetDecimals": network_token.token_decimals,
            },
            // Item order is not arbitrary and is not hidden: managed listings
            // (paid through Apiosk) lead the document, federated ones follow.
            // See db::RANKING_NOTE.
            "ranking": crate::db::RANKING_NOTE,
            // How much of the catalog carries a machine-usable contract. This is
            // published rather than kept internal on purpose: it is the number
            // we hold ourselves to, and it lets an agent weigh how much of this
            // document it can act on without a second round-trip.
            "documentation": coverage.to_value(),
            "itemFields": {
                "metadata.input": "JSON Schema for the query string, request body and extra headers this resource accepts.",
                "metadata.example": "A ready-to-run call: the resource URL with documented example values filled in, the request body, and the equivalent curl. `placeholders` lists required parameters we have no documented value for — fill those in before calling.",
                "metadata.output": "Response schema and a real example payload.",
                "metadata.pricing": "Base price plus any parameter-conditional pricing rules. When `model` is `per-call-conditional` the live 402 for your exact request is authoritative.",
                "metadata.metadataUrl": "Full, untruncated documentation for the listing. Values marked `truncated` are served in full there.",
                "accepts[].outputSchema": "x402 v1 discovery block ({input:{type,method,queryParams|bodyFields}, output}) — the same one the live 402 challenge carries."
            },
            "query": {
                "page": "1-based page number. Default 1, clamped to the last page. Walk the catalog by following links.next until it is null.",
                "perPage": format!("Resources per page. Default {DEFAULT_PER_PAGE}, maximum {MAX_PER_PAGE}."),
                "category": "Exact category match, case-insensitive. See meta.categories for the values and their counts.",
                "contract": "Keep one contract tier: `documented`, `callable` or `probe`. See meta.contracts for what each means and how many there are.",
                "include": "`managed` (default), `federated` or `all`. See meta.publishes.",
                "q": "Free-text search over the listing name, tags, resource URL, path, description and category. Every word must match; results are ranked by where they matched, name and tags first.",
                "documented": "`true` keeps only resources that carry at least one of metadata.input / metadata.example / metadata.output — the ones callable without a second round-trip."
            },
            "notes": "Amounts in accepts[].amount are atomic token units (x402 v2). Free endpoints are not listed here; browse the full catalog for those. This document is paginated: items[] is one page, meta.paidResources is the whole catalog, and resources carrying the most documentation are served first."
        },
        "links": {
            "self": page_url(&query, page.number),
            "first": page_url(&query, 1),
            "last": page_url(&query, page.total_pages()),
            "next": page.next().map(|next| page_url(&query, next)),
            "prev": page.prev().map(|prev| page_url(&query, prev)),
            "documented": documented_url(&query),
            "catalog": format!("{GATEWAY_BASE_URL}/v1/apis"),
            // The same catalog grouped by task rather than by seller: the
            // surface on which two of the resources below are visible to each
            // other as alternatives.
            "capabilities": format!("{GATEWAY_BASE_URL}/v1/capabilities"),
            // This document lists resources. These three say which of them to
            // buy and why — the question a resource list cannot answer about
            // itself. Crawlers land here first, so the chain is advertised here
            // rather than only on surfaces an agent has to go looking for.
            "discover": format!("{GATEWAY_BASE_URL}/v1/discover"),
            "compare": format!("{GATEWAY_BASE_URL}/v1/compare"),
            "decide": format!("{GATEWAY_BASE_URL}/v1/decide"),
            // The chain collapsed into one authenticated call: decide, fill the
            // winner's parameters from the job's words, pay it from the connect
            // token's wallet, return the provider's answer.
            "do": format!("{GATEWAY_BASE_URL}/v1/do"),
            "openapi": format!("{GATEWAY_BASE_URL}/openapi.json"),
            "llmsTxt": format!("{GATEWAY_BASE_URL}/llms.txt"),
            "aiPlugin": format!("{GATEWAY_BASE_URL}/.well-known/ai-plugin.json"),
            "agentCard": format!("{GATEWAY_BASE_URL}/.well-known/agent.json"),
            "mcp": format!("{GATEWAY_BASE_URL}/.well-known/mcp.json"),
        }
    }))
}

/// Resources per page when the caller does not ask for a size. Fully documented
/// resources are the largest ones and they lead the document, so this is sized
/// against them: measured against the live catalog, 15 puts the default response
/// at ~90 KB (~22k tokens) — one read for a crawler and a comfortable fit in
/// context, which the 7.3 MB unpaginated document was not.
const DEFAULT_PER_PAGE: usize = 15;

/// Hard ceiling on `?perPage=`. An indexer that wants the whole catalog pages
/// through it; nothing gets to ask for the entire 7 MB in one response again.
const MAX_PER_PAGE: usize = 100;

const X402_PATH: &str = "/.well-known/x402";

/// Which kind of listing the discovery document publishes.
///
/// Managed listings are the ones Apiosk hosts, prices and settles: we control
/// their documentation, we issue their 402, and we can hold them to a contract
/// an agent can act on. Federated listings are external x402 endpoints we index
/// but do not proxy — the provider's URL, the provider's payTo, and whatever
/// documentation they chose to publish, which in practice is almost none. They
/// outnumber the managed catalog seven to one, so publishing both in one
/// document meant the part we can actually stand behind was a rounding error in
/// it. They stay in the catalog (`/v1/apis?listing_type=federated`) and remain
/// callable; they are simply not what this document leads with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Include {
    Managed,
    Federated,
    All,
}

impl Include {
    fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("all") | Some("any") => Self::All,
            Some("federated") | Some("external") => Self::Federated,
            _ => Self::Managed,
        }
    }

    fn admits(&self, item: &Value) -> bool {
        let federated = item
            .get("metadata")
            .and_then(|metadata| metadata.get("listingType"))
            .and_then(Value::as_str)
            == Some("federated");
        match self {
            Self::All => true,
            Self::Managed => !federated,
            Self::Federated => federated,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Managed => "managed",
            Self::Federated => "federated",
        }
    }
}

/// The filters and page a caller asked for.
///
/// Parsed leniently on purpose: this endpoint is read by crawlers we do not
/// control and cannot ask to fix their requests. Answering `?page=abc` with the
/// first page is more useful than answering it with a 400 nobody reads, so
/// unparseable values fall back to the default rather than rejecting the read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DiscoveryQuery {
    page: usize,
    per_page: usize,
    category: Option<String>,
    /// The caller's `?q=` verbatim, echoed back in `pagination.filters` and links.
    search: Option<String>,
    /// The same term split into words, lowercased. Every word must match, so
    /// `?q=invoice pdf` narrows rather than widening the way an any-word match
    /// would — an agent looking for two things wants the resource that does both.
    search_terms: Vec<String>,
    contract: Option<String>,
    documented_only: bool,
    include: Include,
}

impl DiscoveryQuery {
    fn from_params(params: &HashMap<String, String>) -> Self {
        // Both spellings accepted: crawlers send whichever their client library
        // produces, and a silently ignored `per_page` reads as a broken filter.
        let value = |keys: &[&str]| -> Option<String> {
            keys.iter()
                .find_map(|key| params.get(*key))
                .map(|raw| raw.trim().to_string())
                .filter(|raw| !raw.is_empty())
        };
        let number = |keys: &[&str]| -> Option<usize> {
            value(keys).and_then(|raw| raw.parse::<usize>().ok())
        };

        let search = value(&["q", "search"]).map(|raw| raw.to_lowercase());
        let search_terms = search
            .as_deref()
            .map(|raw| raw.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default();

        Self {
            page: number(&["page"]).unwrap_or(1).max(1),
            per_page: number(&["perPage", "per_page", "limit"])
                .unwrap_or(DEFAULT_PER_PAGE)
                .clamp(1, MAX_PER_PAGE),
            category: value(&["category"]),
            search,
            search_terms,
            contract: value(&["contract"]).map(|raw| raw.to_lowercase()),
            documented_only: value(&["documented"])
                .map(|raw| matches!(raw.to_lowercase().as_str(), "true" | "1" | "yes"))
                .unwrap_or(false),
            include: Include::parse(
                value(&["include", "listingType", "listing_type"])
                    .map(|raw| raw.to_lowercase())
                    .as_deref(),
            ),
        }
    }

    fn matches(&self, item: &Value) -> bool {
        if let Some(category) = &self.category {
            if !item_category(item).eq_ignore_ascii_case(category) {
                return false;
            }
        }
        if let Some(contract) = &self.contract {
            if !contract_tier(item).eq_ignore_ascii_case(contract) {
                return false;
            }
        }
        if self.documented_only && documentation_score(item) == 0 {
            return false;
        }
        if !self.search_terms.is_empty() {
            let haystack = search_haystack(item);
            if !self.search_terms.iter().all(|term| haystack.contains(term)) {
                return false;
            }
        }
        true
    }
}

/// How much of a request contract an item carries, from the point of view of an
/// agent deciding whether it can call the thing.
///
/// Published per item and filterable, because "1,481 resources" is a useless
/// number to an agent if it cannot tell which of them it can act on. The tiers
/// state what we actually know, and never more than that:
///
/// * `documented` — an input schema, a ready-to-run example or an output schema
///   is attached. Callable without a second round-trip.
/// * `callable` — the method and the full resource URL are known, but no
///   parameter contract was published. A GET can be issued as-is; it may still
///   want parameters nobody documented.
/// * `probe` — not even a method is published. Issue the request and read the
///   terms off the live 402.
fn contract_tier(item: &Value) -> &'static str {
    if documentation_score(item) > 0 {
        return "documented";
    }
    let method = item
        .get("metadata")
        .and_then(|metadata| metadata.get("method"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if method.is_empty() {
        "probe"
    } else {
        "callable"
    }
}

/// Stamp each item with its contract tier so agents can filter and rank on it
/// without re-deriving the rule, and so `meta.contracts` and the item agree.
fn stamp_contract_tiers(items: &mut [Value]) {
    for item in items.iter_mut() {
        let tier = contract_tier(item);
        if let Some(metadata) = item
            .get_mut("metadata")
            .and_then(|metadata| metadata.as_object_mut())
        {
            metadata.insert("contract".to_string(), json!(tier));
        }
    }
}

/// How the catalog splits across the contract tiers. The honest headline number
/// for "how much of this can an agent actually use".
fn contract_counts<T: Borrow<Value>>(items: &[T]) -> Value {
    let mut documented = 0;
    let mut callable = 0;
    let mut probe = 0;
    for item in items {
        match contract_tier(item.borrow()) {
            "documented" => documented += 1,
            "callable" => callable += 1,
            _ => probe += 1,
        }
    }
    json!({
        "documented": documented,
        "callable": callable,
        "probe": probe,
        "meaning": {
            "documented": "Carries metadata.input, metadata.example or metadata.output — call it without a second round-trip.",
            "callable": "Method and full resource URL known, no parameter contract published. Issue it as-is; it may want parameters nobody documented.",
            "probe": "No method published. Send the request and read the terms off the live 402."
        }
    })
}

/// Which page of the matched resources this response carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Page {
    number: usize,
    per_page: usize,
    total_items: usize,
}

impl Page {
    /// A requested page past the end is clamped to the last one rather than
    /// answered with an empty `items[]`: an agent that over-walks `links.next`
    /// should land on real resources, not on a document that looks empty.
    fn new(query: &DiscoveryQuery, total_items: usize) -> Self {
        let page = Self {
            number: 1,
            per_page: query.per_page,
            total_items,
        };
        Self {
            number: query.page.min(page.total_pages()),
            ..page
        }
    }

    fn total_pages(&self) -> usize {
        // Always at least one page, so an empty result still has a valid
        // self/first/last link instead of page 0 of 0.
        self.total_items.div_ceil(self.per_page).max(1)
    }

    fn offset(&self) -> usize {
        (self.number - 1) * self.per_page
    }

    fn next(&self) -> Option<usize> {
        (self.number < self.total_pages()).then_some(self.number + 1)
    }

    fn prev(&self) -> Option<usize> {
        (self.number > 1).then_some(self.number - 1)
    }

    fn to_value(self, query: &DiscoveryQuery) -> Value {
        json!({
            "page": self.number,
            "perPage": self.per_page,
            "totalPages": self.total_pages(),
            // Resources left after the filters, which is what the page indexes
            // into. The unfiltered catalog total stays in meta.paidResources.
            "matchedResources": self.total_items,
            "filters": {
                "category": query.category,
                "contract": query.contract,
                "include": query.include.as_str(),
                "q": query.search,
                "documented": query.documented_only,
            },
        })
    }
}

/// `{GATEWAY_BASE_URL}/.well-known/x402` with this query's filters and the given
/// page. Built with a real encoder so a category or search term containing `&`
/// or a space produces a link an agent can follow verbatim.
fn page_url(query: &DiscoveryQuery, page: usize) -> String {
    let mut pairs = url::form_urlencoded::Serializer::new(String::new());
    pairs.append_pair("page", &page.to_string());
    if query.per_page != DEFAULT_PER_PAGE {
        pairs.append_pair("perPage", &query.per_page.to_string());
    }
    if let Some(category) = &query.category {
        pairs.append_pair("category", category);
    }
    if let Some(search) = &query.search {
        pairs.append_pair("q", search);
    }
    if let Some(contract) = &query.contract {
        pairs.append_pair("contract", contract);
    }
    if query.include != Include::Managed {
        pairs.append_pair("include", query.include.as_str());
    }
    if query.documented_only {
        pairs.append_pair("documented", "true");
    }
    format!("{GATEWAY_BASE_URL}{X402_PATH}?{}", pairs.finish())
}

/// The same filters, narrowed to resources an agent can call without a second
/// round-trip. Advertised in `links` because it is the shortcut most agents
/// reading this document actually want.
fn documented_url(query: &DiscoveryQuery) -> String {
    page_url(
        &DiscoveryQuery {
            page: 1,
            documented_only: true,
            ..query.clone()
        },
        1,
    )
}

/// How much of the contract an agent needs is attached to this item: a point
/// each for the input schema, the ready-to-run example and the output schema.
/// Drives both `?documented=true` and the ordering.
fn documentation_score(item: &Value) -> usize {
    let Some(metadata) = item.get("metadata") else {
        return 0;
    };
    ["input", "example", "output"]
        .iter()
        .filter(|block| metadata.get(**block).is_some())
        .count()
}

/// Sort the resources an agent can act on to the front: full documentation
/// first, then the ones whose method and URL are known, then the ones that need
/// a probe. Preserves the existing managed-before-federated order within each
/// tier (`sort_by_key` is stable, see `db::RANKING_NOTE`).
fn rank_documented_first<T: Borrow<Value>>(items: &mut [T]) {
    items.sort_by_key(|item| {
        let item = item.borrow();
        std::cmp::Reverse((documentation_score(item), tier_rank(contract_tier(item))))
    });
}

/// Rank a search result by how well it matches, strongest field first: what the
/// resource is called and how it is tagged beats where it lives, which beats a
/// mention in prose. Ties fall back to the documentation ordering, so among
/// equally relevant hits the callable one wins.
fn rank_by_relevance<T: Borrow<Value>>(items: &mut [T], terms: &[String]) {
    items.sort_by_key(|item| {
        let item = item.borrow();
        let metadata = item.get("metadata");
        let field = |key: &str| {
            metadata
                .and_then(|metadata| metadata.get(key))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_lowercase()
        };
        let tags = metadata
            .and_then(|metadata| metadata.get("tags"))
            .and_then(Value::as_array)
            .map(|tags| {
                tags.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<&str>>()
                    .join(" ")
                    .to_lowercase()
            })
            .unwrap_or_default();

        let strong = format!("{} {} {}", field("name"), field("api"), tags);
        let medium = format!(
            "{} {}",
            item.get("resource")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_lowercase(),
            field("path")
        );
        let weak = format!("{} {}", field("description"), field("category"));

        let score: usize = terms
            .iter()
            .map(|term| {
                usize::from(strong.contains(term)) * 4
                    + usize::from(medium.contains(term)) * 2
                    + usize::from(weak.contains(term))
            })
            .sum();
        std::cmp::Reverse(score)
    });
}

fn item_category(item: &Value) -> &str {
    item.get("metadata")
        .and_then(|metadata| metadata.get("category"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// Lowercased text a `?q=` term is matched against. Deliberately covers the
/// fields a human would search by — what the resource is called, what it does,
/// and the URL it lives at.
fn search_haystack(item: &Value) -> String {
    let mut haystack = item
        .get("resource")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if let Some(metadata) = item.get("metadata") {
        // `documentation` is included deliberately: it is where the words an
        // agent would actually search for live ("SEC Form 4", "relevance
        // score"), none of which appear in a one-line description.
        for field in [
            "api",
            "name",
            "description",
            "documentation",
            "path",
            "category",
        ] {
            if let Some(text) = metadata.get(field).and_then(Value::as_str) {
                haystack.push(' ');
                haystack.push_str(text);
            }
        }
        if let Some(tags) = metadata.get("tags").and_then(Value::as_array) {
            for tag in tags.iter().filter_map(Value::as_str) {
                haystack.push(' ');
                haystack.push_str(tag);
            }
        }
    }
    haystack.to_lowercase()
}

/// Category facets over the whole catalog: how many resources each category
/// holds, how many of those are documented, and the URL that filters to them.
/// Ordered by documented resources first — the categories an agent can act on
/// lead the list, not the ones that merely happen to be large.
fn category_facets<T: Borrow<Value>>(items: &[T]) -> Vec<Value> {
    let mut counts: HashMap<&str, (usize, usize)> = HashMap::new();
    for item in items {
        let item = item.borrow();
        let category = match item_category(item) {
            "" => "uncategorized",
            category => category,
        };
        let entry = counts.entry(category).or_insert((0, 0));
        entry.0 += 1;
        if documentation_score(item) > 0 {
            entry.1 += 1;
        }
    }

    let mut facets: Vec<(&str, usize, usize)> = counts
        .into_iter()
        .map(|(category, (resources, documented))| (category, resources, documented))
        .collect();
    // Name last so the order is total and the document is byte-stable between
    // reads of an unchanged catalog.
    facets.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.cmp(&a.1)).then(a.0.cmp(b.0)));

    facets
        .into_iter()
        .map(|(category, resources, documented)| {
            json!({
                "category": category,
                "resources": resources,
                "documented": documented,
                "url": page_url(
                    &DiscoveryQuery {
                        page: 1,
                        per_page: DEFAULT_PER_PAGE,
                        category: Some(category.to_string()),
                        search: None,
                        search_terms: Vec::new(),
                        contract: None,
                        documented_only: false,
                        include: Include::Managed,
                    },
                    1,
                ),
            })
        })
        .collect()
}

/// Ceiling on the bytes of endpoint documentation attached across the whole
/// document. Per-item caps (see `endpoint_docs`) bound one pathological
/// listing; this bounds the catalog. Past it, items keep their payment
/// requirement (the part an agent cannot work without) and drop the
/// documentation blocks, which are still served in full at `/{slug}/metadata`.
/// Sized well above the current catalog: at ~1.4k resources it is not expected
/// to bind, and `meta.documentation` shows it when it does.
const MAX_DOCUMENTATION_BYTES: usize = 8 * 1024 * 1024;

/// Build the `items[]` array for the x402 discovery document: one entry per
/// paid endpoint, each carrying a spec-shaped `accepts[0]` requirement.
/// Pure so it is unit-testable without a database.
fn build_x402_items(apis: &[ApiInfo], offers: &X402Offers) -> Vec<Value> {
    build_x402_items_within(apis, offers, MAX_DOCUMENTATION_BYTES)
}

fn build_x402_items_within(
    apis: &[ApiInfo],
    offers: &X402Offers,
    documentation_budget: usize,
) -> Vec<Value> {
    let zero = BigDecimal::from(0);
    let mut items = Vec::new();
    let mut budget = documentation_budget;

    for api in apis {
        // Federated (external, already-x402-native) listings are republished
        // verbatim from the provider's own discovery doc — provider resource
        // URL + provider accepts[]/payTo — never built from our platform offers.
        // Keyed on listing_type (the authoritative federated discriminator).
        if api.listing_type == "federated" {
            append_federated_items(&mut items, api, offers);
            continue;
        }

        let endpoints: Vec<EndpointDoc> = match serde_json::from_value(api.endpoint_docs.clone()) {
            Ok(docs) => docs,
            Err(err) => {
                error!(
                    "well-known x402: bad endpoint docs for {}: {}",
                    api.slug, err
                );
                continue;
            }
        };

        for ep in endpoints {
            if !ep.payment_required {
                continue;
            }
            // jsonb numeric arrives as a serde_json Number; round-trip via
            // its string form to keep exact decimal precision.
            let Ok(price) = BigDecimal::from_str(&ep.price_usd.to_string()) else {
                continue;
            };
            if price <= zero {
                continue;
            }

            let resource = ep.resource_url(GATEWAY_BASE_URL, &api.slug);
            let metadata_url = format!("{GATEWAY_BASE_URL}/{}/metadata", api.slug);
            let description = ep.description(&api.name, &api.description, &api.slug);

            // Documentation is attached while the document-wide budget lasts.
            // Payment terms never depend on it: an item past the budget is still
            // fully payable, just terser, and points at `/{slug}/metadata`.
            let documented = budget > 0;

            // The same `input`/`output` blocks the live 402 carries as Bazaar
            // discovery metadata (see `proxy::payment_flow::build_bazaar_metadata`).
            // Attaching them here is what makes the discovery document
            // self-sufficient: a crawler that ingests this array knows the
            // parameters without a second round-trip per resource.
            let discovery_schema = if documented {
                ep.discovery_schema(&metadata_url)
            } else {
                json!({ "input": { "type": "http", "method": ep.method_upper() } })
            };

            let mut accepts = Vec::new();
            // Every offer for one endpoint quotes the same price; keep the first
            // atomic amount for the pricing block below.
            let mut atomic = String::new();
            for (network_token, pay_to) in offers.resolve_all(ep.network_token_id) {
                let requirement = PaymentRequirement::for_network(
                    pay_to,
                    &price,
                    &resource,
                    &description,
                    network_token,
                );
                if atomic.is_empty() {
                    atomic = requirement.max_amount_required.clone();
                }
                let mut accept = requirement.to_v2_value();
                // Discovery-only enrichment. `to_v2_value` deliberately emits
                // the slim wire shape for a live 402 challenge; a discovery
                // document is read, not paid, and indexers (CDP Bazaar's v1
                // extractor among them) look for exactly these fields. Additive,
                // so a strict v2 client ignores them.
                if let Some(object) = accept.as_object_mut() {
                    object.insert("description".to_string(), json!(description));
                    object.insert("mimeType".to_string(), json!("application/json"));
                    object.insert("outputSchema".to_string(), discovery_schema.clone());
                }
                accepts.push(accept);
            }
            let mut metadata = json!({
                "api": api.slug,
                "name": api.name,
                "category": api.category,
                "listingType": api.listing_type,
                "method": ep.method_upper(),
                "path": ep.path,
                "priceUsd": ep.price_usd,
                "description": description,
                "docsUrl": api.docs_url,
                "metadataUrl": metadata_url,
                "tags": listing_tags(api),
                "pricing": ep.pricing(
                    &ep.price_usd.to_string(),
                    &atomic,
                    &offers.default_token.token_symbol,
                ),
            });
            let object = metadata.as_object_mut().expect("metadata is an object");
            if let Some(provider) = provider_block(api) {
                object.insert("provider".to_string(), provider);
            }
            // The three blocks an agent needs to make a correct first call.
            // Omitted rather than emitted empty, so an undocumented endpoint
            // stays as small as it is today and coverage stays measurable.
            if documented {
                if let Some(documentation) = ep.documentation() {
                    object.insert("documentation".to_string(), json!(documentation));
                }
                if let Some(input) = ep.input_contract(&metadata_url) {
                    object.insert("input".to_string(), input);
                }
                if let Some(example) = ep.example(&resource, &metadata_url) {
                    object.insert("example".to_string(), example);
                }
                if let Some(output) = ep.output_contract(&metadata_url) {
                    object.insert("output".to_string(), output);
                }
                if let Some(docs) = ep.doc_links() {
                    object.insert("docs".to_string(), docs);
                }

                // Charge what this item's documentation actually cost. Measured
                // rather than estimated, so one enormous listing cannot spend
                // the budget of ten small ones without being seen to.
                let spent = serde_json::to_string(object).map(|s| s.len()).unwrap_or(0);
                if spent >= budget {
                    error!(
                        "well-known x402: documentation budget of {} bytes exhausted at {} \
                         resources; remaining items are served without input/example/output \
                         blocks (full docs at /{{slug}}/metadata)",
                        MAX_DOCUMENTATION_BYTES,
                        items.len() + 1
                    );
                }
                budget = budget.saturating_sub(spent);
            }

            items.push(json!({
                "resource": resource,
                "type": "http",
                "x402Version": 2,
                "accepts": accepts,
                "lastUpdated": api.created_at.to_rfc3339(),
                "metadata": metadata,
            }));
        }
    }

    items
}

/// The distinct listings the built document actually advertises, each with the
/// contract tier it was published at. One listing usually contributes several
/// items (one per payable endpoint); an impression is per listing published, not
/// per resource, so duplicates collapse.
///
/// A listing whose resources span tiers is credited with its BEST one: that is
/// the offer the agent saw on this page, and crediting the worst would make a
/// listing look unusable because one of its ten endpoints lacks a schema.
fn published_listings(items: &[Value]) -> Vec<(String, &'static str)> {
    let mut listings: Vec<(String, &'static str)> = Vec::new();
    for item in items {
        let Some(slug) = item
            .get("metadata")
            .and_then(|metadata| metadata.get("api"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let tier = contract_tier(item);
        match listings.iter_mut().find(|(existing, _)| existing == slug) {
            Some((_, best)) => {
                if tier_rank(tier) > tier_rank(best) {
                    *best = tier;
                }
            }
            None => listings.push((slug.to_string(), tier)),
        }
    }
    listings
}

/// Order of the contract tiers, best first. Shared by the ranking and the
/// per-listing rollup so the document and the telemetry cannot disagree.
fn tier_rank(tier: &str) -> u8 {
    match tier {
        "documented" => 2,
        "callable" => 1,
        _ => 0,
    }
}

/// How many discovery items carry each block an agent needs. Published in
/// `meta.documentation` so the gaps are visible — to agents deciding how much of
/// the document they can act on, and to us, because a listing without an input
/// contract is a listing an agent cannot call without guessing.
#[derive(Debug, Default, PartialEq, Eq)]
struct DocumentationCoverage {
    resources: usize,
    with_input: usize,
    with_example: usize,
    with_output: usize,
}

impl DocumentationCoverage {
    fn of<T: Borrow<Value>>(items: &[T]) -> Self {
        let has = |item: &T, key: &str| {
            item.borrow()
                .get("metadata")
                .and_then(|m| m.get(key))
                .is_some()
        };
        Self {
            resources: items.len(),
            with_input: items.iter().filter(|item| has(item, "input")).count(),
            with_example: items.iter().filter(|item| has(item, "example")).count(),
            with_output: items.iter().filter(|item| has(item, "output")).count(),
        }
    }

    fn to_value(&self) -> Value {
        json!({
            "withInputSchema": self.with_input,
            "withExampleRequest": self.with_example,
            "withOutputSchema": self.with_output,
            "of": self.resources,
        })
    }
}

/// Search terms an agent (or an indexer's ranking) can match a resource on:
/// the provider's own tags, plus the category and slug, deduplicated and
/// lowercased. The Bazaar ranks on tags, so these are the difference between
/// "indexed" and "findable".
fn listing_tags(api: &ApiInfo) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    let mut push = |value: &str| {
        let value = value.trim().to_ascii_lowercase();
        if !value.is_empty() && !tags.contains(&value) && tags.len() < 12 {
            tags.push(value);
        }
    };

    if let Some(values) = api.listing_metadata.get("tags").and_then(Value::as_array) {
        for value in values.iter().filter_map(Value::as_str) {
            push(value);
        }
    }
    push(&api.category);
    push(&api.slug);
    tags
}

/// Tokens that carry no meaning in a URL path and would only dilute search.
const KEYWORD_STOPWORDS: &[&str] = &[
    "api",
    "apis",
    "v1",
    "v2",
    "v3",
    "json",
    "index",
    "public",
    "rest",
    "http",
    "https",
    "www",
    "com",
    "net",
    "org",
    "get",
    "post",
    "endpoint",
    "endpoints",
    "query",
    "data",
    "x402",
];

/// Search keywords derived from a resource URL: the host's first label and the
/// meaningful path segments.
///
/// Federated listings arrive with tags that are only `[category, slug]` and a
/// description written by our own importer, so the URL is the one place real
/// subject matter survives — `/api/senior/pension-intl` is the only thing in
/// that item that knows it is about pensions. Without this an agent searching
/// the catalog for "pension" finds nothing, though we index twenty such
/// endpoints.
fn resource_keywords(resource: &str) -> Vec<String> {
    let Ok(parsed) = url::Url::parse(resource) else {
        return Vec::new();
    };

    let mut keywords: Vec<String> = Vec::new();
    let mut push = |raw: &str| {
        let token = raw.trim().to_ascii_lowercase();
        if token.len() < 3
            || keywords.len() >= 8
            || token.chars().all(|c| c.is_ascii_digit())
            || KEYWORD_STOPWORDS.contains(&token.as_str())
            || keywords.contains(&token)
        {
            return;
        }
        keywords.push(token);
    };

    // Path first: it is the most specific thing we have, and it should not lose
    // the keyword budget to the host.
    for segment in parsed.path().split('/') {
        for token in segment.split(['-', '_', '.']) {
            push(token);
        }
    }
    // Then the subdomain labels — often the only topical label a provider gives
    // us (`travelpulse.example.com`). The registrable domain and TLD are left
    // out: they name the operator, not the subject, and the operator is already
    // stated properly in `metadata.provider`.
    if let Some(host) = parsed.host_str() {
        let labels: Vec<&str> = host.split('.').collect();
        for label in labels.iter().take(labels.len().saturating_sub(2)) {
            push(label);
        }
    }
    keywords
}

/// True when a listing description says only that we imported it — the shape
/// `federated-import` writes ("N x402 endpoints imported from <url>."). It
/// describes our plumbing, not the resource, and repeating it on every item
/// makes a thousand resources look identical to anything that reads them.
fn describes_only_the_import(description: &str) -> bool {
    let text = description.to_ascii_lowercase();
    text.contains("imported from") && text.contains("x402")
}

/// The description a federated resource would carry before disambiguation: the
/// provider's own words for this resource, else the listing's — but never the
/// importer's own note about having imported it. `None` means we have nothing
/// worth republishing and the caller should derive one from the URL.
fn shared_description_candidate(res: &Value, api: &ApiInfo) -> Option<String> {
    res.get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let listing = api.description.trim();
            (!listing.is_empty() && !describes_only_the_import(listing))
                .then(|| listing.to_string())
        })
}

/// The part of a resource that distinguishes it from its siblings: method and
/// path, e.g. `GET /v1/options/greeks`.
fn resource_detail(method: &str, resource: &str) -> String {
    match url::Url::parse(resource) {
        Ok(parsed) => format!("{method} {}", parsed.path()),
        Err(_) => format!("{method} {resource}"),
    }
}

/// A description for a federated resource when the provider gave us none we can
/// use. Built from what the URL actually tells us — method, path, host — and
/// nothing more: it is not a summary of what the endpoint returns and does not
/// pretend to be one. Its job is to make the item findable and distinguishable
/// from the other 1,295 resources on the same listing.
fn derived_resource_description(method: &str, resource: &str, fallback_name: &str) -> String {
    let Ok(parsed) = url::Url::parse(resource) else {
        return format!("{method} {resource} on {fallback_name}");
    };
    let host = parsed.host_str().unwrap_or(fallback_name);
    let path = parsed.path();
    let keywords = resource_keywords(resource);

    // The keyword list repeats the path in the words an agent would search for
    // ("senior pension intl"), which a raw path does not match on.
    if keywords.is_empty() {
        format!("{method} {path} on {host}.")
    } else {
        format!(
            "{method} {path} on {host}. Subject: {}.",
            keywords.join(", ")
        )
    }
}

/// Who publishes the resource. Agents increasingly weigh provenance before they
/// spend, and "which provider is this" is otherwise a second round-trip.
fn provider_block(api: &ApiInfo) -> Option<Value> {
    let name = api
        .provider_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let username = api
        .provider_username
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if name.is_none() && username.is_none() {
        return None;
    }

    let mut block = serde_json::Map::new();
    if let Some(name) = name {
        block.insert("name".to_string(), json!(name));
    }
    if let Some(username) = username {
        block.insert("username".to_string(), json!(username));
        block.insert(
            "url".to_string(),
            json!(format!("{GATEWAY_BASE_URL}/v1/providers/{username}")),
        );
    }
    if let Some(sector) = api
        .provider_sector
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        block.insert("sector".to_string(), json!(sector));
    }
    Some(Value::Object(block))
}

/// The gateway path a federated resource is served on: the provider URL's path,
/// with scheme+host and any query string stripped.
///
/// Returns `None` for a resource with no usable path — such a resource cannot be
/// addressed through the gateway, and the caller drops it rather than falling
/// back to the provider's own URL.
fn federated_route_path(resource: &str) -> Option<String> {
    let path = resource.split('?').next().unwrap_or(resource);
    let path = match path.find("://") {
        Some(i) => {
            let j = path[i + 3..].find('/')?;
            &path[i + 3 + j..]
        }
        None => path,
    };
    let path = path.trim_end_matches('/');
    if path.is_empty() || !path.starts_with('/') {
        return None;
    }
    Some(path.to_string())
}

/// The provider's asking price, read out of their own `accepts[]`.
///
/// This is the number Apiosk pays a federated provider and the base for the
/// buyer's `list + 15%`. It is read here and never republished — the provider's
/// raw accepts entry (with their payTo) does not survive into the document.
///
/// Delegates to [`crate::federated_intake::min_price_of`], the one reader that
/// already handles both spellings (`amount` and `maxAmountRequired`), numeric
/// as well as string values, and takes the lowest across networks. A second
/// reader here would be a second thing to keep in step, and the failure mode of
/// getting it wrong is pricing a whole listing at zero.
fn federated_list_price(accepts: &Value) -> Option<BigDecimal> {
    let entries = accepts.as_array()?;
    let usd = crate::federated_intake::min_price_of(entries)?;
    BigDecimal::from_str(&usd.to_string())
        .ok()
        .filter(|p| p > &BigDecimal::from(0))
}

/// Emit discovery items for a federated (external, already-x402-native)
/// listing, **masked behind the gateway**.
///
/// These items used to be republished verbatim — the provider's own resource
/// URL and their `accepts[]` (their payTo, their networks, their amounts) — so
/// an agent paid the provider directly and Apiosk saw nothing. Under the hybrid
/// fee model that is a hole in the model, not a feature: the buyer-facing rule
/// is `list + 15%`, and a published direct route to the provider at bare list
/// price is a bypass of exactly that 15%.
///
/// So every federated resource is now presented the same way a managed one is:
///
///   * `resource` is the **gateway URL** (`{GATEWAY_BASE_URL}/{slug}{path}`),
///     never the provider's origin;
///   * `accepts[]` is **rebuilt by Apiosk** — payTo is the platform wallet, and
///     the amount is the buyer price (`list + 15%`), not the provider's amount;
///   * the provider's payTo, networks and amounts do not appear at all.
///
/// A federated listing is therefore indistinguishable from a managed one on the
/// wire, which is the point: the class decides what Apiosk pays the provider,
/// never what the buyer sees or pays.
///
/// `ref_marker` no longer applies. It appended a referral query param to the
/// provider's URL, and no provider URL is emitted any more.
fn append_federated_items(items: &mut Vec<Value>, api: &ApiInfo, offers: &X402Offers) {
    let Some(resources) = api.external_x402.as_array() else {
        return;
    };

    // How often each candidate description is reused across this listing. A
    // description that covers every resource describes the LISTING, not the
    // resource — 39 endpoints on one listing all read "Official French &
    // European company data…", and an agent searching the catalog cannot tell
    // which of them to call. Counted up front so the emit loop below knows
    // which descriptions need the resource's own path appended.
    let mut occurrences: HashMap<String, usize> = HashMap::new();
    for res in resources {
        if let Some(candidate) = shared_description_candidate(res, api) {
            *occurrences.entry(candidate).or_insert(0) += 1;
        }
    }

    for res in resources {
        // A federated resource must carry an explicit resource URL and a
        // non-empty accepts[]; skip anything malformed so the doc stays valid.
        let Some(resource) = res.get("resource").and_then(Value::as_str) else {
            continue;
        };
        let accepts = res.get("accepts").cloned().unwrap_or_else(|| json!([]));
        if !accepts.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
            continue;
        }
        let res_type = res
            .get("type")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("http");
        let method = res
            .get("metadata")
            .and_then(|m| m.get("method"))
            .and_then(Value::as_str)
            .unwrap_or("GET");
        let name = if api.name.trim().is_empty() {
            api.slug.as_str()
        } else {
            api.name.trim()
        };

        // Prefer the provider's own words for THIS resource, then the listing
        // description — but only when the latter describes the API rather than
        // our import of it ("N x402 endpoints imported from <url>." is written
        // once per listing by `federated-import` and says nothing about the
        // resource). Whatever we land on, if it is shared with another resource
        // on the same listing it gets the method and path appended, so no two
        // items in the document read identically. Both fallbacks keep the
        // provider's words where there are any; neither invents a claim about
        // what the endpoint returns.
        let description = match shared_description_candidate(res, api) {
            Some(candidate) => {
                if occurrences.get(&candidate).copied().unwrap_or(0) > 1 {
                    format!("{candidate} ({})", resource_detail(method, resource))
                } else {
                    candidate
                }
            }
            None => derived_resource_description(method, resource, name),
        };

        // Mask the provider's origin: publish the gateway route for this
        // resource instead. `federated_route_path` derives the path the proxy
        // serves; a resource whose path cannot be derived is dropped rather
        // than emitted with the provider's URL, because emitting it would be
        // the bypass this masking exists to close.
        let Some(path) = federated_route_path(resource) else {
            continue;
        };
        let resource_url = format!("{GATEWAY_BASE_URL}/{}{}", api.slug, path);

        // The provider's asking price, read from their own accepts[]. This is
        // the LIST price — what Apiosk pays them — and it is never published.
        let Some(list_price) = federated_list_price(&accepts) else {
            continue;
        };
        // What the buyer is quoted and charged: list + 15%, the same rule a
        // managed listing follows.
        let buyer_price =
            crate::fees::quote_usdc(&list_price, crate::fees::ProviderClass::Federated)
                .buyer_total_usdc();

        // Apiosk's own payment terms, replacing the provider's entirely.
        let apiosk_accepts: Vec<Value> = offers
            .resolve_all(None)
            .into_iter()
            .map(|(network_token, pay_to)| {
                let mut accept = PaymentRequirement::for_network(
                    pay_to,
                    &buyer_price,
                    &resource_url,
                    &description,
                    network_token,
                )
                .to_v2_value();
                if let Some(object) = accept.as_object_mut() {
                    object.insert("description".to_string(), json!(description));
                    object.insert("mimeType".to_string(), json!("application/json"));
                }
                accept
            })
            .collect();

        let mut metadata = res
            .get("metadata")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        metadata.insert("api".to_string(), json!(api.slug));
        metadata.insert("name".to_string(), json!(api.name));
        metadata.insert("category".to_string(), json!(api.category));
        // Both of these used to advertise the listing as externally hosted,
        // which is what told a client to go to the provider directly. The
        // gateway serves this resource now, so it reports itself as the host.
        metadata.insert("listingType".to_string(), json!("api"));
        metadata.insert("hostedExternally".to_string(), json!(false));
        // The provider's own metadata block can carry their origin, endpoint or
        // payment address. None of it survives the mask.
        metadata.remove("payTo");
        metadata.remove("resource");
        metadata.remove("origin");
        metadata.remove("upstream");
        metadata.insert("description".to_string(), json!(description));
        metadata.insert("docsUrl".to_string(), json!(api.docs_url));
        metadata.insert(
            "metadataUrl".to_string(),
            json!(format!("{GATEWAY_BASE_URL}/{}/metadata", api.slug)),
        );
        // Provider tags first, then the listing's, then the ones the URL implies.
        // A federated listing's own tags are `[category, slug]` — identical on
        // every one of its resources — so without the URL keywords the entire
        // listing is one undifferentiated blob to any tag-based filter.
        let mut tags: Vec<String> = metadata
            .get("tags")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|tag| tag.trim().to_ascii_lowercase())
                    .filter(|tag| !tag.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        for tag in listing_tags(api)
            .into_iter()
            .chain(resource_keywords(resource))
        {
            if !tags.contains(&tag) && tags.len() < 16 {
                tags.push(tag);
            }
        }
        metadata.insert("tags".to_string(), json!(tags));
        if let Some(provider) = provider_block(api) {
            metadata.entry("provider".to_string()).or_insert(provider);
        }

        items.push(json!({
            "resource": resource_url,
            "type": res_type,
            "x402Version": 2,
            // Apiosk's terms, not the provider's: platform wallet, buyer price.
            // The provider's payTo/network/amount are deliberately absent.
            "accepts": apiosk_accepts,
            "lastUpdated": api.created_at.to_rfc3339(),
            "metadata": metadata,
        }));
    }
}

/// `GET /.well-known/agent.json` (alias `/.well-known/agent-card.json`) —
/// A2A-style agent card. Static: describes the gateway itself, its uniform
/// discovery/execute contract, and the x402 payment model.
pub async fn well_known_agent_card() -> Response {
    json_response(json!({
        "protocolVersion": "0.3.0",
        "name": "Apiosk Gateway",
        "description": crate::gateway_routes::POSITIONING_LONG,
        "url": GATEWAY_BASE_URL,
        "preferredTransport": "HTTP+JSON",
        "provider": {
            "organization": "Apiosk",
            "url": "https://apiosk.com"
        },
        "version": env!("CARGO_PKG_VERSION"),
        "documentationUrl": "https://apiosk.com/docs",
        "capabilities": {
            "streaming": false,
            "pushNotifications": false,
            "stateTransitionHistory": false
        },
        "defaultInputModes": ["application/json"],
        "defaultOutputModes": ["application/json"],
        "skills": [
            {
                "id": "compare-providers",
                "name": "Discover, compare and decide between providers",
                "description": "The part a provider's own API cannot answer: how it compares to the alternatives. Three chained endpoints, none of which charges anything. GET /v1/discover returns the candidates that can perform a task, each with a stable candidate_id. GET /v1/compare turns those ids into evidence — price, measured latency, measured success rate and input compatibility side by side, each scored 0-100 alongside the weights and per-dimension contributions that produced the number, so it can be recomputed rather than trusted. GET /v1/decide returns one provider with the rule that picked it, every rejected candidate and the exact constraint that removed it, and the runners-up in order. State constraints once (max_price, max_latency_ms, min_reliability, settlement, require_all_inputs, optimize_for) and pass the same ones down the chain. Dimensions Apiosk has not measured are dropped from the weighting and named, never scored zero.",
                "tags": ["comparison", "decision", "discovery", "ranking"],
                "examples": [
                    format!("GET {GATEWAY_BASE_URL}/v1/discover?q=read+a+web+page"),
                    format!("GET {GATEWAY_BASE_URL}/v1/compare?candidates=<id>,<id>&optimize_for=latency"),
                    format!("GET {GATEWAY_BASE_URL}/v1/decide?q=read+a+web+page&max_price=0.05"),
                    format!("GET {GATEWAY_BASE_URL}/v1/decide?capability=<capability-slug>&min_reliability=0.95")
                ]
            },
            {
                "id": "do-job",
                "name": "Do a job end to end: decide, pay, return the answer",
                "description": "POST /v1/do collapses the whole chain into one authenticated call: describe a job in plain words, and Apiosk parses it, finds the candidates, scores them, picks one, fills the winner's own documented parameters in from your words, pays it from the wallet behind your connect token (created once in the buyer portal, carrying the spending caps you set there), and returns the provider's response with the payment receipt. Fail-closed: nothing is paid when no capability matches (404), when every candidate fails your constraints (409), or when the provider requires a parameter your job did not state (422) — the missing values are named instead of guessed. Send dry_run=true for everything except the payment. GET /v1/do documents itself.",
                "tags": ["execution", "payments", "decision", "x402"],
                "examples": [
                    format!("POST {GATEWAY_BASE_URL}/v1/do {{\"job\": \"Get Nvidia earnings news from the last 7 days\", \"max_price\": 0.05}}"),
                    format!("POST {GATEWAY_BASE_URL}/v1/do {{\"job\": \"...\", \"dry_run\": true}}")
                ]
            },
            {
                "id": "discover-paid-resources",
                "name": "Discover paid resources",
                "description": "The x402 discovery document: payable resources with the spec-shaped accepts[] a live 402 carries. Publishes managed listings (hosted, priced and settled through Apiosk) by default; add ?include=federated or ?include=all for the external endpoints Apiosk indexes but does not proxy. Paginated — items[] is one page, and resources are served best-documented first, so the first page is the part you can call without a second round-trip. Walk links.next; narrow with ?category=, ?q=, ?contract=documented and ?perPage= (max 100). meta.categories lists every category with its counts and a ready-made URL.",
                "tags": ["discovery", "x402", "payments"],
                "examples": [
                    format!("GET {GATEWAY_BASE_URL}/.well-known/x402"),
                    format!("GET {GATEWAY_BASE_URL}/.well-known/x402?documented=true"),
                    format!("GET {GATEWAY_BASE_URL}/.well-known/x402?category=finance&perPage=50"),
                    format!("GET {GATEWAY_BASE_URL}/.well-known/x402?q=weather")
                ]
            },
            {
                "id": "discover-apis",
                "name": "Discover APIs",
                "description": "List every active, verified listing with its provider, pricing, endpoints, schemas, examples and pricing rules. Filter on search, category, tag, listing_type, min_price, max_price, created_after, created_before; sort by name, price or newest. Results are ranked managed-first: listings Apiosk publishes and operates come before federated listings it only executes, with the requested sort applied within each group (see meta.ranking). Every listing is called and settled through the gateway either way.",
                "tags": ["discovery", "catalog"],
                "examples": [
                    format!("GET {GATEWAY_BASE_URL}/v1/apis?search=weather"),
                    format!("GET {GATEWAY_BASE_URL}/v1/apis?max_price=0.01&sort=price&order=asc"),
                    format!("GET {GATEWAY_BASE_URL}/v1/apis/{{slug}}"),
                    format!("GET {GATEWAY_BASE_URL}/types")
                ]
            },
            {
                "id": "discover-providers",
                "name": "Discover providers",
                "description": "Browse who publishes on the gateway (name, username, avatar, bio, sector, listing count), most listings first, then open one provider's catalog.",
                "tags": ["discovery", "catalog", "providers"],
                "examples": [
                    format!("GET {GATEWAY_BASE_URL}/v1/providers"),
                    format!("GET {GATEWAY_BASE_URL}/v1/providers/{{username}}")
                ]
            },
            {
                "id": "read-api-metadata",
                "name": "Read API metadata",
                "description": "MCP-style, agent-native manifest for one API: operations, input/output schemas, pricing, tool annotations.",
                "tags": ["discovery", "mcp"],
                "examples": [format!("GET {GATEWAY_BASE_URL}/{{api_slug}}/metadata")]
            },
            {
                "id": "execute-api",
                "name": "Execute an API",
                "description": "Call any listing through the uniform execute contract or the transparent proxy path — every listing is gateway-hosted. Paid endpoints return HTTP 402 with x402 payment requirements until a payment proof is supplied.",
                "tags": ["execution", "x402", "payments"],
                "examples": [
                    format!("POST {GATEWAY_BASE_URL}/{{api_slug}}/execute"),
                    format!("GET {GATEWAY_BASE_URL}/{{api_slug}}/{{path}}")
                ]
            }
        ],
        "payments": {
            "protocol": "x402",
            "challengeStatus": 402,
            "proofHeader": "X-Payment",
            "settlementAsset": "USDC",
            "settlementNetwork": "Base by default; see /.well-known/x402 accepts[] for other enabled EVM networks",
            "discovery": format!("{GATEWAY_BASE_URL}/.well-known/x402")
        }
    }))
}

/// `GET /.well-known/mcp.json` — MCP server discovery. Points MCP-capable
/// agents at the gateway's Model Context Protocol endpoint.
pub async fn well_known_mcp() -> Response {
    json_response(json!({
        "mcpServers": {
            "apiosk": {
                "url": format!("{MCP_BASE_URL}/mcp"),
                "transport": "streamable-http",
                "description": "Apiosk MCP: ask for a job, receive the App-ranked top runnable provider and exact buyer price, then explicitly Approve or Deny execution. Discovery and comparison are read-only; only apiosk_execute can spend."
            }
        },
        "comparison_layer": {
            "over_mcp": ["apiosk", "apiosk_discover", "apiosk_compare"],
            "over_http": [
                format!("{GATEWAY_BASE_URL}/v1/discover"),
                format!("{GATEWAY_BASE_URL}/v1/compare"),
                format!("{GATEWAY_BASE_URL}/v1/decide")
            ],
            "note": "The MCP short path uses the App agent gateway's top pick, so the App and MCP expose the same relevance ranking and price. Execution remains a separate, explicitly approved tool call."
        },
        "documentation": "https://apiosk.com/docs"
    }))
}

/// `GET /.well-known/security.txt` — RFC 9116 security contact.
pub async fn well_known_security_txt() -> Response {
    const BODY: &str = "Contact: mailto:support@apiosk.com\n\
Canonical: https://gateway.apiosk.com/.well-known/security.txt\n\
Preferred-Languages: en, nl\n\
Policy: https://apiosk.com\n\
Expires: 2027-07-01T00:00:00.000Z\n";
    (
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        BODY,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_network_token() -> NetworkToken {
        NetworkToken {
            id: uuid::Uuid::nil(),
            network_name: "base".to_string(),
            network_display_name: "Base".to_string(),
            chain_id: 8453,
            explorer_url: None,
            is_testnet: false,
            token_symbol: "USDC".to_string(),
            token_name: "USD Coin".to_string(),
            token_decimals: 6,
            contract_address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_string(),
        }
    }

    fn test_offers() -> X402Offers {
        X402Offers {
            default_token: test_network_token(),
            tokens_by_id: HashMap::new(),
            caps: Arc::new(SupportedNetworks::base_only()),
            platform_wallet: "0xA039PLATFORM".to_string(),
        }
    }

    fn test_api(slug: &str, endpoint_docs: Value) -> ApiInfo {
        ApiInfo {
            id: slug.to_string(),
            slug: slug.to_string(),
            name: slug.to_string(),
            description: format!("{slug} description"),
            price_usd: BigDecimal::from(0),
            category: "data".to_string(),
            implementation_type: "direct".to_string(),
            model_backed: false,
            listing_type: "api".to_string(),
            docs_url: format!("https://apiosk.com/docs/{slug}"),
            origin_url: "https://upstream.example".to_string(),
            gateway_url: format!("https://gateway.apiosk.com/{slug}"),
            active: true,
            verified: true,
            endpoint_count: 1,
            endpoint_docs,
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
    fn x402_items_include_only_paid_endpoints_with_atomic_amounts() {
        let api = test_api(
            "weather",
            json!([
                {
                    "method": "GET",
                    "path": "/current",
                    "description": "Current weather",
                    "price_usd": 0.02,
                    "payment_required": true
                },
                {
                    "method": "GET",
                    "path": "/free",
                    "description": "Free endpoint",
                    "price_usd": 0,
                    "payment_required": false
                }
            ]),
        );

        let items = build_x402_items(&[api], &test_offers());
        assert_eq!(items.len(), 1);

        let item = &items[0];
        assert_eq!(
            item["resource"],
            "https://gateway.apiosk.com/weather/current"
        );
        assert_eq!(item["type"], "http");
        assert_eq!(item["x402Version"], 2);

        // x402 v2 discovery item: `amount` (not maxAmountRequired) + CAIP-2
        // network so a v2 crawler (x402scan) can ingest it directly.
        let accept = &item["accepts"][0];
        assert_eq!(accept["scheme"], "exact");
        assert_eq!(accept["network"], "eip155:8453");
        // 0.02 USDC at 6 decimals = 20000 atomic units
        assert_eq!(accept["amount"], "20000");
        assert!(accept.get("maxAmountRequired").is_none());
        assert_eq!(accept["payTo"], "0xA039PLATFORM");
        assert_eq!(
            accept["asset"],
            "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
        );
    }

    #[test]
    fn items_carry_the_contract_an_agent_needs_to_call_the_resource() {
        let mut api = test_api(
            "ipdata",
            json!([
                {
                    "method": "GET",
                    "path": "/lookup",
                    "description": "Geolocate an IPv4 or IPv6 address.",
                    "price_usd": 0.004,
                    "payment_required": true,
                    "query_params": {
                        "type": "object",
                        "properties": {
                            "ip": { "type": "string", "example": "8.8.8.8" }
                        },
                        "required": ["ip"]
                    },
                    "response_body": { "type": "object", "properties": { "city": { "type": "string" } } },
                    "response_example": { "city": "Montréal" }
                }
            ]),
        );
        api.listing_metadata = json!({ "tags": ["geolocation", "IP"] });
        api.provider_name = Some("Ipdata".to_string());
        api.provider_username = Some("ipdata".to_string());

        let items = build_x402_items(&[api], &test_offers());
        let metadata = &items[0]["metadata"];

        // Everything needed for a correct first call, without a second trip.
        assert_eq!(
            metadata["input"]["query"]["properties"]["ip"]["example"],
            "8.8.8.8"
        );
        assert_eq!(
            metadata["example"]["url"],
            "https://gateway.apiosk.com/ipdata/lookup?ip=8.8.8.8"
        );
        assert!(metadata["example"]["curl"]
            .as_str()
            .unwrap()
            .contains("PAYMENT-SIGNATURE"));
        assert_eq!(metadata["output"]["example"]["city"], "Montréal");
        assert_eq!(metadata["pricing"]["model"], "per-call");
        assert_eq!(metadata["pricing"]["amount"], "4000");
        assert_eq!(metadata["method"], "GET");
        assert_eq!(metadata["path"], "/lookup");
        assert_eq!(
            metadata["tags"],
            json!(["geolocation", "ip", "data", "ipdata"])
        );
        assert_eq!(metadata["provider"]["username"], "ipdata");

        // The same contract also rides on accepts[], where x402 v1 indexers
        // (CDP Bazaar's extractor) look for it.
        let accept = &items[0]["accepts"][0];
        assert_eq!(accept["description"], "Geolocate an IPv4 or IPv6 address.");
        assert_eq!(accept["outputSchema"]["input"]["method"], "GET");
        assert_eq!(
            accept["outputSchema"]["input"]["queryParams"]["properties"]["ip"]["example"],
            "8.8.8.8"
        );
        assert_eq!(accept["outputSchema"]["output"]["city"], "Montréal");
        // The wire fields a payer needs are untouched.
        assert_eq!(accept["amount"], "4000");
        assert_eq!(accept["payTo"], "0xA039PLATFORM");
    }

    #[test]
    fn impressions_count_listings_published_not_resources_or_catalog_rows() {
        let two_endpoints = json!([
            { "method": "GET", "path": "/a", "price_usd": 0.01, "payment_required": true },
            { "method": "GET", "path": "/b", "price_usd": 0.02, "payment_required": true }
        ]);
        let apis = vec![
            test_api("paid", two_endpoints),
            // Free-only: in the catalog, but never in this document.
            test_api(
                "free",
                json!([{ "method": "GET", "path": "/x", "price_usd": 0, "payment_required": false }]),
            ),
        ];

        let items = build_x402_items(&apis, &test_offers());
        assert_eq!(items.len(), 2);
        // Two resources, one listing published — and the free listing is absent.
        assert_eq!(
            published_listings(&items),
            vec![("paid".to_string(), "callable")]
        );
    }

    #[test]
    fn a_listing_is_credited_with_the_best_tier_it_was_published_at() {
        let mut documented = item("mixed", "data", &["input"]);
        documented["metadata"]["method"] = json!("GET");
        let mut bare = item("mixed", "data", &[]);
        bare["metadata"]["method"] = json!("GET");
        let other = item("probe-only", "data", &[]);

        // Order must not decide the tier: the undocumented resource comes first.
        assert_eq!(
            published_listings(&[bare, documented, other]),
            vec![
                ("mixed".to_string(), "documented"),
                ("probe-only".to_string(), "probe")
            ]
        );
    }

    #[test]
    fn documentation_stops_at_the_budget_but_resources_stay_payable() {
        let documented = json!([
            {
                "method": "GET",
                "path": "/a",
                "price_usd": 0.01,
                "payment_required": true,
                "query_params": {
                    "type": "object",
                    "properties": { "q": { "type": "string", "example": "hello" } }
                }
            }
        ]);
        let apis = vec![
            test_api("first", documented.clone()),
            test_api("second", documented),
        ];

        // A budget large enough for one item's documentation only.
        let items = build_x402_items_within(&apis, &test_offers(), 1);
        assert_eq!(items.len(), 2);
        assert!(items[0]["metadata"].get("input").is_some());
        assert!(items[1]["metadata"].get("input").is_none());

        // Both remain fully payable — the budget only ever drops documentation.
        for item in &items {
            assert_eq!(item["accepts"][0]["amount"], "10000");
            assert_eq!(item["accepts"][0]["payTo"], "0xA039PLATFORM");
            assert!(item["metadata"]["metadataUrl"]
                .as_str()
                .unwrap()
                .ends_with("/metadata"));
        }
    }

    #[test]
    fn undocumented_endpoints_stay_lean_and_are_counted_as_gaps() {
        let api = test_api(
            "weather",
            json!([
                {
                    "method": "GET",
                    "path": "/current",
                    "price_usd": 0.003,
                    "payment_required": true
                }
            ]),
        );

        let items = build_x402_items(&[api], &test_offers());
        let metadata = &items[0]["metadata"];
        assert!(metadata.get("input").is_none());
        assert!(metadata.get("example").is_none());
        assert!(metadata.get("output").is_none());
        // No description column, so the listing description carries the item —
        // never "API call to weather".
        assert_eq!(
            metadata["description"],
            "GET /current — weather description"
        );

        assert_eq!(
            DocumentationCoverage::of(&items),
            DocumentationCoverage {
                resources: 1,
                with_input: 0,
                with_example: 0,
                with_output: 0,
            }
        );
    }

    #[test]
    fn federated_items_describe_the_resource_not_our_plumbing() {
        let mut api = test_api("pulsenetwork", json!([]));
        api.listing_type = "federated".to_string();
        api.is_external = true;
        api.description = "OFAC and sanctions screening for agent payments.".to_string();
        api.external_x402 = json!([
            {
                "resource": "https://x402.pulsenetwork.io/ofac/screen",
                "accepts": [{ "scheme": "exact", "network": "eip155:8453", "payTo": "0xPROVIDER", "maxAmountRequired": "10000" }]
            }
        ]);

        let items = build_x402_items(&[api], &test_offers());
        assert_eq!(
            items[0]["metadata"]["description"],
            "OFAC and sanctions screening for agent payments."
        );
        // Listing tags (category + slug) plus what the URL says the resource is
        // about — without the latter every resource on this listing carries the
        // same two tags and no filter can tell them apart.
        assert_eq!(
            items[0]["metadata"]["tags"],
            json!(["data", "pulsenetwork", "ofac", "screen"])
        );
    }

    #[test]
    fn a_description_shared_across_resources_gets_the_path_appended() {
        let mut api = test_api("laevitas", json!([]));
        api.listing_type = "federated".to_string();
        api.is_external = true;
        api.description = "Derivatives analytics.".to_string();
        // What the live catalog looks like: one provider sentence, repeated
        // verbatim on every resource of the listing.
        api.external_x402 = json!([
            {
                "resource": "https://apiv2.example.ch/options/greeks",
                "description": "Options data — pay per request",
                "accepts": [{ "scheme": "exact", "network": "eip155:8453", "payTo": "0xPROVIDER", "maxAmountRequired": "10000" }],
                "metadata": { "method": "GET" }
            },
            {
                "resource": "https://apiv2.example.ch/options/oi",
                "description": "Options data — pay per request",
                "accepts": [{ "scheme": "exact", "network": "eip155:8453", "payTo": "0xPROVIDER", "maxAmountRequired": "10000" }],
                "metadata": { "method": "GET" }
            },
            {
                "resource": "https://apiv2.example.ch/futures/basis",
                "description": "Dated futures basis for a single expiry",
                "accepts": [{ "scheme": "exact", "network": "eip155:8453", "payTo": "0xPROVIDER", "maxAmountRequired": "10000" }],
                "metadata": { "method": "GET" }
            }
        ]);

        let items = build_x402_items(&[api], &test_offers());
        assert_eq!(
            items[0]["metadata"]["description"],
            "Options data — pay per request (GET /options/greeks)"
        );
        assert_eq!(
            items[1]["metadata"]["description"],
            "Options data — pay per request (GET /options/oi)"
        );
        // A description used once already distinguishes its resource; leave the
        // provider's sentence exactly as written.
        assert_eq!(
            items[2]["metadata"]["description"],
            "Dated futures basis for a single expiry"
        );
    }

    #[test]
    fn a_listing_described_only_by_its_import_falls_through_to_the_url() {
        let mut api = test_api("ads-forgemesh-io", json!([]));
        api.listing_type = "federated".to_string();
        api.is_external = true;
        // Exactly what `federated-import` writes, once, for the whole listing.
        api.description =
            "5 x402 endpoints imported from https://ads.example.com/.well-known/x402.json."
                .to_string();
        api.external_x402 = json!([
            {
                "resource": "https://ads.example.com/api/intent/report",
                "accepts": [{ "scheme": "exact", "network": "eip155:8453", "payTo": "0xPROVIDER", "maxAmountRequired": "10000" }],
                "metadata": { "method": "GET" }
            },
            {
                "resource": "https://ads.example.com/api/campaign/spend",
                "accepts": [{ "scheme": "exact", "network": "eip155:8453", "payTo": "0xPROVIDER", "maxAmountRequired": "10000" }],
                "metadata": { "method": "GET" }
            }
        ]);

        let items = build_x402_items(&[api], &test_offers());
        let first = items[0]["metadata"]["description"].as_str().unwrap();
        let second = items[1]["metadata"]["description"].as_str().unwrap();

        assert!(!first.contains("imported from"), "got {first}");
        assert!(first.contains("/api/intent/report"), "got {first}");
        assert!(second.contains("/api/campaign/spend"), "got {second}");
        assert_ne!(
            first, second,
            "two resources on one imported listing must not read identically"
        );
    }

    #[test]
    fn x402_items_only_add_base_as_global_fallback() {
        let api = test_api(
            "weather",
            json!([
                {
                    "method": "GET",
                    "path": "/current",
                    "description": "Current weather",
                    "price_usd": 0.02,
                    "payment_required": true
                }
            ]),
        );

        let mut offers = test_offers();
        let polygon = NetworkToken {
            id: uuid::Uuid::new_v4(),
            network_name: "polygon".to_string(),
            network_display_name: "Polygon".to_string(),
            chain_id: 137,
            explorer_url: Some("https://polygonscan.com".to_string()),
            is_testnet: false,
            token_symbol: "USDC".to_string(),
            token_name: "USD Coin".to_string(),
            token_decimals: 6,
            contract_address: "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359".to_string(),
        };
        offers.tokens_by_id.insert(polygon.id, polygon);
        offers.caps = Arc::new(SupportedNetworks::with_network_for_test("polygon"));

        let items = build_x402_items(&[api], &offers);
        assert_eq!(items.len(), 1);
        let accepts = items[0]["accepts"].as_array().expect("accepts array");
        assert_eq!(accepts.len(), 1);
        assert_eq!(accepts[0]["network"], "eip155:8453");
    }

    #[test]
    fn x402_items_skip_malformed_endpoint_docs() {
        let api = test_api("broken", json!({"not": "an array"}));
        let items = build_x402_items(&[api], &test_offers());
        assert!(items.is_empty());
    }

    #[test]
    fn federated_items_are_masked_behind_the_gateway() {
        let mut api = test_api("openwebninja", json!([]));
        api.listing_type = "federated".to_string();
        api.is_external = true;
        api.external_x402 = json!([
            {
                "resource": "https://x402.openwebninja.com/ai/answers",
                "type": "http",
                "description": "AI answers",
                "metadata": { "method": "GET", "providerField": "preserved" },
                "accepts": [
                    {
                        "scheme": "exact",
                        "network": "eip155:8453",
                        "payTo": "0x3e7c9cA818f713F19e126E3B7B37B47dC1411570",
                        "asset": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
                        "maxAmountRequired": "10000",
                        "maxTimeoutSeconds": 300
                    }
                ]
            }
        ]);

        let items = build_x402_items(&[api], &test_offers());
        assert_eq!(items.len(), 1);
        let item = &items[0];

        // This test used to assert the exact opposite: that the provider's URL,
        // payTo and amount were republished verbatim. That was a published
        // route around the 15% buyer fee — an agent reading the discovery doc
        // could pay the provider directly at list price and Apiosk would see
        // nothing. The whole point of the mask is that it no longer can.
        assert_eq!(
            item["resource"],
            "https://gateway.apiosk.com/openwebninja/ai/answers"
        );
        assert_eq!(item["type"], "http");
        assert_eq!(item["x402Version"], 2);

        // Apiosk's terms replace the provider's entirely: platform wallet, and
        // $0.01 list quoted to the buyer at $0.0115.
        assert_eq!(
            item["accepts"][0]["payTo"],
            json!(test_offers().platform_wallet)
        );
        assert_eq!(item["accepts"][0]["amount"], "11500");

        // Nothing that resolves to the provider survives.
        let serialized = item.to_string();
        assert!(
            !serialized.contains("0x3e7c9cA818f713F19e126E3B7B37B47dC1411570"),
            "provider payTo leaked: {serialized}"
        );
        assert!(
            !serialized.contains("openwebninja.com"),
            "provider origin leaked: {serialized}"
        );

        // Descriptive metadata still travels — masking the payment route must
        // not make the catalog less useful to an agent choosing an endpoint.
        assert_eq!(item["metadata"]["hostedExternally"].as_bool(), Some(false));
        assert_eq!(item["metadata"]["api"], "openwebninja");
        assert_eq!(item["metadata"]["method"], "GET");
        assert_eq!(item["metadata"]["providerField"], "preserved");
    }

    #[test]
    fn federated_items_skip_malformed_resources() {
        let mut api = test_api("broken-ext", json!([]));
        api.listing_type = "federated".to_string();
        api.is_external = true;
        api.external_x402 = json!([
            { "type": "http" },                                   // no resource URL
            { "resource": "https://x.example/a", "accepts": [] }, // empty accepts[]
            { "resource": "https://x.example/b" },                // missing accepts[]
        ]);
        let items = build_x402_items(&[api], &test_offers());
        assert!(items.is_empty());
    }

    #[test]
    fn listing_type_is_the_authoritative_federated_discriminator() {
        // Safety: a row with is_external=true but listing_type != 'federated'
        // must NOT be republished from external_x402. listing_type is the single
        // authoritative discriminator; keying anything off is_external alone
        // could make a row federated in discovery yet proxyable at the gateway.
        let mut api = test_api("mislabelled", json!([]));
        api.is_external = true; // mirror out of sync — must be ignored
        api.listing_type = "api".to_string();
        api.external_x402 = json!([
            {
                "resource": "https://evil.example/x",
                "accepts": [{ "scheme": "exact", "network": "eip155:8453", "payTo": "0xBAD" }]
            }
        ]);
        let items = build_x402_items(&[api], &test_offers());
        // endpoint_docs is empty and external_x402 is ignored → no items at all.
        assert!(items.is_empty());
    }

    /// The fixture's provider offer, priced so the buyer maths is legible:
    /// $0.10 list → $0.115 buyer.
    fn federated_api_priced(resource: &str, atomic: &str) -> ApiInfo {
        let mut api = test_api("pulsenetwork", json!([]));
        api.listing_type = "federated".to_string();
        api.is_external = true;
        api.external_x402 = json!([
            {
                "resource": resource,
                "accepts": [{
                    "scheme": "exact",
                    "network": "eip155:8453",
                    "payTo": "0xPROVIDER",
                    "maxAmountRequired": atomic
                }]
            }
        ]);
        api
    }

    /// The bypass this masking exists to close: a published provider URL lets a
    /// buyer call the endpoint directly at list price and skip the 15%.
    #[test]
    fn federated_resource_is_published_as_the_gateway_url() {
        let api = federated_api_priced("https://x402.pulsenetwork.io/ofac/screen", "100000");
        let items = build_x402_items(&[api], &test_offers());
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0]["resource"],
            "https://gateway.apiosk.com/pulsenetwork/ofac/screen"
        );
    }

    /// The provider's payTo must not survive into the document in any form —
    /// paying it directly is the same bypass by another route.
    #[test]
    fn federated_items_never_expose_the_provider_pay_to_or_origin() {
        let api = federated_api_priced("https://x402.pulsenetwork.io/ofac/screen", "100000");
        let items = build_x402_items(&[api], &test_offers());
        let serialized = items[0].to_string();
        assert!(
            !serialized.contains("0xPROVIDER"),
            "provider payTo leaked into the discovery item: {serialized}"
        );
        assert!(
            !serialized.contains("x402.pulsenetwork.io"),
            "provider origin leaked into the discovery item: {serialized}"
        );
        assert_eq!(items[0]["metadata"]["hostedExternally"], json!(false));
    }

    /// A federated item must quote list + 15% and pay the platform wallet, so it
    /// is indistinguishable from a managed one on the wire.
    #[test]
    fn federated_items_quote_the_buyer_price_to_the_platform_wallet() {
        let api = federated_api_priced("https://x402.pulsenetwork.io/ofac/screen", "100000");
        let items = build_x402_items(&[api], &test_offers());
        let accept = &items[0]["accepts"][0];
        // $0.10 list → $0.115 buyer, in atomic USDC. The v2 wire shape names
        // this `amount`; the resource lives on the item, not the accept.
        assert_eq!(accept["amount"], "115000");
        assert_eq!(accept["payTo"], json!(test_offers().platform_wallet));
        assert_eq!(
            items[0]["resource"],
            "https://gateway.apiosk.com/pulsenetwork/ofac/screen"
        );
    }

    /// A resource with no addressable path cannot be served through the gateway.
    /// Dropping it is correct; falling back to the provider's URL would reopen
    /// the bypass.
    #[test]
    fn federated_resource_without_a_path_is_dropped_not_passed_through() {
        let api = federated_api_priced("https://x402.pulsenetwork.io", "100000");
        let items = build_x402_items(&[api], &test_offers());
        assert!(items.is_empty());
    }

    // ---- pagination, filtering and ranking of the discovery document ----

    fn query(pairs: &[(&str, &str)]) -> DiscoveryQuery {
        DiscoveryQuery::from_params(
            &pairs
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
        )
    }

    /// A discovery item carrying only what the page logic reads.
    fn item(slug: &str, category: &str, blocks: &[&str]) -> Value {
        let mut metadata = serde_json::Map::new();
        metadata.insert("api".to_string(), json!(slug));
        metadata.insert("name".to_string(), json!(format!("{slug} API")));
        metadata.insert("category".to_string(), json!(category));
        metadata.insert("description".to_string(), json!("does a thing"));
        for block in blocks {
            metadata.insert((*block).to_string(), json!({ "present": true }));
        }
        json!({
            "resource": format!("https://gateway.apiosk.com/{slug}/v1/thing"),
            "metadata": Value::Object(metadata),
        })
    }

    fn slugs(items: &[Value]) -> Vec<String> {
        items
            .iter()
            .map(|item| item["metadata"]["api"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn defaults_to_a_small_first_page() {
        let q = query(&[]);
        assert_eq!(q.page, 1);
        assert_eq!(q.per_page, DEFAULT_PER_PAGE);

        // 1481 resources is the catalog size that produced the 7.3 MB document.
        let page = Page::new(&q, 1481);
        assert_eq!(page.offset(), 0);
        assert_eq!(page.per_page, DEFAULT_PER_PAGE);
        assert_eq!(page.total_pages(), 1481_usize.div_ceil(DEFAULT_PER_PAGE));
        assert_eq!(page.next(), Some(2));
        assert_eq!(page.prev(), None);
    }

    #[test]
    fn per_page_is_capped_so_no_caller_can_ask_for_the_whole_catalog() {
        assert_eq!(query(&[("perPage", "5000")]).per_page, MAX_PER_PAGE);
        assert_eq!(query(&[("per_page", "50")]).per_page, 50);
        assert_eq!(query(&[("limit", "50")]).per_page, 50);
        assert_eq!(query(&[("perPage", "0")]).per_page, 1);
    }

    #[test]
    fn unparseable_params_fall_back_instead_of_failing_the_read() {
        // A crawler we cannot fix sends junk; it still gets a usable page.
        let q = query(&[("page", "abc"), ("perPage", "-3"), ("category", "  ")]);
        assert_eq!(q.page, 1);
        assert_eq!(q.per_page, DEFAULT_PER_PAGE);
        assert_eq!(q.category, None);
    }

    #[test]
    fn a_page_past_the_end_lands_on_the_last_one() {
        let page = Page::new(&query(&[("page", "99"), ("perPage", "20")]), 30);
        assert_eq!(page.total_pages(), 2);
        assert_eq!(page.number, 2);
        assert_eq!(page.offset(), 20);
        assert_eq!(page.next(), None);
        assert_eq!(page.prev(), Some(1));
    }

    #[test]
    fn an_empty_result_still_has_one_valid_page() {
        let page = Page::new(&query(&[]), 0);
        assert_eq!(page.number, 1);
        assert_eq!(page.total_pages(), 1);
        assert_eq!(page.next(), None);
    }

    #[test]
    fn documented_resources_lead_the_document() {
        // Catalog order puts the undocumented ones first; ranking must not.
        let mut items = vec![
            item("bare-a", "data", &[]),
            item("full", "data", &["input", "example", "output"]),
            item("bare-b", "data", &[]),
            item("partial", "data", &["input"]),
        ];
        rank_documented_first(&mut items);
        assert_eq!(
            slugs(&items),
            vec!["full", "partial", "bare-a", "bare-b"],
            "best-documented first, and ties keep catalog order"
        );
    }

    #[test]
    fn documented_filter_keeps_only_resources_callable_without_a_round_trip() {
        let items = vec![
            item("full", "data", &["input", "example", "output"]),
            item("example-only", "data", &["example"]),
            item("bare", "data", &[]),
        ];
        let q = query(&[("documented", "true")]);
        let kept: Vec<Value> = items.into_iter().filter(|i| q.matches(i)).collect();
        assert_eq!(slugs(&kept), vec!["full", "example-only"]);
    }

    #[test]
    fn category_and_search_filters_narrow_the_catalog() {
        let items = [
            item("weather-now", "data", &["input"]),
            item("invoice-parse", "finance", &["input"]),
            item("fx-rates", "Finance", &[]),
        ];

        let by_category = query(&[("category", "finance")]);
        let kept: Vec<Value> = items
            .iter()
            .filter(|i| by_category.matches(i))
            .cloned()
            .collect();
        assert_eq!(
            slugs(&kept),
            vec!["invoice-parse", "fx-rates"],
            "category match is case-insensitive"
        );

        let by_search = query(&[("q", "INVOICE")]);
        let kept: Vec<Value> = items
            .iter()
            .filter(|i| by_search.matches(i))
            .cloned()
            .collect();
        assert_eq!(slugs(&kept), vec!["invoice-parse"]);
    }

    #[test]
    fn page_links_carry_the_filters_and_encode_their_values() {
        let q = query(&[
            ("category", "market data"),
            ("q", "a&b"),
            ("documented", "true"),
        ]);
        let url = page_url(&q, 3);
        assert!(url.starts_with(&format!("{GATEWAY_BASE_URL}{X402_PATH}?")));
        assert!(url.contains("page=3"));
        assert!(url.contains("category=market+data"), "got {url}");
        assert!(url.contains("q=a%26b"), "got {url}");
        assert!(url.contains("documented=true"));
        // The default size is not restated, so an unfiltered link stays short.
        assert_eq!(
            page_url(&query(&[]), 2),
            format!("{GATEWAY_BASE_URL}{X402_PATH}?page=2")
        );
    }

    #[test]
    fn facets_lead_with_the_categories_an_agent_can_act_on() {
        let items = vec![
            item("a", "data", &[]),
            item("b", "data", &[]),
            item("c", "data", &[]),
            item("d", "finance", &["input"]),
            item("e", "finance", &[]),
            item("f", "", &[]),
        ];
        let facets = category_facets(&items);
        assert_eq!(facets[0]["category"], json!("finance"));
        assert_eq!(facets[0]["resources"], json!(2));
        assert_eq!(facets[0]["documented"], json!(1));
        assert_eq!(facets[1]["category"], json!("data"), "larger but unusable");
        assert_eq!(facets[2]["category"], json!("uncategorized"));
        assert!(facets[0]["url"]
            .as_str()
            .unwrap()
            .contains("category=finance"));
    }

    #[test]
    fn url_keywords_recover_the_subject_a_federated_listing_never_stated() {
        assert_eq!(
            resource_keywords("https://seniorpulse.example.com/api/senior/pension-intl?ref=apiosk"),
            vec!["senior", "pension", "intl", "seniorpulse"],
            "the path is the only place this resource says it is about pensions"
        );
        // Stopwords, version segments and bare numbers carry no search value.
        assert_eq!(
            resource_keywords("https://api.example.com/v1/json/42/weather"),
            vec!["weather"]
        );
        assert!(resource_keywords("not a url").is_empty());
    }

    #[test]
    fn import_boilerplate_never_becomes_a_resource_description() {
        assert!(describes_only_the_import(
            "5 x402 endpoints imported from https://ads.example.com/.well-known/x402.json."
        ));
        assert!(!describes_only_the_import(
            "Real-time flight delay predictions for European airports."
        ));

        // Falls through to something derived from the URL, so the thousand
        // resources on one imported listing stop reading identically.
        let derived = derived_resource_description(
            "GET",
            "https://travelpulse.example.com/api/parks/crowds",
            "travelpulse",
        );
        assert!(derived.contains("GET /api/parks/crowds"), "got {derived}");
        assert!(derived.contains("travelpulse.example.com"), "got {derived}");
        assert!(derived.contains("parks, crowds"), "got {derived}");
    }

    #[test]
    fn the_document_publishes_managed_listings_unless_asked_otherwise() {
        let managed = item("weather", "data", &["input"]);
        let mut federated = item("ads-forgemesh-io", "other", &[]);
        federated["metadata"]["listingType"] = json!("federated");
        let items = [managed, federated];

        let default = query(&[]);
        assert_eq!(default.include, Include::Managed);
        let published: Vec<Value> = items
            .iter()
            .filter(|i| default.include.admits(i))
            .cloned()
            .collect();
        assert_eq!(
            slugs(&published),
            vec!["weather"],
            "federated listings are indexed but not what this document leads with"
        );

        // Both escape hatches work, and the URL keeps carrying the choice.
        let all = query(&[("include", "all")]);
        assert_eq!(items.iter().filter(|i| all.include.admits(i)).count(), 2);
        let federated_only = query(&[("include", "federated")]);
        let kept: Vec<Value> = items
            .iter()
            .filter(|i| federated_only.include.admits(i))
            .cloned()
            .collect();
        assert_eq!(slugs(&kept), vec!["ads-forgemesh-io"]);
        assert!(page_url(&federated_only, 2).contains("include=federated"));
        // The default is not restated in links, so the common URL stays short.
        assert!(!page_url(&default, 2).contains("include"));
    }

    #[test]
    fn an_unknown_include_value_falls_back_to_managed() {
        // Never widen on a typo: a bad `?include=` must not silently publish the
        // federated catalog to a crawler that did not ask for it.
        assert_eq!(
            query(&[("include", "everything")]).include,
            Include::Managed
        );
        assert_eq!(query(&[("include", "")]).include, Include::Managed);
    }

    #[test]
    fn contract_tier_states_only_what_we_know() {
        assert_eq!(contract_tier(&item("a", "data", &["input"])), "documented");

        let mut method_only = item("b", "data", &[]);
        method_only["metadata"]["method"] = json!("GET");
        assert_eq!(contract_tier(&method_only), "callable");

        // No method published: the live 402 is the only source of terms.
        assert_eq!(contract_tier(&item("c", "data", &[])), "probe");
    }

    #[test]
    fn contract_tiers_are_stamped_counted_and_filterable() {
        let mut documented = item("a", "data", &["input", "example"]);
        let mut callable = item("b", "data", &[]);
        callable["metadata"]["method"] = json!("GET");
        let probe = item("c", "data", &[]);
        let mut items = vec![documented.clone(), callable.clone(), probe.clone()];

        stamp_contract_tiers(&mut items);
        assert_eq!(items[0]["metadata"]["contract"], json!("documented"));
        assert_eq!(items[1]["metadata"]["contract"], json!("callable"));
        assert_eq!(items[2]["metadata"]["contract"], json!("probe"));

        let counts = contract_counts(&items);
        assert_eq!(counts["documented"], json!(1));
        assert_eq!(counts["callable"], json!(1));
        assert_eq!(counts["probe"], json!(1));

        let only_callable = query(&[("contract", "callable")]);
        let kept: Vec<Value> = items
            .iter()
            .filter(|i| only_callable.matches(i))
            .cloned()
            .collect();
        assert_eq!(slugs(&kept), vec!["b"]);

        // Ranking puts the tiers in the order an agent can use them.
        documented["metadata"]["contract"] = json!("documented");
        callable["metadata"]["contract"] = json!("callable");
        let mut unordered = vec![probe, callable, documented];
        rank_documented_first(&mut unordered);
        assert_eq!(slugs(&unordered), vec!["a", "b", "c"]);
    }

    #[test]
    fn every_search_word_has_to_match() {
        let mut invoice = item("invoice-parse", "finance", &[]);
        invoice["metadata"]["description"] = json!("Extracts invoice data from a PDF document");
        let plain = item("invoice-list", "finance", &[]);

        let both = query(&[("q", "invoice pdf")]);
        assert!(both.matches(&invoice));
        assert!(
            !both.matches(&plain),
            "matching any word would return everything with 'invoice' in it"
        );
    }

    #[test]
    fn search_results_are_ranked_by_where_they_matched() {
        let mut in_prose = item("something-else", "data", &[]);
        in_prose["metadata"]["description"] = json!("mentions pension once, in passing");
        let mut in_path = item("pulse", "data", &[]);
        in_path["resource"] = json!("https://x.example.com/api/senior/pension-intl");
        let mut in_name = item("pension-api", "data", &[]);
        in_name["metadata"]["tags"] = json!(["pension", "retirement"]);

        let mut items = vec![in_prose, in_path, in_name];
        rank_by_relevance(&mut items, &["pension".to_string()]);
        assert_eq!(
            slugs(&items),
            vec!["pension-api", "pulse", "something-else"],
            "name and tags beat the URL, which beats a mention in prose"
        );
    }

    #[test]
    fn pagination_block_reports_filtered_total_not_catalog_total() {
        let q = query(&[("category", "finance"), ("page", "2"), ("perPage", "5")]);
        let value = Page::new(&q, 12).to_value(&q);
        assert_eq!(value["page"], json!(2));
        assert_eq!(value["perPage"], json!(5));
        assert_eq!(value["totalPages"], json!(3));
        assert_eq!(value["matchedResources"], json!(12));
        assert_eq!(value["filters"]["category"], json!("finance"));
        assert_eq!(value["filters"]["documented"], json!(false));
    }
}
