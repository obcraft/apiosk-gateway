//! The performance class a question is budgeted under, decided from its
//! intent and priced plan alone (#2). Deterministic: the same saved task
//! always has the same class, and a question without a plan is unclassified.

use super::QueryClass;
use crate::{parser::Intent, resolver::Plan, state::Task};
use catalog_core::facts::contract::Cardinality;
use std::collections::{BTreeMap, BTreeSet};

/// Workflows that assemble several checks about one organisation.
const DOSSIER_WORKFLOWS: [&str; 6] = [
    "european-company-dossier",
    "tender-company-dossier",
    "company-ownership",
    "company-health",
    "customer-onboarding",
    crate::workflows::SUPPLIER_CHECK,
];
/// Words in a capability or a required input that mark a list of matches
/// rather than one record: a search, a browse or a free-text query.
const LIST_WORDS: [&str; 10] = ["search", "list", "browse", "mentions", "notices", "datasets", "calls", "keyword", "query", "q"];

pub fn classify(task: &Task) -> QueryClass {
    let plan = task.proposal.as_ref().and_then(|plan| serde_json::from_value::<Plan>(plan.clone()).ok());
    let intent = task.intent.as_ref().and_then(|intent| serde_json::from_value::<Intent>(intent.clone()).ok());
    match (&plan, &intent) {
        (Some(plan), Some(intent)) => {
            let workflow = task.workflow.as_ref().map(|workflow| workflow.slug.as_str()).or(plan.workflow_slug.as_deref());
            of(intent, plan, workflow)
        }
        _ => QueryClass::Unclassified,
    }
}

fn of(intent: &Intent, plan: &Plan, workflow: Option<&str>) -> QueryClass {
    let mut goals: BTreeMap<usize, usize> = BTreeMap::new();
    for goal in &intent.goals {
        *goals.entry(goal.subject).or_default() += 1;
    }
    if workflow.is_some_and(|slug| DOSSIER_WORKFLOWS.contains(&slug))
        || crate::uk_supplier::is_check(intent)
        || goals.values().any(|count| *count >= 3)
    {
        return QueryClass::Dossier;
    }
    let subjects: BTreeSet<_> = goals.keys().collect();
    if subjects.len() >= 2 {
        return QueryClass::Comparison;
    }
    // An identifier search that only resolves a subject for the asked
    // lookup does not make the question a list; the asked steps decide.
    let asked: Vec<_> = plan.steps.iter().filter(|step| intent.goals.iter().any(|goal| goal.capability == step.service.contract.capability_slug)).collect();
    let steps = if asked.is_empty() { plan.steps.iter().collect() } else { asked };
    let listing_words = |text: &str| text.split(['.', '-', '_']).any(|word| LIST_WORDS.contains(&word));
    let listing = steps.iter().any(|step| {
        let contract = &step.service.contract;
        contract.cardinality == Cardinality::Many
            || listing_words(&contract.capability_slug)
            || contract.requires.iter().any(|input| listing_words(&input.fact_type))
    });
    if listing { QueryClass::SearchList } else { QueryClass::SimpleLookup }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;
    use serde_json::json;

    fn task(goals: &[(usize, &str)], workflow: Option<&str>) -> Task {
        planned(goals, &goals.iter().map(|(_, capability)| *capability).collect::<Vec<_>>(), workflow)
    }
    fn planned(goals: &[(usize, &str)], steps: &[&str], workflow: Option<&str>) -> Task {
        let catalog = Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/integration-catalog.json")).unwrap());
        let service = |capability: &str| catalog.services.iter().find(|s| s.contract.capability_slug == capability).cloned();
        let subjects = goals.iter().map(|(subject, _)| *subject).max().map_or(0, |last| last + 1);
        let steps: Vec<_> = steps.iter().filter_map(|capability| service(capability)).map(|service| json!({
            "service": service, "entity_ref": uuid::Uuid::nil(), "goal_index": 0})).collect();
        let mut plan = json!({"quote_ref": uuid::Uuid::nil(), "steps": steps, "cap_atomic": 1, "expires_at": "2030-01-01T00:00:00Z", "hash": "fixture"});
        if let Some(workflow) = workflow {
            plan["workflow_slug"] = json!(workflow);
        }
        Task {
            intent: Some(json!({"subjects": (0..subjects).map(|index| json!({"entity_ref":null,"label":format!("Subject {index}"),"values":{}})).collect::<Vec<_>>(),
                "goals": goals.iter().map(|(subject, capability)| json!({"capability":capability,"subject":subject,"constraints":{}})).collect::<Vec<_>>(),
                "answer_operations": [], "unresolved_references": []})),
            proposal: Some(plan),
            ..Task::default()
        }
    }

    #[test]
    fn classes_follow_workflow_subjects_and_list_shaped_steps() {
        assert_eq!(classify(&task(&[(0, "company.accounts")], None)), QueryClass::SimpleLookup);
        assert_eq!(classify(&task(&[(0, "company.search")], None)), QueryClass::SearchList);
        assert_eq!(classify(&task(&[(0, "company.accounts"), (1, "company.accounts")], None)), QueryClass::Comparison);
        assert_eq!(classify(&task(&[(0, "company.accounts"), (0, "company.search"), (0, "company.accounts")], None)), QueryClass::Dossier);
        assert_eq!(classify(&task(&[(0, "company.accounts")], Some("company-health"))), QueryClass::Dossier);
        assert_eq!(classify(&task(&[(0, "company.accounts")], Some("tender-finder"))), QueryClass::SimpleLookup);
        assert_eq!(classify(&planned(&[(0, "company.accounts")], &["company.search", "company.accounts"], None)), QueryClass::SimpleLookup,
            "a search that resolves the company for a lookup is not a list question");
    }

    #[test]
    fn a_question_without_a_priced_plan_is_unclassified() {
        let mut unpriced = task(&[(0, "company.accounts")], None);
        unpriced.proposal = None;
        assert_eq!(classify(&unpriced), QueryClass::Unclassified);
        assert_eq!(classify(&Task::default()), QueryClass::Unclassified);
    }
}
