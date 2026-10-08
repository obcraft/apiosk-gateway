//! One bounded browser-search contingency per company, priced before approval.
use crate::{
    catalog::{fact_value, Catalog},
    parser::Intent,
    resolver::{Plan, Step},
    state::{Fact, Task},
};
use catalog_core::facts::contract::Vocabulary;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fallback {
    pub after: Vec<String>,
    pub query: String,
    #[serde(default)]
    pub queries: std::collections::BTreeMap<String, String>,
}

fn scope(capability: &str) -> Option<&'static str> {
    match capability {
        "company.registry-profile" | "company.registry-search" | "company_registry.search" => {
            Some("official company registration")
        }
        "company.financial-statements" | "company.accounts" | "company.filing-history" => {
            Some("official annual report financial statements")
        }
        "company.significant-control" => Some("official major shareholders ownership"),
        "company.registered-address-context" => Some("official registered address"),
        "soil.bodemloket.feature-info" => Some("official soil investigation registered address"),
        "planning.pdok.feature-info" => Some("official spatial planning registered address"),
        "company.lei-verification" => Some("GLEIF legal entity identifier"),
        "domain.registration" => Some("official website domain registration"),
        "company.sanctions-screen" | "company.sirenic-sanctions-check" => {
            Some("official sanctions registers")
        }
        "news.reuters-search" => Some("latest news publication dates"),
        "web.search" | "web.stableenrich-search" | "web.browserbase-search" => {
            Some("official company publications financial statements shareholders")
        }
        _ => None,
    }
}

pub fn reserve(catalog: &Catalog, task: &Task, intent: &Intent, steps: &mut Vec<Step>) {
    // Existing tasks/quotes are never retrofitted. Explicit source-only checks
    // and workflows keep their original contract.
    if !task
        .conversation
        .last()
        .is_some_and(|turn| crate::report_assets::requested(&turn.question))
    {
        return;
    }
    let Some(service) = catalog
        .services
        .iter()
        .filter(|s| {
            s.contract.capability_slug == "web.browserbase-search"
                && s.contract
                    .requires
                    .iter()
                    .all(|r| r.optional || r.fact_type == "web.source_search_query")
        })
        .min_by_key(|s| s.total())
    else {
        return;
    };
    for subject in &intent.subjects {
        if steps.len() >= crate::protocol::MAX_STEPS {
            break;
        }
        let Some(entity) = subject.entity_ref else {
            continue;
        };
        let Some(name) = fact_value(&task.facts, entity, "company.name")
            .ok()
            .flatten()
            .and_then(|v| v.as_str().map(str::to_owned))
        else {
            continue;
        };
        if steps
            .iter()
            .any(|s| s.entity_ref == entity && s.service.id == service.id)
        {
            continue;
        }
        let eligible: Vec<_> = steps
            .iter()
            .filter(|s| {
                s.entity_ref == entity
                    && s.browser_fallback.is_none()
                    && scope(&s.service.contract.capability_slug).is_some()
                    && intent
                        .goals
                        .get(s.goal_index)
                        .is_some_and(|g| !g.constraints.contains_key("source"))
            })
            .collect();
        if eligible.is_empty() {
            continue;
        }
        let mut topics: Vec<_> = eligible
            .iter()
            .filter_map(|s| scope(&s.service.contract.capability_slug))
            .collect();
        topics.sort_unstable();
        topics.dedup();
        let fallback = Fallback {
            after: eligible.iter().map(|s| s.key()).collect(),
            query: format!("{} {}", name, topics.join(" ")),
            queries: eligible
                .iter()
                .map(|s| {
                    (
                        s.key(),
                        format!(
                            "{} {}",
                            name,
                            scope(&s.service.contract.capability_slug).unwrap()
                        ),
                    )
                })
                .collect(),
        };
        let goal_index = eligible[0].goal_index;
        let candidate = Step {
            service: service.clone(),
            entity_ref: entity,
            goal_index,
            browser_fallback: Some(fallback),
        };
        if inputs(&candidate, task, &catalog.vocabulary).is_ok() {
            steps.push(candidate);
        }
    }
}

pub fn ready(task: &Task, step: &Step) -> bool {
    step.browser_fallback.as_ref().is_none_or(|f| {
        !f.after.is_empty()
            && f.after.iter().all(|key| task.step_finished(key))
            && f.after
                .iter()
                .any(|key| task.failed_steps.contains_key(key))
    })
}

pub fn settle_unused(task: &mut Task, plan: &Plan) {
    for step in &plan.steps {
        if let Some(f) = &step.browser_fallback {
            if !task.step_finished(&step.key())
                && f.after.iter().all(|key| task.step_finished(key))
                && !f
                    .after
                    .iter()
                    .any(|key| task.failed_steps.contains_key(key))
            {
                task.skipped_steps.insert(step.key());
            }
        }
    }
}

