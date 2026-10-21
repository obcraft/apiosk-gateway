use super::*;
use crate::parser::conversation_route::Route;

struct SavedOutput {
    question: String,
    refs: Vec<Uuid>,
    analysis: Option<Value>,
}

fn saved_outputs(task: &Task) -> Vec<SavedOutput> {
    let mut outputs = Vec::new();
    if !task.active_results.is_empty() {
        outputs.push(SavedOutput { question: task.conversation.last().map(|t|t.question.clone()).unwrap_or_default(),
            refs: task.active_results.clone(), analysis: task.analysis.clone() });
    }
    for turn in task.conversation.iter().rev() {
        if let Some(output) = &turn.output {
            let refs = if output.result_refs.is_empty() { output.result_ref.into_iter().collect() } else { output.result_refs.clone() };
            if !refs.is_empty() && !outputs.iter().any(|output| output.refs == refs) {
                outputs.push(SavedOutput { question: turn.question.clone(), refs, analysis: output.analysis.clone() });
            }
        }
    }
    outputs.truncate(6);
    outputs
}

fn output_summaries<'a>(outputs: impl IntoIterator<Item = &'a SavedOutput>) -> Value {
    json!(outputs.into_iter().enumerate().map(|(index, output)| json!({
        "index":index,"question":output.question,
        "answer":output.analysis.as_ref().map(|a|json!({"observations":a["observations"].as_array().map(|items|items.iter().map(|v|&v["text"]).collect::<Vec<_>>()),"limitations":a["limitations"]})),
        "saved_source_count":output.refs.len()
    })).collect::<Vec<_>>())
}

fn saved_output_summaries(task: &Task) -> Value {
    output_summaries(&saved_outputs(task))
}

/// Saved answers Ask may read for one follow-up: the chat's most recent
/// purchases, and their outputs, most recent first.
const ASK_FOLLOW_UP_TASKS: usize = 6;
const ASK_FOLLOW_UP_OUTPUTS: usize = 12;

/// How an Ask chat continues after an answer. A saved answer reads only
/// results the chat already bought; a search starts Ask's own source
/// discovery; a clarification is one question back. None of them quotes,
/// approves or dispatches a source.
#[derive(Debug, PartialEq)]
pub enum AskFollowUp {
    /// These tasks are composing their answer from saved results only.
    SavedAnswer(Vec<Uuid>),
    Search,
    Clarification(String),
}

/// A finished purchase whose saved results can answer one more question in
/// place. A saved answer adds a turn to the task: the oldest turn, the
/// purchase itself, must never be the one dropped to make room for it.
fn answerable(task: &Task) -> bool {
    task.external_purchase.is_none()
        && !task.cancelled
        && task.intake_recovery.is_none()
        && !task.workflow.as_ref().is_some_and(|workflow| workflow.collecting)
        && task.pending_candidates.is_empty()
        && task.pending_dispatches.is_empty()
        && task.actions.iter().all(|action| matches!(action["kind"].as_str(), Some("read" | "cancel" | "retry_analysis")))
        && task.conversation.len() < 6
        && !saved_outputs(task).is_empty()
}

impl Engine {
    /// None means ordinary source planning; Some contains a finished saved
    /// answer or clarification. No path in this method dispatches a source.
    pub(super) async fn route_conversation(&self, p: &Principal, req: &DiscoverRequest,
        id: Uuid, revision: i64, task: &mut Task) -> anyhow::Result<(bool, Option<Response>)> {
        let outputs = saved_outputs(task);
        match self.parser.conversation_route(&req.question, task, &saved_output_summaries(task)).await? {
            Route::NewRequest => Ok((true, None)),
            Route::ContinueRequest => Ok((false, None)),
            Route::Clarification { message } => {
                self.archive_conversation_output(p,id,req.request_id,task).await?;
                clear_plan(task);
                task.capability_intake = None;
                task.record_turn(&req.question,Some(message));
                self.store.save_intent(p.owner,id,revision,task).await?;
                Ok((false,Some(self.view(p,id,req.request_id,Some(Status::NeedsInput)).await?)))
            }
            Route::SavedAnswer { outputs: selected } => {
                let selected = selected.iter()
                    .map(|index| outputs.get(*index).ok_or_else(|| anyhow::anyhow!("unknown saved output")))
                    .collect::<anyhow::Result<Vec<_>>>()?;
                self.queue_saved_answer(p,id,revision,req.request_id,&req.question,task,&selected).await?;
                // The server worker composes this saved-only answer. Persist
                // first so a lost response/reload resumes without a purchase.
                let response = self.view(p,id,req.request_id,Some(Status::Running)).await?;
                Ok((false,Some(response)))
            }
        }
    }

