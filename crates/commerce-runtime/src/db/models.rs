use super::*;
use thiserror::Error;

/// One row for the agent intent survey (`agent_intents`, migration 049). All
/// fields but `intent` and `source` are optional; `ip_address` is validated to
/// parse as an IP before it reaches here so the `::inet` cast can't fail.
#[derive(Debug)]
pub struct AgentIntentInsert<'a> {
    pub request_id: Option<Uuid>,
    pub api_slug: Option<&'a str>,
    pub path: Option<&'a str>,
    pub method: Option<&'a str>,
    pub intent: &'a str,
    pub use_case: Option<&'a str>,
    pub contact: Option<&'a str>,
    pub agent_name: Option<&'a str>,
    pub user_agent: Option<&'a str>,
    pub ip_address: Option<String>,
    pub source: &'a str,
}

#[derive(Debug, Clone, FromRow)]
pub struct Api {
    pub id: Uuid,
    pub slug: String,
    pub origin_url: String,
    pub wallet_address: String,
    /// Explicit platform-owned open-data revenue: no upstream payment leg.
    #[sqlx(default)]
    pub apiosk_revenue: bool,
    pub status: String,
    pub owner_id: Uuid,
    pub commission_rate: BigDecimal,
    pub owner_wallet: Option<String>,
    pub endpoint_url: Option<String>,
    pub is_community: Option<bool>,
    pub active: Option<bool>,
    pub is_verified: Option<bool>,
    /// How this listing is distributed — `'federated'` for external x402 supply
    /// Apiosk only discovers and executes, anything else (or NULL) for managed
    /// supply Apiosk publishes and operates.
    ///
    /// This is what decides the provider-side fee under the hybrid model, so it
    /// has to travel with the row rather than be inferred at settlement time.
    /// See [`crate::fees::ProviderClass::from_listing_type`].
    pub listing_type: Option<String>,
    /// HMAC signing configuration for outbound proxy requests. Either all
    /// four fields are non-null (signing enabled) or all are null
    /// (signing disabled). The secret is read encrypted from the DB and
    /// only decrypted when a request actually fires.
    pub hmac_signing_algorithm: Option<String>,
    pub hmac_signing_header: Option<String>,
    pub hmac_signing_secret_enc: Option<Vec<u8>>,
    pub hmac_signing_payload_format: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct Endpoint {
    pub id: Uuid,
    pub api_id: Uuid,
    pub method: String,
    pub path: String,
    pub payment_required: bool,
    pub price: BigDecimal,
    pub network_token_id: Option<Uuid>,
    /// Per-endpoint upstream origin (scheme + host). Overrides the listing's
    /// endpoint_url/origin_url for this one endpoint so a single listing can
    /// span hosts (KVK: api.kvk.nl + opendata.kvk.nl). None = listing base.
    pub origin_override: Option<String>,
    /// Per-endpoint upstream credentials, same `{auth_headers, auth_query}`
    /// shape as `listing_metadata`. SECRET — this struct is deliberately not
    /// `Serialize`; never add this field to any serialized output. When set,
    /// only these credentials are injected; when `origin_override` is set
    /// without it, nothing is injected (a listing credential never crosses to
    /// an override host).
    pub auth_override: Option<serde_json::Value>,
    /// Successful GET responses are cached in-memory for this many seconds
    /// (payment stays per call — the cache respects upstream rate limits, it
    /// does not change billing). None = no caching.
    pub cache_ttl_seconds: Option<i32>,
}

#[derive(Debug, Clone, FromRow)]
pub struct EndpointPricingRule {
    pub parameter_name: String,
    pub operator: String,
    pub comparison_value: Option<String>,
    pub price: BigDecimal,
    pub priority: i32,
}

#[derive(Debug, Clone, FromRow)]
pub struct EndpointWithDocs {
    pub id: Uuid,
    pub api_id: Uuid,
    pub method: String,
    pub path: String,
    pub payment_required: bool,
    pub price: BigDecimal,
    pub network_token_id: Option<Uuid>,
    pub description: Option<String>,
    pub request_body: Option<serde_json::Value>,
    pub response_body: Option<serde_json::Value>,
    pub request_example: Option<serde_json::Value>,
    pub response_example: Option<serde_json::Value>,
    pub headers: Option<serde_json::Value>,
    pub query_params: Option<serde_json::Value>,
    pub documentation_markdown: Option<String>,
    pub external_docs_url: Option<String>,
    pub openapi_url: Option<String>,
    pub authentication_notes: Option<String>,
    pub pricing_rules: serde_json::Value,
}

/// Documentation bundle for a single resolved endpoint, joined with its
/// parent API's free-text description. Used solely to enrich the outer
/// 402 JSON body with Bazaar-style discovery metadata. NEVER flows into
/// the facilitator /verify or /settle request bodies — those still see
/// the spec-shaped `PaymentRequirement`.
/// What one endpoint accepts, as the two JSON Schemas the provider documented.
///
/// The complete basis for request binding: `crate::request_binding` may emit a
/// value for a parameter that appears in one of these and for nothing else, so
/// a listing that documents nothing is bound to nothing rather than guessed at.
#[derive(Debug, Clone, FromRow)]
pub struct EndpointSchemas {
    pub id: Uuid,
    /// JSON Schema for the query string (`{"type":"object","properties":{…}}`).
    pub query_params: Option<serde_json::Value>,
    /// JSON Schema for the request body.
    pub request_body: Option<serde_json::Value>,
}

#[derive(Debug, Clone, FromRow)]
pub struct EndpointDocsBundle {
    pub description: Option<String>,
    pub request_body: Option<serde_json::Value>,
    pub response_body: Option<serde_json::Value>,
    pub response_example: Option<serde_json::Value>,
    pub query_params: Option<serde_json::Value>,
    pub api_description: String,
    /// Listing name + category, used to build the Bazaar `serviceName`/`tags`
    /// discovery fields so agents can find the resource by search.
    pub api_name: String,
    pub api_category: String,
}

#[derive(Debug, Clone, FromRow)]
pub struct ApiDetail {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub origin_url: String,
    pub docs_url: Option<String>,
    pub category: Option<String>,
    pub listing_type: Option<String>,
    pub listing_metadata: serde_json::Value,
    pub long_description: Option<String>,
    pub example_usage: Option<String>,
    pub authentication_info: Option<serde_json::Value>,
    pub active: Option<bool>,
    pub is_verified: Option<bool>,
    /// Federated listing marker (see ApiInfo.is_external).
    pub is_external: bool,
    /// Verbatim provider x402 resources for a federated listing; `[]` otherwise.
    pub external_x402: serde_json::Value,
    pub created_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct NetworkToken {
    pub id: Uuid,
    pub network_name: String,
    pub network_display_name: String,
    pub chain_id: i32,
    pub explorer_url: Option<String>,
    pub is_testnet: bool,
    pub token_symbol: String,
    pub token_name: String,
    pub token_decimals: i32,
    pub contract_address: String,
}

/// A single wallet attached to a connect token via the
/// `agent_wallet_connect_token_wallets` join table.
#[derive(Debug, Clone, FromRow)]
pub struct AgentConnectTokenWallet {
    pub wallet_id: Uuid,
    pub wallet_address: String,
    pub wallet_status: String,
    pub daily_limit_usdc: BigDecimal,
    pub per_tx_limit_usdc: BigDecimal,
}

impl AgentConnectToken {
}

/// Connect token + the rails it authorizes. A token may authorize 0..N wallets
/// (USDC via x402) and/or the buyer's verified SEPA mandate (EUR via Mollie).
#[derive(Debug, Clone)]
pub struct AgentConnectToken {
    pub token_id: Uuid,
    pub user_id: Uuid,
    pub wallets: Vec<AgentConnectTokenWallet>,
    /// Destination hosts this connection may spend against (migration 069).
    /// Empty = unrestricted, which is what every pre-069 token and every buyer
    /// who never set an allowlist gets. See `proxy::domain_policy`.
    pub allowed_domains: Vec<String>,
    /// The buyer's own name for this connection ("Claude on my laptop"), and
    /// when it stops working. Carried purely so `/v1/me` can answer "which of
    /// my credentials is this, and how long has it got?" — the two questions a
    /// credential test in an agent framework exists to answer, and the two a
    /// hashed token can't answer on its own. Never consulted on the payment
    /// path.
    pub label: Option<String>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The purchase policy governing this connection (migration 083), if the
    /// buyer attached one and the column exists on this database. `None` means
    /// today's behaviour: the compiled per-transaction and daily caps, and
    /// nothing else. Carried through resolution so the evaluator does not need
    /// a second lookup on the payment path.
    pub policy_id: Option<Uuid>,
}

impl AgentConnectToken {
}

/// Outcome of looking up a connect token by hash. Distinguishes the three
/// reasons a token can fail validation so the gateway can return a precise
/// 401 instead of an opaque "invalid, expired, or revoked" message.
#[derive(Debug)]
pub enum AgentConnectTokenLookup {
    Active(AgentConnectToken),
    Expired {
        token_prefix: String,
        expires_at: chrono::DateTime<chrono::Utc>,
    },
    Revoked {
        token_prefix: String,
    },
    Unknown,
}

#[derive(Debug, Clone)]
pub struct PresenceSessionUpsert {
    pub session_id: Uuid,
    pub client_id: String,
    pub client_kind: String,
    pub managed_wallet_id: Option<Uuid>,
    pub wallet_address: Option<String>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct CreditChargeResult {
    pub gateway_usage_id: i64,
    pub request_id: Uuid,
    pub credits_used: i32,
    pub credits_remaining: i32,
    pub amount_eur: BigDecimal,
    pub already_processed: bool,
}

#[derive(Debug, Error)]
pub enum CreditChargeError {
    #[error("insufficient credits")]
    InsufficientCredits,
    #[error("endpoint price is not supported by credits")]
    UnsupportedPrice,
    #[error(transparent)]
    Database(#[from] anyhow::Error),
}

#[derive(Debug, Clone, FromRow, serde::Serialize)]
pub struct ApiInfo {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub price_usd: BigDecimal,
    pub category: String,
    pub implementation_type: String,
    pub model_backed: bool,
    pub listing_type: String,
    pub docs_url: String,
    pub origin_url: String,
    pub gateway_url: String,
    pub active: bool,
    pub verified: bool,
    pub endpoint_count: i64,
    pub endpoint_docs: serde_json::Value,
    pub listing_metadata: serde_json::Value,
    /// Federated listing marker: true when this row is an external,
    /// already-x402-native API that Apiosk indexes but does NOT proxy.
    pub is_external: bool,
    /// Verbatim provider x402 resources (array of {resource, type,
    /// description, accepts:[...]}) for a federated listing; `[]` otherwise.
    pub external_x402: serde_json::Value,
    /// Public provider identity, joined from `provider_profiles`.
    pub provider_name: Option<String>,
    pub provider_username: Option<String>,
    pub provider_avatar_url: Option<String>,
    pub provider_bio: Option<String>,
    pub provider_sector: Option<String>,
    pub created_at: chrono::DateTime<Utc>,
}

/// One row of the provider directory (`GET /v1/providers`) — a provider's
/// public identity plus how many active, visible listings they have. Only
/// providers with a public `username` and at least one visible listing are
/// returned (see `Database::list_providers`).
#[derive(Debug, Clone, FromRow, serde::Serialize)]
pub struct ProviderSummary {
    pub name: String,
    pub username: String,
    pub avatar_url: Option<String>,
    pub bio: Option<String>,
    pub sector: Option<String>,
    pub api_count: i64,
    /// User-facing service families behind the visible listings. A listing
    /// without explicit `service_groups` is one service.
    pub service_count: i64,
    /// Listings the provider runs x402-native and Apiosk only indexes.
    pub federated_count: i64,
    /// Listings proxied and settled by the gateway.
    pub managed_count: i64,
    /// The slug of the listing this card IS, when the card is a listing rather
    /// than an account: a first-party product (`FIRST_PARTY_PROVIDER`) or a
    /// self-serve submission (`SELF_SERVE_PROVIDER`), both of which are
    /// carded under their own slug and hold exactly one listing.
    ///
    /// `None` for a profiled provider and for the unaffiliated bucket, which
    /// are accounts and open as a list of what they publish.
    ///
    /// Why it is published: without it the only way in is the provider page,
    /// which for a first-party listing is a page holding one card that repeats
    /// the name just clicked — and, worse, one titled with the ACCOUNT that
    /// owns the row ("Apiosk Basics") rather than with the product. A card
    /// that is a listing opens the listing.
    pub listing_slug: Option<String>,
    /// Every listing behind this card. One entry for a card that IS a listing,
    /// many for an account. Published so the caller can look each one up in
    /// the measured-performance cache without a query per row.
    pub slugs: Vec<String>,
    /// Priced, active endpoints behind this card — the denominator for the
    /// three counts below, and zero when there is nothing to judge.
    pub paid_endpoints: i64,
    /// How many of them name their inputs, declare which of those are
    /// required, and say what they do. `crate::quality` turns these into the
    /// stars a card wears; the raw counts stay so the card can say what is
    /// missing rather than only how many stars it lost.
    pub endpoints_with_inputs: i64,
    pub endpoints_with_required: i64,
    pub endpoints_with_description: i64,
}

/// Catalog-scoped totals for one provider (`GET /v1/providers/:username`).
/// All three are counted over the same set — the provider's visible listings
/// matching the search term — so `federated + managed == total` regardless of
/// which page is being returned.
#[derive(Debug, Clone, Copy, FromRow, serde::Serialize)]
pub struct ProviderListingCounts {
    pub total: i64,
    pub services: i64,
    pub federated: i64,
    pub managed: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct WalletUsage {
    pub total_requests: i64,
    pub total_spent_usdc: BigDecimal,
    pub by_api: Vec<ApiUsage>,
}

#[derive(Debug, Clone, FromRow, serde::Serialize)]
pub struct ApiUsage {
    pub api_id: String,
    pub requests: i64,
    pub spent_usdc: BigDecimal,
}

pub struct GatewayRequestLogInsert<'a> {
    pub request_id: Uuid,
    pub session_id: Option<Uuid>,
    pub managed_wallet_id: Option<Uuid>,
    pub client_id: Option<&'a str>,
    pub method: &'a str,
    pub path: &'a str,
    pub query_string: Option<&'a str>,
    pub status_code: i32,
    pub latency_ms: i64,
    pub api_slug: Option<&'a str>,
    pub is_api_route: bool,
    pub user_agent: Option<&'a str>,
    pub ip_hash: Option<&'a str>,
    pub origin: Option<&'a str>,
    pub referrer: Option<&'a str>,
    pub request_headers: &'a serde_json::Value,
    pub response_bytes: Option<i64>,
    pub error_code: Option<&'a str>,
}

// ---------------------------------------------------------------------------
// Purchase policy engine (migration 083, mirroring the buyer portal's
// 20260807120000_purchase_policy_engine.sql)
// ---------------------------------------------------------------------------

/// A buyer's rule set, as the gateway reads it.
///
/// **The unknown-key property is the point of this type.** The `rules` column is
/// `jsonb` so a new rule type ships from the portal without a migration, which
/// only works if a gateway that has never heard of that rule keeps it rather
/// than dropping it. `extra` is where those land: a rule this binary does not
/// understand round-trips intact, is visible in a decision row, and is simply
/// not evaluated — which is honest, and is the opposite of failing closed on a
/// field that did not exist when this binary was built.
///
/// Field names are camelCase because the shape is owned by the portal's
/// `src/lib/policy/types.ts` and written by TypeScript.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PolicyRules {
    pub max_per_transaction_usd: Option<f64>,
    pub max_price_per_call_usd: Option<f64>,
    pub max_per_day_usd: Option<f64>,
    pub max_per_month_usd: Option<f64>,
    pub max_calls_per_day: Option<i64>,
    pub approval_threshold_usd: Option<f64>,
    pub require_approval_for_new_sellers: bool,
    pub allowed_domains: Vec<String>,
    pub blocked_domains: Vec<String>,
    pub allowed_sellers: Vec<String>,
    pub blocked_sellers: Vec<String>,
    pub allowed_regions: Vec<String>,
    pub allowed_assets: Vec<String>,
    pub allowed_networks: Vec<String>,
    pub allow_subscriptions: bool,
    /// Every key this binary does not know about, kept verbatim.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl PolicyRules {
    /// Names of the rules present in the policy that this binary cannot
    /// evaluate. Reported rather than silently ignored: a buyer whose newest
    /// rule is not being applied deserves to be able to find that out.
    pub fn unknown_rule_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.extra.keys().cloned().collect();
        names.sort();
        names
    }
}

/// One row of `purchase_policies`.
#[derive(Debug, Clone)]
pub struct PurchasePolicy {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub rules: PolicyRules,
    /// The version the rules were on when this was read. Recorded on every
    /// decision so the log stays truthful after the buyer edits a rule.
    pub version: i32,
    pub enabled: bool,
}

/// A verdict, on its way into the append-only log.
///
/// Written for `allow` as well as for a refusal. A log that only records
/// refusals cannot answer "why did this go through", which is the question
/// asked after money has already moved.
#[derive(Debug, Clone)]
pub struct PolicyDecisionInsert<'a> {
    pub user_id: Uuid,
    pub policy_id: Option<Uuid>,
    pub policy_version: Option<i32>,
    pub token_id: Option<Uuid>,
    pub wallet_id: Option<Uuid>,
    /// `allow`, `deny` or `require_approval`. The database checks it too.
    pub decision: &'a str,
    /// The rule that produced a non-allow verdict, e.g. `maxPerTransactionUsd`.
    pub matched_rule: Option<&'a str>,
    pub reason: Option<&'a str>,
    pub amount_usd: Option<BigDecimal>,
    pub destination_host: Option<&'a str>,
    pub seller: Option<&'a str>,
}

/// A purchase held for a human decision (`purchase_approvals`).
///
/// The terms here are the terms the buyer approved. Redemption re-checks them,
/// so an approval for $0.08 at api.example.com can never settle $8.00 somewhere
/// else.
#[derive(Debug, Clone)]
pub struct PurchaseApproval {
    pub id: Uuid,
    pub user_id: Uuid,
    pub policy_id: Option<Uuid>,
    pub token_id: Option<Uuid>,
    pub wallet_id: Option<Uuid>,
    pub amount_usd: BigDecimal,
    pub destination_host: Option<String>,
    pub seller: Option<String>,
    pub api_slug: Option<String>,
    pub operation: Option<String>,
    pub matched_rule: Option<String>,
    pub reason: Option<String>,
    /// `pending`, `approved`, `denied`, `expired` or `consumed`.
    pub status: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub decided_at: Option<chrono::DateTime<chrono::Utc>>,
    pub consumed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}
