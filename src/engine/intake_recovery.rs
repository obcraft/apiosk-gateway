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