    /// Point the task at these saved outputs and queue one zero-cost
    /// analysis of the latest question. Nothing is fetched or charged.
    #[allow(clippy::too_many_arguments)]
    async fn queue_saved_answer(&self, p: &Principal, id: Uuid, revision: i64, request: Uuid,
        question: &str, task: &mut Task, selected: &[&SavedOutput]) -> anyhow::Result<()> {
        let mut refs = Vec::new();
        let mut operations = vec![question.to_owned()];
        for output in selected {
            operations.push(format!("Historical question for identity and scope only (the latest question is authoritative): {}",output.question));
            for result in &output.refs {
                anyhow::ensure!(task.result_refs.contains(result),"saved output is not part of this task");
                if !refs.contains(result) { refs.push(*result); }
            }
        }
        // Loading is owner/task scoped. Check availability before
        // replacing the active turn, including after expired storage.
        let documents = self.store.results(p.owner,id,&refs).await?;
        anyhow::ensure!(refs.iter().all(|id| documents.contains_key(id)),"Saved source data is no longer available");
        self.archive_conversation_output(p,id,request,task).await?;
        clear_plan(task);
        task.workflow = None;
        task.capability_intake = None;
        task.intake_recovery = None;
        task.saved_answer = true;
        task.active_results = refs;
        task.record_turn(question,None);
        task.intent = Some(json!({"subjects":[],"goals":[],"answer_operations":operations,"unresolved_references":[]}));
        let quote = Uuid::new_v4();
        let plan = Plan { independent_calls: false, compute:None,quote_ref:quote,steps:vec![],cap_atomic:0,
            expires_at:Utc::now()+chrono::Duration::minutes(15),hash:digest(&(quote,&task.active_results,question)),fee_bps:0,workflow_slug:None };
        task.proposal = Some(serde_json::to_value(plan)?);
        task.actions = vec![serde_json::to_value(Action::Analyze { action_id:Uuid::new_v4() })?];
        self.store.save_intent(p.owner,id,revision,task).await?;
        Ok(())
    }

