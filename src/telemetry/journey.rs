//! Journey milestones read from engine state: how a question ended and how
//! complete its answer is. Only counts, closed statuses and ids are sent.

use super::{bind, emit_milestone, Classify, ErrorClass, Milestone, MilestoneFields};
use crate::{protocol::{Response, Status}, state::Task};
use std::collections::BTreeSet;
use uuid::Uuid;

pub(crate) fn status_name(status: &Status) -> &'static str {
    match status {
        Status::Ready => "ready",
        Status::NeedsInput => "needs_input",
        Status::NeedsSelection => "needs_selection",
        Status::RequiresApproval => "requires_approval",
        Status::Running => "running",
        Status::Cancelled => "cancelled",
        Status::Succeeded => "succeeded",
        Status::Partial => "partial",
        Status::Unsupported => "unsupported",
        Status::StateConflict => "state_conflict",
        Status::Failed => "failed",
    }
}

/// How a question ended: priced, waiting for the person, or without a price.
pub fn question(result: &anyhow::Result<Response>) {
    let response = match result {
        Ok(response) => response,
        Err(error) => return question_failed(error),
    };
    let task = response.state.as_ref().map(|state| state.state_ref);
    let quote = response.proposal.as_ref().map(|proposal| proposal.quote_ref);
    if let Some(task) = task {
        bind(task, quote);
    }
    let status = Some(status_name(&response.status));
    match &response.proposal {
        Some(proposal) => {
            let sources: BTreeSet<_> = proposal.step_details.iter().filter_map(|step| step["source"]["slug"].as_str()).collect();
            emit_milestone(Milestone::PriceReady, task, quote, MilestoneFields {
                status,
                query_class: super::current().and_then(|trace| trace.query_class()),
                sources: (!sources.is_empty()).then_some(sources.len()),
                steps: Some(proposal.steps.len()),
                cap_micro_usd: proposal.max_total_atomic.parse().ok(),
                ..Default::default()
            });
        }
        None if matches!(response.status, Status::NeedsInput | Status::NeedsSelection) => {
            emit_milestone(Milestone::NeedsInput, task, None, MilestoneFields { status, ..Default::default() });
        }
        None => emit_milestone(Milestone::Terminal, task, None, MilestoneFields { status, ..Default::default() }),
    }
}

/// A question that failed before it could be answered in any status.
pub fn question_failed(error: &anyhow::Error) {
    let class = error.error_class();
    let status = if class == ErrorClass::Unsupported { "unsupported" } else { "failed" };
    emit_milestone(Milestone::Terminal, None, None, MilestoneFields { status: Some(status), error_class: Some(class), ..Default::default() });
}

/// v1: analysis completed with no failed or pending source and no follow-up
/// question. Extend with the answer-coverage check (#42) once it exists.
pub fn fully_answered(task: &Task) -> bool {
    task.analysis.as_ref().is_some_and(|analysis| analysis["status"] == "completed" && !analysis["follow_up_question"].is_string())
        && task.failed_steps.is_empty()
        && task.pending_dispatches.is_empty()
}

/// The saved answer: its status, completeness and how much evidence it cites.
pub fn answer(task_id: Uuid, quote: Option<Uuid>, task: &Task) {
    let analysis = task.analysis.as_ref();
    let observations = analysis.and_then(|analysis| analysis["observations"].as_array());
    let answer_status = analysis.map(|analysis| match analysis["status"].as_str() {
        Some("completed") => "completed",
        Some("partial") => "partial",
        Some("unavailable") => "unavailable",
        _ => "other",
    });
    emit_milestone(Milestone::AnswerComplete, Some(task_id), quote, MilestoneFields {
        answer_status,
        fully_answered: Some(fully_answered(task)),
        observations: observations.map(Vec::len),
        evidence_refs: observations.map(|items| items.iter().filter_map(|item| item["evidence"].as_array()).map(Vec::len).sum()),
        failed_steps: Some(task.failed_steps.len()),
        pending: Some(task.pending_dispatches.len()),
        ..Default::default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::capture;
    use serde_json::json;

    #[test]
    fn a_follow_up_question_or_failed_step_is_not_a_full_answer() {
        let mut task = Task::default();
        assert!(!fully_answered(&task), "no analysis yet");
        task.analysis = Some(json!({"status":"completed","observations":[]}));
        assert!(fully_answered(&task));
        task.analysis = Some(json!({"status":"completed","follow_up_question":"Which year?"}));
        assert!(!fully_answered(&task));
        task.analysis = Some(json!({"status":"completed"}));
        task.failed_steps.insert("step".into(), json!({"code":"incomplete_source"}));
        assert!(!fully_answered(&task));
    }

    #[test]
    fn answers_report_counts_and_questions_report_how_they_ended() {
        let lines = capture::Lines::default();
        let _guard = lines.install();
        let task = Task { analysis: Some(json!({"status":"partial","observations":[
            {"text":"Quokka Canary 4f1c B.V. is active","evidence":[{"pointer":"/data/a"},{"pointer":"/data/b"}]},
            {"text":"second","evidence":[{"pointer":"/data/c"}]}]})), ..Task::default() };
        answer(Uuid::new_v4(), None, &task);
        question(&Ok(Response::new(Uuid::new_v4(), Status::NeedsInput)));
        question(&Err(anyhow::Error::new(crate::parser::ModelFailure::Unsupported)));
        let telemetry = lines.telemetry();
        assert_eq!(telemetry[0]["milestone"], "answer_complete");
        assert_eq!(telemetry[0]["answer_status"], "partial");
        assert_eq!(telemetry[0]["observations"], 2);
        assert_eq!(telemetry[0]["evidence_refs"], 3);
        assert_eq!(telemetry[0]["fully_answered"], false);
        assert_eq!(telemetry[1]["milestone"], "needs_input");
        assert_eq!(telemetry[2]["milestone"], "terminal");
        assert_eq!(telemetry[2]["status"], "unsupported");
        assert!(!lines.text().contains("Quokka"), "answer text never reaches telemetry");
    }
}
