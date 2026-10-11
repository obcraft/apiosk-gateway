use super::*;
use crate::compute;
use serde::{Deserialize, Serialize};
use sqlx::Row;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpgradeRequest {
    pub request_id: Uuid,
    pub state: crate::state::Envelope,
    pub enabled: bool,
    pub model_id: Option<String>,
    /// Saved output identity. Omitted when changing an unapproved plan.
    pub output_quote_ref: Option<Uuid>,
}
fn eligible(task: &Task, p: &Principal) -> bool {
    compute::enabled_for(p.owner)
        && p.is_person()
        && p.connection.is_none()
        && task.external_purchase.is_none()
        && task.workflow.is_none()
        && task.supplier_check.is_none()
        && !task.cancelled
        && task.pending_dispatches.is_empty()
        && task.pending_candidates.is_empty()
}
impl Engine {
    pub async fn analysis_upgrade(
        &self,
        p: &Principal,
        req: UpgradeRequest,
    ) -> anyhow::Result<Response> {
        if let Some(response) = self
            .request_claim(
                p,
                req.request_id,
                &digest(&("analysis_upgrade", &req)),
                "analysis_upgrade",
            )
            .await?
        {
            return Ok(response);
        }
        let result = self.prepare_upgrade(p, &req).await;
        Ok(self.complete(p, req.request_id, result).await)
    }
    async fn prepare_upgrade(
        &self,
        p: &Principal,
        req: &UpgradeRequest,
    ) -> anyhow::Result<Response> {
        self.signer.verify(p.owner, &req.state, false)?;
        let id = req.state.state_ref;
        self.store.check_spending(p).await?;
        let mut tx = self.store.pool.begin().await?;
        let row = sqlx::query("SELECT revision,document,connection_id FROM tasks WHERE id=$1 AND owner_id=$2 AND expires_at>now() FOR UPDATE")
            .bind(id).bind(p.owner).fetch_one(&mut *tx).await?;
        let mut task: Task = serde_json::from_value(row.get("document"))?;
        anyhow::ensure!(
            row.get::<i64, _>("revision") == req.state.revision,
            "state_conflict"
        );
        anyhow::ensure!(
            row.get::<Option<Uuid>, _>("connection_id").is_none()
                && task.workspace_id == p.workspace
                && eligible(&task, p),
            "Extended analysis is unavailable for this task"
        );
        let busy: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM execution_leases WHERE task_id=$1 AND expires_at>now()) OR EXISTS(SELECT 1 FROM executions WHERE task_id=$1 AND status IN ('dispatching','unresolved'))")
            .bind(id).fetch_one(&mut *tx).await?;
        anyhow::ensure!(!busy, "Wait for the current execution to finish");
        let choice =
            compute::Quote::for_model(req.model_id.as_deref().unwrap_or(compute::DEFAULT_MODEL))?;
        let mut plan: Plan;
        let question: String;
        if let Some(output_quote) = req.output_quote_ref {
            anyhow::ensure!(
                req.enabled
                    && task.actions.iter().all(|a| matches!(
                        a["kind"].as_str(),
                        Some("retry_analysis" | "unsupported_reason" | "cancel" | "read")
                    )),
                "Finish the current request before upgrading a saved result"
            );
            let selected = if task
                .proposal
                .as_ref()
                .is_some_and(|v| v["quote_ref"] == json!(output_quote))
                && !task.active_results.is_empty()
            {
                Some((
                    task.active_results.clone(),
                    task.analysis.clone(),
                    task.conversation
                        .last()
                        .map(|t| t.question.clone())
                        .unwrap_or_default(),
                ))
            } else {
                task.conversation.iter().find_map(|turn| {
                    let output = turn.output.as_ref()?;
                    if output
                        .proposal
                        .as_ref()
                        .is_none_or(|v| v["quote_ref"] != json!(output_quote))
                    {
                        return None;
                    }
                    let mut refs = output.result_refs.clone();
                    if refs.is_empty() {
                        refs.extend(output.result_ref);
                    }
                    Some((refs, output.analysis.clone(), turn.question.clone()))
                })
            }
            .ok_or_else(|| anyhow::anyhow!("Saved output is unavailable"))?;
            anyhow::ensure!(
                !selected.0.is_empty() && !selected.2.trim().is_empty(),
                "No saved evidence or question to analyze"
            );
            // Owner/task-scoped references; the caller cannot supply documents.
            for source in &selected.0 {
                let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM results WHERE id=$1 AND owner_id=$2 AND task_id=$3)")
                    .bind(source).bind(p.owner).bind(id).fetch_one(&mut *tx).await?;
                anyhow::ensure!(exists, "Saved source not found");
            }
            question = selected.2;
            task.archive_output(task.proposal.clone());
            task.saved_answer = false;
            task.record_turn(&format!("Extended analysis: {question}"), None);
            task.active_results = selected.0;
            task.analysis = selected.1;
            task.failed_steps.clear();
            task.execution_error = None;
            task.intake_recovery = None;
            plan = Plan {
                compute: Some(choice.clone()),
                quote_ref: Uuid::new_v4(),
                steps: vec![],
                cap_atomic: choice.buyer_cap(),
                expires_at: Utc::now() + chrono::Duration::minutes(15),
                hash: String::new(),
                fee_bps: 1500,
                workflow_slug: None,
            };
        } else {
            plan = serde_json::from_value(
                task.proposal
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("No plan to update"))?,
            )?;
            let approved: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM authorizations WHERE task_id=$1 AND quote_ref=$2)",
            )
            .bind(id)
            .bind(plan.quote_ref)
            .fetch_one(&mut *tx)
            .await?;
            anyhow::ensure!(
                !approved
                    && ((task.active_results.is_empty()
                        && task.completed.is_empty()
                        && !plan.steps.is_empty())
                        || (plan.steps.is_empty() && plan.compute.is_some() && req.enabled))
                    && plan.fee_bps == 1500,
                "This plan has already started"
            );
            question = task
                .compute
                .as_ref()
                .filter(|_| plan.steps.is_empty())
                .map(|r| r.question.clone())
                .unwrap_or_else(|| {
                    task.conversation
                        .last()
                        .map(|t| t.question.clone())
                        .unwrap_or_default()
                });
            anyhow::ensure!(!question.trim().is_empty(), "Missing question");
            plan.compute = req.enabled.then_some(choice);
            plan.quote_ref = Uuid::new_v4();
            // Preserve the source quote's validity window.
            plan.apply_markup(1500);
        }
        task.compute = plan
            .compute
            .as_ref()
            .map(|q| compute::Run::new(plan.quote_ref, question, q.model_id.clone()));
        plan.hash = digest(&(&plan.steps, &plan.compute));
        if plan.steps.is_empty() {
            task.actions = vec![serde_json::to_value(Action::Compute {
                action_id: task.compute.as_ref().unwrap().action_id,
            })?];
        }
        task.proposal = Some(serde_json::to_value(plan)?);
        anyhow::ensure!(task.bounded(), "Task capacity reached");
        sqlx::query("UPDATE tasks SET document=$3,revision=revision+1,updated_at=now() WHERE id=$1 AND owner_id=$2")
            .bind(id).bind(p.owner).bind(serde_json::to_value(&task)?).execute(&mut *tx).await?;
        tx.commit().await?;
        crate::server_events::task_changed(id);
        self.view(p, id, req.request_id, None).await
    }

    pub(super) async fn compute_view(
        &self,
        p: &Principal,
        id: Uuid,
        task: &Task,
        plan: Option<&Plan>,
        billing: &Option<Value>,
    ) -> anyhow::Result<Value> {
        let available = eligible(task, p) && !self.worker_active(id).await?;
        let idle = task.actions.iter().all(|a| {
            matches!(
                a["kind"].as_str(),
                Some("retry_analysis" | "unsupported_reason" | "cancel" | "read")
            )
        });
        let mut outputs = Vec::new();
        if available && idle {
            if !task.active_results.is_empty() {
                if let Some(plan) = plan {
                    outputs.push(plan.quote_ref);
                }
            }
            for output in task.conversation.iter().filter_map(|t| t.output.as_ref()) {
                if !output.result_refs.is_empty() || output.result_ref.is_some() {
                    if let Some(quote) = output
                        .proposal
                        .as_ref()
                        .and_then(|p| p["quote_ref"].as_str())
                        .and_then(|s| s.parse().ok())
                    {
                        outputs.push(quote);
                    }
                }
            }
        }
        let mut view = compute::options(plan.and_then(|p| p.compute.as_ref()));
        view["can_choose"] = json!(
            available
                && plan.is_some_and(|p| p.fee_bps == 1500
                    && ((!p.steps.is_empty()
                        && task.active_results.is_empty()
                        && task.completed.is_empty())
                        || (p.steps.is_empty()
                            && p.compute.is_some()
                            && task.compute.as_ref().is_some_and(|r| r.status == "pending"))))
                && !billing
                    .as_ref()
                    .is_some_and(|b| b["authorization_active"] == true
                        || b["executions"].as_array().is_some_and(|v| !v.is_empty()))
        );
        view["compute_only"] =
            json!(plan.is_some_and(|p| p.steps.is_empty() && p.compute.is_some()));
        view["enabled"] = json!(plan.is_some_and(|p| p.compute.is_some()));
        view["output_quotes"] = json!(outputs);
        view["current_output_quote_ref"] = json!(plan.filter(|plan| !task.active_results.is_empty() && outputs.contains(&plan.quote_ref)).map(|plan|plan.quote_ref));
        if let Some(run) = &task.compute {
            if plan.is_some_and(|p| p.quote_ref == run.quote_ref) {
                view["status"] = json!(run.status);
            }
        }
        Ok(view)
    }

    pub(super) async fn compute_receipt(
        &self,
        p: &Principal,
        id: Uuid,
        task: &Task,
    ) -> anyhow::Result<Option<Uuid>> {
        let Some(run) = task.compute.as_ref().filter(|r| {
            r.prepared.is_some() && !matches!(r.status.as_str(), "completed" | "unavailable")
        }) else {
            return Ok(None);
        };
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM executions WHERE id=$1 AND owner_id=$2 AND task_id=$3)",
        )
        .bind(run.action_id)
        .bind(p.owner)
        .bind(id)
        .fetch_one(&self.store.pool)
        .await?;
        Ok(exists.then_some(run.action_id))
    }

    pub(super) async fn run_compute(
        &self,
        p: &Principal,
        bearer: &str,
        id: Uuid,
        revision: i64,
        task: &mut Task,
        req: &ExecuteRequest,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(
            p.is_person() && req.input.is_null(),
            "Invalid compute request"
        );
        let plan: Plan = serde_json::from_value(
            task.proposal
                .clone()
                .ok_or_else(|| anyhow::anyhow!("Missing compute quote"))?,
        )?;
        let run = task
            .compute
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Missing compute choice"))?
            .clone();
        anyhow::ensure!(
            run.action_id == req.action_id
                && run.quote_ref == plan.quote_ref
                && req.quote_ref == Some(plan.quote_ref)
                && plan
                    .compute
                    .as_ref()
                    .is_some_and(|q| q.valid() && q.model_id == run.model_id)
                && run
                    .prepared
                    .as_ref()
                    .is_none_or(|p| p.model_id == run.model_id),
            "Compute quote changed"
        );
        let documents = self
            .store
            .results(p.owner, id, &task.active_results)
            .await?;
        let sources: Vec<_> = task
            .active_results
            .iter()
            .filter_map(|r| {
                documents
                    .get(r)
                    .map(|doc| json!({"result_ref":r,"document":doc}))
            })
            .collect();
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM executions WHERE id=$1 AND owner_id=$2 AND task_id=$3)",
        )
        .bind(run.action_id)
        .bind(p.owner)
        .bind(id)
        .fetch_one(&self.store.pool)
        .await?;
        let dispatch = if exists {
            match self.executor.poll_compute(p, id, run.action_id).await? {
                Some(body) if body["ok"] == true => Dispatch::Done {
                    execution_id: run.action_id,
                    body,
                },
                Some(body) => Dispatch::Failed { body },
                None => Dispatch::Pending {
                    execution_id: run.action_id,
                },
            }
        } else {
            anyhow::ensure!(!task.cancelled, "Task cancelled");
            let auth: Option<Uuid> = sqlx::query_scalar("SELECT id FROM authorizations WHERE task_id=$1 AND owner_id=$2 AND quote_ref=$3 AND plan_hash=$4 AND revoked_at IS NULL AND expires_at>now()")
            .bind(id).bind(p.owner).bind(plan.quote_ref).bind(&plan.hash).fetch_optional(&self.store.pool).await?;
            let Some(auth) = auth else {
                return Ok(false);
            };
            if run.prepared.is_none() {
                match crate::parser::compute_analysis::Prepared::for_model(
                    &run.model_id,
                    &run.question,
                    &sources,
                    &task.failed_steps,
                    task.analysis.as_ref(),
                ) {
                    Ok(prepared) => {
                        let current = task.compute.as_mut().unwrap();
                        current.prepared = Some(prepared);
                        current.status = "preparing".into();
                    }
                    Err(_) => {
                        task.compute.as_mut().unwrap().status = "unavailable".into();
                        task.actions.clear();
                    }
                }
                return Ok(true);
            }
            let prepared = run.prepared.as_ref().unwrap();
            self.executor
                .dispatch_compute(
                    p,
                    bearer,
                    id,
                    revision,
                    run.action_id,
                    auth,
                    &plan,
                    &prepared.payload,
                )
                .await?
        };
        let prepared = run
            .prepared
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Missing saved compute input"))?;
        match dispatch {
            Dispatch::Done { body, .. } => {
                match prepared.validate(&body, &sources, &task.failed_steps) {
                    Ok(answer) => {
                        task.analysis = Some(answer);
                        task.compute.as_mut().unwrap().status = "completed".into();
                    }
                    Err(error) => {
                        tracing::warn!(task=%id,error=%error,"paid analysis did not pass evidence validation");
                        task.compute.as_mut().unwrap().status = "unavailable".into();
                    }
                }
                task.actions.clear();
            }
            Dispatch::Failed { .. } => {
                task.compute.as_mut().unwrap().status = "unavailable".into();
                task.actions.clear();
            }
            Dispatch::Pending { .. } | Dispatch::Waiting { .. } => {
                task.compute.as_mut().unwrap().status = "running".into();
            }
        }
        Ok(true)
    }
}
