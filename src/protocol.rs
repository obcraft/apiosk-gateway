use crate::state::Envelope;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub mod limits;

pub const VERSION: &str = "2";
pub const MAX_INLINE: usize = 32 * 1024;
pub const MAX_STEPS: usize = 12;
pub const MAX_DEPTH: usize = 5;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ContextDelta {
    /// App workflow selection, only accepted when opening a fresh chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_slug: Option<String>,
    #[serde(default)]
    pub explicit_values: Vec<ExplicitValue>,
    #[serde(default)]
    pub requested_focus_refs: Vec<Uuid>,
    /// App endpoint selection (`chat_selection_v1`): the literal question is
    /// unchanged; the gateway binds the validated endpoints and known values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<Selection>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub version: u8,
    #[serde(default)]
    pub mode: SelectionMode,
    pub endpoints: Vec<SelectedEndpoint>,
}
/// `restrict`: only the selected endpoints may serve the requested goals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionMode {
    #[default]
    Restrict,
}
/// Exactly one of `endpoint_id` (a catalog endpoint) or `listing` (a
/// published endpoint without a planner contract).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedEndpoint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listing: Option<ListingRef>,
    #[serde(default)]
    pub inputs: Vec<KnownInput>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListingRef {
    pub slug: String,
    pub method: String,
    pub path: String,
}
/// A value the person entered for one of the endpoint's contract inputs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnownInput {
    pub fact_type: String,
    pub subject_role: String,
    pub value: Value,
}
/// `POST /v2/tasks/:id/requote` (`requote_v1`, #23): refresh an expired quote
/// from the stored intent and selection, with no model call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequoteRequest {
    pub request_id: Uuid,
    pub state: Envelope,
    pub quote_ref: Uuid,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplicitValue {
    pub entity_ref: Uuid,
    pub semantic_type: String,
    pub value: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoverRequest {
    pub request_id: Uuid,
    pub question: String,
    #[serde(default)]
    pub state: Option<Envelope>,
    #[serde(default)]
    pub context_delta: ContextDelta,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteRequest {
    pub request_id: Uuid,
    pub action_id: Uuid,
    pub state: Envelope,
    #[serde(default)]
    pub quote_ref: Option<Uuid>,
    #[serde(default)]
    pub authorization_ref: Option<Uuid>,
    pub idempotency_key: Uuid,
    #[serde(default)]
    pub input: Value,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ready,
    NeedsInput,
    NeedsSelection,
    RequiresApproval,
    Running,
    Cancelled,
    Succeeded,
    Partial,
    Unsupported,
    StateConflict,
    Failed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NextAction {
    pub action_id: Uuid,
    pub kind: String,
    pub label: String,
    pub requires_authorization: bool,
    pub input_schema: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub label: String,
    pub quote_ref: Uuid,
    pub price_status: String,
    pub currency: String,
    /// JSON decimal string to avoid loss in JavaScript for integer atomic USDC.
    pub max_total_atomic: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub approval_url: String,
    pub steps: Vec<String>,
    #[serde(default)]
    pub step_details: Vec<Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub protocol_version: String,
    pub request_id: Uuid,
    pub status: Status,
    pub intent_ref: Option<Uuid>,
    pub context_view: Value,
    pub proposal: Option<Proposal>,
    pub result: Option<Value>,
    #[serde(default)]
    pub billing: Option<Value>,
    pub next_actions: Vec<NextAction>,
    pub state: Option<Envelope>,
    pub errors: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}
impl Response {
    pub fn new(request_id: Uuid, status: Status) -> Self {
        Self {
            protocol_version: VERSION.into(),
            request_id,
            status,
            intent_ref: None,
            context_view: serde_json::json!({}),
            proposal: None,
            result: None,
            billing: None,
            next_actions: vec![],
            state: None,
            errors: vec![],
            retry_after_ms: None,
        }
    }
    pub fn error(request_id: Uuid, status: Status, code: &str, message: &str) -> Self {
        let mut r = Self::new(request_id, status);
        r.errors
            .push(serde_json::json!({"code":code,"message":message}));
        r
    }
}

pub const EVENTS_HEARTBEAT_MS: u64 = 10_000;
pub const EVENTS_STALE_AFTER_MS: u64 = 25_000;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamOpen {
    pub protocol: String,
    pub heartbeat_ms: u64,
    pub stale_after_ms: u64,
    pub expires_at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Heartbeat {
    pub at: chrono::DateTime<chrono::Utc>,
    pub revision: Option<i64>,
    pub sync_key: Option<String>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamEndReason {
    Terminal,
    Expired,
    AccessLost,
    Error,
    Shutdown,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamEnd {
    pub reason: StreamEndReason,
    pub revision: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn client_cannot_supply_approval_boolean() {
        assert!(serde_json::from_value::<DiscoverRequest>(
            serde_json::json!({"request_id":Uuid::new_v4(),"question":"x","approved":true})
        )
        .is_err());
    }
    #[test]
    fn a_request_without_selection_keeps_its_digest() {
        // Idempotency hashes the parsed request: a retry sent before this
        // field existed must hash the same after a deploy.
        let old = serde_json::json!({"request_id":Uuid::nil(),"question":"x","state":null,"context_delta":{"explicit_values":[],"requested_focus_refs":[]}});
        let parsed: DiscoverRequest = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), old);
    }
    #[test]
    fn selection_rejects_unknown_fields_and_modes() {
        let endpoint = serde_json::json!({"endpoint_id":Uuid::nil(),"inputs":[{"fact_type":"company.id","subject_role":"subject","value":"1"}]});
        let parsed: Selection = serde_json::from_value(serde_json::json!({"version":1,"endpoints":[endpoint]})).unwrap();
        assert_eq!(parsed.mode, SelectionMode::Restrict);
        assert!(serde_json::from_value::<Selection>(serde_json::json!({"version":1,"mode":"prefer","endpoints":[]})).is_err());
        assert!(serde_json::from_value::<Selection>(serde_json::json!({"version":1,"endpoints":[],"price":1})).is_err());
        assert!(serde_json::from_value::<SelectedEndpoint>(serde_json::json!({"endpoint_id":Uuid::nil(),"approved":true})).is_err());
    }
    #[test]
    fn the_largest_valid_chat_request_fits_the_body_limit() {
        // Four UTF-8 bytes per question character, six bytes per escaped
        // control character in the entered values, 64-character fact types.
        let budget = &limits::limits().selection;
        let inputs_per_endpoint = budget.max_inputs_per_endpoint;
        let per_value = budget.max_total_value_chars / (budget.max_endpoints * inputs_per_endpoint);
        let endpoint = SelectedEndpoint { endpoint_id: Some(Uuid::nil()), listing: None, inputs: (0..inputs_per_endpoint).map(|_| KnownInput {
            fact_type: "f".repeat(64), subject_role: "subject".into(), value: Value::String("\u{1}".repeat(per_value)) }).collect() };
        let request = DiscoverRequest { request_id: Uuid::nil(), question: "😀".repeat(limits::limits().question.max_chars), state: None,
            context_delta: ContextDelta { selection: Some(Selection { version: 1, mode: SelectionMode::Restrict, endpoints: vec![endpoint; budget.max_endpoints] }), ..Default::default() } };
        let bytes = serde_json::to_vec(&request).unwrap().len() + 1024;
        assert!(bytes < 64 * 1024, "{bytes} bytes with an envelope");
    }
    #[test]
    fn all_statuses_use_same_envelope() {
        for status in [
            Status::Ready,
            Status::NeedsInput,
            Status::NeedsSelection,
            Status::RequiresApproval,
            Status::Running,
            Status::Cancelled,
            Status::Succeeded,
            Status::Partial,
            Status::Unsupported,
            Status::StateConflict,
            Status::Failed,
        ] {
            let r = Response::new(Uuid::new_v4(), status);
            let json = serde_json::to_value(&r).unwrap();
            assert_eq!(json["protocol_version"], "2");
            assert!(json.get("state").is_some());
            serde_json::from_value::<Response>(json).unwrap();
        }
    }
}