pub fn query(f: &Fallback, task: &Task) -> String {
    let mut queries: Vec<_> = f
        .queries
        .iter()
        .filter(|(key, _)| task.failed_steps.contains_key(*key))
        .map(|(_, query)| query.as_str())
        .collect();
    queries.sort_unstable();
    queries.dedup();
    if queries.is_empty() {
        f.query.clone()
    } else {
        queries.join("; ")
    }
}

pub fn inputs(step: &Step, task: &Task, vocab: &Vocabulary) -> anyhow::Result<Value> {
    let Some(f) = &step.browser_fallback else {
        match crate::pre_kyb::screening(task, step) {
            crate::pre_kyb::Screening::NotApplicable => (),
            crate::pre_kyb::Screening::Waiting => anyhow::bail!("Waiting for the registered company identity before screening."),
            crate::pre_kyb::Screening::Unavailable => anyhow::bail!("The registered company name is unavailable for screening."),
            crate::pre_kyb::Screening::Ready(name) => {
                let mut bound = task.clone();
                bound.facts.retain(|fact| fact.entity_ref != step.entity_ref || fact.semantic_type != "company.name");
                bound.facts.push(name);
                return step.service.inputs(&bound, step.entity_ref, vocab);
            }
        }
        return step.service.inputs(task, step.entity_ref, vocab);
    };
    // The exact query is part of the hashed quote. Validate it with the real
    // service contract; never mutate the service, price, or permanent facts.
    let mut bound = task.clone();
    bound.facts.retain(|v| {
        !(v.entity_ref == step.entity_ref && v.semantic_type == "web.source_search_query")
    });
    let query = query(f, task);
    bound.facts.push(Fact {
        entity_ref: step.entity_ref,
        semantic_type: "web.source_search_query".into(),
        value: json!(query),
        provenance: json!({"kind":"quoted_browser_fallback"}),
        observed_at: Utc::now(),
        expires_at: Utc::now() + chrono::Duration::minutes(1),
        verification: "derived_search_query".into(),
    });
    step.service.inputs(&bound, step.entity_ref, vocab)
}

