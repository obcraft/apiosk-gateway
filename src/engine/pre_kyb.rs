use super::*;
use crate::pre_kyb;
use std::collections::BTreeMap;

impl Engine {
    /// Evaluate a finished pre-KYB screening from this plan's saved documents.
    /// Every step of the approved plan counts, whatever its subject: the VAT,
    /// domain and IBAN steps are planned under their own subjects.
    pub(super) async fn pre_kyb_verdict(&self, p: &Principal, id: Uuid, task: &Task, plan: &Plan) -> anyhow::Result<Value> {
        let workflow = task.workflow.as_ref().ok_or_else(|| anyhow::anyhow!("missing workflow context"))?;
        let input = pre_kyb::Input::from_workflow(&workflow.input);
        let mut sources = Vec::new();
        for result in &task.active_results {
            let document = self.store.result(p.owner, id, *result).await?;
            if plan.steps.iter().any(|step| document["source"]["endpoint_id"] == json!(step.service.endpoint_id)) {
                sources.push(json!({"result_ref":result,"document":document}));
            }
        }
        let failures: BTreeMap<String, Value> = plan
            .steps
            .iter()
            .filter_map(|step| {
                task.failed_steps.get(&step.key()).map(|error| {
                    (format!("{} {}", step.service.api_slug, step.service.path), error.clone())
                })
            })
            .collect();
        Ok(pre_kyb::evaluate(&input, &sources, &failures, Utc::now()))
    }
}
