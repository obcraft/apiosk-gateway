use axum::{
    body::Body,
    http::{header::HeaderMap, HeaderValue, Response, StatusCode},
};
use base64::{
    engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD},
    Engine,
};
use bigdecimal::BigDecimal;
use chrono::Utc;
use ring::signature::{self, Ed25519KeyPair};
use serde::{Deserialize, Serialize};
use simple_asn1::{ASN1Block, BigInt};
use thiserror::Error;
use tracing::{error, info};

use crate::{config::Config, db::NetworkToken};

/// Payment proof header name (x402 v1 protocol)
pub const PAYMENT_HEADER: &str = "x-payment";

/// Payment proof header name (x402 v2 protocol). v2 clients (x402scan's
/// embedded wallet, current reference SDKs) submit their signed payment here;
/// v1 clients keep using `x-payment`. The gateway accepts both.
pub const PAYMENT_SIGNATURE_HEADER: &str = "payment-signature";

/// x402 v2 challenge header (server → client): carries the base64-encoded
/// `PaymentRequired` JSON. v2 makes this header the canonical challenge
/// transport ("response bodies are a server implementation concern"); we emit
/// it alongside the JSON body so both header-first and body-parsing clients
/// (and the x402scan indexer) can read the challenge.
pub const PAYMENT_REQUIRED_HEADER: &str = "payment-required";

/// Hard cap on each facilitator round-trip (verify/settle). The shared HTTP
/// client has no global timeout (it also proxies upstream APIs, which may stream
/// slowly), so without a per-request cap a stalled CDP endpoint would hang the
/// paid-request hot path indefinitely. Bounded so a facilitator blip fails fast
/// (and settle is retried) instead of wedging the request.
const FACILITATOR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);

#[allow(dead_code)]
pub const PAYMENT_RESPONSE_HEADER: &str = "x-payment-response";

// Facilitator URL is configurable via AppState (passed as parameter)
// Facilitator URL is now configurable via AppState (passed as parameter)

#[derive(Debug, Error)]
pub enum PaymentError {
    #[error("Invalid payment proof format")]
    InvalidFormat,
    #[error("Payment verification failed: {0}")]
    VerificationFailed(String),
    #[error("Facilitator request failed: {0}")]
    FacilitatorError(String),
    #[error("Settlement failed: {0}")]
    SettlementFailed(String),
}

/// x402 Payment Requirement for 402 responses
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRequirement {
    pub scheme: String,
    pub network: String,
    pub max_amount_required: String,
    pub resource: String,
    pub description: String,
    pub mime_type: String,
    pub pay_to: String,
    pub max_timeout_seconds: u64,
    pub asset: String,
    /// Extra metadata required by the x402 facilitator: the EIP-712 domain
    /// (`{name, version}`).
    pub extra: serde_json::Value,
    /// x402 v2 discovery extensions block (`extensions.bazaar`). Carried on the
    /// PaymentRequirement so it travels all the way into the `/settle` payload,
    /// which is what the CDP Facilitator reads (`extractDiscoveryInfo`) to
    /// catalog the resource in the Bazaar after a successful settlement.
    ///
    /// Skipped during serialization when `None`, so every code path that does
    /// NOT attach Bazaar metadata produces a byte-for-byte identical requirement
    /// to before — verify/settle behaviour for those paths is unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<serde_json::Value>,
    /// x402 **v1** discovery payload (`outputSchema`). Our verify/settle are
    /// `x402Version: 1`, and on v1 the CDP Facilitator's `extractDiscoveryInfo`
    /// reads discovery info from `paymentRequirements.outputSchema` — NOT from
    /// `.extensions` (the v2 location). Carrying it here is what actually gets
    /// the resource cataloged in the Bazaar after settlement; without it,
    /// settlement succeeds but discovery extraction returns null and the route
    /// is never indexed. Skipped when `None` for byte-identical legacy output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
}

/// EVM `extra` block: EIP-712 domain (token `name()` + `version()`).
pub fn evm_requirement_extra(token_name: &str) -> serde_json::Value {
    serde_json::json!({ "name": token_name, "version": "2" })
}

/// Map a v1 network name (what the DB `network_tokens.network_name` column
/// stores) to its CAIP-2 identifier — the network format x402 v2 mandates.
/// Unknown names (or values that are already CAIP-2) pass through unchanged,
/// so a mis-seeded row degrades to "facilitator rejects it" rather than a
/// silently wrong chain id.
pub fn caip2_network_id(network_name: &str) -> String {
    match network_name {
        "base" => "eip155:8453",
        "base-sepolia" => "eip155:84532",
        "polygon" => "eip155:137",
        "polygon-amoy" => "eip155:80002",
        "arbitrum" => "eip155:42161",
        "avalanche" => "eip155:43114",
        other => other,
    }
    .to_string()
}

/// Inverse of [`caip2_network_id`]: normalize a network identifier from a
/// payment payload (v2 payloads carry CAIP-2, v1 payloads carry the plain
/// name) back to the v1 name the offer set and DB rows use.
pub fn v1_network_name(network: &str) -> String {
    match network {
        "eip155:8453" => "base",
        "eip155:84532" => "base-sepolia",
        "eip155:137" => "polygon",
        "eip155:80002" => "polygon-amoy",
        "eip155:42161" => "arbitrum",
        "eip155:43114" => "avalanche",
        other => other,
    }
    .to_string()
}

/// Bazaar discovery metadata appended to the OUTER 402 body for indexers
/// (Coinbase CDP Bazaar, agent registries) — never injected into the
/// `PaymentRequirement` itself. The facilitator continues to receive the
/// spec-shaped requirement on /verify and /settle.
///
/// Sourced from `api_endpoints` (description, request_body, response_body,
/// response_example, query_params) and `apis` (description fallback).
#[derive(Debug, Clone, Default)]
pub struct BazaarMetadata {
    /// Endpoint-level description, falling back to api.description. May be
    /// `None` when neither column is populated.
    pub description: Option<String>,
    /// Optional input schema (JSON Schema fragment from
    /// `api_endpoints.request_body`).
    pub input_schema: Option<serde_json::Value>,
    /// Optional query-params dictionary for GET-ish endpoints
    /// (`api_endpoints.query_params`).
    pub query_params: Option<serde_json::Value>,
    /// Optional response example (`api_endpoints.response_example`).
    pub response_example: Option<serde_json::Value>,
    /// Optional response body schema (`api_endpoints.response_body`).
    pub response_schema: Option<serde_json::Value>,
    /// HTTP method of the resolved endpoint (e.g. "GET", "POST"). Drives
    /// the `info.input` shape: GET/HEAD/DELETE → `queryParams`,
    /// POST/PUT/PATCH → `bodySchema`.
    pub method: String,
    /// MIME of the proxied response. The gateway only proxies JSON APIs in
    /// the x402-paid scope, so this is always "json".
    pub output_mime: &'static str,
    /// Human service name (`apis.name`) surfaced as the Bazaar `serviceName`
    /// so the resource shows a real title — not "API call to <slug>".
    pub service_name: Option<String>,
    /// Search tags (built from `apis.category` + slug). The Bazaar ranks/searches
    /// on these, so they are the difference between "indexed" and "discoverable".
    pub tags: Vec<String>,
    /// Optional brand/icon URL surfaced as the Bazaar `iconUrl`.
    pub icon_url: Option<String>,
}

impl BazaarMetadata {
    fn input_block(&self) -> serde_json::Value {
        let mut input = serde_json::json!({
            "type": "http",
            "method": self.method,
        });
        let method_upper = self.method.to_ascii_uppercase();
        let prefers_query_params = matches!(method_upper.as_str(), "GET" | "HEAD" | "DELETE");

        if prefers_query_params {
            let qp = self
                .query_params
                .clone()
                .unwrap_or_else(|| serde_json::json!({}));
            input
                .as_object_mut()
                .expect("input is an object literal")
                .insert("queryParams".to_string(), qp);
        } else {
            // Spec shape for body methods is `bodyType` + `body`
            // (BodyDiscoveryInfo) — NOT `bodySchema`, which the Bazaar spec
            // validator (`validateDiscoveryExtensionSpec`) does not know.
            // `body` carries the request-body field map, mirroring the v1
            // extractor's `bodyFields` source.
            let body = self
                .input_schema
                .as_ref()
                .and_then(|s| s.get("properties").cloned())
                .or_else(|| self.input_schema.clone())
                .unwrap_or_else(|| serde_json::json!({}));
            let obj = input.as_object_mut().expect("input is an object literal");
            obj.insert("bodyType".to_string(), serde_json::json!("json"));
            obj.insert("body".to_string(), body);
        }
        input
    }

    fn output_block(&self) -> serde_json::Value {
        let mut output = serde_json::json!({ "type": self.output_mime });
        let obj = output.as_object_mut().expect("output is an object literal");
        if let Some(example) = self.response_example.clone() {
            obj.insert("example".to_string(), example);
        }
        // Advertise the endpoint's response JSON Schema (from
        // `api_endpoints.response_body`) alongside the example so agents can
        // discover the response shape, not just a sample. The `schema_block`
        // validator only requires `output` to be an object, so an extra
        // `schema` key validates cleanly.
        if let Some(schema) = self.response_schema.clone() {
            obj.insert("schema".to_string(), schema);
        }
        output
    }

    /// The full `extensions.bazaar` block as the CDP Facilitator expects it on
    /// the PaymentRequirement. Single source of truth for both the outer 402
    /// body (`into_response_with_bazaar`) and the `/settle` payload
    /// (`PaymentRequirement::with_bazaar_extension`) so discovery and settlement
    /// advertise identical metadata.
    pub fn discovery_extension(&self) -> serde_json::Value {
        serde_json::json!({
            "bazaar": {
                "info": {
                    "input": self.input_block(),
                    "output": self.output_block(),
                },
                "schema": self.schema_block(),
            }
        })
    }

    /// Build the x402 **v1** `outputSchema` that the CDP Facilitator's
    /// `extractDiscoveryInfoV1` consumes to catalog the resource. On a v1
    /// settle (which is what we send) the extractor looks ONLY at
    /// `paymentRequirements.outputSchema`; `extensions` is the v2 location and
    /// is ignored. So this — not `discovery_extension` — is what gets Apiosk's
    /// routes indexed in the Bazaar.
    ///
    /// Shape follows the reference SDK's `hasV1OutputSchema` +
    /// `extractDiscoveryInfoV1`:
    /// `{ input: { type:"http", method, discoverable:true, queryParams|bodyFields }, output }`.
    /// `discoverable: true` is stated explicitly (the extractor defaults it to
    /// true, but we keep the intent durable). `output` is taken verbatim as the
    /// response example, so we pass the realistic example and omit it when none
    /// exists.
    pub fn v1_output_schema(&self) -> serde_json::Value {
        let mut input = serde_json::json!({
            "type": "http",
            "method": self.method,
            "discoverable": true,
        });
        let obj = input.as_object_mut().expect("input is an object literal");
        let method_upper = self.method.to_ascii_uppercase();
        if matches!(method_upper.as_str(), "GET" | "HEAD" | "DELETE") {
            obj.insert(
                "queryParams".to_string(),
                self.query_params
                    .clone()
                    .unwrap_or_else(|| serde_json::json!({})),
            );
        } else {
            // POST/PUT/PATCH: the v1 extractor reads bodyFields/body/data —
            // surface the request-body schema's `properties` (or the schema).
            let body_fields = self
                .input_schema
                .as_ref()
                .and_then(|s| s.get("properties").cloned())
                .or_else(|| self.input_schema.clone())
                .unwrap_or_else(|| serde_json::json!({}));
            obj.insert("bodyFields".to_string(), body_fields);
        }

        let mut output_schema = serde_json::Map::new();
        output_schema.insert("input".to_string(), input);
        if let Some(example) = self.response_example.clone() {
            output_schema.insert("output".to_string(), example);
        }
        serde_json::Value::Object(output_schema)
    }

    fn schema_block(&self) -> serde_json::Value {
        // The CDP Facilitator's `validateDiscoveryExtension` compiles THIS
        // schema with Ajv and validates `info` (the `{input, output}`
        // ENVELOPE) against it. `info.input` is the HTTP envelope
        // (`{type, method, bodyType, body|queryParams}`) — it is NOT the
        // endpoint's request-body schema. Embedding the request-body schema
        // under `properties.input` (as this method did until 2026-07-10)
        // makes every extension fail its own validation (e.g. required
        // body params like `url` are "missing" from the envelope), and a
        // failed validation silently drops the resource from the Bazaar —
        // that froze ALL Apiosk indexing from 2026-07-05 onward.
        //
        // Use the permissive envelope schema, byte-identical to the
        // reference SDK's `buildBazaarExtensionFromDiscoveryInfo`. The real
        // request-body shape still travels in `info.input.body` and in the
        // v1 `outputSchema` (`v1_output_schema`).
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {
                "input": { "type": "object" },
                "output": { "type": "object" },
            },
            "required": ["input"],
        })
    }
}

