//! Ask V2 run contract, version 1 (contracts/ask-v2-run.md).
//!
//! Shared serde shapes and the hard rules every Phase 2 module relies on:
//! planner input, score records, source plans, progress events, per-source
//! rows, evidence/provenance and synthesis output. Types and pure checks only:
//! no I/O, no model call, no planning. Change this module only additively
//! within one version; a breaking change bumps the matching `*_VERSION`.
//!
//! Money is integer micro-USD ("atomic"), serialized as a decimal string so
//! JavaScript never rounds it, matching `protocol::Proposal::max_total_atomic`.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, future::Future};
use uuid::Uuid;

pub const RUN_CONTRACT_VERSION: u32 = 1;
pub const EVENT_SCHEMA_VERSION: u32 = 1;
pub const PLANNER_INPUT_VERSION: u32 = 1;
pub const SCORE_RECORD_VERSION: u32 = 1;
pub const SOURCE_PLAN_VERSION: u32 = 1;
pub const EVIDENCE_VERSION: u32 = 1;
pub const SYNTHESIS_VERSION: u32 = 1;
/// The only grounded intent the planner accepts (intent_fields::ground_response).
pub const INTENT_SCHEMA_VERSION: u32 = 2;
/// Product policy floor, not a measured optimum. No configuration may go lower.
pub const MIN_FIT_THRESHOLD: f64 = 90.0;
/// Same model as intent extraction (`intent::MODEL`); never user-selectable.
pub const SYNTHESIS_MODEL: &str = "openai/gpt-oss-120b";
/// One bounded answer repair, using saved evidence only.
pub const MAX_REPAIR_ATTEMPTS: u32 = 1;
pub const EVENT_PAGE_LIMIT: u32 = 200;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ContractError {
    #[error("fit score must be a finite number from 0 to 100")]
    InvalidScore,
    #[error("the active threshold must be a finite number from 90 to 100")]
    InvalidThreshold,
    #[error("event sequence {next} does not follow {last}")]
    SequenceGap { last: u64, next: u64 },
    #[error("invalid event: {0}")]
    InvalidEvent(&'static str),
    #[error("invalid amount")]
    InvalidAmount,
    #[error("quote totals do not match: {0}")]
    QuoteMismatch(&'static str),
}

// ---------------------------------------------------------------------------
// Scores and the 90/100 floor
// ---------------------------------------------------------------------------

/// Source fit for one request/subject/assigned-field scope: finite, 0..=100.
/// Deserializing NaN, infinity or an out-of-range number fails the record;
/// an unscorable candidate is `None`, never 0 and never a default.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct FitScore(f64);
impl FitScore {
    pub fn new(value: f64) -> Result<Self, ContractError> {
        if value.is_finite() && (0.0..=100.0).contains(&value) { Ok(Self(value)) } else { Err(ContractError::InvalidScore) }
    }
    pub fn get(self) -> f64 { self.0 }
}
impl TryFrom<f64> for FitScore {
    type Error = ContractError;
    fn try_from(value: f64) -> Result<Self, Self::Error> { Self::new(value) }
}
impl From<FitScore> for f64 {
    fn from(score: FitScore) -> f64 { score.0 }
}

/// The threshold pinned in a plan. It can be raised, never configured below 90.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct ActiveThreshold(f64);
impl ActiveThreshold {
    pub fn new(value: f64) -> Result<Self, ContractError> {
        if value.is_finite() && (MIN_FIT_THRESHOLD..=100.0).contains(&value) { Ok(Self(value)) } else { Err(ContractError::InvalidThreshold) }
    }
    pub fn get(self) -> f64 { self.0 }
    /// Gates first: a failed mandatory gate excludes regardless of score. The
    /// comparison uses the unrounded value, so 89.99 fails a 90 threshold.
    pub fn decide(self, score: Option<FitScore>, gates_passed: bool) -> ThresholdDecision {
        match (gates_passed, score) {
            (false, _) => ThresholdDecision::GateFailed,
            (true, None) => ThresholdDecision::Unscorable,
            (true, Some(score)) if score.get() >= self.0 => ThresholdDecision::Qualifies,
            (true, Some(_)) => ThresholdDecision::BelowThreshold,
        }
    }
}
impl Default for ActiveThreshold {
    fn default() -> Self { Self(MIN_FIT_THRESHOLD) }
}
impl TryFrom<f64> for ActiveThreshold {
    type Error = ContractError;
    fn try_from(value: f64) -> Result<Self, Self::Error> { Self::new(value) }
}
impl From<ActiveThreshold> for f64 {
    fn from(threshold: ActiveThreshold) -> f64 { threshold.0 }
}

/// A raw number (from a scorer or storage) qualifies only if it is a valid
/// score at or above the threshold. NaN, null and out-of-range never pass.
pub fn qualifies(raw: Option<f64>, threshold: ActiveThreshold) -> bool {
    raw.and_then(|value| FitScore::new(value).ok())
        .is_some_and(|score| threshold.decide(Some(score), true) == ThresholdDecision::Qualifies)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThresholdDecision { Qualifies, BelowThreshold, Unscorable, GateFailed }

/// Honest labelling: a hand-weighted score is never presented as calibrated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalibrationStatus { UncalibratedHeuristic, Calibrated }

/// `Enforced` selects sources; `Shadow` is recorded for evaluation only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreMode { Enforced, Shadow }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScorePolicy {
    /// e.g. "fit-heuristic-v1"; pinned in every plan.
    pub score_version: String,
    pub calibration_status: CalibrationStatus,
    pub mode: ScoreMode,
    pub threshold: ActiveThreshold,
}

/// Mandatory eligibility gates (spec §4). Lexical retrieval is not a gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Gate {
    /// Exact capability or an explicitly reviewed equivalence.
    Capability,
    /// Documented coverage of assigned fields with valid extraction mappings.
    FieldCoverage,
    /// Subject type, geography/jurisdiction and filters.
    SubjectFit,
    /// Explicitly requested sources only.
    SourceRestriction,
    /// Literal inputs or an executable, bounded dependency path.
    Inputs,
    /// Active/accepted contract, verified readiness, not suspended, permitted.
    Admission,
    /// Known price under the supported price/effect semantics.
    Price,
    Freshness,
    /// Bounded pagination, fan-out and cardinality.
    Bounds,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateResult {
    pub gate: Gate,
    pub passed: bool,
    /// Short public reason code, e.g. "jurisdiction_mismatch".
    pub reason: String,
}

/// A named score input. Missing evidence stays missing (`value: None`) and
/// never receives full credit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreSignal {
    pub name: String,
    pub value: Option<f64>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_age_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_size: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FieldScope {
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub optional: Vec<String>,
}

/// One endpoint scored for one request, subject set and assigned output set.
/// Rejected records stay in diagnostics (ask_run_scores), never in a plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreRecord {
    pub score_record_version: u32,
    /// Stable: "{request_id}:{subject_ids joined by +}:{contract_id}".
    pub candidate_id: String,
    pub request_id: String,
    pub subject_ids: Vec<String>,
    /// catalog::Service::id (capability_execution_contracts.id).
    pub contract_id: Uuid,
    pub endpoint_id: Uuid,
    /// catalog::Service::api_slug.
    pub source_id: String,
    pub fit_score: Option<FitScore>,
    pub score_version: String,
    pub calibration_status: CalibrationStatus,
    pub mode: ScoreMode,
    pub threshold: ActiveThreshold,
    pub decision: ThresholdDecision,
    pub signals: Vec<ScoreSignal>,
    pub gates: Vec<GateResult>,
    pub assigned: FieldScope,
    pub gaps: FieldScope,
}
impl ScoreRecord {
    /// Recomputes the decision from gates, score and threshold; a stored
    /// decision that disagrees is a contract violation.
    pub fn consistent(&self) -> bool {
        self.decision == self.threshold.decide(self.fit_score, self.gates.iter().all(|g| g.passed))
    }
}

// ---------------------------------------------------------------------------
// Grounded intent (HTTP shape of POST /v2/ask-v2/intent) and planner input
// ---------------------------------------------------------------------------

/// Exactly the schema-version-2 envelope returned by the intent handler. The
/// planner never accepts it from a client; it is produced and saved server-side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroundedIntentEnvelope {
    pub schema_version: u32,
    pub intent: GroundedIntent,
    pub model: String,
    #[serde(default)]
    pub timing: Value,
    #[serde(default)]
    pub attempts: Option<u32>,
    #[serde(default)]
    pub usage: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroundedIntent {
    pub language: String,
    pub subjects: Vec<IntentSubject>,
    pub capabilities: Vec<IntentCapability>,
    pub requests: Vec<GroundedRequest>,
    pub output: IntentOutput,
    pub field_definitions: Vec<FieldDefinition>,
    pub field_catalog_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentSubject {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub label: String,
    #[serde(default)]
    pub identifiers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentCapability {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub category: String,
    /// "draft" capabilities have no admitted contract and cannot be executed.
    pub status: String,
    /// "database" or "local".
    pub origin: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind { Lookup, Search, Action, Answer }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnresolvedField {
    pub field: String,
    /// "undocumented" | "ambiguous".
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct UnresolvedFields {
    pub required: Vec<UnresolvedField>,
    pub optional: Vec<UnresolvedField>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroundedRequest {
    pub id: String,
    pub capability_slug: String,
    pub subject_ids: Vec<String>,
    pub kind: RequestKind,
    pub goal: String,
    /// Canonical field IDs only ("<fact-type>#<schema-pointer>").
    pub required_fields: Vec<String>,
    pub optional_fields: Vec<String>,
    pub filters: BTreeMap<String, String>,
    pub sources: Vec<String>,
    /// "resolved" | "unresolved".
    pub field_resolution: String,
    pub unresolved_fields: UnresolvedFields,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentOutput {
    pub format: String,
    pub operations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldDefinition {
    pub id: String,
    pub fact_type: String,
    pub schema_pointer: String,
}

/// Validated, server-owned planning input (planner::adapt). Not the old
/// capability_intake::ParsedRequest; never cast between them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannerInput {
    pub planner_input_version: u32,
    pub intent_schema_version: u32,
    pub field_catalog_version: String,
    pub question_revision: u32,
    pub language: String,
    pub subjects: Vec<IntentSubject>,
    pub capabilities: Vec<IntentCapability>,
    /// Same shape as the grounded request, unresolved fields preserved.
    pub requests: Vec<GroundedRequest>,
    pub field_definitions: Vec<FieldDefinition>,
    pub output: IntentOutput,
    /// User answers applied since extraction (each one bumped question_revision).
    #[serde(default)]
    pub clarifications: Vec<ClarificationAnswer>,
    /// Schemas pinned when the selected quote is saved; unrelated catalog refreshes cannot reinterpret purchased evidence.
    #[serde(default)]
    pub fact_schemas: BTreeMap<String, Value>,
}

// ---------------------------------------------------------------------------
// Run stages and progress events
// ---------------------------------------------------------------------------

/// Spec §7 stage list; also the persisted `ask_runs.stage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Understanding,
    ValidatingIntent,
    NeedsClarification,
    FindingSources,
    FilteringSources,
    RankingSources,
    BuildingPlan,
    AwaitingApproval,
    Executing,
    ValidatingResults,
    Summarizing,
    VerifyingAnswer,
    Completed,
    Partial,
    NoQualifyingSources,
    Failed,
    Cancelled,
}
impl Stage {
    pub const ALL: [Stage; 17] = [
        Stage::Understanding, Stage::ValidatingIntent, Stage::NeedsClarification, Stage::FindingSources,
        Stage::FilteringSources, Stage::RankingSources, Stage::BuildingPlan, Stage::AwaitingApproval,
        Stage::Executing, Stage::ValidatingResults, Stage::Summarizing, Stage::VerifyingAnswer,
        Stage::Completed, Stage::Partial, Stage::NoQualifyingSources, Stage::Failed, Stage::Cancelled,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Understanding => "understanding",
            Stage::ValidatingIntent => "validating_intent",
            Stage::NeedsClarification => "needs_clarification",
            Stage::FindingSources => "finding_sources",
            Stage::FilteringSources => "filtering_sources",
            Stage::RankingSources => "ranking_sources",
            Stage::BuildingPlan => "building_plan",
            Stage::AwaitingApproval => "awaiting_approval",
            Stage::Executing => "executing",
            Stage::ValidatingResults => "validating_results",
            Stage::Summarizing => "summarizing",
            Stage::VerifyingAnswer => "verifying_answer",
            Stage::Completed => "completed",
            Stage::Partial => "partial",
            Stage::NoQualifyingSources => "no_qualifying_sources",
            Stage::Failed => "failed",
            Stage::Cancelled => "cancelled",
        }
    }
    pub fn terminal(self) -> bool {
        matches!(self, Stage::Completed | Stage::Partial | Stage::NoQualifyingSources | Stage::Failed | Stage::Cancelled)
    }
    /// Waiting for the person; no spinner and no background work.
    pub fn waiting_for_user(self) -> bool {
        matches!(self, Stage::NeedsClarification | Stage::AwaitingApproval)
    }
    /// Default localized template key for a stage-level event.
    pub fn message_key(self) -> String { format!("run.stage.{}", self.as_str()) }
    /// The run state machine. Cancellation and failure are reachable from
    /// every non-terminal stage; nothing leaves a terminal stage.
    pub fn can_transition(self, next: Stage) -> bool {
        use Stage::*;
        if self.terminal() { return false; }
        if matches!(next, Failed | Cancelled) { return true; }
        match self {
            Understanding => next == ValidatingIntent,
            ValidatingIntent => matches!(next, NeedsClarification | FindingSources),
            // An answer creates a new question revision; planning restarts.
            NeedsClarification => next == FindingSources,
            FindingSources => matches!(next, FilteringSources | NoQualifyingSources),
            FilteringSources => matches!(next, RankingSources | NoQualifyingSources),
            RankingSources => matches!(next, BuildingPlan | NoQualifyingSources),
            BuildingPlan => matches!(next, AwaitingApproval | NeedsClarification | NoQualifyingSources),
            // A replan (expired quote or changed catalog) creates a new plan revision.
            AwaitingApproval => matches!(next, Executing | BuildingPlan),
            Executing => next == ValidatingResults,
            // Nothing usable returned: partial answer with explicit gaps.
            ValidatingResults => matches!(next, Summarizing | Partial),
            Summarizing => next == VerifyingAnswer,
            // One bounded repair goes back to Summarizing.
            VerifyingAnswer => matches!(next, Summarizing | Completed | Partial),
            Completed | Partial | NoQualifyingSources | Failed | Cancelled => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventStatus { Running, Completed, Waiting, Failed, Cancelled }

/// Real backend counts only; absent means unknown, never zero. Candidate,
/// selected-source, call and result counts stay distinct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Counts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidates_checked: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidates_total: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eligible_candidates: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualifying_candidates: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_sources: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calls_total: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calls_finished: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_accepted: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_excluded: Option<u32>,
}

/// User-safe error: a code and template key, never a raw provider error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicError {
    pub code: String,
    pub message_key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgressEvent {
    pub event_schema_version: u32,
    /// "{run_id}:{sequence}".
    pub event_id: String,
    pub run_id: Uuid,
    pub question_revision: u32,
    pub plan_revision: Option<u32>,
    /// Gapless per run, starting at 1.
    pub sequence: u64,
    pub timestamp: DateTime<Utc>,
    pub stage: Stage,
    pub status: EventStatus,
    pub message_key: String,
    #[serde(default)]
    pub message_args: BTreeMap<String, Value>,
    #[serde(default)]
    pub counts: Counts,
    /// catalog api_slug of a per-source event.
    pub source_id: Option<String>,
    /// resolver::Step::key() of a per-source event.
    pub step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_status: Option<SourceRowStatus>,
    /// Since the run was created; measured, never estimated.
    pub elapsed_ms: u64,
    pub error: Option<PublicError>,
}

pub fn event_id(run_id: Uuid, sequence: u64) -> String { format!("{run_id}:{sequence}") }

/// Parse a replay cursor: an event id of this run, or a bare sequence.
pub fn parse_cursor(run_id: Uuid, cursor: &str) -> Option<u64> {
    let sequence = match cursor.rsplit_once(':') {
        Some((run, sequence)) if run == run_id.to_string() => sequence,
        Some(_) => return None,
        None => cursor,
    };
    sequence.parse().ok()
}

/// The persisted log is gapless: the first event is 1, each next is last + 1.
/// `last` is 0 for an empty log.
pub fn check_append(last: u64, next: u64) -> Result<(), ContractError> {
    if last.checked_add(1) == Some(next) { Ok(()) } else { Err(ContractError::SequenceGap { last, next }) }
}

impl ProgressEvent {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.event_schema_version != EVENT_SCHEMA_VERSION { return Err(ContractError::InvalidEvent("schema version")); }
        if self.sequence == 0 { return Err(ContractError::InvalidEvent("sequence starts at 1")); }
        if self.event_id != event_id(self.run_id, self.sequence) { return Err(ContractError::InvalidEvent("event_id")); }
        if self.question_revision == 0 { return Err(ContractError::InvalidEvent("question revision starts at 1")); }
        if self.source_status.is_some() && self.step_id.is_none() { return Err(ContractError::InvalidEvent("source status without step")); }
        Ok(())
    }
}

/// How a client (or a reconnecting stream) treats an incoming event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Incoming {
    Apply,
    /// Already applied; ignore.
    Duplicate,
    /// Older question revision; ignore.
    StaleRevision,
    /// Missing range; recover it from GET .../events?after={from - 1}.
    Gap { from: u64, to: u64 },
}

/// Idempotent reducer rule shared with the App (ask-v2-run.ts documents it).
pub fn classify(last_applied: u64, current_question_revision: u32, event: &ProgressEvent) -> Incoming {
    if event.question_revision < current_question_revision { return Incoming::StaleRevision; }
    if event.sequence <= last_applied { return Incoming::Duplicate; }
    if event.sequence > last_applied + 1 { return Incoming::Gap { from: last_applied + 1, to: event.sequence - 1 }; }
    Incoming::Apply
}

/// Progress side channel for long Phase 2 steps (planning, synthesis). The
/// run module implements it by persisting the event in the same transaction
/// as the state it describes. Static dispatch keeps it Send without boxing.
pub trait ProgressSink: Send + Sync {
    fn emit(&self, update: StageUpdate) -> impl Future<Output = anyhow::Result<()>> + Send;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageUpdate {
    pub stage: Stage,
    pub status: EventStatus,
    #[serde(default)]
    pub counts: Counts,
    #[serde(default)]
    pub message_args: BTreeMap<String, Value>,
}

// ---------------------------------------------------------------------------
// Per-source execution rows
// ---------------------------------------------------------------------------

/// Spec §7 status set, plus `skipped` for a pre-approved fallback that was not
/// needed. HTTP 200 alone is `succeeded` only after evidence validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRowStatus {
    Validating,
    Queued,
    WaitingForPrerequisite,
    Running,
    Succeeded,
    NoMatchingRecords,
    Partial,
    TimedOut,
    Failed,
    Cancelled,
    Skipped,
}
impl SourceRowStatus {
    pub fn terminal(self) -> bool {
        !matches!(self, SourceRowStatus::Queued | SourceRowStatus::WaitingForPrerequisite | SourceRowStatus::Running)
    }
}

/// Charge state per call, from the existing ledger (Store::billing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChargeStatus { NotStarted, Reserved, Captured, Refunded, Released, PendingReconciliation }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceRow {
    pub step_id: String,
    pub source_id: String,
    pub source_name: String,
    pub status: SourceRowStatus,
    /// step_ids this row waits for.
    #[serde(default)]
    pub waiting_for: Vec<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub execution_id: Option<Uuid>,
    pub result_ref: Option<Uuid>,
    /// A response was saved; not the same as accepted evidence.
    pub response_received: bool,
    pub evidence_accepted: u32,
    pub evidence_excluded: u32,
    pub charge_status: ChargeStatus,
    pub charged_atomic: Amount,
    pub error: Option<PublicError>,
}

// ---------------------------------------------------------------------------
// Source plan and quote
// ---------------------------------------------------------------------------

/// Integer micro-USD serialized as a decimal string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Amount(pub i64);
impl Serialize for Amount {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> { serializer.serialize_str(&self.0.to_string()) }
}
impl<'de> Deserialize<'de> for Amount {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Amount::parse(&text).map_err(serde::de::Error::custom)
    }
}
impl Amount {
    /// Non-negative decimal digits only: no sign, decimals or exponents.
    pub fn parse(text: &str) -> Result<Self, ContractError> {
        if text.is_empty() || text.len() > 18 || !text.bytes().all(|c| c.is_ascii_digit()) { return Err(ContractError::InvalidAmount); }
        text.parse().map(Amount).map_err(|_| ContractError::InvalidAmount)
    }
}

/// One API's maximum for this plan: provider price plus Apiosk fee, for all
/// its bounded calls (catalog::Service::total_with × max_calls).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostBreakdown {
    pub provider_atomic: Amount,
    pub fee_atomic: Amount,
    pub total_atomic: Amount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRole {
    /// Covers requested fields.
    Primary,
    /// Supplies an input another selected source needs.
    Dependency,
    /// Quoted contingency (resolver::Step::browser_fallback); within the cap.
    PreapprovedFallback,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignment {
    pub request_id: String,
    pub subject_id: String,
    pub fields: FieldScope,
}

/// `step_id` produces `fact_type`, which this source requires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyBinding {
    pub step_id: String,
    pub fact_type: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedSource {
    /// resolver::Step::key() = "{contract_id}:{entity_ref}".
    pub step_id: String,
    /// Index into the saved resolver::Plan::steps (same order).
    pub step_index: u32,
    pub source_id: String,
    pub source_name: String,
    pub endpoint_id: Uuid,
    pub contract_id: Uuid,
    pub capability_slug: String,
    pub role: SourceRole,
    /// Selected sources always qualified; never null here.
    pub fit_score: FitScore,
    pub selection_reasons: Vec<String>,
    pub assignments: Vec<Assignment>,
    pub depends_on: Vec<DependencyBinding>,
    /// 0 = dispatched immediately; n waits for groups < n.
    pub execution_group: u32,
    /// Bounded fan-out × pagination.
    pub max_calls: u32,
    pub cost: CostBreakdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    /// Every required field of every request is covered.
    Complete,
    /// Explicit partial plan; required gaps are listed before approval.
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapReason {
    /// Unresolved in intent grounding (undocumented field name).
    Undocumented,
    /// Unresolved in intent grounding (more than one canonical field).
    Ambiguous,
    /// No eligible candidate covers it.
    NoEligibleSource,
    /// Eligible candidates exist; none reached the threshold.
    BelowThreshold,
    /// Needed input is neither literal nor reachable through a bounded dependency.
    MissingInput,
    /// A requested source is not available for this capability.
    SourceUnavailable,
    /// The capability is a draft or has no admitted contract.
    CapabilityNotExecutable,
    /// The request is an external action, outside source planning.
    UnsupportedKind,
    /// Search bound reached before a covering combination was found.
    SearchBound,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequirementGap {
    pub request_id: String,
    pub subject_id: Option<String>,
    /// Canonical id, or the original name for an unresolved field; None = whole request.
    pub field: Option<String>,
    pub required: bool,
    pub reason: GapReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Coverage {
    pub covered_required: Vec<Assignment>,
    pub covered_optional: Vec<Assignment>,
    pub required_gaps: Vec<RequirementGap>,
    pub optional_gaps: Vec<RequirementGap>,
}

/// Subject id from the intent ↔ the Task entity the worker binds inputs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectBinding {
    pub subject_id: String,
    pub entity_ref: Uuid,
    pub label: String,
}

/// The authoritative quote is the saved resolver::Plan; these numbers repeat it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanQuote {
    /// resolver::Plan::quote_ref (authorizations.quote_ref).
    pub quote_ref: Uuid,
    /// resolver::Plan::hash (authorizations.plan_hash).
    pub plan_hash: String,
    pub currency: String,
    pub fee_bps: i64,
    pub pricing_rule: String,
    pub provider_atomic: Amount,
    pub fee_atomic: Amount,
    /// = resolver::Plan::cap_atomic; the single amount the person approves.
    pub total_atomic: Amount,
    /// Usage can be lower than the cap; show "maximum", never "price".
    pub variable: bool,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState { Awaiting, Approved, Expired, Superseded }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Approval {
    pub state: ApprovalState,
    pub approved_at: Option<DateTime<Utc>>,
    /// authorizations.id
    pub authorization_ref: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SearchReport {
    pub combinations_evaluated: u32,
    /// True: the best valid plan found is reported, not a global optimum.
    pub bound_reached: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourcePlan {
    pub source_plan_version: u32,
    pub run_id: Uuid,
    pub plan_id: Uuid,
    pub plan_revision: u32,
    pub question_revision: u32,
    pub intent_schema_version: u32,
    /// catalog::Catalog::version the plan was matched against.
    pub catalog_version: String,
    pub field_catalog_version: String,
    pub score_policy: ScorePolicy,
    pub status: PlanStatus,
    pub subjects: Vec<SubjectBinding>,
    pub sources: Vec<PlannedSource>,
    pub coverage: Coverage,
    pub quote: PlanQuote,
    pub approval: Approval,
    pub candidates: Counts,
    pub search: SearchReport,
    /// Only from measurements; never an invented ETA.
    pub expected_latency_ms: Option<u64>,
    pub latency_basis: Option<String>,
}

/// The quote the person sees must be internally consistent: every API's
/// provider + fee = its total, and the per-API totals add up to the one
/// approved total (which must equal resolver::Plan::cap_atomic).
pub fn check_quote_totals(plan: &SourcePlan) -> Result<(), ContractError> {
    let (mut provider, mut fee, mut total) = (0i64, 0i64, 0i64);
    for source in &plan.sources {
        let cost = &source.cost;
        if cost.provider_atomic.0.checked_add(cost.fee_atomic.0) != Some(cost.total_atomic.0) {
            return Err(ContractError::QuoteMismatch("source provider + fee != total"));
        }
        provider = provider.checked_add(cost.provider_atomic.0).ok_or(ContractError::InvalidAmount)?;
        fee = fee.checked_add(cost.fee_atomic.0).ok_or(ContractError::InvalidAmount)?;
        total = total.checked_add(cost.total_atomic.0).ok_or(ContractError::InvalidAmount)?;
    }
    let quote = &plan.quote;
    if quote.provider_atomic.0 != provider || quote.fee_atomic.0 != fee { return Err(ContractError::QuoteMismatch("breakdown")); }
    if quote.total_atomic.0 != total { return Err(ContractError::QuoteMismatch("total != sum of sources")); }
    Ok(())
}

// ---------------------------------------------------------------------------
// Clarification
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClarificationOption {
    pub option_id: String,
    pub label: String,
    pub detail: Option<String>,
}

/// One focused question. Answers apply to the saved planner input; they never
/// trigger another extraction model call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClarificationRequest {
    pub clarification_id: String,
    pub question_revision: u32,
    pub reason: String,
    pub message_key: String,
    #[serde(default)]
    pub message_args: BTreeMap<String, Value>,
    pub request_id: Option<String>,
    pub subject_id: Option<String>,
    #[serde(default)]
    pub options: Vec<ClarificationOption>,
    /// A free literal value instead of options (e.g. a registration number).
    pub value_input: Option<ValueInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValueInput {
    /// Fact type the value is validated against (catalog vocabulary).
    pub fact_type: String,
    pub label: String,
    pub max_length: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClarificationAnswer {
    pub clarification_id: String,
    /// The revision the answer was given against (it creates revision + 1).
    pub question_revision: u32,
    pub option_id: Option<String>,
    pub value: Option<String>,
}

// ---------------------------------------------------------------------------
// Evidence and provenance
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldOutcome { Supported, Missing, Ambiguous, Conflicting, Stale, Invalid, SourceFailed }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus { Accepted, Excluded }

/// A name-only match stays a candidate unless evidence establishes identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityStatus { Confirmed, Candidate }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceRecord {
    pub evidence_version: u32,
    /// Server-issued, unique per run: "ev-{n}". Models cite only these.
    pub evidence_id: String,
    pub run_id: Uuid,
    pub plan_revision: u32,
    pub request_id: String,
    pub subject_id: String,
    pub field_id: String,
    pub value: Value,
    pub unit: Option<String>,
    pub currency: Option<String>,
    pub period: Option<String>,
    pub source_id: String,
    pub endpoint_id: Uuid,
    pub contract_id: Uuid,
    pub step_id: String,
    /// executions.id
    pub execution_id: Uuid,
    /// results.id (the saved response document).
    pub response_ref: Uuid,
    /// JSON Pointer into the saved results.document, including the row index.
    pub response_pointer: String,
    pub retrieved_at: DateTime<Utc>,
    pub source_record_date: Option<String>,
    /// Only a URL present in the saved response; never constructed.
    pub source_url: Option<String>,
    pub identity: IdentityStatus,
    pub validation_status: ValidationStatus,
    pub outcome: FieldOutcome,
    pub exclusion_reason: Option<String>,
    /// Set when saved evidence of an earlier run was reused (original time kept).
    pub reused_from_run: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldOutcomeRecord {
    pub request_id: String,
    pub subject_id: String,
    pub field_id: String,
    pub required: bool,
    pub outcome: FieldOutcome,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Synthesis output
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    /// Must cite accepted evidence.
    Factual,
    /// States missing information; cites nothing or the failed/excluded context.
    Gap,
    /// States disagreement; cites every conflicting evidence id.
    Conflict,
    /// Framing without factual content (no numbers, names or dates).
    Note,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub claim_id: String,
    pub kind: ClaimKind,
    pub text: String,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    /// Structured values stated in `text`, each compared to its evidence.
    #[serde(default)]
    pub values: Vec<ClaimValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimValue {
    pub evidence_id: String,
    pub value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerBlockKind { Paragraph, List, Table }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableCell {
    pub text: String,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnswerTable {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<TableCell>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnswerBlock {
    pub block_id: String,
    pub kind: AnswerBlockKind,
    #[serde(default)]
    pub claims: Vec<Claim>,
    pub table: Option<AnswerTable>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SynthesisStatus {
    /// First model answer passed verification.
    Verified,
    /// The one bounded repair passed verification.
    Repaired,
    /// Model output stayed invalid (or no model): validated facts and gaps only.
    DeterministicFallback,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ModelUsage {
    /// None = unknown, never reported as zero.
    pub cost_usd: Option<f64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

/// Normal path: one extraction and one synthesis call. Extra attempts are counted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ModelCalls {
    pub extraction: u32,
    pub synthesis: u32,
    pub repair: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CitedSource {
    pub source_id: String,
    pub source_name: String,
    pub endpoint_id: Uuid,
    pub source_record_date: Option<String>,
    pub retrieved_at: DateTime<Utc>,
    pub evidence_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SynthesisOutput {
    pub synthesis_version: u32,
    pub run_id: Uuid,
    pub plan_revision: u32,
    pub model: String,
    pub language: String,
    pub status: SynthesisStatus,
    pub blocks: Vec<AnswerBlock>,
    /// Required and optional gaps, from field outcomes and plan gaps.
    pub gaps: Vec<FieldOutcomeRecord>,
    /// Only sources with accepted, cited evidence.
    pub sources: Vec<CitedSource>,
    pub calls: ModelCalls,
    pub usage: ModelUsage,
    /// Claims removed by verification (claim_id → reason code).
    #[serde(default)]
    pub rejected_claims: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CitationIssue {
    FactualWithoutEvidence { claim_id: String },
    UnknownEvidence { claim_id: String, evidence_id: String },
    ExcludedEvidence { claim_id: String, evidence_id: String },
}

/// Necessary, not sufficient: every factual claim and table cell cites only
/// existing accepted evidence. Value and subject checks are synthesis's job.
pub fn citation_issues(output: &SynthesisOutput, evidence: &[EvidenceRecord]) -> Vec<CitationIssue> {
    let status: BTreeMap<&str, ValidationStatus> = evidence.iter().map(|e| (e.evidence_id.as_str(), e.validation_status)).collect();
    let mut issues = Vec::new();
    let mut check = |claim_id: &str, ids: &[String]| {
        for id in ids {
            match status.get(id.as_str()) {
                None => issues.push(CitationIssue::UnknownEvidence { claim_id: claim_id.into(), evidence_id: id.clone() }),
                Some(ValidationStatus::Excluded) => issues.push(CitationIssue::ExcludedEvidence { claim_id: claim_id.into(), evidence_id: id.clone() }),
                Some(ValidationStatus::Accepted) => {}
            }
        }
    };
    for block in &output.blocks {
        for claim in &block.claims {
            check(&claim.claim_id, &claim.evidence_ids);
            let values: Vec<String> = claim.values.iter().map(|v| v.evidence_id.clone()).collect();
            check(&claim.claim_id, &values);
        }
        for (row, cells) in block.table.iter().flat_map(|t| t.rows.iter().enumerate()) {
            for (column, cell) in cells.iter().enumerate() {
                check(&format!("{}:{row}:{column}", block.block_id), &cell.evidence_ids);
            }
        }
    }
    for block in &output.blocks {
        for claim in block.claims.iter().filter(|c| c.kind == ClaimKind::Factual && c.evidence_ids.is_empty()) {
            issues.push(CitationIssue::FactualWithoutEvidence { claim_id: claim.claim_id.clone() });
        }
    }
    issues
}

// ---------------------------------------------------------------------------
// HTTP: /v2/ask-v2/runs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartRunRequest {
    /// Idempotency key: the same id and question return the same run.
    pub request_id: Uuid,
    pub question: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApproveRunRequest {
    pub plan_revision: u32,
    pub quote_ref: Uuid,
    /// Must equal the displayed total exactly (executor approve rule).
    pub max_total_atomic: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CancelRunRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplanRunRequest {
    pub plan_revision: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClarifyRunRequest {
    pub question_revision: u32,
    pub clarification_id: String,
    pub option_id: Option<String>,
    pub value: Option<String>,
}

/// What the current person may do now; computed by the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RunActions {
    pub approve: bool,
    pub cancel: bool,
    pub replan: bool,
    pub clarify: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSnapshot {
    pub run_contract_version: u32,
    pub run_id: Uuid,
    pub request_id: Uuid,
    pub question: String,
    pub language: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub stage: Stage,
    pub terminal: bool,
    pub question_revision: u32,
    pub plan_revision: Option<u32>,
    /// Cursor for GET .../events?after= and .../stream?after=.
    pub last_sequence: u64,
    /// The newest stage-level event, for the one-line progress display.
    pub latest_event: Option<ProgressEvent>,
    pub clarification: Option<ClarificationRequest>,
    pub plan: Option<SourcePlan>,
    /// Requirement gaps when no plan exists (no_qualifying_sources); with a
    /// plan, its coverage lists the gaps instead.
    #[serde(default)]
    pub gaps: Vec<RequirementGap>,
    #[serde(default)]
    pub sources: Vec<SourceRow>,
    /// Existing Store::billing view for the plan's quote, unchanged shape.
    pub billing: Option<Value>,
    #[serde(default)]
    pub evidence: Vec<EvidenceRecord>,
    #[serde(default)]
    pub field_outcomes: Vec<FieldOutcomeRecord>,
    pub answer: Option<SynthesisOutput>,
    pub error: Option<PublicError>,
    pub actions: RunActions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventPage {
    pub run_id: Uuid,
    pub events: Vec<ProgressEvent>,
    pub last_sequence: u64,
    /// More events exist after the last one returned.
    pub more: bool,
    pub stage: Stage,
    pub terminal: bool,
}

/// SSE `heartbeat` data: connection health only; never advances progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunHeartbeat {
    pub at: DateTime<Utc>,
    pub last_sequence: u64,
}

/// SSE `stream_end` data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunStreamEnd {
    pub reason: crate::protocol::StreamEndReason,
    pub last_sequence: u64,
}

pub const STREAM_PROTOCOL: &str = "ask-run-events-1";

/// Every run error code and its HTTP status; the body is server::failure's
/// `errors[0].{code,message}` shape the App already reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunErrorCode {
    InvalidRunRequest,
    InvalidQuestion,
    RunUnavailable,
    IdempotencyConflict,
    RunTerminal,
    RunNotAwaitingApproval,
    PlanRevisionChanged,
    QuoteMismatch,
    QuoteExpired,
    ApprovalRefused,
    ClarificationStale,
    InvalidClarification,
    ReplanBlocked,
    Busy,
    RateLimited,
    IntentNotConfigured,
    CatalogUnavailable,
    RunStorageUnavailable,
}
impl RunErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRunRequest => "invalid_run_request",
            Self::InvalidQuestion => "invalid_question",
            Self::RunUnavailable => "run_unavailable",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::RunTerminal => "run_terminal",
            Self::RunNotAwaitingApproval => "run_not_awaiting_approval",
            Self::PlanRevisionChanged => "plan_revision_changed",
            Self::QuoteMismatch => "quote_mismatch",
            Self::QuoteExpired => "quote_expired",
            Self::ApprovalRefused => "approval_refused",
            Self::ClarificationStale => "clarification_stale",
            Self::InvalidClarification => "invalid_clarification",
            Self::ReplanBlocked => "replan_blocked",
            Self::Busy => "busy",
            Self::RateLimited => "rate_limited",
            Self::IntentNotConfigured => "intent_not_configured",
            Self::CatalogUnavailable => "catalog_unavailable",
            Self::RunStorageUnavailable => "run_storage_unavailable",
        }
    }
    pub fn http_status(self) -> u16 {
        match self {
            Self::InvalidRunRequest | Self::InvalidQuestion | Self::InvalidClarification => 400,
            Self::ApprovalRefused => 403,
            Self::RunUnavailable => 404,
            Self::IdempotencyConflict | Self::RunTerminal | Self::RunNotAwaitingApproval | Self::PlanRevisionChanged
            | Self::QuoteMismatch | Self::QuoteExpired | Self::ClarificationStale | Self::ReplanBlocked => 409,
            Self::Busy | Self::RateLimited => 429,
            Self::IntentNotConfigured | Self::CatalogUnavailable | Self::RunStorageUnavailable => 503,
        }
    }
}

// ---------------------------------------------------------------------------
// In-process interfaces between Phase 2 modules (not HTTP)
// ---------------------------------------------------------------------------

/// What the run module saves into the run's existing gateway_v2.tasks row so
/// the existing worker, authorization, reservation and ledger path executes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionSeed {
    /// Becomes Task::proposal. Same hash/cap/expiry rules as resolver::resolve.
    pub plan: crate::resolver::Plan,
    /// One per SubjectBinding.
    pub entities: Vec<crate::state::Entity>,
    /// Literal identifiers as user_asserted facts (engine::input::user_fact rules).
    pub facts: Vec<crate::state::Fact>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageTrace {
    pub stage: Stage,
    pub elapsed_ms: u64,
    pub counts: Counts,
}

/// Measured planner stages; the run module turns them into events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PlannerTrace {
    pub stages: Vec<StageTrace>,
}

#[derive(Debug, Clone)]
pub enum PlanOutcome {
    Ready { plan: Box<SourcePlan>, seed: Box<ExecutionSeed>, scores: Vec<ScoreRecord>, trace: PlannerTrace },
    NeedsClarification { clarification: Box<ClarificationRequest>, scores: Vec<ScoreRecord>, trace: PlannerTrace },
    NoQualifyingSources { gaps: Vec<RequirementGap>, scores: Vec<ScoreRecord>, trace: PlannerTrace },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn threshold() -> ActiveThreshold { ActiveThreshold::default() }

    #[test]
    fn scores_must_be_finite_and_within_zero_to_one_hundred() {
        for valid in [0.0, 42.5, 90.0, 100.0] { assert!(FitScore::new(valid).is_ok(), "{valid}"); }
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.01, 100.01, 1000.0] {
            assert_eq!(FitScore::new(invalid), Err(ContractError::InvalidScore), "{invalid}");
        }
        assert!(serde_json::from_value::<FitScore>(json!(150)).is_err());
        assert!(serde_json::from_value::<FitScore>(json!(-1)).is_err());
        assert!(serde_json::from_value::<FitScore>(json!("95")).is_err());
        assert_eq!(serde_json::from_value::<Option<FitScore>>(json!(null)).unwrap(), None);
    }

    #[test]
    fn the_threshold_uses_the_unrounded_score() {
        assert!(!qualifies(Some(89.99), threshold()));
        assert!(!qualifies(Some(89.999_999_9), threshold()));
        assert!(qualifies(Some(90.0), threshold()));
        assert!(qualifies(Some(100.0), threshold()));
        let raised = ActiveThreshold::new(95.5).unwrap();
        assert!(!qualifies(Some(95.49), raised));
        assert!(qualifies(Some(95.5), raised));
    }

    #[test]
    fn nan_null_and_out_of_range_scores_never_pass() {
        for raw in [None, Some(f64::NAN), Some(f64::INFINITY), Some(100.5), Some(1000.0), Some(-5.0)] {
            assert!(!qualifies(raw, threshold()), "{raw:?}");
        }
        assert_eq!(threshold().decide(None, true), ThresholdDecision::Unscorable);
    }

    #[test]
    fn a_failed_gate_excludes_regardless_of_score() {
        let perfect = FitScore::new(100.0).unwrap();
        assert_eq!(threshold().decide(Some(perfect), false), ThresholdDecision::GateFailed);
        assert_eq!(threshold().decide(Some(perfect), true), ThresholdDecision::Qualifies);
        assert_eq!(threshold().decide(Some(FitScore::new(89.99).unwrap()), true), ThresholdDecision::BelowThreshold);
    }

    #[test]
    fn the_active_threshold_can_never_be_configured_below_ninety() {
        for invalid in [89.99, 0.0, 50.0, -90.0, 100.01, f64::NAN, f64::INFINITY] {
            assert_eq!(ActiveThreshold::new(invalid), Err(ContractError::InvalidThreshold), "{invalid}");
        }
        assert_eq!(ActiveThreshold::default().get(), 90.0);
        assert!(ActiveThreshold::new(90.0).is_ok() && ActiveThreshold::new(100.0).is_ok());
        assert!(serde_json::from_value::<ActiveThreshold>(json!(85)).is_err(), "a stored policy cannot lower it either");
        assert!(serde_json::from_value::<ScorePolicy>(json!({"score_version":"fit-heuristic-v1","calibration_status":"uncalibrated_heuristic","mode":"enforced","threshold":89.9})).is_err());
    }

    #[test]
    fn score_records_must_agree_with_their_gates_and_threshold() {
        let mut record = ScoreRecord {
            score_record_version: SCORE_RECORD_VERSION, candidate_id: "r1:s1:c".into(), request_id: "r1".into(),
            subject_ids: vec!["s1".into()], contract_id: Uuid::nil(), endpoint_id: Uuid::nil(), source_id: "kvk".into(),
            fit_score: Some(FitScore::new(91.0).unwrap()), score_version: "fit-heuristic-v1".into(),
            calibration_status: CalibrationStatus::UncalibratedHeuristic, mode: ScoreMode::Enforced, threshold: threshold(),
            decision: ThresholdDecision::Qualifies, signals: vec![], gates: vec![GateResult { gate: Gate::Capability, passed: true, reason: "exact".into() }],
            assigned: FieldScope::default(), gaps: FieldScope::default(),
        };
        assert!(record.consistent());
        record.gates.push(GateResult { gate: Gate::SubjectFit, passed: false, reason: "jurisdiction_mismatch".into() });
        assert!(!record.consistent(), "a failed gate cannot be stored as qualifying");
        let mut stored = serde_json::to_value(&record).unwrap();
        stored["fit_score"] = json!(120);
        assert!(serde_json::from_value::<ScoreRecord>(stored).is_err());
    }

    fn event(sequence: u64, question_revision: u32) -> ProgressEvent {
        let run_id = Uuid::from_u128(7);
        ProgressEvent {
            event_schema_version: EVENT_SCHEMA_VERSION, event_id: event_id(run_id, sequence), run_id, question_revision,
            plan_revision: None, sequence, timestamp: Utc::now(), stage: Stage::FilteringSources, status: EventStatus::Running,
            message_key: Stage::FilteringSources.message_key(), message_args: BTreeMap::new(),
            counts: Counts { candidates_checked: Some(8), candidates_total: Some(12), eligible_candidates: Some(3), ..Counts::default() },
            source_id: None, step_id: None, source_status: None, elapsed_ms: 840, error: None,
        }
    }

    #[test]
    fn event_sequences_are_gapless_and_monotonic_per_run() {
        assert!(check_append(0, 1).is_ok());
        assert!(check_append(16, 17).is_ok());
        for (last, next) in [(0, 0), (0, 2), (17, 17), (17, 16), (17, 19), (u64::MAX, 0)] {
            assert_eq!(check_append(last, next), Err(ContractError::SequenceGap { last, next }));
        }
        assert!(event(17, 1).validate().is_ok());
        let mut wrong = event(17, 1);
        wrong.event_id = event_id(wrong.run_id, 16);
        assert!(wrong.validate().is_err());
        assert!(event(0, 1).validate().is_err());
        let mut orphan = event(3, 1);
        orphan.source_status = Some(SourceRowStatus::Running);
        assert!(orphan.validate().is_err());
    }

    #[test]
    fn reducers_ignore_duplicates_and_old_question_revisions_and_report_gaps() {
        assert_eq!(classify(16, 1, &event(17, 1)), Incoming::Apply);
        assert_eq!(classify(17, 1, &event(17, 1)), Incoming::Duplicate);
        assert_eq!(classify(17, 1, &event(3, 1)), Incoming::Duplicate);
        assert_eq!(classify(10, 2, &event(11, 1)), Incoming::StaleRevision);
        assert_eq!(classify(10, 1, &event(14, 1)), Incoming::Gap { from: 11, to: 13 });
        let run = Uuid::from_u128(7);
        assert_eq!(parse_cursor(run, &event_id(run, 17)), Some(17));
        assert_eq!(parse_cursor(run, "17"), Some(17));
        assert_eq!(parse_cursor(run, &event_id(Uuid::from_u128(8), 17)), None, "another run's cursor");
        assert_eq!(parse_cursor(run, "x"), None);
    }

    #[test]
    fn the_spec_event_example_round_trips() {
        let run = Uuid::from_u128(7);
        let example = json!({"event_schema_version":1,"event_id":event_id(run,17),"run_id":run,"question_revision":1,"plan_revision":null,
            "sequence":17,"timestamp":"2026-10-12T20:10:00Z","stage":"filtering_sources","status":"running","message_key":"planner.filtering_sources",
            "message_args":{},"counts":{"candidates_checked":8,"candidates_total":12,"eligible_candidates":3},"source_id":null,"step_id":null,"elapsed_ms":840,"error":null});
        let parsed: ProgressEvent = serde_json::from_value(example.clone()).unwrap();
        assert!(parsed.validate().is_ok());
        assert_eq!(serde_json::to_value(&parsed).unwrap(), example);
    }

    #[test]
    fn stages_follow_the_run_state_machine() {
        use Stage::*;
        for stage in Stage::ALL {
            assert_eq!(serde_json::to_value(stage).unwrap(), json!(stage.as_str()));
            if stage.terminal() {
                assert!(Stage::ALL.iter().all(|next| !stage.can_transition(*next)), "{stage:?} is final");
            } else {
                assert!(stage.can_transition(Cancelled) && stage.can_transition(Failed), "{stage:?}");
            }
        }
        assert!(AwaitingApproval.can_transition(Executing));
        assert!(!BuildingPlan.can_transition(Executing), "no dispatch before approval");
        assert!(!FindingSources.can_transition(AwaitingApproval));
        assert!(VerifyingAnswer.can_transition(Summarizing), "one bounded repair");
        assert!(!Executing.can_transition(Completed), "results are validated first");
        assert!(NeedsClarification.waiting_for_user() && AwaitingApproval.waiting_for_user());
    }

    #[test]
    fn amounts_are_exact_decimal_strings() {
        assert_eq!(serde_json::to_value(Amount(230_000)).unwrap(), json!("230000"));
        assert_eq!(serde_json::from_value::<Amount>(json!("0")).unwrap(), Amount(0));
        for invalid in [json!("-1"), json!("1.5"), json!("1e3"), json!(""), json!(230000), json!("9999999999999999999")] {
            assert!(serde_json::from_value::<Amount>(invalid.clone()).is_err(), "{invalid}");
        }
    }

    fn plan(costs: &[(i64, i64, i64)], quote: (i64, i64, i64)) -> SourcePlan {
        let source = |index: usize, (provider, fee, total): (i64, i64, i64)| PlannedSource {
            step_id: format!("{}:{}", Uuid::from_u128(index as u128), Uuid::nil()), step_index: index as u32, source_id: "kvk".into(),
            source_name: "KVK".into(), endpoint_id: Uuid::nil(), contract_id: Uuid::from_u128(index as u128), capability_slug: "company.profile".into(),
            role: SourceRole::Primary, fit_score: FitScore::new(92.0).unwrap(), selection_reasons: vec![], assignments: vec![], depends_on: vec![],
            execution_group: 0, max_calls: 1, cost: CostBreakdown { provider_atomic: Amount(provider), fee_atomic: Amount(fee), total_atomic: Amount(total) },
        };
        SourcePlan {
            source_plan_version: SOURCE_PLAN_VERSION, run_id: Uuid::nil(), plan_id: Uuid::nil(), plan_revision: 1, question_revision: 1,
            intent_schema_version: INTENT_SCHEMA_VERSION, catalog_version: "c".into(), field_catalog_version: "c".into(),
            score_policy: ScorePolicy { score_version: "fit-heuristic-v1".into(), calibration_status: CalibrationStatus::UncalibratedHeuristic, mode: ScoreMode::Enforced, threshold: threshold() },
            status: PlanStatus::Complete, subjects: vec![], sources: costs.iter().enumerate().map(|(i, c)| source(i, *c)).collect(),
            coverage: Coverage::default(),
            quote: PlanQuote { quote_ref: Uuid::nil(), plan_hash: "h".into(), currency: "USD".into(), fee_bps: 1500, pricing_rule: "provider_cost_plus_15_v1".into(),
                provider_atomic: Amount(quote.0), fee_atomic: Amount(quote.1), total_atomic: Amount(quote.2), variable: false, expires_at: Utc::now() },
            approval: Approval { state: ApprovalState::Awaiting, approved_at: None, authorization_ref: None },
            candidates: Counts::default(), search: SearchReport::default(), expected_latency_ms: None, latency_basis: None,
        }
    }

    #[test]
    fn the_single_approved_total_is_the_sum_of_every_selected_api() {
        assert!(check_quote_totals(&plan(&[(200_000, 30_000, 230_000), (3, 1, 4)], (200_003, 30_001, 230_004))).is_ok());
        assert!(check_quote_totals(&plan(&[(200_000, 30_000, 230_000), (3, 1, 4)], (200_000, 30_000, 230_000))).is_err(), "a per-call price is not the total");
        assert!(check_quote_totals(&plan(&[(200_000, 30_000, 230_001)], (200_000, 30_000, 230_001))).is_err());
        assert!(check_quote_totals(&plan(&[], (0, 0, 0))).is_ok());
    }

    fn evidence(id: &str, status: ValidationStatus) -> EvidenceRecord {
        EvidenceRecord {
            evidence_version: EVIDENCE_VERSION, evidence_id: id.into(), run_id: Uuid::nil(), plan_revision: 1, request_id: "r1".into(), subject_id: "s1".into(),
            field_id: "company.profile#/properties/legal_name".into(), value: json!("Example B.V."), unit: None, currency: None, period: None,
            source_id: "kvk".into(), endpoint_id: Uuid::nil(), contract_id: Uuid::nil(), step_id: "c:e".into(), execution_id: Uuid::nil(), response_ref: Uuid::nil(),
            response_pointer: "/data/legal_name".into(), retrieved_at: Utc::now(), source_record_date: None, source_url: None,
            identity: IdentityStatus::Confirmed, validation_status: status, outcome: FieldOutcome::Supported, exclusion_reason: None, reused_from_run: None,
        }
    }

    #[test]
    fn published_claims_cite_only_existing_accepted_evidence() {
        let claim = |id: &str, kind: ClaimKind, ids: &[&str]| Claim { claim_id: id.into(), kind, text: "t".into(), evidence_ids: ids.iter().map(|s| s.to_string()).collect(), values: vec![] };
        let output = SynthesisOutput {
            synthesis_version: SYNTHESIS_VERSION, run_id: Uuid::nil(), plan_revision: 1, model: SYNTHESIS_MODEL.into(), language: "en".into(),
            status: SynthesisStatus::Verified, gaps: vec![], sources: vec![], calls: ModelCalls { extraction: 1, synthesis: 1, repair: 0 }, usage: ModelUsage::default(),
            rejected_claims: BTreeMap::new(),
            blocks: vec![AnswerBlock { block_id: "b1".into(), kind: AnswerBlockKind::Paragraph, table: Some(AnswerTable { columns: vec!["Name".into()], rows: vec![vec![TableCell { text: "x".into(), evidence_ids: vec!["ev-9".into()] }]] }),
                claims: vec![claim("c1", ClaimKind::Factual, &["ev-1"]), claim("c2", ClaimKind::Factual, &[]), claim("c3", ClaimKind::Factual, &["ev-2"]), claim("c4", ClaimKind::Gap, &[])] }],
        };
        let issues = citation_issues(&output, &[evidence("ev-1", ValidationStatus::Accepted), evidence("ev-2", ValidationStatus::Excluded)]);
        assert_eq!(issues, vec![
            CitationIssue::ExcludedEvidence { claim_id: "c3".into(), evidence_id: "ev-2".into() },
            CitationIssue::UnknownEvidence { claim_id: "b1:0:0".into(), evidence_id: "ev-9".into() },
            CitationIssue::FactualWithoutEvidence { claim_id: "c2".into() },
        ]);
    }

    #[test]
    fn the_grounded_intent_response_deserializes_without_casting_to_parsed_request() {
        let response = json!({"schema_version":2,"model":"openai/gpt-oss-120b","timing":{"parse_ms":900},"attempts":1,"usage":{"cost_usd":null},
            "intent":{"language":"en","subjects":[{"id":"s1","type":"company","label":"Example","identifiers":{}}],
            "capabilities":[{"slug":"company.profile","name":"Company profile","description":"d","category":"company","status":"active","origin":"database"}],
            "requests":[{"id":"r1","capability_slug":"company.profile","subject_ids":["s1"],"kind":"lookup","goal":"Profile",
                "required_fields":["company.profile#/properties/legal_name"],"optional_fields":[],"filters":{},"sources":[],
                "field_resolution":"unresolved","unresolved_fields":{"required":[{"field":"registration_details","reason":"undocumented"}],"optional":[]}}],
            "output":{"format":"chat","operations":[]},
            "field_definitions":[{"id":"company.profile#/properties/legal_name","fact_type":"company.profile","schema_pointer":"/properties/legal_name"}],
            "field_catalog_version":"abc"}});
        let envelope: GroundedIntentEnvelope = serde_json::from_value(response.clone()).unwrap();
        assert_eq!(envelope.intent.requests[0].unresolved_fields.required[0].field, "registration_details");
        let mut unknown = response;
        unknown["intent"]["requests"][0]["coverage"] = json!("complete");
        assert!(serde_json::from_value::<GroundedIntentEnvelope>(unknown).is_err(), "unknown keys cannot smuggle coverage claims");
    }

    #[test]
    fn error_codes_have_stable_names_and_statuses() {
        assert_eq!(serde_json::to_value(RunErrorCode::QuoteExpired).unwrap(), json!(RunErrorCode::QuoteExpired.as_str()));
        assert_eq!(RunErrorCode::QuoteExpired.http_status(), 409);
        assert_eq!(RunErrorCode::RunUnavailable.http_status(), 404);
        assert_eq!(RunErrorCode::CatalogUnavailable.http_status(), 503);
    }
}
