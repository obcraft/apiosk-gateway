//! A model outage pauses an unpaid question on its existing, scoped task.
//! Retrying can reuse validated discovery, never a price or paid execution.
use super::*;
use crate::state::{IntakeRecovery, IntakeStage};

pub(super) struct InterruptedIntake<'a> {
    pub id: Uuid,
    pub revision: i64,
    pub request: &'a DiscoverRequest,
    pub stage: IntakeStage,
    pub turn_recorded: bool,
}

impl Engine {
    /// Resume the exact saved App question. Preparing a fresh quote neither
    /// inherits an approval nor starts the execution worker.
    pub async fn resume_plan(&self, p: &Principal, id: Uuid, req: DiscoverRequest) -> anyhow::Result<Response> {
        anyhow::ensure!(p.is_person(), "Resuming a plan requires a person in the App");
        let state = req.state.as_ref().ok_or_else(|| anyhow::anyhow!("A saved state is required"))?;
        anyhow::ensure!(state.state_ref == id, "Resume path must match its saved state");
        self.signer.verify(p.owner, state, true)?;
        if let Some(response) = self.request_claim(p, req.request_id, &digest(&("resume-plan", id, &req)), "resume-plan").await? {
            return Ok(response);
        }
        let result = async {
            let (revision, task, connection) = self.store.load_scoped(p, id).await?;
            anyhow::ensure!(connection == p.connection, "Resume requires the original App context");
            if state.revision != revision || state.expires_at <= Utc::now() {
                return self.view(p, id, req.request_id, Some(Status::StateConflict)).await;
            }
            anyhow::ensure!(task.intake_recovery.as_ref().is_some_and(|saved| saved.matches(&req))
                && task.proposal.is_none(), "Only the paused question can be resumed");
            self.discover_inner(p, &req, true).await
        }.await;
        Ok(self.complete(p, req.request_id, result).await)
    }

    pub(super) async fn pause_intake(
        &self,
        p: &Principal,
        task: &mut Task,
        interrupted: InterruptedIntake<'_>,
        error: anyhow::Error,
    ) -> anyhow::Result<Response> {
        let InterruptedIntake { id, revision, request:req, stage, turn_recorded } = interrupted;
        if !matches!(error.downcast_ref::<crate::parser::ModelFailure>(), Some(crate::parser::ModelFailure::Unavailable)) {
            return Err(error);
        }
        tracing::warn!(task = %id, ?stage, "Ask interpretation paused; question retained for recovery");
        self.archive_conversation_output(p, id, req.request_id, task).await?;
        // Retain context for a follow-up, but remove every executable action
        // and old price. Nothing is approved or replayed by recovery.
        let previous_intent = task.intent.clone();
        clear_plan(task);
        task.intent = previous_intent;
        if turn_recorded {
            if let Some(turn) = task.conversation.last_mut() { turn.reply = None; }
        } else {
            task.record_turn(&req.question, None);
        }
        task.intake_recovery = Some(IntakeRecovery {
            question: req.question.clone(),
            context_delta: req.context_delta.clone(),
            stage,
            created_at: Utc::now(),
        });
        self.store.save_intent(p.owner, id, revision, task).await?;
        self.view(p, id, req.request_id, Some(Status::Failed)).await
    }
}