#[derive(Debug, Serialize)]
struct CdpJwtHeader<'a> {
    alg: &'a str,
    typ: &'static str,
    kid: &'a str,
    nonce: String,
}

#[derive(Debug, Serialize)]
struct CdpJwtClaims<'a> {
    sub: &'a str,
    iss: &'static str,
    aud: [&'static str; 1],
    nbf: usize,
    exp: usize,
    uri: String,
}

impl PaymentRequirement {
    /// Create a new payment requirement with network token configuration from
    /// the database.
    pub fn new(
        wallet_address: &str,
        amount: &BigDecimal,
        resource_url: &str,
        description: &str,
        network_token: &NetworkToken,
    ) -> Self {
        Self::for_network(
            wallet_address,
            amount,
            resource_url,
            description,
            network_token,
        )
    }

    /// Network-aware constructor.
    pub fn for_network(
        wallet_address: &str,
        amount: &BigDecimal,
        resource_url: &str,
        description: &str,
        network_token: &NetworkToken,
    ) -> Self {
        // Convert amount to atomic units based on token decimals
        // e.g., 0.01 USDC (6 decimals) = 10000 atomic units
        // e.g., 0.01 DAI (18 decimals) = 10000000000000000 atomic units
        let multiplier = BigDecimal::from(10_i64.pow(network_token.token_decimals as u32));
        let amount_atomic = (amount * multiplier)
            .to_string()
            .split('.')
            .next()
            .unwrap_or("0")
            .to_string();

        Self {
            scheme: "exact".to_string(),
            network: network_token.network_name.clone(),
            max_amount_required: amount_atomic,
            resource: resource_url.to_string(),
            description: description.to_string(),
            mime_type: "application/json".to_string(),
            pay_to: wallet_address.to_string(),
            max_timeout_seconds: 300,
            asset: network_token.contract_address.clone(),
            // EIP-712 domain — name must match the on-chain ERC-20 name()
            // return value (USDC on Base: "USD Coin", NOT "USDC"); we use
            // token_name (full name) not token_symbol.
            extra: evm_requirement_extra(&network_token.token_name),
            // No discovery metadata by default. Attached explicitly via
            // `with_bazaar_extension` on the paid-request path so it reaches
            // `/settle`; left `None` everywhere else for byte-identical output.
            extensions: None,
            output_schema: None,
        }
    }

    /// Attach the x402 v2 Bazaar discovery extension to this requirement so it
    /// is serialized into the `/settle` payload. The CDP Facilitator reads
    /// `paymentRequirements.extensions.bazaar` on a successful settlement and
    /// catalogs the resource in the Bazaar — without this, settlement succeeds
    /// but the route never gets indexed.
    ///
    /// The extension shape mirrors the one published in the outer 402 body
    /// (`into_response_with_bazaar`), so what an agent discovers in the 402 and
    /// what CDP indexes from settlement stay consistent.
    ///
    /// Because our settle is `x402Version: 1`, the facilitator's v1 discovery
    /// extractor reads `outputSchema` (NOT `extensions`), so we attach BOTH:
    /// `extensions` for v2-aware clients and human/agent discovery, and
    /// `output_schema` — the field that actually drives v1 cataloging today.
    pub fn with_bazaar_extension(mut self, bazaar: &BazaarMetadata) -> Self {
        self.extensions = Some(bazaar.discovery_extension());
        self.output_schema = Some(bazaar.v1_output_schema());
        self
    }

    /// Serialize this requirement as an x402 **v1** `PaymentRequirements`
    /// object — the exact camelCase shape the released Coinbase client SDKs
    /// (`x402`/`x402-fetch` 0.6.x through 1.2.x) validate with
    /// `PaymentRequirementsSchema.parse()`: bare `network` name,
    /// `maxAmountRequired`, `resource` (URL string), `description`,
    /// `mimeType`, `payTo`, `maxTimeoutSeconds`, `asset`, plus the optional
    /// `outputSchema`/`extensions` discovery fields when attached.
    ///
    /// This is the shape the 402 **body** `accepts[]` must carry: those SDKs
    /// read the body only (never the `PAYMENT-REQUIRED` header) and their
    /// zod enum REJECTS CAIP-2 network ids (`eip155:8453`) outright — an
    /// incident on 2026-07-06 made every SDK payer fail client-side when the
    /// body briefly switched to v2 shapes.
    pub fn to_v1_value(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}));
        // Spec purity: `PaymentRequirementsV1` has NO `extensions` field
        // (reference types: "NO extensions field - V1 doesn't support
        // extensions"). The only v1 sellers the CDP Bazaar demonstrably still
        // ingests (checked live 2026-07-10) send outputSchema-only v1
        // requirements. `extensions` stays an internal field feeding the v2
        // header/`to_v2_value` — it must never leak onto the v1 wire.
        if let Some(obj) = value.as_object_mut() {
            obj.remove("extensions");
        }
        value
    }

    /// Serialize this requirement as an x402 **v2** `PaymentRequirements`
    /// object: exactly `{scheme, network (CAIP-2), amount, asset, payTo,
    /// maxTimeoutSeconds, extra}`.
    ///
    /// v2 deliberately drops several v1 fields from the requirement itself:
    /// `maxAmountRequired` is renamed to `amount`; the resource descriptor
    /// (`resource`/`description`/`mimeType`) moves to the top-level `resource`
    /// object; and the v1 discovery field `outputSchema` is removed. Crucially,
    /// v2 requirements carry **no per-entry `extensions`** — discovery metadata
    /// lives only in the top-level `extensions` envelope (which the client
    /// echoes back), never on a PaymentRequirements object. Emitting a stray
    /// `extensions` here would risk rejection by strict v2 validators (e.g.
    /// x402scan's zod schema).
    ///
    /// Used for the v2 `PAYMENT-REQUIRED` header `accepts[]`, the
    /// `/.well-known/x402` discovery doc, and the `paymentRequirements`
    /// field of v2 facilitator verify/settle calls — NEVER for the 402 body
    /// `accepts[]` (see `to_v1_value`).
    pub fn to_v2_value(&self) -> serde_json::Value {
        let mut item = serde_json::Map::new();
        item.insert("scheme".to_string(), serde_json::json!(self.scheme));
        item.insert(
            "network".to_string(),
            serde_json::json!(caip2_network_id(&self.network)),
        );
        item.insert(
            "amount".to_string(),
            serde_json::json!(self.max_amount_required),
        );
        item.insert("asset".to_string(), serde_json::json!(self.asset));
        item.insert("payTo".to_string(), serde_json::json!(self.pay_to));
        item.insert(
            "maxTimeoutSeconds".to_string(),
            serde_json::json!(self.max_timeout_seconds),
        );
        item.insert("extra".to_string(), self.extra.clone());
        serde_json::Value::Object(item)
    }

    /// Emit the spec-shaped 402 with no Bazaar enrichment. Preserved as a
    /// thin wrapper so existing callers (and unit tests) still produce a
    /// valid response; new callers should prefer
    /// `into_response_with_bazaar` so the JSON body advertises
    /// `description` + `extensions.bazaar` for discovery.
    #[allow(dead_code)]
    pub fn into_response(self) -> Response<Body> {
        self.into_response_with_bazaar(BazaarMetadata::default())
    }

    /// Emit the 402 with Bazaar discovery metadata appended to the OUTER
    /// body (top-level `description` + `extensions.bazaar`). `accepts[0]`
    /// reflects whatever the caller attached to the requirement: on the CDP
    /// path the caller enriches it via `with_bazaar_extension` so the 402
    /// carries `accepts[0].outputSchema` (+ `extensions`) — that is the field
    /// CDP's Bazaar indexer crawls to catalog the resource. For un-enriched
    /// callers `accepts[0]` stays spec-minimal. These are metadata fields the
    /// facilitator ignores for EIP-712 verification, so /verify and /settle
    /// are unaffected either way.
    pub fn into_response_with_bazaar(self, bazaar: BazaarMetadata) -> Response<Body> {
        self.into_payment_required_response(bazaar, "Payment required", None)
    }

    /// Multi-network variant of [`Self::into_response_with_bazaar`]: `self` is
    /// the primary requirement (`accepts[0]`, the network the provider chose)
    /// and `alternates` are additional networks the buyer may pay on instead
    /// (one per configured payout chain — Base fallback, Polygon, Arbitrum).
    /// Every entry prices the SAME resource at the same USDC amount;
    /// only network/asset/payTo/extra differ. Spec-conform: x402 `accepts` is
    /// an array precisely so a buyer can pick the rail that suits them.
    pub fn into_response_with_bazaar_and_alternates(
        self,
        bazaar: BazaarMetadata,
        alternates: Vec<PaymentRequirement>,
    ) -> Response<Body> {
        self.into_payment_required_response_with_alternates(
            alternates,
            bazaar,
            "Payment required",
            None,
        )
    }

    /// Emit a 402 that signals the submitted payment proof was REJECTED
    /// (missing, expired, or invalid) — distinct from the generic
    /// "Payment required" challenge. Without this, an invalid payment and a
    /// "you haven't paid yet" challenge are byte-identical, so a caller (or an
    /// operator debugging) cannot tell a rejected payment apart from a fresh
    /// one. The concrete facilitator reason is surfaced as `errorReason`, and
    /// `accepts` is still present so the caller can retry with a valid payment.
    pub fn into_payment_invalid_response(
        self,
        bazaar: BazaarMetadata,
        reason: &str,
    ) -> Response<Body> {
        self.into_payment_required_response(bazaar, "payment_verification_failed", Some(reason))
    }

    /// Shared 402 body builder. `error_code` is the top-level `error`; when
    /// `reason` is set it is added as `errorReason` for diagnosability.
    /// `accepts` + `extensions` are always emitted so a caller can (re)pay.
    fn into_payment_required_response(
        self,
        bazaar: BazaarMetadata,
        error_code: &str,
        reason: Option<&str>,
    ) -> Response<Body> {
        self.into_payment_required_response_with_alternates(Vec::new(), bazaar, error_code, reason)
    }

    fn into_payment_required_response_with_alternates(
        self,
        alternates: Vec<PaymentRequirement>,
        bazaar: BazaarMetadata,
        error_code: &str,
        reason: Option<&str>,
    ) -> Response<Body> {
        // Capture the resource descriptor fields before `self` is moved into
        // `accepts`, so we can emit the v2 top-level `resource` object.
        let resource_url = self.resource.clone();
        let resource_mime = self.mime_type.clone();
        let resource_fallback_desc = self.description.clone();
        let mut requirements = vec![self];
        requirements.extend(alternates);
        // DUAL-STACK challenge — one dialect per transport:
        //
        //   BODY  = x402 v1. The released Coinbase client SDKs (`x402-fetch`
        //   0.6.x–1.2.x, i.e. every real payer today) read ONLY the JSON body
        //   and validate each accepts[] entry with a strict zod schema: bare
        //   `network` names, `maxAmountRequired`, per-entry `resource`/
        //   `description`/`mimeType`. CAIP-2 ids or v2 field names make the
        //   parse THROW and the route unpayable (live incident 2026-07-06:
        //   every SDK payment failed client-side with invalid_enum_value on
        //   "eip155:8453"). The body is therefore byte-compatible with the
        //   pre-v2 known-good shape, and only networks in the SDK enum are
        //   offered here (`arbitrum` is not — one unknown entry poisons the
        //   whole `accepts.map(parse)`).
        //
        //   PAYMENT-REQUIRED header = x402 v2 (`amount` + CAIP-2), the v2
        //   canonical transport. Header-first v2 clients and indexers that
        //   reject v1 challenges (x402scan) read this; v1 SDKs never look at
        //   it. Both transports advertise identical payment terms.
        //
        // The facilitator side stays version-adaptive (see `verify_payment`/
        // `settle_payment`): whichever dialect the buyer pays in is echoed.
        const SDK_V1_BODY_NETWORKS: &[&str] = &["base", "base-sepolia", "polygon", "polygon-amoy"];
        let accepts: Vec<serde_json::Value> = requirements
            .iter()
            .filter(|r| SDK_V1_BODY_NETWORKS.contains(&r.network.as_str()))
            .map(|r| r.to_v1_value())
            .collect();
        let v2_accepts: Vec<serde_json::Value> =
            requirements.iter().map(|r| r.to_v2_value()).collect();
        let mut body = serde_json::Map::new();
        body.insert("x402Version".to_string(), serde_json::json!(1));
        body.insert(
            "error".to_string(),
            serde_json::Value::String(error_code.to_string()),
        );
        if let Some(reason) = reason.map(str::trim).filter(|s| !s.is_empty()) {
            body.insert(
                "errorReason".to_string(),
                serde_json::Value::String(reason.to_string()),
            );
        }
        body.insert(
            "accepts".to_string(),
            serde_json::Value::Array(accepts.clone()),
        );
        // Only advertise a top-level `description` when we actually have
        // one. Emitting an empty string would push noise into Bazaar's
        // search index and dilute resource ranking.
        if let Some(desc) = bazaar
            .description
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            body.insert(
                "description".to_string(),
                serde_json::Value::String(desc.to_string()),
            );
        }
        // v2-style `resource` descriptor: carries the discovery fields the
        // Bazaar ranks + searches on (serviceName, tags, iconUrl). This is the
        // difference between "indexed" and "agents actually find us". Each
        // field is soft-dropped (omitted) when empty/invalid so a bare listing
        // still produces a valid body. Lengths bounded per the Bazaar spec
        // (serviceName/tags ≤ 32 chars, ≤ 5 tags).
        {
            let mut resource = serde_json::Map::new();
            resource.insert("url".to_string(), serde_json::Value::String(resource_url));
            resource.insert(
                "mimeType".to_string(),
                serde_json::Value::String(resource_mime),
            );
            let desc = bazaar
                .description
                .as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .or_else(|| Some(resource_fallback_desc).filter(|s| !s.trim().is_empty()));
            if let Some(desc) = desc {
                resource.insert("description".to_string(), serde_json::Value::String(desc));
            }
            if let Some(name) = bazaar
                .service_name
                .as_ref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
            {
                let name: String = name.chars().take(32).collect();
                resource.insert("serviceName".to_string(), serde_json::Value::String(name));
            }
            let tags: Vec<serde_json::Value> = bazaar
                .tags
                .iter()
                .map(|t| t.trim())
                .filter(|t| !t.is_empty() && t.len() <= 32 && t.is_ascii())
                .take(5)
                .map(|t| serde_json::Value::String(t.to_string()))
                .collect();
            if !tags.is_empty() {
                resource.insert("tags".to_string(), serde_json::Value::Array(tags));
            }
            if let Some(icon) = bazaar
                .icon_url
                .as_ref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty() && s.len() <= 2048)
            {
                resource.insert(
                    "iconUrl".to_string(),
                    serde_json::Value::String(icon.to_string()),
                );
            }
            body.insert("resource".to_string(), serde_json::Value::Object(resource));
        }
        body.insert("extensions".to_string(), bazaar.discovery_extension());

        // v2 canonical challenge transport: the `PAYMENT-REQUIRED` header,
        // base64-encoded JSON. Unlike the (v1) body, the header is fully v2:
        // `x402Version: 2` with `to_v2_value` accepts entries (`amount` +
        // CAIP-2 network, no per-entry extensions), sharing the body's
        // error/resource descriptors and top-level Bazaar extension. CDP's
        // `/validate` endpoint reads the canonical header and rejects a route
        // when `extensions.bazaar` is present only in the legacy JSON body.
        // Header-first v2 clients and indexers read this; v1 SDK clients parse
        // the body; both see identical discovery metadata and payment terms.
        let payment_required_header = {
            let mut lean = serde_json::Map::new();
            lean.insert("x402Version".to_string(), serde_json::json!(2));
            for key in ["error", "errorReason", "resource", "extensions"] {
                if let Some(v) = body.get(key) {
                    lean.insert(key.to_string(), v.clone());
                }
            }
            lean.insert("accepts".to_string(), serde_json::json!(v2_accepts));
            serde_json::to_vec(&serde_json::Value::Object(lean))
                .ok()
                .map(|bytes| BASE64.encode(bytes))
                .and_then(|encoded| HeaderValue::from_str(&encoded).ok())
        };

        let body = serde_json::Value::Object(body);
        let body_bytes = match serde_json::to_vec(&body) {
            Ok(bytes) => bytes,
            Err(err) => {
                error!("Failed to serialize payment requirement response: {}", err);
                br#"{"error":"payment_response_serialization_failed"}"#.to_vec()
            }
        };

        let mut response = Response::new(Body::from(body_bytes));
        *response.status_mut() = StatusCode::PAYMENT_REQUIRED;
        response
            .headers_mut()
            .insert("Content-Type", HeaderValue::from_static("application/json"));
        response
            .headers_mut()
            .insert("X-Payment-Protocol", HeaderValue::from_static("x402"));
        if let Some(value) = payment_required_header {
            response
                .headers_mut()
                .insert(PAYMENT_REQUIRED_HEADER, value);
        }
        response
    }
}

