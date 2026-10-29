use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
pub struct BalanceQuery {
    pub address: String,
}

#[derive(Debug, Deserialize)]
pub struct UsageQuery {
    pub address: String,
    #[serde(default = "default_period")]
    pub period: String,
}

fn default_period() -> String {
    "all".to_string()
}

#[derive(Debug, Deserialize)]
pub struct ApiListQuery {
    pub search: Option<String>,
    /// One exact slug. `search` is a MATCH — it reaches into endpoint paths and
    /// descriptions on purpose — so a client that already knows which listing
    /// it wants must say so here, or read a page of near-misses: `search=tavily`
    /// returns the five listings whose endpoints mention Tavily before it
    /// returns Tavily. Applied on top of every other filter and of the
    /// catalogue's visibility rules, never around them.
    pub slug: Option<String>,
    pub category: Option<String>,
    pub tag: Option<String>,
    pub listing_type: Option<String>,
    pub min_price: Option<f64>,
    pub max_price: Option<f64>,
    pub created_after: Option<String>,
    pub created_before: Option<String>,
    #[serde(default = "default_sort")]
    pub sort: String,
    #[serde(default = "default_order")]
    pub order: String,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    /// Set to `true` to include sandbox/test listings that an automated
    /// agent would otherwise pick up (e.g. slugs that look like repeated
    /// keyboard mashes, 404-probe APIs, smoke-test entries). Default is
    /// `false` so the "cheapest API" discovery query an agent makes
    /// returns production-quality results only.
    #[serde(default)]
    pub include_test: bool,
}

fn default_sort() -> String {
    "name".to_string()
}

fn default_order() -> String {
    "asc".to_string()
}

fn default_limit() -> i64 {
    500
}

#[derive(Debug, Serialize)]
pub struct ApiListResponse {
    pub apis: Vec<ApiInfoResponse>,
    pub meta: ApiListMeta,
}

#[derive(Debug, Serialize)]
pub struct ApiListMeta {
    pub total: i64,
    pub returned: usize,
    pub limit: i64,
    pub offset: i64,
    pub search: Option<String>,
    pub category: Option<String>,
    pub tag: Option<String>,
    pub listing_type: Option<String>,
    pub min_price: Option<f64>,
    pub max_price: Option<f64>,
    pub created_after: Option<String>,
    pub created_before: Option<String>,
    pub sort: String,
    pub order: String,
    /// How the results are ranked before `sort` is applied — see
    /// [`crate::db::RANKING_NOTE`]. Published so the managed-first preference
    /// is visible to any agent reading the catalog, not silent.
    pub ranking: &'static str,
}

#[derive(Debug, Serialize)]
pub struct ApiInfoResponse {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    /// The provider's raw list price, unchanged for existing callers.
    pub price_usd: f64,
    /// The price the buyer is actually debited: `price_usd` plus Apiosk's 15%
    /// fee (crate::fees). Additive — a caller that only reads `price_usd` is
    /// unaffected; a caller that wants to quote the real cost reads this and
    /// does not re-apply the fee.
    pub buyer_price_usd: f64,
    pub category: String,
    pub raw_category: String,
    pub implementation_type: String,
    pub model_backed: bool,
    pub listing_type: String,
    pub docs_url: String,
    pub origin_url: String,
    pub gateway_url: String,
    pub active: bool,
    pub verified: bool,
    pub endpoint_count: i64,
    pub operations: Vec<OperationInfo>,
    pub endpoints: Vec<EndpointDocResponse>,
    pub listing_metadata: serde_json::Value,
    pub created_at: String,
    /// "production" for normal listings, "test" for sandbox/smoke entries.
    /// See `looks_like_test_listing` in src/v1_routes/catalog.rs for the
    /// heuristic. Agents should treat anything except "production" as
    /// non-billable / non-functional unless they know the operator.
    pub listing_quality: String,
    /// Always `false`. Every listing is called through the gateway.
    ///
    /// This used to be `true` for federated listings and documented as "the
    /// agent pays the provider directly at `external_resources[].resource`".
    /// That was a published route around the buyer's 15%, so both this flag and
    /// the resources it pointed at are now inert. Retained as a field so
    /// existing clients keep parsing; a client branching on it takes the
    /// gateway path, which is the only path.
    pub hosted_externally: bool,
    /// Always an empty array. Held the provider's verbatim x402 resources —
    /// their origin, `payTo` and list price — which is the same bypass one level
    /// down. Retained for wire compatibility only.
    pub external_resources: serde_json::Value,
    /// Public listing-owner identity for catalog attribution.
    pub provider: Option<ProviderPublic>,
}

