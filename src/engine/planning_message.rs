//! A coverage failure ends discovery; it is not a request for invented input.
use crate::{protocol::Status, state::Task};
use serde_json::{json, Value};

pub(super) fn is_terminal_discovery(task: &Task, message: &str) -> bool {
    message == crate::parser::conversation_reply::NO_RESULTS
        || task.capability_intake.as_ref()
            .and_then(crate::parser::plan_coverage::discovery_message)
            .is_some_and(|sources| sources == message)
}

pub(super) fn finish_without_plan(task: &mut Task, diagnostics: Value) -> Status {
    let no_results = task.conversation.last().and_then(|turn| turn.reply.as_deref())
        == Some(crate::parser::conversation_reply::NO_RESULTS);
    let message = if no_results { crate::parser::conversation_reply::NO_RESULTS.into() } else { task.capability_intake.as_ref()
        .and_then(crate::parser::plan_coverage::discovery_message)
        .unwrap_or_else(|| crate::parser::conversation_reply::NO_RESULTS.into()) };
    task.proposal = None;
    task.actions.clear();
    let mut action = diagnostics;
    if !action.is_object() { action = json!({}); }
    action["kind"] = json!("unsupported_reason");
    action["message"] = json!(message);
    task.actions.push(action);
    if let Some(turn) = task.conversation.last_mut() { turn.reply = Some(message); }
    Status::Unsupported
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discovery_task() -> Task {
        let intake = serde_json::from_value(json!({
            "parsed_request":{"language":"en","subjects":[],
                "capabilities":[{"slug":"research.records","name":"Records","description":"Find records",
                    "domain":"research","inputs":[],"outputs":["records"],"subject_id":null,"source_hint":null,"confidence":1}],
                "deliverable":{"format":"chat","operations":[]},
                "meta":{"model":"fixture","parsed_at":"fixture","text_sha256":"fixture"}},
            "matches":[{"slug":"research.records","query":"records","operations":[],
                "apiosk":[{"origin":"apiosk","name":"Matched Register","description":"Related records",
                    "source_slug":"matched-register","capability":null,"endpoint_id":null,"url":null,
                    "method":"GET","price":null,"network":null,"calls_30d":null,"availability":"discovery_only"}],
                "coinbase":[],"coinbase_status":"skipped"}],
            "selected_capabilities":[],"apiosk_sources_searched":1,"catalog_version":"fixture"
        })).unwrap();
        Task { capability_intake: Some(intake), ..Default::default() }
    }

    #[test]
    fn confirmed_no_results_is_not_replaced_by_a_related_source() {
        let mut task = discovery_task();
        let message = crate::parser::conversation_reply::NO_RESULTS;
        task.record_turn("Find records", Some(message.into()));
        assert!(crate::parser::plan_coverage::discovery_message(task.capability_intake.as_ref().unwrap()).is_some());
        assert!(is_terminal_discovery(&task, message));
        assert_eq!(finish_without_plan(&mut task, json!({})), Status::Unsupported);
        assert_eq!(task.conversation.last().unwrap().reply.as_deref(), Some(message));
        assert_eq!(task.actions[0]["message"], message);
        assert!(!task.actions[0]["message"].as_str().unwrap().contains("Matched Register"));
    }

    #[test]
    fn genuine_source_discovery_finishes_and_preserves_the_full_intake() {
        let mut task = discovery_task();
        let before = serde_json::to_value(&task.capability_intake).unwrap();
        let message = crate::parser::plan_coverage::discovery_message(task.capability_intake.as_ref().unwrap()).unwrap();
        task.record_turn("Find records", Some(message.clone()));
        assert!(is_terminal_discovery(&task, &message));
        assert_eq!(finish_without_plan(&mut task, json!({})), Status::Unsupported);
        assert_eq!(task.actions.len(), 1);
        assert_eq!(task.actions[0]["kind"], "unsupported_reason");
        assert_eq!(task.actions[0]["message"], "Matching sources:\n- Matched Register");
        assert_eq!(task.conversation.last().unwrap().reply.as_deref(), Some(message.as_str()));
        assert_eq!(serde_json::to_value(&task.capability_intake).unwrap(), before);
        assert!(task.proposal.is_none());
    }

    #[test]
    fn coverage_failure_ends_without_a_clarification_or_executable_action() {
        let mut task = Task::default();
        task.record_turn("Find records", None);
        task.proposal = Some(json!({"quote_ref":"old"}));
        task.actions = vec![json!({"kind":"run"}), json!({"kind":"planning_clarification"})];
        let fields = json!(["unmapped_scope", "unmapped_output"]);
        assert_eq!(finish_without_plan(&mut task, json!({"missing_output_contracts":fields})), Status::Unsupported);
        assert!(task.proposal.is_none());
        assert_eq!(task.actions.len(), 1);
        assert_eq!(task.actions[0]["kind"], "unsupported_reason");
        assert_eq!(task.actions[0]["missing_output_contracts"], fields);
        assert_eq!(task.conversation.last().unwrap().reply.as_deref(), Some(crate::parser::conversation_reply::NO_RESULTS));
        assert!(!task.actions[0]["message"].as_str().unwrap().contains("unmapped"));
        assert!(is_terminal_discovery(&task, crate::parser::conversation_reply::NO_RESULTS));
        assert!(!is_terminal_discovery(&task, "Which reporting year do you need?"));
    }
}
