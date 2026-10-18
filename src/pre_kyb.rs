//! Pre-KYB screening must use the resolved registry name, never a search term.
use crate::{
    resolver::{Plan, Step},
    state::{Fact, Task},
};

pub(crate) enum Screening {
    NotApplicable,
    Waiting,
    Unavailable,
    Ready(Fact),
}

pub(crate) fn screening(task: &Task, step: &Step) -> Screening {
    if step.service.contract.capability_slug != "company.sanctions-screen"
        || !(task
            .workflow
            .as_ref()
            .is_some_and(|workflow| workflow.slug == "pre-kyb-screening")
            || task
                .proposal
                .as_ref()
                .is_some_and(|plan| plan["workflow_slug"] == "pre-kyb-screening"))
    {
        return Screening::NotApplicable;
    }
    let Some(plan) = task
        .proposal
        .as_ref()
        .and_then(|value| serde_json::from_value::<Plan>(value.clone()).ok())
    else {
        return Screening::Unavailable;
    };
    let profiles: Vec<_> = plan
        .steps
        .iter()
        .filter(|profile| {
            profile.entity_ref == step.entity_ref
                && profile.service.contract.capability_slug == "company.registry-profile"
        })
        .collect();
    if profiles.is_empty() {
        return Screening::Unavailable;
    }
    if profiles
        .iter()
        .any(|profile| !task.step_finished(&profile.key()))
    {
        return Screening::Waiting;
    }
    if profiles.iter().any(|profile| {
        !task.completed.contains(&profile.key())
            || task.failed_steps.contains_key(&profile.key())
            || task.skipped_steps.contains(&profile.key())
    }) {
        return Screening::Unavailable;
    }
    let names: Vec<_> = task
        .facts
        .iter()
        .filter(|fact| {
            fact.entity_ref == step.entity_ref
                && fact.semantic_type == "company.name"
                && fact.verification == "source_observed"
                && fact.expires_at > chrono::Utc::now()
                && fact
                    .value
                    .as_str()
                    .is_some_and(|name| !name.trim().is_empty())
                && fact.provenance["kind"] == "provider_response"
                && profiles.iter().any(|profile| {
                    fact.provenance["contract_id"] == serde_json::json!(profile.service.id)
                })
                && fact.provenance["result_ref"]
                    .as_str()
                    .and_then(|id| id.parse().ok())
                    .is_some_and(|id| task.active_results.contains(&id))
        })
        .collect();
    match names.first() {
        Some(name) if names.iter().all(|other| other.value == name.value) => {
            Screening::Ready((*name).clone())
        }
        _ => Screening::Unavailable,
    }
}
