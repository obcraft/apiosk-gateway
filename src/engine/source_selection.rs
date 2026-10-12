//! Reconfigure an unapproved research quote in place. The task row is locked
//! again after planning, so concurrent approval or another revision wins
//! cleanly; this route never creates an authorization or runs a source.
use super::*;
use crate::ask_v2::source_selection::Selection;
use serde::{Deserialize, Serialize};
use sqlx::Row;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSelectionRequest {
    pub request_id: Uuid,
    pub state: crate::state::Envelope,
    pub selection: Selection,
}

pub(crate) fn eligible(task: &Task) -> bool {
    task.source_selection.is_some()
        && task.external_purchase.is_none()
        && task.workflow.is_none()
        && task.supplier_check.is_none()
        && !task.cancelled
        && task.completed.is_empty()
        && task.active_results.is_empty()
        && task.pending_dispatches.is_empty()
        && task.pending_candidates.is_empty()
        && task.failed_steps.is_empty()
        && task
            .compute
            .as_ref()
            .is_none_or(|run| run.status == "pending")
}

impl Engine {
    #[cfg(test)]
    pub(crate) async fn preview_selected_sources(
        &self,
        question: &str,
        context: crate::ask_v2::source_selection::Context,
        catalog: &Catalog,
    ) -> anyhow::Result<(Status, Task)> {
        let intake = context.expanded(catalog)?;
        let selection = context.model_selection();
        let mut task = Task {
            capability_intake: Some(intake.clone()),
            source_selection: Some(context),
            ..Default::default()
        };
        task.record_turn(question, None);
        let reply = match task.source_selection.as_ref().unwrap().compile(catalog)? {
            Some(reply) => reply,
            None => {
                self.parser
                    .chat_from_capabilities(question, Some(&selection), &task, catalog, &intake)
                    .await?
            }
        };
        if !reply.discover {
            task.conversation.last_mut().unwrap().reply = Some(reply.message);
            return Ok((Status::NeedsInput, task));
        }
        task.output_grounding = crate::parser::source_grounding::Grounding::new(&intake, catalog, reply.output_bindings);
        let mut intent = reply
            .intent
            .ok_or_else(|| anyhow::anyhow!("no selected-source intent"))?;
        input::bind_subjects(&mut task, catalog, &mut intent)?;
        name_lookup::seed(&mut task, catalog, &mut intent)?;
        task.intent = Some(serde_json::to_value(&intent)?);
        let status = self.plan(&mut task, catalog, &intent)?;
        Ok((status, task))
    }

    pub async fn select_sources(
        &self,
        p: &Principal,
        req: SourceSelectionRequest,
    ) -> anyhow::Result<Response> {
        anyhow::ensure!(
            p.is_person() && !req.request_id.is_nil(),
            "Source selection requires an App person"
        );
        if let Some(response) = self
            .request_claim(
                p,
                req.request_id,
                &digest(&("source_selection", &req)),
                "source_selection",
            )
            .await?
        {
            return Ok(response);
        }
        let result = self.select_sources_inner(p, &req).await;
        Ok(self.complete(p, req.request_id, result).await)
    }