/// Extract payment proof from request headers. Accepts both the x402 v2
/// header (`payment-signature`, preferred) and the v1 header (`x-payment`),
/// so v1 and v2 clients can pay the same route.
pub fn extract_payment_proof(headers: &HeaderMap<HeaderValue>) -> Option<String> {
    headers
        .get(PAYMENT_SIGNATURE_HEADER)
        .or_else(|| headers.get(PAYMENT_HEADER))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

pub fn is_cdp_facilitator_url(facilitator_url: &str) -> bool {
    reqwest::Url::parse(facilitator_url)
        .ok()
        .and_then(|url| {
            url.host_str()
                .map(|host| host.eq_ignore_ascii_case("api.cdp.coinbase.com"))
        })
        .unwrap_or(false)
}

pub fn is_testnet_only_x402_facilitator(facilitator_url: &str) -> bool {
    reqwest::Url::parse(facilitator_url)
        .ok()
        .and_then(|url| {
            url.host_str().map(|host| {
                host.eq_ignore_ascii_case("x402.org") || host.eq_ignore_ascii_case("www.x402.org")
            })
        })
        .unwrap_or(false)
}

pub fn hosted_x402_wallet_checkout_unavailable_reason(
    config: &Config,
    network_token: &NetworkToken,
) -> Option<String> {
    if network_token.is_testnet {
        return None;
    }

    // A `X402_LOCAL_SETTLEMENT_ENABLED` escape hatch used to return None here
    // for Base: with the local EIP-3009 backend on, wallet checkout did not
    // depend on the facilitator at all. That backend settled the BUYER's own
    // signed authorization on-chain, which is no longer a way Apiosk moves
    // money, so the facilitator checks below are the only ones left.

    if is_testnet_only_x402_facilitator(&config.facilitator_url) {
        return Some(
            "Wallet checkout is disabled because the gateway is configured with the testnet-only x402.org facilitator. Switch FACILITATOR_URL to https://api.cdp.coinbase.com/platform/v2/x402 and configure CDP_API_KEY_ID plus CDP_API_KEY_SECRET for Base mainnet."
                .to_string(),
        );
    }

    if is_cdp_facilitator_url(&config.facilitator_url)
        && (config.cdp_api_key_id.is_none() || config.cdp_api_key_secret.is_none())
    {
        return Some(
            "Wallet checkout is disabled because the gateway points at the CDP facilitator without CDP_API_KEY_ID and CDP_API_KEY_SECRET."
                .to_string(),
        );
    }

    None
}

/// Verify request for x402 verification. `payment_requirements` is a
/// pre-shaped JSON value (v1 struct form or v2 `to_v2_value`) so verify can
/// mirror whichever protocol version the client's payload used.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifyRequest {
    x402_version: u8,
    payment_payload: serde_json::Value,
    payment_requirements: serde_json::Value,
}

/// Detect the x402 protocol version the client used (from its decoded payload)
/// and build the matching facilitator `paymentRequirements` value. A v1 payload
/// keeps the exact byte-for-byte v1 path — the proven revenue path is untouched;
/// a v2 payload (x402scan's wallet, current reference SDKs) gets the v2-shaped
/// requirements the facilitator cross-checks against the v2 payload.
///
/// The version discriminator is the payload's own `x402Version` (defaulting to
/// 1 when absent), so the gateway always echoes back exactly what the client
/// spoke rather than forcing a version on it.
fn facilitator_payment_context(
    payload: &serde_json::Value,
    requirement: &PaymentRequirement,
) -> (u8, serde_json::Value) {
    let version = payload
        .get("x402Version")
        .and_then(|v| v.as_u64())
        .unwrap_or(1);
    if version >= 2 {
        (2, requirement.to_v2_value())
    } else {
        // v1: spec-pure `PaymentRequirementsV1` — `outputSchema` (the field
        // the CDP Bazaar's v1 `extractDiscoveryInfoV1` reads to catalog the
        // resource) is kept, `extensions` is stripped (`to_v1_value`). The
        // June-era settles that demonstrably indexed used exactly this shape.
        (1, requirement.to_v1_value())
    }
}

/// Version-adaptive payload shaping for verify/settle.
///
/// **v1 payloads pass through UNTOUCHED.** `PaymentPayloadV1` is exactly
/// `{x402Version, scheme, network, payload}` — the reference types state
/// "NO extensions field - V1 doesn't support extensions", and there is no
/// v1 `resource` field either. On v1 the CDP Bazaar catalogs from
/// `paymentRequirements.outputSchema` (`extractDiscoveryInfoV1`), so nothing
/// needs to ride on the payload. Injecting non-spec fields into a v1 payload
/// (as this function did on 2026-07-10, commits 17e7462/d94a3f3) risks the
/// discovery pipeline's strict v1 parse rejecting the whole settle.
///
/// **v2 payloads** get `resource` and the requirement's discovery
/// `extensions` filled in when the client omitted them — v2 is where those
/// fields legitimately live.
fn payment_payload_for_facilitator(
    mut payload: serde_json::Value,
    requirement: &PaymentRequirement,
) -> serde_json::Value {
    let is_v2 = payload
        .get("x402Version")
        .and_then(|v| v.as_u64())
        .unwrap_or(1)
        >= 2;
    if !is_v2 {
        return payload;
    }
    if let Some(obj) = payload.as_object_mut() {
        let missing_resource = obj
            .get("resource")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .map(str::is_empty)
            .unwrap_or(true);
        if missing_resource {
            obj.insert(
                "resource".to_string(),
                serde_json::Value::String(requirement.resource.clone()),
            );
        }

        if let Some(requirement_extensions) = requirement.extensions.as_ref() {
            match obj.get_mut("extensions") {
                Some(existing_extensions)
                    if existing_extensions.is_object() && requirement_extensions.is_object() =>
                {
                    let existing = existing_extensions
                        .as_object_mut()
                        .expect("checked object above");
                    let required = requirement_extensions
                        .as_object()
                        .expect("checked object above");
                    for (key, value) in required {
                        existing.entry(key.clone()).or_insert_with(|| value.clone());
                    }
                }
                Some(existing_extensions) if existing_extensions.is_null() => {
                    *existing_extensions = requirement_extensions.clone();
                }
                None => {
                    obj.insert("extensions".to_string(), requirement_extensions.clone());
                }
                Some(_) => {}
            }
        }
    }
    payload
}

/// Response from facilitator verify endpoint
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyResponse {
    pub is_valid: bool,
    pub invalid_reason: Option<String>,
    #[allow(dead_code)]
    pub payer: Option<String>,
}

/// Settle request for x402 settlement. Same version-adaptive
/// `payment_requirements` treatment as [`VerifyRequest`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SettleRequest {
    x402_version: u8,
    payment_payload: serde_json::Value,
    payment_requirements: serde_json::Value,
}

/// Response from facilitator settle endpoint
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettleResponse {
    pub success: bool,
    pub transaction: Option<String>,
    #[allow(dead_code)]
    pub network: Option<String>,
    #[allow(dead_code)]
    pub payer: Option<String>,
    pub error_reason: Option<String>,
}

/// Header the CDP Facilitator sets on verify/settle responses to report how it
/// handled each declared x402 extension. For Bazaar, a value of `processing`
/// means our discovery metadata was accepted and indexing is queued; `rejected`
/// means our `extensions.bazaar.info.input` failed strict JSON Schema validation
/// against `schema.properties.input`, so the route will NOT be cataloged.
const EXTENSION_RESPONSES_HEADER: &str = "extension-responses";

/// Log the CDP Facilitator's `EXTENSION-RESPONSES` header so operators can see
/// whether Bazaar discovery metadata was accepted (`processing`) or rejected.
/// Best-effort and non-fatal: the header is absent on the testnet x402.org
/// facilitator and on any response that carried no extensions, in which case we
/// stay silent. `operation` is "verify" or "settle" for log attribution.
fn log_extension_responses(headers: &reqwest::header::HeaderMap, operation: &str) {
    let Some(value) = headers
        .get(EXTENSION_RESPONSES_HEADER)
        .and_then(|v| v.to_str().ok())
    else {
        return;
    };

    if value.contains("rejected") {
        error!(
            "CDP Bazaar extension REJECTED on {} (resource will not be indexed): {}",
            operation, value
        );
    } else {
        info!("CDP Bazaar extension accepted on {}: {}", operation, value);
    }
}

fn log_facilitator_bazaar_payload(
    operation: &str,
    x402_version: u8,
    payment: &serde_json::Value,
    requirement: &PaymentRequirement,
) {
    let payload_resource = payment
        .get("resource")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let payload_has_bazaar = payment
        .get("extensions")
        .and_then(|v| v.get("bazaar"))
        .is_some();
    let requirement_has_bazaar = requirement
        .extensions
        .as_ref()
        .and_then(|v| v.get("bazaar"))
        .is_some();
    let requirement_has_output_schema = requirement.output_schema.is_some();

    if payload_has_bazaar || requirement_has_bazaar || requirement_has_output_schema {
        info!(
            "CDP Bazaar {} payload: x402Version={}, resource={}, payload_has_bazaar={}, requirement_has_bazaar={}, requirement_has_output_schema={}",
            operation,
            x402_version,
            payload_resource,
            payload_has_bazaar,
            requirement_has_bazaar,
            requirement_has_output_schema
        );
    }
}

