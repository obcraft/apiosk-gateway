use super::*;
use crate::{
    followups::related_services,
    parser::{Goal, Subject},
    state::Envelope,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FollowupRequest {
    pub request_id: Uuid,
    pub state: Envelope,
    pub service_ref: Uuid,
    pub entity_ref: Uuid,
}

impl Engine {
    pub async fn followup(&self, p: &Principal, req: FollowupRequest) -> anyhow::Result<Response> {
        anyhow::ensure!(!single_call_mode(), "Automatic follow-up research is paused. Start a new single-call question.");
        anyhow::ensure!(
            p.is_person(),
            "Related services require a person in the App"
        );
        if let Some(r) = self
            .request_claim(p, req.request_id, &digest(&("follow-up", &req)))
            .await?
        {
            return Ok(r);
        }
        let result = self.followup_inner(p, &req).await;
        Ok(self.complete(p, req.request_id, result).await)
    }
    async fn followup_inner(
        &self,
        p: &Principal,
        req: &FollowupRequest,
    ) -> anyhow::Result<Response> {
        self.signer.verify(p.owner, &req.state, true)?;
        let id = req.state.state_ref;
        let (revision, mut task, connection) = self.store.load_scoped(&p, id).await?;
        anyhow::ensure!(
            connection.is_none(),
            "Continue connected chatbot tasks through their chatbot"
        );
        if req.state.expires_at <= Utc::now() || revision != req.state.revision {
            return self
                .view(p, id, req.request_id, Some(Status::StateConflict))
                .await;
        }
        self.prepare_conversation(p.owner, id, &mut task).await?;
        let catalog = self.catalog.fresh().await?;
        let selected = related_services(&catalog, &task)
            .into_iter()
            .find(|s| s.service_ref == req.service_ref && s.entity_ref == req.entity_ref)
            .ok_or_else(|| {
                anyhow::anyhow!("This related service is no longer available. Refresh the task.")
            })?;
        let service = catalog
            .services
            .iter()
            .find(|s| s.id == selected.service_ref)
            .unwrap()
            .clone();
        service.inputs(&task, req.entity_ref, &catalog.vocabulary)?;
        let intent = Intent {
            subjects: vec![Subject {
                entity_ref: Some(req.entity_ref),
                label: selected.subject.clone(),
                values: Default::default(),
            }],
            goals: vec![Goal {
                capability: service.contract.capability_slug.clone(),
                subject: 0,
                constraints: [("source".into(), json!(service.api_slug))].into(),
            }],
            answer_operations: vec![],
            unresolved_references: vec![],
        };
        let steps = vec![resolver::Step {
            service,
            entity_ref: req.entity_ref,
            goal_index: 0,
        }];
        let plan = Plan {
            quote_ref: Uuid::new_v4(),
            cap_atomic: steps[0].service.total(),
            hash: digest(&steps),
            steps,
            expires_at: Utc::now() + chrono::Duration::minutes(10),
        };
        self.archive_conversation_output(p, id, req.request_id, &mut task).await?;
        task.active_results.clear();
        task.analysis = None;
        task.failed_steps.clear();
        task.intent = Some(serde_json::to_value(intent)?);
        task.proposal = Some(serde_json::to_value(plan)?);
        task.focus.entity_refs = vec![req.entity_ref];
        task.focus.goal_refs = vec![Uuid::new_v4()];
        task.record_turn(
            &format!(
                "Get {} for {} using {}.",
                selected.title, selected.subject, selected.provider
            ),
            Some("Review the plan and total price below.".into()),
        );
        task.actions = vec![serde_json::to_value(Action::Run {
            action_id: Uuid::new_v4(),
            step: 0,
        })?];
        self.store.save_intent(p.owner, id, revision, &task).await?;
        self.view(p, id, req.request_id, Some(Status::Ready)).await
    }
}