/// Public, non-sensitive identity of the human/organization that owns a
/// listing, joined from `provider_profiles` on `apis.owner_id`. Shown on the
/// API detail page (logo + name). Never includes auth/wallet fields.
#[derive(Debug, Serialize)]
pub struct ProviderPublic {
    /// Display name, falling back to the username handle.
    pub name: String,
    pub username: Option<String>,
    /// Public avatar/logo URL (Supabase Storage), or null when unset.
    pub avatar_url: Option<String>,
    pub bio: Option<String>,
    pub sector: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ApiDetailResponse {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub long_description: Option<String>,
    pub category: Option<String>,
    pub listing_type: Option<String>,
    pub docs_url: Option<String>,
    pub gateway_base_url: String,
    pub example_usage: Option<String>,
    pub authentication: Option<serde_json::Value>,
    pub active: bool,
    pub verified: bool,
    /// "production" or "test" — see ApiInfoResponse.listing_quality.
    pub listing_quality: String,
    /// Price the gateway will charge for the *default* operation
    /// (= first entry in `operations` below). Multi-operation APIs MUST
    /// inspect `operations[].price_usd` to budget per-call instead of
    /// trusting this top-level field. Single source of truth: the
    /// `operations` array.
    pub price_usd: f64,
    /// First-class operation list. Every callable endpoint on the API
    /// shows up here with `id` (matches the slug used in `/execute`),
    /// `method`, `path`, `price_usd`, and `input_schema`. Listing
    /// metadata still echoes this for older clients but new agents
    /// should read this field directly.
    pub operations: Vec<OperationInfo>,
    pub listing_metadata: serde_json::Value,
    pub created_at: String,
    pub endpoints: Vec<EndpointDocResponse>,
    /// Always `false` — see `ApiInfoResponse::hosted_externally`. Every
    /// listing has a `gateway_base_url` and is called through it.
    pub hosted_externally: bool,
    /// Always `[]` — see `ApiInfoResponse::external_resources`.
    pub external_resources: serde_json::Value,
    /// The listing owner's public profile (logo/name/bio), joined from
    /// `provider_profiles`; null when the owner has no profile row.
    pub provider: Option<ProviderPublic>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OperationInfo {
    /// Stable id for `/execute` envelopes. Equals the endpoint path with
    /// the leading slash stripped (e.g. `/current` → `"current"`).
    pub id: String,
    pub method: String,
    pub path: String,
    /// Authoritative per-operation price. Same number that ends up in
    /// `X-Apiosk-Price-Listed` after a paid call.
    pub price_usd: f64,
    pub payment_required: bool,
    pub description: Option<String>,
    /// The REQUEST BODY schema, and only that. A GET endpoint has none, which
    /// is why `query_params` below is not an alternative spelling of this one.
    pub input_schema: Option<serde_json::Value>,
    /// The QUERY-STRING schema — the parameters a GET actually takes.
    ///
    /// Added because leaving it out made every GET endpoint in the catalogue
    /// look like it took no arguments at all. `api_endpoints.query_params` has
    /// always held these and the HTML docs page has always rendered them
    /// (`render.rs`, "Parameters"); this is the same data on the route a
    /// machine reads. CityFALCON and KVK are GET-only, so on `/v1/apis` they
    /// published four and six endpoints respectively with no inputs — and a
    /// caller that sends no parameters gets a 400 from the origin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_params: Option<serde_json::Value>,
    /// What re-prices this call, when a parameter does.
    ///
    /// `price_usd` above is the DEFAULT. An endpoint can charge more for a
    /// given argument — CityFALCON's `/stories` is $0.03 until
    /// `with_sentiment=true` makes it $0.07 — and the proxy applies that at
    /// call time and reports it back in `X-Apiosk-Price-Listed`. Publishing
    /// only the default meant a buyer was quoted one number, approved a
    /// ceiling built from it, and had the call refused for exceeding a price
    /// nobody had told them about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing_rules: Option<serde_json::Value>,
    pub output_schema: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EndpointDocResponse {
    /// `api_endpoints.id` — the id `POST /v1/do` executes a call by.
    ///
    /// The one field that decides whether a BROWSED endpoint can be bought
    /// where it was found. A reviewed call is made by id, never by URL: the
    /// gateway re-reads the price, the schema and the settlement behind that
    /// id at call time. Without it the catalogue is a brochure — every
    /// endpoint readable on /v1/apis and none of them payable — which is what
    /// the app's Explore pages were until this was published.
    ///
    /// `Option` because a federated listing has no `api_endpoints` row to take
    /// an id from (see `federated_intake`): it is indexed, not proxied, and is
    /// called on the provider's own host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_id: Option<uuid::Uuid>,
    pub method: String,
    pub path: String,
    pub full_url: String,
    pub description: Option<String>,
    pub price_usd: f64,
    pub payment_required: bool,
    pub request_body: Option<serde_json::Value>,
    pub response_body: Option<serde_json::Value>,
    pub request_example: Option<serde_json::Value>,
    pub response_example: Option<serde_json::Value>,
    pub headers: Option<serde_json::Value>,
    pub query_params: Option<serde_json::Value>,
    #[serde(default)]
    pub documentation_markdown: Option<String>,
    #[serde(default)]
    pub external_docs_url: Option<String>,
    #[serde(default)]
    pub openapi_url: Option<String>,
    #[serde(default)]
    pub authentication_notes: Option<String>,
    #[serde(default)]
    pub pricing_rules: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct BalanceResponse {
    pub address: String,
    pub balance_usdc: f64,
    pub spent_today_usdc: f64,
    pub spent_week_usdc: f64,
    pub spent_month_usdc: f64,
}

#[derive(Debug, Serialize)]
pub struct UsageResponse {
    pub total_requests: i64,
    pub total_spent_usdc: f64,
    pub period: String,
    pub by_api: HashMap<String, ApiUsageStats>,
}

#[derive(Debug, Serialize)]
pub struct ApiUsageStats {
    pub requests: i64,
    pub spent_usdc: f64,
}

#[derive(Debug, Serialize)]
pub struct TypeInfo {
    pub key: String,
    pub description: String,
    pub endpoint: String,
}

#[derive(Debug, Serialize)]
pub struct TypesResponse {
    pub gateway: String,
    pub supported_types: Vec<TypeInfo>,
    pub examples: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct TypeListResponse {
    pub listing_type: String,
    pub apis: Vec<ApiInfoResponse>,
    pub meta: ApiListMeta,
}

#[derive(Debug, Deserialize)]
pub struct ProviderListQuery {
    pub search: Option<String>,
    pub sort: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

/// One entry in the provider directory (`GET /v1/providers`) — public
/// identity plus how many visible listings this provider has.
#[derive(Debug, Serialize)]
pub struct ProviderInfoResponse {
    pub name: String,
    pub username: String,
    pub avatar_url: Option<String>,
    pub bio: Option<String>,
    pub sector: Option<String>,
    pub api_count: i64,
    /// User-facing service families, distinct from catalogue listings.
    pub service_count: i64,
    /// Federated listings: the provider is x402-native and Apiosk only indexes them.
    pub federated_count: i64,
    /// Managed listings: proxied and settled by the gateway.
    pub managed_count: i64,
    /// The listing this card IS, when the card is a listing rather than an
    /// account — a first-party product or a self-serve submission, both
    /// carded under their own slug. `null` for an account, which opens as the
    /// list of what it publishes. See `db::ProviderSummary::listing_slug`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listing_slug: Option<String>,
    /// How well this source is documented, and how it has been measured, as
    /// one to three stars with the sentence that explains them. Absent when
    /// the card has no priced endpoint to judge — see `crate::quality`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<crate::quality::CardQuality>,
}

#[derive(Debug, Serialize)]
pub struct ProviderListResponse {
    pub providers: Vec<ProviderInfoResponse>,
    pub total: i64,
    pub search: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ProviderApiListQuery {
    pub search: Option<String>,
    #[serde(default = "default_sort")]
    pub sort: String,
    #[serde(default = "default_order")]
    pub order: String,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

#[derive(Debug, Serialize)]
pub struct ProviderApisResponse {
    pub provider: ProviderInfoResponse,
    pub apis: Vec<ApiInfoResponse>,
    pub meta: ApiListMeta,
}
