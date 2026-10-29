//! Typed telemetry events. Each variant maps 1:1 to a table created by
//! migration `052_gateway_telemetry_v2.sql`. These are plain data — built on the
//! request path, handed to the bounded queue, and drained by the background
//! writer. They carry OWNED values (they cross a channel / outlive the request).
//!
//! Nothing here is authoritative for money: the financial ledgers
//! (`gateway_usage`, `settlements`) are unchanged and remain the source of truth.

use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use serde_json::Value as Json;
use uuid::Uuid;

/// Controlled vocabulary for `gateway_requests_v2.request_kind`.
/// MUST stay in sync with the CHECK constraint in migration 052.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Api,
    GatewayApi,
    Discovery,
    Checkout,
    Health,
    Asset,
}

impl RequestKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RequestKind::Api => "api",
            RequestKind::GatewayApi => "gateway_api",
            RequestKind::Discovery => "discovery",
            RequestKind::Checkout => "checkout",
            RequestKind::Health => "health",
            RequestKind::Asset => "asset",
        }
    }
}

/// Controlled vocabulary for `gateway_requests_v2.outcome`.
/// MUST stay in sync with the CHECK constraint in migration 052.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    ClientError,
    ServerError,
    PaymentRequired,
    PaymentFailed,
    AuthFailed,
    RateLimited,
    UpstreamError,
    GatewayError,
    ClientDisconnected,
    Discovery,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Success => "success",
            Outcome::ClientError => "client_error",
            Outcome::ServerError => "server_error",
            Outcome::PaymentRequired => "payment_required",
            Outcome::PaymentFailed => "payment_failed",
            Outcome::AuthFailed => "auth_failed",
            Outcome::RateLimited => "rate_limited",
            Outcome::UpstreamError => "upstream_error",
            Outcome::GatewayError => "gateway_error",
            Outcome::ClientDisconnected => "client_disconnected",
            Outcome::Discovery => "discovery",
        }
    }
}

/// Controlled vocabulary for `gateway_requests_v2.failure_stage`.
/// MUST stay in sync with the CHECK constraint in migration 052.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureStage {
    Routing,
    Authentication,
    PaymentChallenge,
    PaymentVerification,
    PaymentSettlement,
    RateLimit,
    Upstream,
    Response,
    Internal,
}

impl FailureStage {
    pub fn as_str(self) -> &'static str {
        match self {
            FailureStage::Routing => "routing",
            FailureStage::Authentication => "authentication",
            FailureStage::PaymentChallenge => "payment_challenge",
            FailureStage::PaymentVerification => "payment_verification",
            FailureStage::PaymentSettlement => "payment_settlement",
            FailureStage::RateLimit => "rate_limit",
            FailureStage::Upstream => "upstream",
            FailureStage::Response => "response",
            FailureStage::Internal => "internal",
        }
    }
}

/// One row for `gateway_requests_v2` — the spine of a single request.
#[derive(Debug, Clone, Default)]
pub struct RequestRecord {
    pub request_id: Uuid,
    pub trace_id: Option<Uuid>,
    pub parent_request_id: Option<Uuid>,

    pub occurred_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,

    pub method: String,
    pub host: Option<String>,
    pub path: String,
    pub route_template: Option<String>,
    pub query_keys: Vec<String>,
    pub http_version: Option<String>,
    pub scheme: Option<String>,

    pub api_id: Option<Uuid>,
    pub endpoint_id: Option<Uuid>,
    pub api_slug: Option<String>,
    pub request_kind: Option<RequestKind>,

    pub status_code: Option<i16>,
    pub outcome: Option<Outcome>,
    pub failure_stage: Option<FailureStage>,
    pub error_code: Option<String>,
    pub retryable: Option<bool>,

    pub latency_ms: Option<i64>,
    pub request_bytes: Option<i64>,
    pub response_bytes: Option<i64>,
    pub request_content_type: Option<String>,
    pub response_content_type: Option<String>,
    pub time_to_first_byte_ms: Option<i64>,
    pub client_disconnected: Option<bool>,

    pub ip_address: Option<String>,
    pub country_code: Option<String>,
    pub region: Option<String>,
    pub asn: Option<i64>,
    pub network_name: Option<String>,
    pub network_type: Option<String>,