    /// Ask's follow-up after an answer in its chat. `task_refs` are the
    /// chat's purchases, most recent first; tasks the caller cannot open, or
    /// that are still busy, are left out rather than reported. The tasks of a
    /// saved answer are queued here; the caller starts their workers.
    pub async fn ask_follow_up(&self, p: &Principal, request: Uuid, question: &str, task_refs: &[Uuid]) -> anyhow::Result<AskFollowUp> {
        let mut saved = Vec::new();
        for id in task_refs.iter().take(ASK_FOLLOW_UP_TASKS) {
            if saved.iter().any(|(saved, _, _)| saved == id) { continue; }
            let (revision, mut task, connection) = match self.store.load_scoped(p, *id).await {
                Ok(loaded) => loaded,
                Err(error) if matches!(error.downcast_ref::<crate::store::StoreAccessError>(), Some(crate::store::StoreAccessError::TaskUnavailable)) => continue,
                Err(error) => return Err(error),
            };
            if connection.is_some() || self.worker_active(*id).await? { continue; }
            self.prepare_conversation(p.owner, *id, &mut task).await?;
            if answerable(&task) { saved.push((*id, revision, task)); }
        }
        let Some((_, _, latest)) = saved.first() else { return Ok(AskFollowUp::Search) };
        // One numbered list across the chat: the router picks the answers
        // the question is about, whichever purchase saved them.
        let listed: Vec<(usize, SavedOutput)> = saved.iter().enumerate()
            .flat_map(|(index, (_, _, task))| saved_outputs(task).into_iter().map(move |output| (index, output)))
            .take(ASK_FOLLOW_UP_OUTPUTS).collect();
        let route = self.parser.conversation_route(question, latest, &output_summaries(listed.iter().map(|(_, output)| output))).await?;
        let selected = match route {
            Route::NewRequest | Route::ContinueRequest => return Ok(AskFollowUp::Search),
            Route::Clarification { message } => return Ok(AskFollowUp::Clarification(message)),
            Route::SavedAnswer { outputs } => outputs,
        };
        let mut queued = Vec::new();
        for (index, (id, revision, task)) in saved.iter_mut().enumerate() {
            let outputs: Vec<_> = selected.iter().filter_map(|output| listed.get(*output))
                .filter(|(task, _)| *task == index).map(|(_, output)| output).collect();
            if outputs.is_empty() { continue; }
            // A purchase that changed meanwhile keeps its state; the others
            // still answer.
            match self.queue_saved_answer(p, *id, *revision, request, question, task, &outputs).await {
                Ok(()) => queued.push(*id),
                Err(error) => tracing::warn!(task = %id, error = %error, "saved Ask answer not queued"),
            }
        }
        anyhow::ensure!(!queued.is_empty(), "Saved answers are unavailable");
        Ok(AskFollowUp::SavedAnswer(queued))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bought() -> Task {
        let mut task = Task::default();
        task.record_turn("Employer rating for Starbucks", None);
        let result = Uuid::new_v4();
        task.result_refs = vec![result];
        task.active_results = vec![result];
        task.analysis = Some(json!({"status":"completed","observations":[{"text":"Rated {{a}}","evidence":[]}],"limitations":[]}));
        task.actions = vec![json!({"kind":"read"})];
        task
    }

    #[test]
    fn only_a_settled_purchase_with_saved_results_and_room_for_a_turn_answers_in_place() {
        assert!(answerable(&bought()));
        for change in ["running", "choice", "interrupted", "cancelled", "no_results", "full"] {
            let mut task = bought();
            match change {
                "running" => task.actions.push(json!({"kind":"analyze"})),
                "choice" => task.pending_candidates.push(json!({"entity_ref":Uuid::new_v4()})),
                "interrupted" => task.intake_recovery = Some(crate::state::IntakeRecovery { question: "Retry".into(), context_delta: Default::default(), stage: crate::state::IntakeStage::Planning, created_at: Utc::now() }),
                "cancelled" => task.cancelled = true,
                "no_results" => { task.active_results.clear(); task.result_refs.clear(); }
                _ => for turn in 0..5 { task.record_turn(&format!("Follow-up {turn}"), None); },
            }
            assert!(!answerable(&task), "{change}");
        }
    }

    #[test]
    fn a_saved_answer_turn_and_its_purchase_are_listed_once_most_recent_first() {
        let mut task = bought();
        let purchase = task.active_results.clone();
        task.archive_output(Some(json!({"quote_ref":Uuid::new_v4(),"steps":["Glassdoor"]})));
        task.record_turn("Vertaal naar het Nederlands", None);
        task.active_results = purchase.clone();
        let outputs = saved_outputs(&task);
        assert_eq!(outputs.len(), 1, "the same saved results are one output");
        assert_eq!((outputs[0].question.as_str(), &outputs[0].refs), ("Vertaal naar het Nederlands", &purchase));
        let summaries = output_summaries(&outputs);
        assert_eq!(summaries[0]["index"], 0);
        assert_eq!(summaries[0]["saved_source_count"], 1);
    }
}