/// Only known record containers are treated as empty. A zero sanctions match
/// is a successful screening result; false/zero values are never data gaps.
pub fn empty_records(step: &Step, data: &Value) -> bool {
    if scope(&step.service.contract.capability_slug).is_none()
        || step.service.contract.capability_slug.contains("sanctions")
    {
        return false;
    }
    if data.is_null() {
        return true;
    }
    // An empty Companies House PSC register is a valid register response,
    // not a source failure or permission to buy a web fallback. It does not
    // establish that the company has no owners.
    if step.service.api_slug == "companies-house"
        && step.service.contract.capability_slug == "company.significant-control"
        && data["items"].as_array().is_some_and(Vec::is_empty)
        && data["total_results"].as_u64() == Some(0)
        && data["start_index"].as_u64() == Some(0)
    {
        return false;
    }
    if let Some(rows) = data.as_array() {
        return rows.is_empty();
    }
    [
        "results",
        "items",
        "records",
        "resultaten",
        "resultats",
        "filings",
    ]
    .iter()
    .any(|key| {
        data.get(key)
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use uuid::Uuid;

    pub(crate) fn fixture() -> (Catalog, Task, Intent, Plan) {
        let mut value: Value =
            serde_json::from_str(include_str!("../tests/fixtures/integration-catalog.json"))
                .unwrap();
        let mut browser = value["services"][0].clone();
        browser["id"] = json!(Uuid::new_v4());
        browser["endpoint_id"] = json!(Uuid::new_v4());
        browser["api_slug"] = json!("browserbase");
        browser["name"] = json!("Browserbase");
        browser["path"] = json!("/search");
        browser["method"] = json!("POST");
        browser["contract"] = json!({"capability_slug":"web.browserbase-search","version":1,"cardinality":"one","requires":[{"fact_type":"web.source_search_query","subject_role":"subject","optional":false}],"input_map":{"web.source_search_query":{"location":"body","name":"query"}},"output_map":{"base":"$","produces":{"web.browserbase_search_page":{"path":"$"}}},"max_fan_out":1});
        browser["sample_payload"] = json!({"results":[{"url":"https://example.com"}]});
        browser["list_atomic"] = json!(10000);
        value["services"].as_array_mut().unwrap().push(browser);
        value["vocabulary"].as_array_mut().unwrap().extend([
            json!({"slug":"web.source_search_query","value_schema":{"type":"string","minLength":1}}),
            json!({"slug":"web.browserbase_search_page","value_schema":{"type":"object"}})
        ]);
        let catalog = Catalog::build(serde_json::from_value(value).unwrap());
        let mut task = Task::default();
        task.record_turn("Create a financing report for Example NV", None);
        let mut intent: Intent=serde_json::from_value(json!({"subjects":[{"label":"Example NV","values":{"company.name":"Example NV"}}],"goals":[{"capability":"company.accounts","subject":0,"constraints":{}}],"answer_operations":["Create a report"],"unresolved_references":[]})).unwrap();
        crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan) =
            crate::resolver::resolve(&catalog, &task, &intent)
        else {
            panic!("expected plan")
        };
        (catalog, task, intent, plan)
    }

    #[test]
    fn quoted_reserve_is_conditional_and_includes_markup() {
        let (catalog, mut task, _, mut plan) = fixture();
        let fallback = plan.steps.last().unwrap();
        assert!(fallback.browser_fallback.is_some());
        assert!(!ready(&task, fallback));
        assert_eq!(
            plan.cap_atomic,
            plan.steps.iter().map(|s| s.service.total()).sum::<i64>()
        );
        let primary = plan
            .steps
            .iter()
            .find(|s| s.service.contract.capability_slug == "company.accounts")
            .unwrap();
        task.failed_steps
            .insert(primary.key(), json!({"code":"source_unavailable"}));
        for step in plan.steps.iter().filter(|s| s.browser_fallback.is_none()) {
            task.completed.push(step.key());
        }
        assert!(ready(&task, fallback));
        let query = inputs(fallback, &task, &catalog.vocabulary).unwrap();
        assert!(query["query"]
            .as_str()
            .unwrap()
            .contains("Example NV official annual report"));
        let old = plan.cap_atomic;
        plan.apply_markup(2000);
        assert!(plan.cap_atomic > old);
        assert_eq!(
            plan.cap_atomic,
            plan.steps
                .iter()
                .map(|s| s.service.total_with(2000))
                .sum::<i64>()
        );
    }

    #[test]
    fn success_skips_without_claiming_a_result_and_old_quotes_stay_old() {
        let (_, mut task, _, plan) = fixture();
        let fallback = plan.steps.last().unwrap();
        for step in plan.steps.iter().filter(|s| s.browser_fallback.is_none()) {
            task.completed.push(step.key());
        }
        settle_unused(&mut task, &plan);
        assert!(task.skipped_steps.contains(&fallback.key()));
        assert!(!task.completed.contains(&fallback.key()));
        assert!(!ready(&task, fallback));
        let mut old = serde_json::to_value(&plan.steps[0]).unwrap();
        old.as_object_mut().unwrap().remove("browser_fallback");
        assert!(serde_json::from_value::<Step>(old)
            .unwrap()
            .browser_fallback
            .is_none());
        let mut changed = plan.clone();
        changed
            .steps
            .last_mut()
            .unwrap()
            .browser_fallback
            .as_mut()
            .unwrap()
            .query
            .push_str(" changed");
        assert_ne!(
            crate::state::digest(&changed.steps),
            plan.hash,
            "query changes require a new approval hash"
        );
    }

    #[test]
    fn explicit_source_and_non_report_do_not_add_a_contingency() {
        let (catalog, mut task, mut intent, _) = fixture();
        intent.goals[0]
            .constraints
            .insert("source".into(), json!("fixture"));
        let crate::resolver::Resolution::Plan(plan) =
            crate::resolver::resolve(&catalog, &task, &intent)
        else {
            panic!()
        };
        assert!(plan.steps.iter().all(|s| s.browser_fallback.is_none()));
        intent.goals[0].constraints.clear();
        task.conversation.clear();
        let crate::resolver::Resolution::Plan(plan) =
            crate::resolver::resolve(&catalog, &task, &intent)
        else {
            panic!()
        };
        assert!(plan.steps.iter().all(|s| s.browser_fallback.is_none()));
    }

    #[test]
    fn empty_records_are_not_zero_sanctions_matches_or_zero_financials() {
        let (_, _, _, plan) = fixture();
        let mut step = plan
            .steps
            .iter()
            .find(|s| s.service.contract.capability_slug == "company.accounts")
            .unwrap()
            .clone();
        assert!(empty_records(&step, &json!({"results":[]})));
        assert!(!empty_records(&step, &json!({"revenue":0})));
        step.service.contract.capability_slug = "company.sanctions-screen".into();
        assert!(!empty_records(&step, &json!({"results":[],"matches":0})));
    }

    #[test]
    fn confirmed_empty_companies_house_psc_register_is_not_a_source_failure() {
        let (_, _, _, plan) = fixture();
        let mut step = plan.steps[0].clone();
        step.service.api_slug = "companies-house".into();
        step.service.contract.capability_slug = "company.significant-control".into();
        let empty = json!({"items":[],"total_results":0,"start_index":0});
        assert!(!empty_records(&step, &empty));
        assert!(empty_records(&step, &json!({"items":[],"total_results":3,"start_index":0})));
        assert!(empty_records(&step, &json!({"items":[]})));
        assert!(empty_records(&step, &Value::Null));
        step.service.api_slug = "other-source".into();
        assert!(empty_records(&step, &empty));
    }
}