    async fn select_sources_inner(
        &self,
        p: &Principal,
        req: &SourceSelectionRequest,
    ) -> anyhow::Result<Response> {
        self.signer.verify(p.owner, &req.state, false)?;
        let id = req.state.state_ref;
        let (revision, mut task, connection) = self.store.load_scoped(p, id).await?;
        anyhow::ensure!(
            connection.is_none() && task.workspace_id == p.workspace && eligible(&task),
            "This plan cannot be changed after it starts"
        );
        if revision != req.state.revision {
            return self
                .view(p, id, req.request_id, Some(Status::StateConflict))
                .await;
        }
        let mut context = task.source_selection.clone().unwrap();
        anyhow::ensure!(
            task.conversation
                .last()
                .is_some_and(|turn| turn.question == context.question),
            "Source choices belong to a different question"
        );
        context.selection = req.selection.clone();
        let catalog = self.catalog.get().await?;
        context.refresh(&catalog);
        let intake = context.expanded(&catalog)?;
        let selection = context.model_selection();
        let reply = match context.compile(&catalog)? {
            Some(reply) => reply,
            None => {
                self.parser
                    .chat_from_capabilities(
                        &context.question,
                        Some(&selection),
                        &task,
                        &catalog,
                        &intake,
                    )
                    .await?
            }
        };

        // Do not hold a database transaction during any model transport.
        // The revision and approval are authoritative after taking this lock.
        let mut tx = self.store.pool.begin().await?;
        let row=sqlx::query("SELECT revision,document,connection_id FROM tasks WHERE id=$1 AND owner_id=$2 AND expires_at>now() FOR UPDATE")
            .bind(id).bind(p.owner).fetch_one(&mut *tx).await?;
        let current: Task = serde_json::from_value(row.get("document"))?;
        if row.get::<i64, _>("revision") != revision {
            tx.rollback().await?;
            return self
                .view(p, id, req.request_id, Some(Status::StateConflict))
                .await;
        }
        anyhow::ensure!(
            row.get::<Option<Uuid>, _>("connection_id").is_none()
                && current.workspace_id == p.workspace
                && eligible(&current),
            "This plan has already started"
        );
        let quote = current
            .proposal
            .as_ref()
            .and_then(|plan| plan["quote_ref"].as_str())
            .and_then(|id| Uuid::parse_str(id).ok());
        let blocked:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM authorizations WHERE task_id=$1 AND quote_ref=$2) OR EXISTS(SELECT 1 FROM execution_leases WHERE task_id=$1 AND expires_at>now()) OR EXISTS(SELECT 1 FROM executions WHERE task_id=$1 AND status IN ('dispatching','unresolved'))")
            .bind(id).bind(quote).fetch_one(&mut *tx).await?;
        anyhow::ensure!(!blocked, "This plan is already approved or executing");
        let compute = current
            .proposal
            .as_ref()
            .and_then(|proposal| serde_json::from_value::<Plan>(proposal.clone()).ok())
            .and_then(|plan| plan.compute);
        // Replace facts derived only from the old unapproved input choices.
        // Existing source evidence is disallowed by eligibility, and original
        // question identifiers are rebound from the stored interpreted scope.
        task.facts.retain(|fact| {
            fact.provenance.get("kind").and_then(Value::as_str) != Some("user_input")
        });
        // Unapproved draft changes do not accumulate abandoned entities. Keep
        // any entity cited by retained facts, a historical output, or the new
        // intent. Saved-result histories retain their complete identity map.
        if task.result_refs.is_empty() {
            let history = serde_json::to_string(
                &task
                    .conversation
                    .iter()
                    .filter_map(|turn| turn.output.as_ref())
                    .collect::<Vec<_>>(),
            )?;
            task.entities.retain(|entity| {
                task.facts
                    .iter()
                    .any(|fact| fact.entity_ref == entity.entity_ref)
                    || reply.intent.as_ref().is_some_and(|intent| {
                        intent
                            .subjects
                            .iter()
                            .any(|subject| subject.entity_ref == Some(entity.entity_ref))
                    })
                    || history.contains(&entity.entity_ref.to_string())
            });
        }
        task.focus.entity_refs.clear();
        task.output_grounding = crate::parser::source_grounding::Grounding::new(&intake, &catalog, reply.output_bindings.clone());
        task.capability_intake = Some(intake);
        task.source_selection = Some(context);
        task.intake_recovery = None;
        task.intent = None;
        task.proposal = None;
        task.actions.clear();
        task.compute = None;
        let status = if reply.discover {
            let mut intent = reply.intent.ok_or_else(|| {
                anyhow::anyhow!("Selected sources did not produce a complete intent")
            })?;
            input::bind_subjects(&mut task, &catalog, &mut intent)?;
            name_lookup::seed(&mut task, &catalog, &mut intent)?;
            task.focus.entity_refs = intent
                .subjects
                .iter()
                .filter_map(|subject| subject.entity_ref)
                .collect();
            task.intent = Some(serde_json::to_value(&intent)?);
            let status = self.plan(&mut task, &catalog, &intent)?;
            if status == Status::Ready {
                if let Some(choice) = compute {
                    let mut plan: Plan = serde_json::from_value(task.proposal.clone().unwrap())?;
                    plan.compute = Some(choice.clone());
                    plan.apply_markup(plan.fee_bps);
                    plan.hash = digest(&(&plan.steps, &plan.compute));
                    task.compute = Some(crate::compute::Run::new(
                        plan.quote_ref,
                        task.source_selection.as_ref().unwrap().question.clone(),
                        choice.model_id,
                    ));
                    task.proposal = Some(serde_json::to_value(plan)?);
                }
            }
            status
        } else {
            Status::NeedsInput
        };
        if let Some(turn) = task.conversation.last_mut() {
            if !task
                .actions
                .iter()
                .any(|action| action["kind"] == "planning_clarification")
            {
                turn.reply = Some(reply.message);
            }
        }
        anyhow::ensure!(task.bounded(), "Task capacity reached");
        sqlx::query("UPDATE tasks SET document=$3,revision=revision+1,updated_at=now() WHERE id=$1 AND owner_id=$2 AND revision=$4")
            .bind(id).bind(p.owner).bind(serde_json::to_value(&task)?).bind(revision).execute(&mut *tx).await?;
        tx.commit().await?;
        crate::server_events::task_changed(id);
        self.view(p, id, req.request_id, Some(status)).await
    }
}