/// Verify payment proof via x402 facilitator
pub async fn verify_payment(
    http_client: &reqwest::Client,
    config: &Config,
    facilitator_url: &str,
    proof: &str,
    requirement: &PaymentRequirement,
) -> Result<VerifyResponse, PaymentError> {
    let payment = payment_payload_for_facilitator(decode_payment_payload(proof)?, requirement);
    let (x402_version, payment_requirements) = facilitator_payment_context(&payment, requirement);
    log_facilitator_bazaar_payload("verify", x402_version, &payment, requirement);

    let request = VerifyRequest {
        x402_version,
        payment_payload: payment,
        payment_requirements,
    };

    let endpoint = facilitator_endpoint_url(facilitator_url, "verify")?;
    let response = apply_facilitator_auth(
        http_client
            .post(endpoint.clone())
            .timeout(FACILITATOR_TIMEOUT)
            .json(&request),
        config,
        &endpoint,
        "POST",
    )?
    .send()
    .await
    .map_err(|e| PaymentError::FacilitatorError(e.to_string()))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        error!("Facilitator verify failed: {} - {}", status, body);
        return Err(PaymentError::FacilitatorError(format!(
            "HTTP {}: {}",
            status, body
        )));
    }

    // Read the discovery-acceptance header before `.json()` consumes the
    // response. Verify alone never triggers indexing (settle does), but the
    // facilitator already reports validation here, giving us the earliest
    // signal if our Bazaar schema is malformed.
    log_extension_responses(response.headers(), "verify");

    let verify_response: VerifyResponse = response
        .json()
        .await
        .map_err(|e| PaymentError::FacilitatorError(e.to_string()))?;

    if !verify_response.is_valid {
        return Err(PaymentError::VerificationFailed(
            verify_response.invalid_reason.unwrap_or_default(),
        ));
    }

    Ok(verify_response)
}

/// Settle payment via x402 facilitator
pub async fn settle_payment(
    http_client: &reqwest::Client,
    config: &Config,
    facilitator_url: &str,
    proof: &str,
    requirement: &PaymentRequirement,
) -> Result<SettleResponse, PaymentError> {
    let payment = payment_payload_for_facilitator(decode_payment_payload(proof)?, requirement);
    let (x402_version, payment_requirements) = facilitator_payment_context(&payment, requirement);
    log_facilitator_bazaar_payload("settle", x402_version, &payment, requirement);

    let request = SettleRequest {
        x402_version,
        payment_payload: payment,
        payment_requirements,
    };

    let endpoint = facilitator_endpoint_url(facilitator_url, "settle")?;
    let response = apply_facilitator_auth(
        http_client
            .post(endpoint.clone())
            .timeout(FACILITATOR_TIMEOUT)
            .json(&request),
        config,
        &endpoint,
        "POST",
    )?
    .send()
    .await
    .map_err(|e| PaymentError::FacilitatorError(e.to_string()))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        error!("Facilitator settle failed: {} - {}", status, body);
        return Err(PaymentError::SettlementFailed(format!(
            "HTTP {}: {}",
            status, body
        )));
    }

    // Settlement is the moment CDP catalogs the resource, so this header is the
    // authoritative signal that our Bazaar metadata was accepted for indexing.
    log_extension_responses(response.headers(), "settle");

    let settle_response: SettleResponse = response
        .json()
        .await
        .map_err(|e| PaymentError::FacilitatorError(e.to_string()))?;

    if !settle_response.success {
        return Err(PaymentError::SettlementFailed(
            settle_response.error_reason.clone().unwrap_or_default(),
        ));
    }

    info!(
        "Payment settled: tx={:?}, payer={:?}",
        settle_response.transaction, settle_response.payer
    );

    Ok(settle_response)
}

pub fn decode_payment_payload(proof: &str) -> Result<serde_json::Value, PaymentError> {
    serde_json::from_str(proof)
        .or_else(|_| {
            let decoded = BASE64
                .decode(proof)
                .map_err(|_| PaymentError::InvalidFormat)?;
            serde_json::from_slice(&decoded).map_err(|_| PaymentError::InvalidFormat)
        })
        .map_err(|_| PaymentError::InvalidFormat)
}

// Roughly 300 lines lived here: the LOCAL x402 backend.
//
// `local_x402_settlement_enabled()` read `X402_LOCAL_SETTLEMENT_ENABLED`, and
// when it was on for Base, `verify_payment_with_backend` / `settle_payment_-
// with_backend` bypassed the facilitator and went straight to the chain —
// verifying an EIP-3009 `transferWithAuthorization` the BUYER had signed
// (`verify_local_eip3009`, `eip3009_digest`, `parse_u256`) and then having
// `SettlementClient` broadcast it against the ApioskSettlement contract.
//
// That was the buyer-pays-on-chain rail, and it is gone. Apiosk keeps exactly
// one: the buyer's Apiosk balance is debited in the App's Supabase, and the
// treasury signs and pays the provider's own x402. Verification of an inbound
// proof is delegated to the pluggable x402 facilitator (`FACILITATOR_URL`) and
// nothing else, so `verify_payment` and `settle_payment` above are the whole
// story — the `_with_backend` wrappers that chose between them are gone too,
// and their callers now name the facilitator functions directly.

pub(crate) fn facilitator_endpoint_url(
    base_url: &str,
    operation: &str,
) -> Result<reqwest::Url, PaymentError> {
    let base = base_url.trim_end_matches('/');
    reqwest::Url::parse(&format!("{base}/{operation}"))
        .map_err(|e| PaymentError::FacilitatorError(format!("Invalid facilitator URL: {e}")))
}

pub(crate) fn apply_facilitator_auth(
    builder: reqwest::RequestBuilder,
    config: &Config,
    endpoint: &reqwest::Url,
    method: &str,
) -> Result<reqwest::RequestBuilder, PaymentError> {
    if let Some(token) = cdp_bearer_token(config, endpoint, method)? {
        Ok(builder.bearer_auth(token))
    } else {
        Ok(builder)
    }
}

fn cdp_bearer_token(
    config: &Config,
    endpoint: &reqwest::Url,
    method: &str,
) -> Result<Option<String>, PaymentError> {
    if !is_cdp_facilitator_url(config.facilitator_url.as_str()) {
        return Ok(None);
    }

    let api_key_id = config.cdp_api_key_id.as_deref().ok_or_else(|| {
        PaymentError::FacilitatorError(
            "CDP facilitator requires CDP_API_KEY_ID to be configured.".to_string(),
        )
    })?;
    let api_key_secret = config.cdp_api_key_secret.as_deref().ok_or_else(|| {
        PaymentError::FacilitatorError(
            "CDP facilitator requires CDP_API_KEY_SECRET to be configured.".to_string(),
        )
    })?;

    let signing_input = build_cdp_signing_input(endpoint, method)?;
    let now = Utc::now().timestamp().max(0) as usize;
    let alg = if normalize_private_key_secret(api_key_secret)
        .trim_start()
        .starts_with("-----BEGIN")
    {
        "ES256"
    } else {
        "EdDSA"
    };

    let header = CdpJwtHeader {
        alg,
        typ: "JWT",
        kid: api_key_id,
        nonce: uuid::Uuid::new_v4().simple().to_string(),
    };
    let claims = CdpJwtClaims {
        sub: api_key_id,
        iss: "cdp",
        aud: ["cdp_service"],
        nbf: now,
        exp: now + 120,
        uri: signing_input,
    };

    Ok(Some(sign_compact_jwt(api_key_secret, &header, &claims)?))
}

fn build_cdp_signing_input(endpoint: &reqwest::Url, method: &str) -> Result<String, PaymentError> {
    let mut host = endpoint
        .host_str()
        .ok_or_else(|| {
            PaymentError::FacilitatorError("Facilitator URL is missing a host.".to_string())
        })?
        .to_string();
    if let Some(port) = endpoint.port() {
        host.push(':');
        host.push_str(&port.to_string());
    }

    let mut path = endpoint.path().to_string();
    if let Some(query) = endpoint.query() {
        path.push('?');
        path.push_str(query);
    }

    Ok(format!("{} {}{}", method.to_uppercase(), host, path))
}

fn sign_compact_jwt(
    api_key_secret: &str,
    header: &CdpJwtHeader<'_>,
    claims: &CdpJwtClaims<'_>,
) -> Result<String, PaymentError> {
    let encoded_header = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(header).map_err(|e| PaymentError::FacilitatorError(e.to_string()))?,
    );
    let encoded_claims = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(claims).map_err(|e| PaymentError::FacilitatorError(e.to_string()))?,
    );
    let message = format!("{encoded_header}.{encoded_claims}");
    let signature = sign_cdp_message(api_key_secret, header.alg, message.as_bytes())?;
    Ok(format!("{message}.{}", URL_SAFE_NO_PAD.encode(signature)))
}

fn sign_cdp_message(
    api_key_secret: &str,
    alg: &str,
    message: &[u8],
) -> Result<Vec<u8>, PaymentError> {
    match alg {
        "EdDSA" => {
            let key_bytes = BASE64
                .decode(normalize_private_key_secret(api_key_secret).trim())
                .map_err(|e| {
                    PaymentError::FacilitatorError(format!("Invalid CDP Ed25519 key: {e}"))
                })?;
            if key_bytes.len() != 64 {
                return Err(PaymentError::FacilitatorError(
                    "Invalid CDP Ed25519 key length; expected 64 decoded bytes.".to_string(),
                ));
            }

            let seed = &key_bytes[..32];
            let public_key = &key_bytes[32..];
            let key_pair =
                Ed25519KeyPair::from_seed_and_public_key(seed, public_key).map_err(|_| {
                    PaymentError::FacilitatorError("Failed to parse CDP Ed25519 key.".to_string())
                })?;
            Ok(key_pair.sign(message).as_ref().to_vec())
        }
        "ES256" => {
            let pkcs8 = parse_ec_private_key_to_pkcs8(api_key_secret)?;
            let rng = ring::rand::SystemRandom::new();
            let key_pair = signature::EcdsaKeyPair::from_pkcs8(
                &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
                &pkcs8,
                &rng,
            )
            .map_err(|_| {
                PaymentError::FacilitatorError("Failed to parse CDP EC private key.".to_string())
            })?;
            let signature = key_pair.sign(&rng, message).map_err(|_| {
                PaymentError::FacilitatorError("Failed to sign CDP facilitator JWT.".to_string())
            })?;
            Ok(signature.as_ref().to_vec())
        }
        _ => Err(PaymentError::FacilitatorError(format!(
            "Unsupported CDP JWT algorithm: {alg}"
        ))),
    }
}

fn parse_ec_private_key_to_pkcs8(secret: &str) -> Result<Vec<u8>, PaymentError> {
    let normalized = normalize_private_key_secret(secret);
    let pem = pem::parse(normalized.as_bytes())
        .map_err(|e| PaymentError::FacilitatorError(format!("Invalid CDP EC key PEM: {e}")))?;

    match pem.tag.as_str() {
        "PRIVATE KEY" => Ok(pem.contents),
        "EC PRIVATE KEY" => wrap_sec1_ec_private_key_in_pkcs8(&pem.contents),
        other => Err(PaymentError::FacilitatorError(format!(
            "Unsupported CDP EC key type: {other}"
        ))),
    }
}

fn wrap_sec1_ec_private_key_in_pkcs8(sec1_der: &[u8]) -> Result<Vec<u8>, PaymentError> {
    let pkcs8 = ASN1Block::Sequence(
        0,
        vec![
            ASN1Block::Integer(0, BigInt::from(0)),
            ASN1Block::Sequence(
                0,
                vec![
                    ASN1Block::ObjectIdentifier(0, simple_asn1::oid!(1, 2, 840, 10045, 2, 1)),
                    ASN1Block::ObjectIdentifier(0, simple_asn1::oid!(1, 2, 840, 10045, 3, 1, 7)),
                ],
            ),
            ASN1Block::OctetString(0, sec1_der.to_vec()),
        ],
    );

    simple_asn1::to_der(&pkcs8).map_err(|e| {
        PaymentError::FacilitatorError(format!("Failed to encode CDP EC key as PKCS#8: {e}"))
    })
}