    pub user_agent: Option<String>,
    pub agent_name: Option<String>,
    pub agent_version: Option<String>,
    pub agent_framework: Option<String>,
    pub sdk_name: Option<String>,
    pub sdk_version: Option<String>,
    pub client_kind: Option<String>,

    pub user_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub client_id: Option<String>,
    pub managed_wallet_id: Option<Uuid>,
    pub agent_connect_token_id: Option<Uuid>,
    pub wallet_address: Option<String>,

    pub referrer: Option<String>,
    pub origin: Option<String>,
    pub metadata: Json,
}

/// One row for `gateway_request_events` — a lifecycle breadcrumb.
#[derive(Debug, Clone)]
pub struct LifecycleEvent {
    pub request_id: Uuid,
    pub occurred_at: Option<DateTime<Utc>>,
    /// Free-form but drawn from the known set documented on the table (e.g.
    /// `request_received`, `payment_verified`, `response_completed`).
    pub event_type: String,
    pub duration_ms: Option<i64>,
    pub status: Option<String>,
    pub error_code: Option<String>,
    pub metadata: Json,
}

/// One row for `gateway_payment_attempts`. Telemetry-only; the ledgers remain
/// authoritative. Stores a KEYED fingerprint of the proof, never the proof.
#[derive(Debug, Clone, Default)]
pub struct PaymentAttempt {
    pub request_id: Uuid,
    pub attempt_number: i16,
    pub rail_requested: Option<String>,
    pub rail_selected: Option<String>,
    pub x402_version: Option<i16>,
    pub network: Option<String>,
    pub chain: Option<String>,
    pub token: Option<String>,
    pub offered_amount: Option<BigDecimal>,
    pub authorized_amount: Option<BigDecimal>,
    pub wallet_address: Option<String>,
    pub proof_fingerprint: Option<String>,
    pub verification_outcome: Option<String>,
    pub settlement_outcome: Option<String>,
    pub facilitator_latency_ms: Option<i64>,
    pub facilitator_status: Option<String>,
    pub tx_hash: Option<String>,
    pub failure_code: Option<String>,
    pub fallback_rail: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub metadata: Json,
}

/// One row for `gateway_upstream_attempts` — a single provider call / retry.
#[derive(Debug, Clone, Default)]
pub struct UpstreamAttempt {
    pub request_id: Uuid,
    pub api_id: Option<Uuid>,
    pub endpoint_id: Option<Uuid>,
    pub attempt_number: i16,
    pub upstream_host: Option<String>,
    pub upstream_status: Option<i16>,
    pub connect_ms: Option<i64>,
    pub tls_ms: Option<i64>,
    pub time_to_first_byte_ms: Option<i64>,
    pub total_ms: Option<i64>,
    pub request_bytes: Option<i64>,
    pub response_bytes: Option<i64>,
    pub error_category: Option<String>,
    pub is_resell: Option<bool>,
    pub upstream_x402_amount: Option<BigDecimal>,
    pub retry_reason: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub metadata: Json,
}

/// One row for `gateway_discovery_events` — the previously-discarded
/// `/.well-known`, `/llms.txt`, `/openapi.json`, manifest traffic.
#[derive(Debug, Clone, Default)]
pub struct DiscoveryEvent {
    pub request_id: Uuid,
    pub occurred_at: Option<DateTime<Utc>>,
    pub resource: Option<String>,
    pub method: Option<String>,
    pub status_code: Option<i16>,
    pub ip_address: Option<String>,
    pub client_fingerprint: Option<String>,
    pub user_agent: Option<String>,
    pub crawler_class: Option<String>,
    pub cache_result: Option<String>,
    pub latency_ms: Option<i64>,
    pub response_bytes: Option<i64>,
    pub metadata: Json,
}

/// The envelope pushed onto the bounded telemetry queue.
#[derive(Debug, Clone)]
pub enum TelemetryEvent {
    Request(Box<RequestRecord>),
    Lifecycle(LifecycleEvent),
    Payment(Box<PaymentAttempt>),
    Upstream(Box<UpstreamAttempt>),
    Discovery(Box<DiscoveryEvent>),
}
