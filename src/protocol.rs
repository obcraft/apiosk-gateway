use crate::state::Envelope;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub const VERSION: &str = "2";
pub const MAX_QUESTION: usize = 4000;
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

/// A worker lease is the only execution signal LIVE-DB owns; queueing and
/// provisional-result fields are EXECUTION's (#27/#30) to fill in later.
/// Until then a task is only ever `Idle` or `Active`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    Idle,
    Queued,
    Active,
    Stalled,
    Expired,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionView {
    pub state: ExecutionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisional: Option<Value>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StepCounts {
    pub total: u32,
    pub completed: u32,
    pub failed: u32,
    pub skipped: u32,
    pub running: u32,
    pub pending: u32,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConversationMeta {
    pub total: u32,
    pub window_start_seq: u32,
    pub title: Option<String>,
}
/// `e` is one of `i q a s x` (idle, queued, active, stalled, expired) and `a`
/// is `0` or `1`. Opaque to clients: they only compare it for equality.
pub fn sync_key(revision: i64, state: ExecutionState, authorization_active: bool) -> String {
    let e = match state {
        ExecutionState::Idle => 'i',
        ExecutionState::Queued => 'q',
        ExecutionState::Active => 'a',
        ExecutionState::Stalled => 's',
        ExecutionState::Expired => 'x',
    };
    format!("{revision}.{e}.{}", u8::from(authorization_active))
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStatus {
    pub protocol_version: String,
    pub task_ref: Uuid,
    pub revision: i64,
    pub observed_at: chrono::DateTime<chrono::Utc>,
    pub sync_key: String,
    pub status: Status,
    pub execution: ExecutionView,
    pub worker_active: bool,
    pub analyzing: bool,
    pub authorization_active: bool,
    pub quote_ref: Option<Uuid>,
    pub quote_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub steps: StepCounts,
    pub result_count: u32,
    pub conversation: ConversationMeta,
    pub has_errors: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    pub events_path: Option<String>,
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

/// `before_seq` defaults to the window's own start when omitted by the caller.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnsQuery {
    pub before_seq: Option<u32>,
    pub limit: Option<u8>,
}
pub const TURNS_PAGE_DEFAULT: u8 = 5;
pub const TURNS_PAGE_MAX: u8 = 10;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnView {
    pub seq: u32,
    pub question: String,
    pub reply: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub output: Option<Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnsPage {
    pub protocol_version: String,
    pub task_ref: Uuid,
    pub total: u32,
    pub turns: Vec<TurnView>,
    pub next_before_seq: Option<u32>,
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
