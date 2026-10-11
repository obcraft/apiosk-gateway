use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub fn digest(value: &impl Serialize) -> String {
    hex::encode(Sha256::digest(
        serde_json::to_vec(value).expect("serializable contract"),
    ))
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct Focus {
    pub entity_refs: Vec<Uuid>,
    pub goal_refs: Vec<Uuid>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub schema_version: String,
    pub state_ref: Uuid,
    pub revision: i64,
    pub expires_at: DateTime<Utc>,
    pub focus: Focus,
    pub state_token: String,
}
pub struct Signer(Vec<u8>);
impl Signer {
    pub(crate) fn guest_signature(&self, payload: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).unwrap();
        mac.update(b"ask-guest-preview-v1\0");
        mac.update(payload);
        hex::encode(mac.finalize().into_bytes())
    }
    pub(crate) fn verify_guest(&self, payload: &[u8], signature: &str) -> anyhow::Result<()> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).unwrap();
        mac.update(b"ask-guest-preview-v1\0");
        mac.update(payload);
        mac.verify_slice(&hex::decode(signature)?).map_err(|_| anyhow::anyhow!("invalid guest preview signature"))
    }
    pub(crate) fn search_signature(&self, payload: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).unwrap();
        mac.update(b"ask-search-snapshot-v1\0");
        mac.update(payload);
        hex::encode(mac.finalize().into_bytes())
    }
    pub(crate) fn verify_search(&self, payload: &[u8], signature: &str) -> anyhow::Result<()> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).unwrap();
        mac.update(b"ask-search-snapshot-v1\0");
        mac.update(payload);
        mac.verify_slice(&hex::decode(signature)?).map_err(|_| anyhow::anyhow!("invalid search snapshot signature"))
    }
    pub fn events_signature(&self, owner: Uuid, task: Uuid, connection: Option<Uuid>, expires: i64) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).unwrap();
        mac.update(&serde_json::to_vec(&("task-events-v1", owner, task, connection, expires)).unwrap());
        hex::encode(mac.finalize().into_bytes())
    }
    pub fn verify_events(&self, owner: Uuid, task: Uuid, connection: Option<Uuid>, expires: i64, signature: &str) -> anyhow::Result<()> {
        anyhow::ensure!(expires > Utc::now().timestamp(), "event link expired");
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).unwrap();
        mac.update(&serde_json::to_vec(&("task-events-v1", owner, task, connection, expires)).unwrap());
        mac.verify_slice(&hex::decode(signature)?).map_err(|_| anyhow::anyhow!("invalid event signature"))
    }
    pub fn research_signature(&self, owner: Uuid, task: Uuid, quote: Uuid, snapshot: &str, expires: i64) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).unwrap();
        mac.update(&serde_json::to_vec(&("research-report-v1",owner,task,quote,snapshot,expires)).unwrap());
        hex::encode(mac.finalize().into_bytes())
    }
    pub fn verify_research(&self, owner: Uuid, task: Uuid, quote: Uuid, snapshot: &str, expires: i64, signature: &str) -> anyhow::Result<()> {
        anyhow::ensure!(expires > Utc::now().timestamp(), "report link expired");
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).unwrap();
        mac.update(&serde_json::to_vec(&("research-report-v1",owner,task,quote,snapshot,expires)).unwrap());
        mac.verify_slice(&hex::decode(signature)?).map_err(|_|anyhow::anyhow!("invalid report signature"))
    }
    pub fn report_signature(&self, owner: Uuid, task: Uuid, result: Uuid, expires: i64) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).unwrap();
        mac.update(&serde_json::to_vec(&("annual-report-v1", owner, task, result, expires)).unwrap());
        hex::encode(mac.finalize().into_bytes())
    }
    pub fn verify_report(&self, owner: Uuid, task: Uuid, result: Uuid, expires: i64, signature: &str) -> anyhow::Result<()> {
        anyhow::ensure!(expires > Utc::now().timestamp(), "report link expired");
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).unwrap();
        mac.update(&serde_json::to_vec(&("annual-report-v1", owner, task, result, expires)).unwrap());
        mac.verify_slice(&hex::decode(signature)?).map_err(|_| anyhow::anyhow!("invalid report signature"))
    }
    /// A result's captured map image, for an inline <img>. Its own domain, so a
    /// report link cannot be replayed as an asset link or the other way round.
    pub fn asset_signature(&self, owner: Uuid, task: Uuid, result: Uuid, index: usize, expires: i64) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).unwrap();
        mac.update(&serde_json::to_vec(&("result-asset-v1", owner, task, result, index, expires)).unwrap());
        hex::encode(mac.finalize().into_bytes())
    }
    pub fn verify_asset(&self, owner: Uuid, task: Uuid, result: Uuid, index: usize, expires: i64, signature: &str) -> anyhow::Result<()> {
        anyhow::ensure!(expires > Utc::now().timestamp(), "asset link expired");
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).unwrap();
        mac.update(&serde_json::to_vec(&("result-asset-v1", owner, task, result, index, expires)).unwrap());
        mac.verify_slice(&hex::decode(signature)?).map_err(|_| anyhow::anyhow!("invalid asset signature"))
    }
    pub fn new(key: Vec<u8>) -> anyhow::Result<Self> {
        anyhow::ensure!(
            key.len() >= 32,
            "state signing key must be at least 32 bytes"
        );
        Ok(Self(key))
    }
    fn payload(owner: Uuid, envelope: &Envelope) -> Vec<u8> {
        serde_json::to_vec(&(
            owner,
            &envelope.schema_version,
            envelope.state_ref,
            envelope.revision,
            envelope.expires_at,
            &envelope.focus,
        ))
        .unwrap()
    }
    pub fn issue(&self, owner: Uuid, task: Uuid, revision: i64, focus: Focus) -> Envelope {
        let mut e = Envelope {
            schema_version: "2".into(),
            state_ref: task,
            revision,
            expires_at: Utc::now() + chrono::Duration::minutes(30),
            focus,
            state_token: String::new(),
        };
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).unwrap();
        mac.update(&Self::payload(owner, &e));
        e.state_token = hex::encode(mac.finalize().into_bytes());
        e
    }
    pub fn verify(&self, owner: Uuid, e: &Envelope, allow_expired: bool) -> anyhow::Result<()> {
        anyhow::ensure!(
            e.schema_version == "2" && e.revision >= 0,
            "invalid state version"
        );
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).unwrap();
        mac.update(&Self::payload(owner, e));
        mac.verify_slice(&hex::decode(&e.state_token)?)
            .map_err(|_| anyhow::anyhow!("invalid state signature"))?;
        anyhow::ensure!(
            allow_expired || e.expires_at > Utc::now(),
            "state expired; recover task"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub entity_ref: Uuid,
    pub label: String,
    pub selected: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fact {
    pub entity_ref: Uuid,
    pub semantic_type: String,
    pub value: Value,
    pub provenance: Value,
    pub observed_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub verification: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTurn {
    pub question: String,
    pub reply: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<TurnOutput>,
}
/// Read-only presentation of the plan that preceded the next message. Store a
/// result reference, not a duplicate payload or an expiring download URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnOutput {
    pub proposal: Option<Value>,
    pub result_ref: Option<Uuid>,
    #[serde(default)]
    pub result_refs: Vec<Uuid>,
    #[serde(default)]
    pub analysis: Option<Value>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntakeStage { Parsing, Planning }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntakeRecovery {
    pub question: String,
    pub context_delta: crate::protocol::ContextDelta,
    pub stage: IntakeStage,
    pub created_at: DateTime<Utc>,
}
impl IntakeRecovery {
    pub fn matches(&self, request: &crate::protocol::DiscoverRequest) -> bool {
        self.question == request.question && digest(&self.context_delta) == digest(&request.context_delta)
    }
    pub fn reusable(&self, intake: &crate::capability_intake::Intake, catalog_version: &str) -> bool {
        intake.catalog_version == catalog_version
            && DateTime::parse_from_rfc3339(&intake.parsed_request.meta.parsed_at).is_ok_and(|parsed| {
                let age = Utc::now().signed_duration_since(parsed);
                age >= chrono::Duration::zero() && age < chrono::Duration::minutes(5)
            })
    }
    pub fn view(&self) -> Value {
        serde_json::json!({"question":self.question,"context_delta":self.context_delta,"stage":self.stage,
            "message": "Processing paused because the planning service is temporarily unavailable. Your question is saved. Resume to continue, or edit your question."})
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Task {
    /// Paid analysis stores a verified answer on the task rather than a new
    /// source document. Retain its settled receipt when another turn starts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settled_compute: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub saved_answer: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compute: Option<crate::compute::Run>,
    /// A selected Bazaar purchase uses the existing balance ledger and treasury
    /// executor, without pretending an external URL is a reviewed catalog API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_purchase: Option<crate::ask_v2::purchase::Purchase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intake_recovery: Option<IntakeRecovery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_intake: Option<crate::capability_intake::Intake>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_selection: Option<crate::ask_v2::source_selection::Context>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<Uuid>,
    #[serde(default)]
    pub pending_dispatches: Vec<PendingDispatch>,
    #[serde(default)]
    pub conversation: Vec<ConversationTurn>,
    #[serde(default)]
    pub last_archived_result_ref: Option<Uuid>,
    #[serde(default)]
    pub conversation_timeline_version: u8,
    pub entities: Vec<Entity>,
    pub facts: Vec<Fact>,
    pub focus: Focus,
    pub intent: Option<Value>,
    pub actions: Vec<Value>,
    pub proposal: Option<Value>,
    pub completed: Vec<String>,
    #[serde(default)]
    pub skipped_steps: std::collections::BTreeSet<String>,
    pub result_refs: Vec<Uuid>,
    pub pending_candidates: Vec<Value>,
    /// Why the listed candidates are not every match; shown with them only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_note: Option<String>,
    pub original_subject: Option<Uuid>,
    pub cancelled: bool,
    #[serde(default)]
    pub execution_error: Option<Value>,
    #[serde(default)]
    pub active_results: Vec<Uuid>,
    #[serde(default)]
    pub failed_steps: std::collections::BTreeMap<String, Value>,
    #[serde(default)]
    pub analysis: Option<Value>,
    /// Invoice identifiers read by code from the current supplier-check question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supplier_check: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<crate::workflow_catalog::WorkflowContext>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingDispatch {
    pub step: usize,
    pub action_id: Uuid,
    pub inputs: Value,
}
impl Task {
    pub fn step_finished(&self, key: &str) -> bool {
        self.completed.iter().any(|done| done == key) || self.failed_steps.contains_key(key) || self.skipped_steps.contains(key)
    }
    pub fn record_turn(&mut self, question: &str, reply: Option<String>) {
        self.conversation_timeline_version = 1;
        self.conversation.push(ConversationTurn {
            question: question.to_owned(),
            reply,
            output: None,
        });
        let remove = self.conversation.len().saturating_sub(6);
        self.conversation.drain(..remove);
    }
    pub fn active_result_ref(&self) -> Option<Uuid> {
        self.result_refs.last().copied().filter(|result| {
            self.last_archived_result_ref != Some(*result) && !self.conversation.iter().any(|turn| {
                turn.output.as_ref().and_then(|output| output.result_ref) == Some(*result)
            })
        })
    }
    pub fn archive_output(&mut self, proposal: Option<Value>) {
        if let Some(run) = self.compute.as_ref().filter(|run| matches!(run.status.as_str(), "completed" | "unavailable")) {
            if !self.settled_compute.contains(&run.action_id) { self.settled_compute.push(run.action_id); }
        }
        let result_ref = self.active_result_ref();
        // A saved-only follow-up reuses existing result IDs and has no public
        // quote. Its new answer still belongs to this turn in the transcript.
        if proposal.is_none() && result_ref.is_none() && self.active_results.is_empty() { return; }
        if result_ref.is_some() { self.last_archived_result_ref = result_ref; }
        // Older /discover tasks can have a result without conversational text.
        if self.conversation.is_empty() { self.record_turn("", None); }
        if let Some(turn) = self.conversation.last_mut() {
            turn.output = Some(TurnOutput { proposal, result_ref, result_refs: std::mem::take(&mut self.active_results), analysis: self.analysis.take() });
        }
    }
    pub fn restore_legacy_output(&mut self, snapshot: &Value, result_ref: Uuid) {
        let Some(previous) = snapshot["conversation"].as_array() else { return; };
        // Only move a result when a saved response proves the exact preceding
        // conversation. Never guess from company names or an alleged year.
        if previous.is_empty() || previous.len() >= self.conversation.len() { return; }
        if !previous.iter().zip(&self.conversation).all(|(a, b)| {
            a["question"] == b.question && a["reply"] == serde_json::json!(b.reply)
        }) { return; }
        self.conversation[previous.len() - 1].output = Some(TurnOutput {
            proposal: snapshot.get("proposal").filter(|p| !p.is_null()).cloned(),
            result_ref: Some(result_ref),
            result_refs: vec![result_ref],
            analysis: None,
        });
        self.last_archived_result_ref = Some(result_ref);
    }
    /// Interpretation needs dialogue and identity, not rendered reports, prices,
    /// repeated source evidence or complete answer payloads. Keep all entity references
    /// so a smaller prompt cannot make an ambiguous pronoun appear unambiguous.
    pub fn parser_context(&self) -> Value {
        let answer = |analysis: &Value| {
            serde_json::json!({
                "status":analysis["status"],
                "observations":analysis["observations"].as_array().map(|items| items.iter().filter_map(|item| item["text"].as_str()).collect::<Vec<_>>()).unwrap_or_default(),
                "limitations":analysis["limitations"],
                "follow_up_question":analysis["follow_up_question"],
                "verdict":analysis.get("verdict").map(|verdict| serde_json::json!({"decision":verdict["decision"],"headline":verdict["headline"],"reasons":verdict["reasons"]})),
            })
        };
        let mut context = self.context_view();
        let object = context.as_object_mut().expect("context object");
        object.remove("analysis");
        object.remove("result_refs");
        object.remove("capability_intake");
        object.remove("source_selection");
        object.remove("intake_recovery");
        if let Some(turns) = object.get_mut("conversation").and_then(Value::as_array_mut) {
            for turn in turns {
                let summary = turn.pointer("/output/analysis").filter(|value| !value.is_null()).map(&answer);
                let turn = turn.as_object_mut().expect("conversation turn");
                turn.remove("output");
                if let Some(summary) = summary { turn.insert("answer".into(), summary); }
            }
        }
        // Follow-ups must retain the original retrieval requirements, even
        // though the presentation of the old quote has been omitted.
        object.insert("previous_intent".into(), self.intent.clone().unwrap_or(Value::Null));
        // Values/years in the last answer resolve "that amount" or "the year
        // before". Keep the validated prose, without repeating its evidence.
        if let Some(analysis) = &self.analysis { object.insert("previous_answer".into(), answer(analysis)); }
        context
    }
    pub fn context_view(&self) -> Value {
        serde_json::json!({"conversation":self.conversation,"entities":self.entities,"focus":self.focus,"capability_intake":self.capability_intake,"source_selection":self.source_selection.as_ref().map(|selection|selection.view()),"intake_recovery":self.intake_recovery.as_ref().map(IntakeRecovery::view),
            "known_facts":self.facts.iter().filter(|f| f.expires_at > Utc::now() && matches!(f.semantic_type.as_str(), "company.registration.nl.kvk" | "company_registry.kvknummer" | "company.kvknummer" | "company.name")).collect::<Vec<_>>(),
            "analysis":self.analysis,"result_refs":self.result_refs.iter().rev().take(8).collect::<Vec<_>>()})
    }
    pub fn bounded(&self) -> bool {
        self.conversation.len() <= 6
            && self.entities.len() <= 100
            && self.facts.len() <= 1000
            && self.actions.len() <= 24
            && self.result_refs.len() <= 100
            && serde_json::to_vec(self).is_ok_and(|b| b.len() <= 1024 * 1024)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn intake_reuse_expires_and_is_scoped_to_exact_question_context_and_catalog() {
        let question = "Company accounts for Example";
        let request = crate::protocol::DiscoverRequest { request_id: Uuid::new_v4(), question: question.into(), state: None, context_delta: Default::default() };
        let recovery = IntakeRecovery { question: question.into(), context_delta: Default::default(), stage: IntakeStage::Planning, created_at: Utc::now() };
        assert!(recovery.matches(&request));
        let mut changed = request.clone();
        changed.question.push('?');
        assert!(!recovery.matches(&changed));
        changed = request.clone();
        changed.context_delta.requested_focus_refs.push(Uuid::new_v4());
        assert!(!recovery.matches(&changed));
        changed = request.clone();
        changed.context_delta.workflow_slug = Some("another-workflow".into());
        assert!(!recovery.matches(&changed));
        let mut intake: crate::capability_intake::Intake = serde_json::from_value(serde_json::json!({
            "parsed_request":{"language":"en","subjects":[],"capabilities":[],"deliverable":{"format":"chat","operations":[]},
                "meta":{"model":"fixture","parsed_at":Utc::now().to_rfc3339(),"text_sha256":"fixture"}},
            "matches":[],"apiosk_sources_searched":37,"catalog_version":"v1","selected_capabilities":[]
        })).unwrap();
        assert!(recovery.reusable(&intake, "v1"));
        assert!(!recovery.reusable(&intake, "v2"));
        for parsed_at in [(Utc::now() - chrono::Duration::minutes(6)).to_rfc3339(), (Utc::now() + chrono::Duration::minutes(1)).to_rfc3339(), "invalid".into()] {
            intake.parsed_request.meta.parsed_at = parsed_at;
            assert!(!recovery.reusable(&intake, "v1"));
        }
        let task = Task { intake_recovery: Some(recovery), ..Task::default() };
        assert_eq!(task.context_view()["intake_recovery"]["question"], question);
        assert!(task.parser_context().get("intake_recovery").is_none(), "operational recovery messages are not user intent");
    }

    #[test]
    fn asset_links_are_scoped_to_one_image_and_never_accept_a_report_signature() {
        let signer = Signer::new(vec![7; 32]).unwrap();
        let (owner, task, result) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let expires = Utc::now().timestamp() + 60;
        let signature = signer.asset_signature(owner, task, result, 0, expires);
        assert!(signer.verify_asset(owner, task, result, 0, expires, &signature).is_ok());
        assert!(signer.verify_asset(owner, task, result, 1, expires, &signature).is_err());
        assert!(signer.verify_report(owner, task, result, expires, &signature).is_err());
        let report = signer.report_signature(owner, task, result, expires);
        assert!(signer.verify_asset(owner, task, result, 0, expires, &report).is_err());
        let expired = signer.asset_signature(owner, task, result, 0, Utc::now().timestamp() - 1);
        assert!(signer.verify_asset(owner, task, result, 0, Utc::now().timestamp() - 1, &expired).is_err());
    }
    #[test]
    fn parser_context_omits_presentation_but_preserves_followup_requirements() {
        let mut task = Task::default();
        let entity = Uuid::new_v4();
        task.entities.push(Entity { entity_ref: entity, label: "Example".into(), selected: true });
        task.focus.entity_refs.push(entity);
        task.intent = Some(serde_json::json!({"goals":[{"constraints":{"period":"2024","source":"kvk"}}]}));
        task.record_turn("Compare Example in 2024", Some("Which filing?".into()));
        task.conversation[0].output = Some(TurnOutput {
            proposal: Some(serde_json::json!({"steps":["large saved quote".repeat(1000)]})),
            result_ref: Some(Uuid::new_v4()), result_refs: vec![], analysis: task.intent.clone(),
        });
        task.analysis = Some(serde_json::json!({"status":"completed","observations":[{"text":"The filing reports EUR 500 in 2024.","evidence":[{"value":"large evidence".repeat(1000)}]}],"limitations":[],"follow_up_question":"Which filing topic do you mean?"}));
        let input = task.parser_context();
        assert_eq!(input["conversation"][0]["reply"], "Which filing?");
        assert_eq!(input["entities"][0]["entity_ref"], serde_json::json!(entity));
        assert_eq!(input["previous_intent"], task.intent.clone().unwrap());
        assert!(input["conversation"][0].get("output").is_none());
        assert!(input.get("analysis").is_none());
        assert!(input.get("result_refs").is_none());
        assert_eq!(input["previous_answer"]["observations"][0], "The filing reports EUR 500 in 2024.");
        assert_eq!(input["previous_answer"]["follow_up_question"], "Which filing topic do you mean?");
        assert!(!input.to_string().contains("large evidence"));
        assert!(input.to_string().len() * 10 < task.context_view().to_string().len());
        assert!(task.conversation[0].output.is_some(), "saved history is unchanged");
    }
    #[test]
    fn legacy_order_requires_an_exact_saved_conversation_prefix() {
        let mut task = Task::default();
        task.record_turn("Mollie accounts", Some("Fetching".into()));
        task.record_turn("Now 2022", Some("Which report?".into()));
        let result = Uuid::new_v4();
        let wrong = serde_json::json!({"conversation":[{"question":"Other company","reply":"Fetching"}],"proposal":null});
        task.restore_legacy_output(&wrong, result);
        assert!(task.conversation[0].output.is_none());
        let correct = serde_json::json!({"conversation":[{"question":"Mollie accounts","reply":"Fetching"}],"proposal":{"steps":["accounts"]}});
        task.restore_legacy_output(&correct, result);
        assert_eq!(task.conversation[0].output.as_ref().unwrap().result_ref, Some(result));
        assert!(task.conversation[1].output.is_none());
    }
    #[test]
    fn archives_results_once_and_does_not_reactivate_them_after_history_truncation() {
        let mut task = Task::default();
        task.record_turn("Mollie accounts", None);
        let first = Uuid::new_v4();
        task.result_refs.push(first);
        assert_eq!(task.active_result_ref(), Some(first));
        task.archive_output(Some(serde_json::json!({"steps":["accounts"]})));
        task.record_turn("The next report", None);
        assert_eq!(task.active_result_ref(), None);
        let restored: Task = serde_json::from_value(serde_json::to_value(&task).unwrap()).unwrap();
        assert_eq!(restored.conversation[0].output.as_ref().unwrap().result_ref, Some(first));
        assert!(restored.conversation[1].output.is_none());
        for _ in 0..6 { task.record_turn("More", None); }
        assert_eq!(task.active_result_ref(), None);
        let second = Uuid::new_v4();
        task.result_refs.push(second);
        assert_eq!(task.active_result_ref(), Some(second));
    }
    #[test]
    fn signed_state_is_account_and_focus_bound() {
        let s = Signer::new(vec![9; 32]).unwrap();
        let owner = Uuid::new_v4();
        let mut e = s.issue(owner, Uuid::new_v4(), 0, Focus::default());
        assert!(s.verify(owner, &e, false).is_ok());
        assert!(s.verify(Uuid::new_v4(), &e, false).is_err());
        e.focus.entity_refs.push(Uuid::new_v4());
        assert!(s.verify(owner, &e, false).is_err());
    }
    #[test]
    fn conversation_retains_six_recent_turns_and_reads_old_tasks() {
        let mut task = Task::default();
        for n in 0..10 {
            task.record_turn(&n.to_string(), Some("reply".into()));
        }
        assert_eq!(task.conversation.len(), 6);
        assert_eq!(task.conversation[0].question, "4");
        let mut legacy = serde_json::to_value(Task::default()).unwrap();
        legacy.as_object_mut().unwrap().remove("conversation");
        assert!(serde_json::from_value::<Task>(legacy)
            .unwrap()
            .conversation
            .is_empty());
    }
    #[test]
    fn context_is_bounded_by_focus_and_recent_results() {
        let task = Task {
            result_refs: (0..100).map(|_| Uuid::new_v4()).collect(),
            ..Task::default()
        };
        assert_eq!(
            task.context_view()["result_refs"].as_array().unwrap().len(),
            8
        );
    }
}
