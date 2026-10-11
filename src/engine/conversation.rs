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

impl Engine {
    /// None means ordinary source planning; Some contains a finished saved
    /// answer or clarification. No path in this method dispatches a source.
    pub(super) async fn route_conversation(&self, p: &Principal, req: &DiscoverRequest,
        id: Uuid, revision: i64, task: &mut Task) -> anyhow::Result<(bool, Option<Response>)> {
        let outputs = saved_outputs(task);
        let summaries = json!(outputs.iter().enumerate().map(|(index, output)| json!({
            "index":index,"question":output.question,
            "answer":output.analysis.as_ref().map(|a|json!({"observations":a["observations"].as_array().map(|items|items.iter().map(|v|&v["text"]).collect::<Vec<_>>()),"limitations":a["limitations"]})),
            "saved_source_count":output.refs.len()
        })).collect::<Vec<_>>());
        match self.parser.conversation_route(&req.question, task, &summaries).await? {
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
                let mut refs = Vec::new();
                let mut operations = vec![req.question.clone()];
                for index in selected {
                    let output = &outputs[index];
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
                self.archive_conversation_output(p,id,req.request_id,task).await?;
                clear_plan(task);
                task.workflow = None;
                task.capability_intake = None;
                task.intake_recovery = None;
                task.saved_answer = true;
                task.active_results = refs;
                task.record_turn(&req.question,None);
                task.intent = Some(json!({"subjects":[],"goals":[],"answer_operations":operations,"unresolved_references":[]}));
                let quote = Uuid::new_v4();
                let plan = Plan { compute:None,quote_ref:quote,steps:vec![],cap_atomic:0,
                    expires_at:Utc::now()+chrono::Duration::minutes(15),hash:digest(&(quote,&task.active_results,&req.question)),fee_bps:0,workflow_slug:None };
                task.proposal = Some(serde_json::to_value(plan)?);
                let action = Uuid::new_v4();
                task.actions = vec![serde_json::to_value(Action::Analyze { action_id:action })?];
                self.store.save_intent(p.owner,id,revision,task).await?;
                // The server worker composes this saved-only answer. Persist
                // first so a lost response/reload resumes without a purchase.
                let response = self.view(p,id,req.request_id,Some(Status::Running)).await?;
                Ok((false,Some(response)))
            }
        }
    }
}