fn normalize_private_key_secret(secret: &str) -> String {
    secret.replace("\\n", "\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn test_network_token() -> NetworkToken {
        NetworkToken {
            id: Uuid::new_v4(),
            network_name: "base".to_string(),
            network_display_name: "Base".to_string(),
            chain_id: 8453,
            explorer_url: Some("https://basescan.org".to_string()),
            is_testnet: false,
            token_symbol: "USDC".to_string(),
            token_name: "USD Coin".to_string(),
            token_decimals: 6,
            contract_address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_string(),
        }
    }

    #[test]
    fn test_payment_requirement_serialization() {
        let nt = test_network_token();
        let req = PaymentRequirement::new(
            "0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb",
            &BigDecimal::from(1), // 1 USDC
            "https://api.example.com/data",
            "Test API",
            &nt,
        );

        assert_eq!(req.scheme, "exact");
        assert_eq!(req.network, "base");
        assert_eq!(req.max_amount_required, "1000000"); // 1 USDC in atomic units (6 decimals)
        assert_eq!(req.asset, "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
    }

    #[test]
    fn test_payment_requirement_dai_decimals() {
        let nt = NetworkToken {
            id: Uuid::new_v4(),
            network_name: "ethereum".to_string(),
            network_display_name: "Ethereum".to_string(),
            chain_id: 1,
            explorer_url: Some("https://etherscan.io".to_string()),
            is_testnet: false,
            token_symbol: "DAI".to_string(),
            token_name: "Dai Stablecoin".to_string(),
            token_decimals: 18,
            contract_address: "0x6B175474E89094C44Da98b954EescdeCB5eC86B".to_string(),
        };

        let req = PaymentRequirement::new(
            "0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb",
            &BigDecimal::from(1), // 1 DAI
            "https://api.example.com/data",
            "Test API",
            &nt,
        );

        assert_eq!(req.network, "ethereum");
        assert_eq!(req.max_amount_required, "1000000000000000000"); // 1 DAI in atomic units (18 decimals)
    }

    /// EVM stays EIP-712 and never emits feePayer.
    #[test]
    fn evm_requirement_keeps_eip712_domain_and_no_fee_payer() {
        let nt = test_network_token();
        let req = PaymentRequirement::new(
            "0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb",
            &"0.02".parse::<BigDecimal>().unwrap(),
            "https://api.example.com/data",
            "Test",
            &nt,
        );
        assert_eq!(req.extra["name"], "USD Coin");
        assert_eq!(req.extra["version"], "2");
        assert!(req.extra.get("feePayer").is_none());
    }

    #[test]
    fn test_extract_payment_proof() {
        let mut headers = HeaderMap::new();
        headers.insert("x-payment", HeaderValue::from_static("test_proof_123"));

        let proof = extract_payment_proof(&headers);
        assert_eq!(proof, Some("test_proof_123".to_string()));
    }

    #[test]
    fn test_extract_payment_proof_missing() {
        let headers = HeaderMap::new();
        let proof = extract_payment_proof(&headers);
        assert_eq!(proof, None);
    }

    #[test]
    fn hosted_wallet_checkout_requires_mainnet_capable_facilitator() {
        let config = Config {
            database_url: "postgres://example".to_string(),
            quote_signing_secret: "test-quote-secret".to_string(),
            ask_read_timeout_ms: crate::config::DEFAULT_ASK_READ_TIMEOUT_MS,
            ask_rerank_timeout_ms: crate::config::DEFAULT_ASK_RERANK_TIMEOUT_MS,
            ask_rerank_attempts: crate::config::DEFAULT_ASK_RERANK_ATTEMPTS,
            ask_engine_v2: false,
            debug_endpoints_token: None,
            parse_key: None,
            dashboard_url: "https://dashboard.apiosk.com".to_string(),
            buyer_portal_url: "https://buyer.apiosk.com".to_string(),
            mcp_proxy_upstream_urls: vec!["https://mcp.apiosk.com/mcp".to_string()],
            payment_providers: vec![],
            supabase_url: None,
            supabase_api_key: None,
            supabase_service_role_key: None,
            platform_wallet: "0x0000000000000000000000000000000000000001".to_string(),
            default_commission_rate: 0.03,
            facilitator_url: "https://x402.org/facilitator".to_string(),
            cdp_api_key_id: None,
            cdp_api_key_secret: None,
            ga_measurement_id: None,
            ga_api_secret: None,
            require_agent_connect_token: false,
            connect_hmac_secret: None,
            app_service_key: None,
            treasury_connect_token_ref: None,
            connect_portal_url: "https://provider.apiosk.com".to_string(),
            connect_settlement_asset: None,
            connect_network: "base".to_string(),
            federated_ref_param: "ref".to_string(),
            federated_ref_marker: "apiosk".to_string(),
            federated_ref_inject_url: false,
            federated_self_serve_enabled: false,
            telemetry: crate::telemetry::TelemetrySettings::default(),
            openrouter_api_key: None,
            openrouter_model: crate::query_plan::DEFAULT_OPENROUTER_MODEL.to_string(),
            openrouter_referer: "https://apiosk.com".to_string(),
        };

        let reason = hosted_x402_wallet_checkout_unavailable_reason(&config, &test_network_token());

        assert!(reason.is_some());
        assert!(reason
            .unwrap()
            .contains("testnet-only x402.org facilitator"));
    }

    #[test]
    fn hosted_wallet_checkout_allows_testnet_x402_org() {
        let config = Config {
            database_url: "postgres://example".to_string(),
            quote_signing_secret: "test-quote-secret".to_string(),
            ask_read_timeout_ms: crate::config::DEFAULT_ASK_READ_TIMEOUT_MS,
            ask_rerank_timeout_ms: crate::config::DEFAULT_ASK_RERANK_TIMEOUT_MS,
            ask_rerank_attempts: crate::config::DEFAULT_ASK_RERANK_ATTEMPTS,
            ask_engine_v2: false,
            debug_endpoints_token: None,
            parse_key: None,
            dashboard_url: "https://dashboard.apiosk.com".to_string(),
            buyer_portal_url: "https://buyer.apiosk.com".to_string(),
            mcp_proxy_upstream_urls: vec!["https://mcp.apiosk.com/mcp".to_string()],
            payment_providers: vec![],
            supabase_url: None,
            supabase_api_key: None,
            supabase_service_role_key: None,
            platform_wallet: "0x0000000000000000000000000000000000000001".to_string(),
            default_commission_rate: 0.03,
            facilitator_url: "https://x402.org/facilitator".to_string(),
            cdp_api_key_id: None,
            cdp_api_key_secret: None,
            ga_measurement_id: None,
            ga_api_secret: None,
            require_agent_connect_token: false,
            connect_hmac_secret: None,
            app_service_key: None,
            treasury_connect_token_ref: None,
            connect_portal_url: "https://provider.apiosk.com".to_string(),
            connect_settlement_asset: None,
            connect_network: "base".to_string(),
            federated_ref_param: "ref".to_string(),
            federated_ref_marker: "apiosk".to_string(),
            federated_ref_inject_url: false,
            federated_self_serve_enabled: false,
            telemetry: crate::telemetry::TelemetrySettings::default(),
            openrouter_api_key: None,
            openrouter_model: crate::query_plan::DEFAULT_OPENROUTER_MODEL.to_string(),
            openrouter_referer: "https://apiosk.com".to_string(),
        };
        let mut testnet = test_network_token();
        testnet.is_testnet = true;
        testnet.network_name = "base-sepolia".to_string();
        testnet.chain_id = 84532;

        assert!(hosted_x402_wallet_checkout_unavailable_reason(&config, &testnet).is_none());
    }

    /// Baseline `Config` for pure facilitator/checkout tests: CDP mainnet
    /// facilitator with both keys present. Mutate the facilitator + CDP-key
    /// fields per test; everything else is irrelevant to the functions here.
    fn base_test_config() -> Config {
        Config {
            database_url: "postgres://example".to_string(),
            quote_signing_secret: "test-quote-secret".to_string(),
            ask_read_timeout_ms: crate::config::DEFAULT_ASK_READ_TIMEOUT_MS,
            ask_rerank_timeout_ms: crate::config::DEFAULT_ASK_RERANK_TIMEOUT_MS,
            ask_rerank_attempts: crate::config::DEFAULT_ASK_RERANK_ATTEMPTS,
            ask_engine_v2: false,
            debug_endpoints_token: None,
            parse_key: None,
            dashboard_url: "https://dashboard.apiosk.com".to_string(),
            buyer_portal_url: "https://buyer.apiosk.com".to_string(),
            mcp_proxy_upstream_urls: vec!["https://mcp.apiosk.com/mcp".to_string()],
            payment_providers: vec![],
            supabase_url: None,
            supabase_api_key: None,
            supabase_service_role_key: None,
            platform_wallet: "0x0000000000000000000000000000000000000001".to_string(),
            default_commission_rate: 0.02,
            facilitator_url: "https://api.cdp.coinbase.com/platform/v2/x402".to_string(),
            cdp_api_key_id: Some("key-id".to_string()),
            cdp_api_key_secret: Some("key-secret".to_string()),
            ga_measurement_id: None,
            ga_api_secret: None,
            require_agent_connect_token: false,
            connect_hmac_secret: None,
            app_service_key: None,
            treasury_connect_token_ref: None,
            connect_portal_url: "https://provider.apiosk.com".to_string(),
            connect_settlement_asset: None,
            connect_network: "base".to_string(),
            federated_ref_param: "ref".to_string(),
            federated_ref_marker: "apiosk".to_string(),
            federated_ref_inject_url: false,
            federated_self_serve_enabled: false,
            telemetry: crate::telemetry::TelemetrySettings::default(),
            openrouter_api_key: None,
            openrouter_model: crate::query_plan::DEFAULT_OPENROUTER_MODEL.to_string(),
            openrouter_referer: "https://apiosk.com".to_string(),
        }
    }

    #[test]
    fn is_cdp_facilitator_url_matches_only_the_cdp_host() {
        assert!(is_cdp_facilitator_url(
            "https://api.cdp.coinbase.com/platform/v2/x402"
        ));
        // Host comparison is case-insensitive.
        assert!(is_cdp_facilitator_url(
            "https://API.CDP.COINBASE.COM/platform/v2/x402"
        ));
        // The testnet facilitator is not CDP.
        assert!(!is_cdp_facilitator_url("https://www.x402.org/facilitator"));
        // A look-alike attacker host must never be accepted as CDP.
        assert!(!is_cdp_facilitator_url(
            "https://api.cdp.coinbase.com.attacker.example/x402"
        ));
        // Unparseable input is rejected, not panicked.
        assert!(!is_cdp_facilitator_url("not a url"));
    }

    #[test]
    fn is_testnet_only_x402_facilitator_matches_x402_org_hosts() {
        assert!(is_testnet_only_x402_facilitator(
            "https://x402.org/facilitator"
        ));
        assert!(is_testnet_only_x402_facilitator(
            "https://www.x402.org/facilitator"
        ));
        // Case-insensitive.
        assert!(is_testnet_only_x402_facilitator(
            "https://WWW.X402.ORG/facilitator"
        ));
        assert!(!is_testnet_only_x402_facilitator(
            "https://api.cdp.coinbase.com/platform/v2/x402"
        ));
        assert!(!is_testnet_only_x402_facilitator("garbage"));
    }

    #[test]
    fn hosted_checkout_blocked_when_cdp_keys_missing_on_mainnet() {
        // Mainnet token + CDP facilitator but no CDP credentials => verify/settle
        // would fail auth, so hosted wallet checkout must report as unavailable.
        let mut config = base_test_config();
        config.cdp_api_key_id = None;
        let reason = hosted_x402_wallet_checkout_unavailable_reason(&config, &test_network_token());
        assert!(reason.is_some());
        assert!(reason.unwrap().contains("CDP_API_KEY_ID"));

        let mut config = base_test_config();
        config.cdp_api_key_secret = None;
        assert!(
            hosted_x402_wallet_checkout_unavailable_reason(&config, &test_network_token())
                .is_some()
        );
    }

    #[test]
    fn hosted_checkout_allowed_when_cdp_fully_configured() {
        // Both keys present + CDP facilitator + mainnet token => available.
        let config = base_test_config();
        assert!(
            hosted_x402_wallet_checkout_unavailable_reason(&config, &test_network_token())
                .is_none()
        );
    }

    #[test]
    fn extract_payment_proof_handles_empty_and_non_utf8_headers() {
        // A present-but-empty header still yields Some("") — the caller treats it
        // as an invalid proof downstream, not as "no header at all".
        let mut headers = HeaderMap::new();
        headers.insert(PAYMENT_HEADER, HeaderValue::from_static(""));
        assert_eq!(extract_payment_proof(&headers), Some(String::new()));

        // A non-UTF-8 header value is dropped rather than panicking.
        let mut headers = HeaderMap::new();
        headers.insert(
            PAYMENT_HEADER,
            HeaderValue::from_bytes(&[0xFF, 0xFE]).unwrap(),
        );
        assert_eq!(extract_payment_proof(&headers), None);
    }

    #[test]
    fn decode_payment_payload_rejects_url_safe_base64_alphabet() {
        // x402 proofs are standard base64 (or raw JSON). A payload encoded with
        // the URL-safe alphabet (`-`/`_`) must be rejected, not silently decoded.
        let json = r#"{"scheme":"exact","tag":"ÿÿÿ"}"#;
        let standard = BASE64.encode(json.as_bytes());
        let url_safe = URL_SAFE_NO_PAD.encode(json.as_bytes());
        // Self-check: this payload genuinely diverges between the two alphabets,
        // otherwise the rejection assertion below would be vacuous.
        assert_ne!(standard.trim_end_matches('='), url_safe);
        assert!(super::decode_payment_payload(&standard).is_ok());
        assert!(matches!(
            super::decode_payment_payload(&url_safe),
            Err(PaymentError::InvalidFormat)
        ));
    }

    #[test]
    fn payment_requirement_truncates_sub_atomic_amounts() {
        let nt = test_network_token(); // USDC, 6 decimals
                                       // 0.0000001 USDC is below one atomic unit (1e-6) => truncates to "0".
        let req = PaymentRequirement::new(
            "0xWallet",
            &"0.0000001".parse::<BigDecimal>().unwrap(),
            "https://api.example.com/data",
            "Test",
            &nt,
        );
        assert_eq!(req.max_amount_required, "0");
        // Exact-zero amount.
        let zero = PaymentRequirement::new(
            "0xWallet",
            &BigDecimal::from(0),
            "https://api.example.com/data",
            "Test",
            &nt,
        );
        assert_eq!(zero.max_amount_required, "0");
        // Scheme/timeout defaults and absence of optional discovery fields.
        assert_eq!(req.scheme, "exact");
        assert_eq!(req.max_timeout_seconds, 300);
        assert!(req.extensions.is_none());
        assert!(req.output_schema.is_none());
    }

    #[test]
    fn facilitator_endpoint_url_trims_trailing_slash_and_rejects_garbage() {
        let url = super::facilitator_endpoint_url("https://api.example.com/", "verify").unwrap();
        assert_eq!(url.as_str(), "https://api.example.com/verify");
        let url = super::facilitator_endpoint_url("https://api.example.com", "settle").unwrap();
        assert_eq!(url.as_str(), "https://api.example.com/settle");
        assert!(matches!(
            super::facilitator_endpoint_url("not a url", "verify"),
            Err(PaymentError::FacilitatorError(_))
        ));
    }

    #[tokio::test]
    async fn verify_payment_short_circuits_on_undecodable_proof() {
        // An un-decodable proof is rejected BEFORE any facilitator round-trip, so
        // this runs with a throwaway client and never touches the network.
        let config = base_test_config();
        let req = sample_requirement();
        let client = reqwest::Client::new();
        let result = verify_payment(
            &client,
            &config,
            "https://facilitator.invalid/",
            "@@ not decodable @@",
            &req,
        )
        .await;
        assert!(matches!(result, Err(PaymentError::InvalidFormat)));
    }

    #[tokio::test]
    async fn settle_payment_short_circuits_on_undecodable_proof() {
        let config = base_test_config();
        let req = sample_requirement();
        let client = reqwest::Client::new();
        let result = settle_payment(
            &client,
            &config,
            "https://facilitator.invalid/",
            "@@ not decodable @@",
            &req,
        )
        .await;
        assert!(matches!(result, Err(PaymentError::InvalidFormat)));
    }

    // -----------------------------------------------------------------------
    // x402 proof / facilitator contract tests
    // -----------------------------------------------------------------------
    // These tests pin down the wire format the gateway sends to the
    // facilitator and the shape it accepts back. They protect against
    // accidental drift from the x402 v1 spec (camelCase keys, x402Version=1,
    // base64-or-raw payload acceptance) when refactoring serialization
    // attributes.

    #[test]
    fn decode_payment_payload_accepts_raw_json() {
        let raw = r#"{"scheme":"exact","payload":{"signature":"0xdead"}}"#;
        let parsed = super::decode_payment_payload(raw).expect("raw JSON should decode");
        assert_eq!(parsed.get("scheme").and_then(|v| v.as_str()), Some("exact"));
    }

    #[test]
    fn decode_payment_payload_accepts_base64_json() {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine as _;
        let json = r#"{"scheme":"exact","x402Version":1}"#;
        let encoded = STANDARD.encode(json.as_bytes());
        let parsed = super::decode_payment_payload(&encoded).expect("base64 JSON should decode");
        assert_eq!(parsed.get("x402Version").and_then(|v| v.as_u64()), Some(1));
    }

    #[test]
    fn decode_payment_payload_rejects_garbage() {
        let result = super::decode_payment_payload("not json and not base64");
        assert!(matches!(result, Err(PaymentError::InvalidFormat)));
    }

    #[test]
    fn verify_request_serializes_to_x402_camel_case() {
        // A v1 payload keeps the exact byte-for-byte v1 path: x402Version 1 and
        // v1-shaped paymentRequirements (maxAmountRequired, mimeType, plain
        // network). This pins the proven revenue path against drift.
        let requirement = sample_requirement();
        let (version, requirements) = super::facilitator_payment_context(
            &serde_json::json!({"x402Version": 1, "scheme": "exact", "network": "base"}),
            &requirement,
        );
        let req = super::VerifyRequest {
            x402_version: version,
            payment_payload: serde_json::json!({"scheme":"exact"}),
            payment_requirements: requirements,
        };
        let value = serde_json::to_value(&req).unwrap();
        assert!(
            value.get("paymentPayload").is_some(),
            "expected paymentPayload key"
        );
        assert!(
            value.get("paymentRequirements").is_some(),
            "expected paymentRequirements key"
        );
        assert_eq!(value["x402Version"], 1);
        let req_field = &value["paymentRequirements"];
        assert!(req_field.get("scheme").is_some());
        assert!(req_field.get("payTo").is_some(), "payTo must be camelCase");
        assert!(req_field.get("maxAmountRequired").is_some());
        assert!(req_field.get("mimeType").is_some());
        assert_eq!(req_field["network"], "base");
    }

    #[test]
    fn verify_request_v2_payload_uses_v2_requirements() {
        // A v2 payload (x402Version 2) makes verify/settle echo v2: the
        // requirements carry `amount` + CAIP-2 `network` and drop the v1-only
        // per-item resource/mimeType/maxAmountRequired fields.
        let requirement = sample_requirement();
        let (version, requirements) = super::facilitator_payment_context(
            &serde_json::json!({
                "x402Version": 2,
                "accepted": { "network": "eip155:8453" }
            }),
            &requirement,
        );
        assert_eq!(version, 2);
        assert_eq!(requirements["amount"], "1000000"); // 1 USDC, 6 decimals
        assert_eq!(requirements["network"], "eip155:8453");
        assert!(requirements.get("maxAmountRequired").is_none());
        assert!(requirements.get("mimeType").is_none());
        assert!(requirements.get("resource").is_none());
        assert!(requirements.get("payTo").is_some());
        assert!(requirements.get("extra").is_some());
    }

    #[test]
    fn settle_request_serializes_to_x402_camel_case() {
        let requirement = sample_requirement();
        let (version, requirements) = super::facilitator_payment_context(
            &serde_json::json!({"x402Version": 1, "scheme": "exact"}),
            &requirement,
        );
        let req = super::SettleRequest {
            x402_version: version,
            payment_payload: serde_json::json!({"scheme":"exact"}),
            payment_requirements: requirements,
        };
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(value["x402Version"], 1);
        assert!(value.get("paymentPayload").is_some());
        assert!(value.get("paymentRequirements").is_some());
    }

    #[test]
    fn facilitator_payment_payload_v1_passes_through_untouched() {
        // `PaymentPayloadV1` is exactly {x402Version, scheme, network,
        // payload} — "NO extensions field - V1 doesn't support extensions",
        // and no `resource` field either. The Bazaar catalogs v1 settles
        // from paymentRequirements.outputSchema, so nothing may be injected
        // into the v1 payload (regression: 2026-07-10 injections risked the
        // discovery pipeline's strict v1 parse rejecting the settle).
        let requirement = sample_requirement();
        let original = serde_json::json!({
            "x402Version": 1,
            "scheme": "exact",
            "network": "base",
            "payload": { "authorization": {} }
        });
        let payload = super::payment_payload_for_facilitator(original.clone(), &requirement);

        assert_eq!(payload, original, "v1 payload must not be modified");
    }

    #[test]
    fn facilitator_payment_payload_v2_includes_resource_for_bazaar_indexing() {
        // On v2, `resource` legitimately lives on the payload; fill it from
        // the authoritative requirement when the client omitted it.
        let requirement = sample_requirement();
        let payload = super::payment_payload_for_facilitator(
            serde_json::json!({
                "x402Version": 2,
                "scheme": "exact",
                "network": "eip155:8453",
                "payload": { "authorization": {} }
            }),
            &requirement,
        );

        assert_eq!(payload["resource"], requirement.resource);
    }

    #[test]
    fn facilitator_payment_payload_preserves_existing_resource() {
        let requirement = sample_requirement();
        let payload = super::payment_payload_for_facilitator(
            serde_json::json!({
                "x402Version": 1,
                "resource": "https://buyer.example/resource"
            }),
            &requirement,
        );

        assert_eq!(payload["resource"], "https://buyer.example/resource");
    }

    #[test]
    fn facilitator_payment_payload_includes_bazaar_extensions_for_v2_indexing() {
        // CDP Bazaar v2 discovery reads `paymentPayload.extensions.bazaar`.
        // Older clients only send the signed authorization fields, so the
        // gateway forwards the authoritative Bazaar extension from the matching
        // payment requirement.
        let bazaar = BazaarMetadata {
            description: Some("Validate a BIC code".to_string()),
            query_params: Some(serde_json::json!({
                "bic": { "type": "string", "required": true }
            })),
            response_example: Some(serde_json::json!({ "valid": true })),
            method: "GET".to_string(),
            output_mime: "json",
            ..Default::default()
        };
        let requirement = sample_requirement().with_bazaar_extension(&bazaar);
        let payload = super::payment_payload_for_facilitator(
            serde_json::json!({
                "x402Version": 2,
                "scheme": "exact",
                "network": "eip155:8453",
                "payload": { "authorization": {} }
            }),
            &requirement,
        );

        assert_eq!(
            payload["extensions"]["bazaar"],
            requirement.extensions.as_ref().unwrap()["bazaar"]
        );
    }

    #[test]
    fn facilitator_payment_payload_preserves_existing_extensions() {
        let bazaar = BazaarMetadata {
            response_example: Some(serde_json::json!({ "ok": true })),
            method: "POST".to_string(),
            output_mime: "json",
            ..Default::default()
        };
        let requirement = sample_requirement().with_bazaar_extension(&bazaar);
        let payload = super::payment_payload_for_facilitator(
            serde_json::json!({
                "x402Version": 1,
                "extensions": {
                    "bazaar": { "client": "kept" },
                    "other": { "client": true }
                }
            }),
            &requirement,
        );

        assert_eq!(payload["extensions"]["bazaar"]["client"], "kept");
        assert_eq!(payload["extensions"]["other"]["client"], true);
    }

    #[test]
    fn facilitator_context_defaults_missing_version_to_v1() {
        // A payload with no x402Version (some minimal v1 clients) must default
        // to the v1 path, never the v2 shape — otherwise an in-flight v1 payer
        // would be cross-checked against v2 requirements and rejected.
        let requirement = sample_requirement();
        let (version, requirements) = super::facilitator_payment_context(
            &serde_json::json!({"scheme": "exact"}),
            &requirement,
        );
        assert_eq!(version, 1);
        assert!(requirements.get("maxAmountRequired").is_some());
    }

    #[test]
    fn payment_requirement_extra_uses_eip712_domain_keys() {
        // The facilitator validates EIP-712 signatures against the domain
        // metadata in `extra`. If we ever rename `name`/`version` here,
        // every existing in-flight signed payment becomes invalid; this
        // test pins the field names.
        let v = evm_requirement_extra("USD Coin");
        assert_eq!(v["name"], "USD Coin");
        assert_eq!(v["version"], "2");
    }

    // -----------------------------------------------------------------------
    // Bazaar enrichment tests
    // -----------------------------------------------------------------------
    // These pin the OUTER 402 body shape that Coinbase CDP Bazaar (and other
    // x402 indexers) scrape for discovery. The PaymentRequirement itself
    // stays untouched on the wire, so /verify and /settle continue to see
    // exactly the 10 camelCase fields they always did.

    /// Drain a Body into a JSON value. Used to inspect the outer 402 body.
    async fn body_to_json(resp: Response<Body>) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body should be small enough to read in test");
        serde_json::from_slice(&bytes).expect("402 body should be JSON")
    }

    fn sample_requirement() -> PaymentRequirement {
        PaymentRequirement::new(
            "0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb",
            &BigDecimal::from(1),
            "https://api.example.com/data",
            "Test API",
            &test_network_token(),
        )
    }

    #[tokio::test]
    async fn bazaar_enriched_402_has_top_level_description_and_extensions() {
        let bazaar = BazaarMetadata {
            description: Some("Forecast weather for a city".to_string()),
            input_schema: Some(serde_json::json!({ "type": "object" })),
            query_params: None,
            response_example: Some(serde_json::json!({ "temp": 21 })),
            response_schema: Some(serde_json::json!({ "type": "object" })),
            method: "POST".to_string(),
            output_mime: "json",
            ..Default::default()
        };
        let resp = sample_requirement().into_response_with_bazaar(bazaar);
        assert_eq!(resp.status(), StatusCode::PAYMENT_REQUIRED);
        assert_eq!(
            resp.headers().get("Content-Type").unwrap(),
            "application/json"
        );
        assert_eq!(resp.headers().get("X-Payment-Protocol").unwrap(), "x402");

        // v2 canonical challenge transport: the base64 PAYMENT-REQUIRED header
        // decodes to the same x402Version/accepts the body carries.
        let hdr = resp
            .headers()
            .get(super::PAYMENT_REQUIRED_HEADER)
            .expect("PAYMENT-REQUIRED header must be present")
            .to_str()
            .unwrap()
            .to_string();
        let decoded: serde_json::Value =
            serde_json::from_slice(&BASE64.decode(&hdr).expect("base64")).expect("json");
        assert_eq!(decoded["x402Version"], 2);
        assert_eq!(decoded["accepts"][0]["amount"], "1000000");
        assert_eq!(decoded["accepts"][0]["network"], "eip155:8453");
        assert!(decoded["extensions"]["bazaar"].is_object());

        let body = body_to_json(resp).await;
        // BODY is x402 v1 — the dialect every released Coinbase x402-fetch
        // SDK (0.6.x–1.2.x) parses. v2 in the body breaks all SDK payers
        // (2026-07-06 incident).
        assert_eq!(body["x402Version"], 1);
        assert_eq!(body["error"], "Payment required");
        assert_eq!(body["description"], "Forecast weather for a city");

        // `accepts` carries the full v1 PaymentRequirements shape per entry:
        // bare network name, maxAmountRequired, per-item resource/mimeType.
        let accepts = body["accepts"]
            .as_array()
            .expect("accepts must be an array");
        assert_eq!(accepts.len(), 1);
        let req = &accepts[0];
        assert!(req.get("payTo").is_some(), "payTo must stay camelCase");
        assert_eq!(req["maxAmountRequired"], "1000000");
        assert_eq!(req["network"], "base");
        assert_eq!(req["resource"], "https://api.example.com/data");
        assert_eq!(req["mimeType"], "application/json");
        assert!(
            req.get("amount").is_none(),
            "v2 `amount` lives in the PAYMENT-REQUIRED header, not the v1 body"
        );
        // The v2 top-level resource object stays for body-reading indexers.
        assert_eq!(body["resource"]["mimeType"], "application/json");

        // Bazaar block lives only at the outer level under `extensions`.
        let bazaar = &body["extensions"]["bazaar"];
        assert_eq!(bazaar["info"]["input"]["type"], "http");
        assert_eq!(bazaar["info"]["input"]["method"], "POST");
        assert_eq!(bazaar["info"]["output"]["type"], "json");
        assert_eq!(
            bazaar["schema"]["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
        assert!(bazaar["schema"]["properties"]["input"].is_object());
        assert!(bazaar["schema"]["properties"]["output"].is_object());
        assert_eq!(
            bazaar["schema"]["required"]
                .as_array()
                .and_then(|a| a.iter().any(|v| v == "input").then_some(())),
            Some(())
        );
    }

    /// End-to-end DUAL-STACK contract test. The 402 challenge must satisfy
    /// two different consumers at once:
    ///
    ///   HEADER (`PAYMENT-REQUIRED`): the x402 v2 contract x402scan's zod
    ///   validation applies to classify the resource as v2 — a change that
    ///   breaks it silently de-registers every endpoint from that index.
    ///
    ///   BODY: the x402 v1 contract Coinbase's `x402-fetch` SDKs
    ///   (0.6.x–1.2.x) enforce via `PaymentRequirementsSchema.parse()` on
    ///   EVERY accepts entry — bare network enum, maxAmountRequired,
    ///   per-item resource/description/mimeType. One non-conforming entry
    ///   throws and makes the route unpayable for every SDK client
    ///   (live incident 2026-07-06).
    ///
    /// Exercises the multi-network (`accepts[1..]`) path.
    #[tokio::test]
    async fn challenge_satisfies_x402scan_v2_contract() {
        // A second payable network so `accepts` carries more than one entry.
        let polygon = NetworkToken {
            id: Uuid::new_v4(),
            network_name: "polygon".to_string(),
            network_display_name: "Polygon".to_string(),
            chain_id: 137,
            explorer_url: None,
            is_testnet: false,
            token_symbol: "USDC".to_string(),
            token_name: "USD Coin".to_string(),
            token_decimals: 6,
            contract_address: "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359".to_string(),
        };
        let alternate = PaymentRequirement::new(
            "0xB0bAlternatePayout00000000000000000000000",
            &BigDecimal::from(1),
            "https://api.example.com/data",
            "Test API",
            &polygon,
        );
        let bazaar = BazaarMetadata {
            description: Some("Realtime quote".to_string()),
            method: "GET".to_string(),
            output_mime: "json",
            service_name: Some("Example Quotes".to_string()),
            tags: vec!["finance".to_string()],
            ..Default::default()
        };
        let resp =
            sample_requirement().into_response_with_bazaar_and_alternates(bazaar, vec![alternate]);

        // ---- HEADER: full x402 v2 contract (x402scan) --------------------
        let header_json: serde_json::Value = serde_json::from_slice(
            &BASE64
                .decode(
                    resp.headers()
                        .get(super::PAYMENT_REQUIRED_HEADER)
                        .expect("PAYMENT-REQUIRED header")
                        .to_str()
                        .unwrap(),
                )
                .expect("standard base64"),
        )
        .expect("header is JSON");
        let body = body_to_json(resp).await;

        // 1. x402scan classifies on x402Version === 2 (a JSON NUMBER).
        assert_eq!(header_json["x402Version"], serde_json::json!(2));
        assert!(header_json["x402Version"].is_number());
        // 2. Top-level `resource` object with a url (x402scan display).
        assert!(header_json["resource"].is_object());
        assert_eq!(
            header_json["resource"]["url"],
            "https://api.example.com/data"
        );
        // 3. Non-empty accepts[]; both networks present as CAIP-2.
        let hdr_accepts = header_json["accepts"].as_array().expect("accepts array");
        assert_eq!(hdr_accepts.len(), 2);
        assert_eq!(hdr_accepts[0]["network"], "eip155:8453");
        assert_eq!(hdr_accepts[1]["network"], "eip155:137");
        // 4. Every entry is the exact v2 PaymentRequirements shape — and
        //    NOTHING else (no v1 leftovers, no stray extensions that a
        //    strict zod parser would reject).
        for entry in hdr_accepts {
            let obj = entry.as_object().expect("entry is object");
            assert_eq!(obj["scheme"], "exact");
            assert!(obj["amount"].is_string(), "amount must be a string");
            assert!(obj["network"].as_str().unwrap().contains(':'), "CAIP-2");
            assert!(obj.get("asset").is_some());
            assert!(obj.get("payTo").is_some());
            assert!(obj["maxTimeoutSeconds"].is_number());
            assert!(obj.get("extra").is_some());
            assert!(
                obj.get("maxAmountRequired").is_none(),
                "v2 renamed maxAmountRequired -> amount"
            );
            assert!(
                obj.get("extensions").is_none(),
                "v2 requirements carry NO per-entry extensions"
            );
            assert!(
                obj.get("outputSchema").is_none(),
                "v2 dropped the v1 outputSchema discovery field"
            );
            assert!(
                obj.get("resource").is_none() && obj.get("mimeType").is_none(),
                "resource descriptor is hoisted to the top level in v2"
            );
        }

        // ---- BODY: full x402 v1 contract (Coinbase x402-fetch SDKs) ------
        // Mirrors `PaymentRequirementsSchema` from x402@0.6.6/1.2.0: every
        // field it marks required must be present, `network` must be a bare
        // enum name, and `accepts.map(parse)` means ONE bad entry breaks all.
        assert_eq!(body["x402Version"], serde_json::json!(1));
        let accepts = body["accepts"].as_array().expect("accepts array");
        assert_eq!(accepts.len(), 2);
        assert_eq!(accepts[0]["network"], "base");
        assert_eq!(accepts[1]["network"], "polygon");
        for entry in accepts {
            let obj = entry.as_object().expect("entry is object");
            assert_eq!(obj["scheme"], "exact");
            assert!(
                obj["maxAmountRequired"].is_string(),
                "maxAmountRequired required by the SDK zod schema"
            );
            assert!(
                obj["resource"].as_str().unwrap().starts_with("https://"),
                "per-entry resource must be a URL string in v1"
            );
            assert!(obj["description"].is_string(), "description required");
            assert!(obj["mimeType"].is_string(), "mimeType required");
            assert!(obj.get("payTo").is_some());
            assert!(obj["maxTimeoutSeconds"].is_number());
            assert!(obj.get("asset").is_some());
            assert!(
                !obj["network"].as_str().unwrap().contains(':'),
                "v1 network must be a bare name — CAIP-2 fails the SDK enum"
            );
            assert!(
                obj.get("amount").is_none(),
                "v2 `amount` must not leak into v1 body entries"
            );
        }
        // The v2 resource object stays at the body top level for indexers
        // that read the body (harmless to v1 SDKs: zod strips unknown keys).
        assert!(body["resource"].is_object());

        // 5. Discovery lives in the top-level `extensions.bazaar.info` in
        //    both transports. CDP's v2 validator reads the canonical header;
        //    legacy SDKs and human tooling can still read it from the body.
        assert!(header_json["extensions"]["bazaar"]["info"]["input"].is_object());
        assert!(body["extensions"]["bazaar"]["info"]["input"].is_object());
    }

    /// `arbitrum` is allowlisted for payouts but is NOT in the Coinbase x402
    /// SDK `NetworkSchema` enum (0.6.x–1.2.x). Because those SDKs run
    /// `accepts.map(PaymentRequirementsSchema.parse)`, a single arbitrum
    /// entry in the v1 BODY would throw and make the whole 402 unpayable for
    /// every SDK client. It must therefore be offered ONLY via the v2
    /// PAYMENT-REQUIRED header.
    #[tokio::test]
    async fn arbitrum_alternate_is_header_only() {
        let arbitrum = NetworkToken {
            id: Uuid::new_v4(),
            network_name: "arbitrum".to_string(),
            network_display_name: "Arbitrum One".to_string(),
            chain_id: 42161,
            explorer_url: None,
            is_testnet: false,
            token_symbol: "USDC".to_string(),
            token_name: "USD Coin".to_string(),
            token_decimals: 6,
            contract_address: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831".to_string(),
        };
        let alternate = PaymentRequirement::new(
            "0xB0bAlternatePayout00000000000000000000000",
            &BigDecimal::from(1),
            "https://api.example.com/data",
            "Test API",
            &arbitrum,
        );
        let resp = sample_requirement()
            .into_response_with_bazaar_and_alternates(BazaarMetadata::default(), vec![alternate]);

        let header_json: serde_json::Value = serde_json::from_slice(
            &BASE64
                .decode(
                    resp.headers()
                        .get(super::PAYMENT_REQUIRED_HEADER)
                        .expect("PAYMENT-REQUIRED header")
                        .to_str()
                        .unwrap(),
                )
                .expect("standard base64"),
        )
        .expect("header is JSON");
        let body = body_to_json(resp).await;

        // Header (v2): both networks offered.
        let hdr_accepts = header_json["accepts"].as_array().unwrap();
        assert_eq!(hdr_accepts.len(), 2);
        assert_eq!(hdr_accepts[0]["network"], "eip155:8453");
        assert_eq!(hdr_accepts[1]["network"], "eip155:42161");

        // Body (v1): arbitrum filtered out; Base offer remains payable.
        let accepts = body["accepts"].as_array().unwrap();
        assert_eq!(accepts.len(), 1);
        assert_eq!(accepts[0]["network"], "base");
    }

    #[tokio::test]
    async fn bazaar_get_uses_query_params_not_body_schema() {
        let bazaar = BazaarMetadata {
            description: Some("List forecasts".to_string()),
            input_schema: None,
            query_params: Some(serde_json::json!({ "city": { "type": "string" } })),
            response_example: None,
            response_schema: None,
            method: "GET".to_string(),
            output_mime: "json",
            ..Default::default()
        };
        let resp = sample_requirement().into_response_with_bazaar(bazaar);
        let body = body_to_json(resp).await;
        let input = &body["extensions"]["bazaar"]["info"]["input"];
        assert!(
            input.get("queryParams").is_some(),
            "GET should advertise queryParams"
        );
        assert!(
            input.get("bodySchema").is_none(),
            "GET must not advertise bodySchema"
        );
        assert_eq!(input["queryParams"]["city"]["type"], "string");
    }

    #[tokio::test]
    async fn bazaar_post_uses_body_schema_not_query_params() {
        let bazaar = BazaarMetadata {
            description: None,
            input_schema: Some(serde_json::json!({ "type": "object", "required": ["city"] })),
            query_params: None,
            response_example: None,
            response_schema: None,
            method: "POST".to_string(),
            output_mime: "json",
            ..Default::default()
        };
        let resp = sample_requirement().into_response_with_bazaar(bazaar);
        let body = body_to_json(resp).await;
        let input = &body["extensions"]["bazaar"]["info"]["input"];
        // Spec (BodyDiscoveryInfo): body methods carry `bodyType` + `body`.
        // `bodySchema` is not a spec field and the pre-2026-07-10 shape made
        // CDP's spec validator drop the resource.
        assert_eq!(input["bodyType"], "json");
        assert!(input.get("body").is_some(), "POST should advertise body");
        assert!(
            input.get("bodySchema").is_none(),
            "bodySchema is not a Bazaar spec field"
        );
        assert!(
            input.get("queryParams").is_none(),
            "POST must not advertise queryParams"
        );
    }

    #[tokio::test]
    async fn bazaar_omits_example_keys_when_no_source_data() {
        // No response_example, no response_schema → no `example` key in info.output
        // and no `example` key under schema.properties.output.properties.
        let bazaar = BazaarMetadata {
            description: None,
            input_schema: None,
            query_params: None,
            response_example: None,
            response_schema: None,
            method: "GET".to_string(),
            output_mime: "json",
            ..Default::default()
        };
        let resp = sample_requirement().into_response_with_bazaar(bazaar);
        let body = body_to_json(resp).await;
        let output = &body["extensions"]["bazaar"]["info"]["output"];
        assert_eq!(output["type"], "json");
        assert!(
            output.get("example").is_none(),
            "info.output.example MUST be absent when no example supplied"
        );

        // The schema is the permissive envelope (matches the reference SDK's
        // buildBazaarExtensionFromDiscoveryInfo): output is just
        // `{ "type": "object" }` with no nested properties.
        let output_schema = &body["extensions"]["bazaar"]["schema"]["properties"]["output"];
        assert_eq!(output_schema["type"], "object");
        assert!(
            output_schema.get("properties").is_none(),
            "envelope schema must not embed endpoint-specific output properties"
        );
    }

    #[tokio::test]
    async fn bazaar_default_metadata_emits_placeholder_input_schema() {
        // No docs loaded → still serialise a valid extensions.bazaar with the
        // documented placeholder input schema.
        let resp = sample_requirement().into_response_with_bazaar(BazaarMetadata::default());
        let body = body_to_json(resp).await;

        // Top-level description is OMITTED entirely when no docs — emitting
        // "" would dilute Bazaar's search index. Confirm absence rather than
        // emptiness.
        assert!(
            body.get("description").is_none(),
            "empty description must be omitted, not emitted as empty string"
        );

        let bazaar = &body["extensions"]["bazaar"];
        // Default method is "" so the input block uses queryParams branch.
        assert_eq!(bazaar["info"]["input"]["type"], "http");
        // The schema is the permissive envelope: `info.input` (the HTTP
        // envelope) is validated against it by CDP's Ajv check, so it must
        // describe the ENVELOPE, never the endpoint's request-body schema.
        let input_schema = &bazaar["schema"]["properties"]["input"];
        assert_eq!(*input_schema, serde_json::json!({ "type": "object" }));

        // Top-level required pins "input".
        let top_required = bazaar["schema"]["required"].as_array().unwrap();
        assert!(top_required.iter().any(|v| v == "input"));
    }

    /// Regression guard for the 2026-07-05→10 Bazaar freeze: replicate the CDP
    /// Facilitator's `validateDiscoveryExtension` (Ajv: validate `info`
    /// against `schema`) structurally — every extension we emit must satisfy
    /// its own embedded schema, for GET and POST, with and without docs.
    #[tokio::test]
    async fn bazaar_extension_info_satisfies_its_own_schema() {
        let cases = vec![
            BazaarMetadata::default(),
            BazaarMetadata {
                method: "GET".to_string(),
                query_params: Some(serde_json::json!({ "city": { "type": "string" } })),
                output_mime: "json",
                ..Default::default()
            },
            BazaarMetadata {
                method: "POST".to_string(),
                input_schema: Some(serde_json::json!({
                    "type": "object",
                    "properties": { "url": { "type": "string" } },
                    "required": ["url"],
                })),
                response_example: Some(serde_json::json!({ "ok": true })),
                output_mime: "json",
                ..Default::default()
            },
        ];
        for meta in cases {
            let ext = meta.discovery_extension();
            let info = &ext["bazaar"]["info"];
            let schema = &ext["bazaar"]["schema"];
            // The envelope schema requires `input` as an object and permits
            // `output` as an object — verify info against those constraints
            // exactly as Ajv would.
            for key in schema["required"].as_array().unwrap() {
                let key = key.as_str().unwrap();
                assert!(info.get(key).is_some(), "info.{key} required by schema");
            }
            assert!(info["input"].is_object(), "info.input must be an object");
            if let Some(out) = info.get("output") {
                assert!(out.is_object(), "info.output must be an object");
            }
            // And the schema's input spec must be the permissive envelope —
            // no endpoint request-body schema may leak in here.
            assert_eq!(
                schema["properties"]["input"],
                serde_json::json!({ "type": "object" })
            );
        }
    }

    #[tokio::test]
    async fn into_response_legacy_matches_into_response_with_bazaar_default() {
        // Backward-compat: legacy callers using the no-arg into_response()
        // must still produce the same structural keys as the new method
        // invoked with a default BazaarMetadata. (Identity of accepts[0] is
        // the load-bearing bit — Bazaar fields are present but empty.)
        let legacy_body = body_to_json(sample_requirement().into_response()).await;
        let new_body =
            body_to_json(sample_requirement().into_response_with_bazaar(BazaarMetadata::default()))
                .await;

        assert_eq!(legacy_body["x402Version"], new_body["x402Version"]);
        assert_eq!(legacy_body["error"], new_body["error"]);
        assert_eq!(legacy_body["accepts"], new_body["accepts"]);
        assert!(legacy_body.get("extensions").is_some());
        assert!(legacy_body["extensions"].get("bazaar").is_some());
    }

    #[tokio::test]
    async fn bazaar_402_emits_resource_with_service_name_and_tags() {
        // The discoverability fix: the 402 carries a v2 `resource` object with
        // serviceName + tags (what the Bazaar ranks/searches on) so agents can
        // actually FIND the route, not just have it indexed.
        let bazaar = BazaarMetadata {
            description: Some("Real-time US stock quotes".to_string()),
            method: "GET".to_string(),
            output_mime: "json",
            service_name: Some("Finnhub Stock Data".to_string()),
            tags: vec![
                "stocks".to_string(),
                "finance".to_string(),
                "quotes".to_string(),
            ],
            ..Default::default()
        };
        let body = body_to_json(sample_requirement().into_response_with_bazaar(bazaar)).await;

        assert_eq!(body["resource"]["serviceName"], "Finnhub Stock Data");
        assert_eq!(body["resource"]["tags"][0], "stocks");
        assert_eq!(body["resource"]["tags"].as_array().unwrap().len(), 3);
        assert_eq!(body["resource"]["description"], "Real-time US stock quotes");
        assert!(body["resource"]["url"]
            .as_str()
            .unwrap()
            .starts_with("http"));
        // Still a valid x402 challenge — additive, payment path untouched.
        assert_eq!(body["error"], "Payment required");
        assert!(body["accepts"].is_array());
    }

    #[tokio::test]
    async fn bazaar_402_omits_resource_fields_when_absent() {
        // Soft-drop: a bare listing (no serviceName/tags) still produces a valid
        // body with `resource.url` but no empty serviceName/tags keys.
        let body =
            body_to_json(sample_requirement().into_response_with_bazaar(BazaarMetadata::default()))
                .await;
        assert!(body["resource"]["url"].is_string());
        assert!(body["resource"].get("serviceName").is_none());
        assert!(body["resource"].get("tags").is_none());
    }

    #[tokio::test]
    async fn payment_invalid_response_is_distinct_and_diagnosable() {
        // A rejected payment proof must be distinguishable from a fresh
        // "Payment required" challenge AND must carry the reason — otherwise an
        // invalid payment is an opaque, undebuggable 402 (the company-search
        // failure mode). It still includes `accepts` so the caller can retry.
        let challenge =
            body_to_json(sample_requirement().into_response_with_bazaar(BazaarMetadata::default()))
                .await;
        let rejected = body_to_json(sample_requirement().into_payment_invalid_response(
            BazaarMetadata::default(),
            "invalid_exact_evm_payload_signature",
        ))
        .await;

        assert_eq!(challenge["error"], "Payment required");
        assert!(challenge.get("errorReason").is_none());

        assert_eq!(rejected["error"], "payment_verification_failed");
        assert_eq!(
            rejected["errorReason"],
            "invalid_exact_evm_payload_signature"
        );
        assert!(
            rejected["error"] != challenge["error"],
            "rejected payment must NOT look like a fresh challenge"
        );
        // Still spec-shaped (x402 v1 body): accepts present with
        // `maxAmountRequired` so the caller can re-pay.
        assert!(rejected["accepts"].is_array());
        assert!(rejected["accepts"][0]["maxAmountRequired"].is_string());
        assert_eq!(rejected["accepts"][0]["network"], "base");
    }

    #[test]
    fn bare_requirement_is_spec_minimal_in_verify_or_settle_request() {
        // A requirement that has NOT been enriched (no `with_bazaar_extension`)
        // must serialize to the bare, spec-shaped envelope — no `extensions`,
        // no `outputSchema` — so every non-discovery code path is byte-for-byte
        // unchanged. (Enrichment is opt-in; see the companion test below.)
        let requirement = sample_requirement();

        let verify = super::VerifyRequest {
            x402_version: 1,
            payment_payload: serde_json::json!({"scheme":"exact"}),
            payment_requirements: serde_json::to_value(&requirement).unwrap(),
        };
        let settle = super::SettleRequest {
            x402_version: 1,
            payment_payload: serde_json::json!({"scheme":"exact"}),
            payment_requirements: serde_json::to_value(&requirement).unwrap(),
        };

        for value in [
            serde_json::to_value(&verify).unwrap(),
            serde_json::to_value(&settle).unwrap(),
        ] {
            assert!(
                value.get("extensions").is_none(),
                "facilitator request body must not carry top-level `extensions`"
            );
            let req = &value["paymentRequirements"];
            assert!(
                req.get("extensions").is_none(),
                "bare paymentRequirements must not carry `extensions`"
            );
            assert!(
                req.get("outputSchema").is_none(),
                "bare paymentRequirements must not carry `outputSchema`"
            );
            // And the 10 spec fields must still be present in camelCase.
            for key in [
                "scheme",
                "network",
                "maxAmountRequired",
                "resource",
                "description",
                "mimeType",
                "payTo",
                "maxTimeoutSeconds",
                "asset",
                "extra",
            ] {
                assert!(
                    req.get(key).is_some(),
                    "paymentRequirements.{} must be present",
                    key
                );
            }
        }
    }

    #[test]
    fn enriched_settle_requirement_carries_v1_output_schema_for_bazaar() {
        // The FIX, pinned: on the paid-request path the gateway attaches Bazaar
        // discovery metadata via `with_bazaar_extension`. Because our settle is
        // x402Version 1, the CDP Facilitator's v1 discovery extractor reads
        // `paymentRequirements.outputSchema` (NOT `.extensions`). If this field
        // goes missing, settlement still succeeds but the route is never
        // cataloged — the exact failure that kept Apiosk out of the Bazaar.
        //
        // NOTE: carrying `extensions`/`outputSchema` on the requirement does
        // NOT break EIP-712 verification — the signed authorization covers the
        // transfer fields (from/to/value/validity/nonce), not requirement
        // metadata. Production settles prove this.
        let meta = BazaarMetadata {
            description: Some("Realtime quote".to_string()),
            input_schema: None,
            query_params: Some(serde_json::json!({
                "symbol": { "type": "string", "required": true }
            })),
            response_example: Some(serde_json::json!({ "c": 214.29 })),
            response_schema: None,
            method: "GET".to_string(),
            output_mime: "json",
            ..Default::default()
        };
        let requirement = sample_requirement().with_bazaar_extension(&meta);

        let settle = super::SettleRequest {
            x402_version: 1,
            payment_payload: serde_json::json!({"scheme":"exact"}),
            payment_requirements: serde_json::to_value(&requirement).unwrap(),
        };
        let value = serde_json::to_value(&settle).unwrap();
        let req = &value["paymentRequirements"];

        // v1 discovery location — this is what actually drives indexing.
        let output_schema = req
            .get("outputSchema")
            .expect("enriched settle requirement MUST carry outputSchema");
        assert_eq!(output_schema["input"]["type"], "http");
        assert_eq!(output_schema["input"]["method"], "GET");
        assert_eq!(output_schema["input"]["discoverable"], true);
        // GET → queryParams (not bodyFields); reflects the declared params.
        assert!(
            output_schema["input"]["queryParams"]
                .get("symbol")
                .is_some(),
            "GET outputSchema.input must carry queryParams"
        );
        // `output` is taken verbatim by extractDiscoveryInfoV1 as the example.
        assert_eq!(output_schema["output"]["c"], 214.29);

        // v2 extension still present (for v2-aware clients / forward compat).
        assert!(req["extensions"]["bazaar"].is_object());

        // Spec fields remain intact alongside the discovery metadata.
        for key in [
            "scheme",
            "network",
            "maxAmountRequired",
            "resource",
            "payTo",
            "asset",
        ] {
            assert!(req.get(key).is_some(), "spec field {} must remain", key);
        }
    }
}
