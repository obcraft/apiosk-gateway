//! All six published homepage examples must reach a complete, priced plan.
//! Recorded source contracts are the only fixtures: intake, discovery, coverage,
//! subject binding and pricing run their production implementations. There is no
//! prepared-plan stub, database, model access, source request or payment.
use crate::{
    catalog::{Catalog, Snapshot},
    parser::Parser,
    resolver::Resolution,
    state::Task,
};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::atomic::Ordering};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Starter {
    id: String,
    question: String,
    subject: String,
    source_hint: String,
    outputs: Vec<String>,
    provider: String,
    operation: String,
    method: String,
    path: String,
    inputs: Value,
}

fn catalog() -> Catalog {
    // Include competing unrelated operations as well as the expected routes;
    // a one-service fixture could conceal incorrect discovery and selection.
    let mut services = BTreeMap::new();
    let mut vocabulary = BTreeMap::new();
    let mut known_sources = BTreeMap::new();
    for text in [
        include_str!("../tests/fixtures/workflows/catalog.json"),
        include_str!("../tests/fixtures/profession-output-coverage/catalog.json"),
        include_str!("../tests/fixtures/tenderned-browse.json"),
        include_str!("../tests/fixtures/frankfurter/catalog.json"),
        include_str!("../tests/fixtures/company-reputation/catalog.json"),
    ] {
        let snapshot: Snapshot = serde_json::from_str(text).unwrap();
        for service in snapshot.services {
            services.insert(
                (
                    service.api_slug.clone(),
                    service.contract.capability_slug.clone(),
                ),
                service,
            );
        }
        for fact in snapshot.vocabulary {
            vocabulary.insert(fact.slug.clone(), fact);
        }
        for source in snapshot.known_sources {
            known_sources.insert(source.slug.clone(), source);
        }
    }
    // These reviewed production schemas already accompany the saved profession
    // catalog, whose historical opaque payload fields predate their migration.
    let reviewed: Value = serde_json::from_str(include_str!(
        "../tests/fixtures/profession-output-coverage/reviewed-schemas.json"
    ))
    .unwrap();
    for (slug, fact) in &mut vocabulary {
        if let Some(schema) = reviewed["schemas"].get(slug) {
            fact.value_schema = schema.clone();
        }
    }
    let catalog = Catalog::build(Snapshot {
        services: services.into_values().collect(),
        vocabulary: vocabulary.into_values().collect(),
        known_sources: known_sources.into_values().collect(),
    });
    assert!(
        catalog.rejected.is_empty(),
        "rejected recorded contracts: {:?}",
        catalog.rejected
    );
    catalog
}

async fn complete_plan(id: &str) {
    let starters: Vec<Starter> =
        serde_json::from_str(include_str!("../tests/fixtures/homepage-starters.json")).unwrap();
    assert_eq!(
        starters.len(),
        6,
        "each published question must have an acceptance case"
    );
    let case = starters.iter().find(|case| case.id == id).unwrap();
    let catalog = catalog();
    // An unsupported URL scheme makes accidental model fallback fail without
    // reaching a local listener or an external API.
    let parser = Parser::new(
        reqwest::Client::new(),
        "offline://model-calls-forbidden".into(),
        String::new(),
        "homepage-gate".into(),
    );
    let task = Task::default();
    let intake = parser
        .ask_intake(&case.question, &task, &catalog, None)
        .await
        .unwrap();
    let requested = &intake.parsed_request;
    assert_eq!(
        requested.meta.text_sha256,
        hex::encode(Sha256::digest(case.question.as_bytes()))
    );
    assert_eq!(requested.language, "en");
    assert_eq!(
        requested.subjects.len(),
        1,
        "{id}: no dropped or invented subject"
    );
    assert_eq!(requested.subjects[0].label, case.subject);
    assert_eq!(
        requested.capabilities.len(),
        1,
        "{id}: preserve the complete retrieval scope"
    );
    let capability = &requested.capabilities[0];
    assert_eq!(
        capability.outputs, case.outputs,
        "{id}: no requested field may disappear before coverage checking"
    );
    assert_eq!(
        capability.source_hint.as_deref(),
        Some(case.source_hint.as_str())
    );
    assert!(
        capability.missing.is_empty(),
        "{id}: the published example supplies its required inputs"
    );
    assert_eq!(
        capability.subject_id.as_deref(),
        Some(requested.subjects[0].id.as_str())
    );
    assert_eq!(requested.deliverable.format, "chat");
    if id == "tenderned" {
        assert_eq!(
            requested.subjects[0].identifiers["country"],
            "the Netherlands"
        );
    }
    assert!(
        intake.selected_capabilities.contains(&case.operation),
        "{id}: expected executable operation must be discovered"
    );

    let reply = parser
        .chat_from_capabilities(&case.question, None, &task, &catalog, &intake)
        .await
        .unwrap();
    assert_eq!(
        parser.calls.load(Ordering::Relaxed),
        0,
        "{id}: no model dependency for a complete published example"
    );
    assert!(reply.discover, "{id}: {}", reply.message);
    let mut intent = reply
        .intent
        .expect("a priced plan requires complete intent");
    assert!(
        intent.unresolved_references.is_empty(),
        "{id}: no unresolved subject"
    );
    assert_eq!(intent.subjects.len(), 1);
    assert_eq!(intent.subjects[0].label, case.subject);
    assert_eq!(
        intent.goals.len(),
        1,
        "{id}: exactly one requested source operation"
    );
    assert_eq!(intent.goals[0].capability, case.operation);
    assert_eq!(intent.goals[0].subject, 0);
    assert_eq!(intent.goals[0].constraints["source"], case.provider);

    let mut task = Task {
        capability_intake: Some(intake),
        ..task
    };
    crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
    let resolution = crate::resolver::resolve(&catalog, &task, &intent);
    let Resolution::Plan(plan) = resolution else {
        panic!("{id}: expected complete plan, got {resolution:?}")
    };
    assert_eq!(
        plan.steps.len(),
        1,
        "{id}: no unrelated or duplicate purchase"
    );
    assert!(
        !plan.quote_ref.is_nil(),
        "{id}: the plan must have a reviewable quote"
    );
    assert!(
        plan.cap_atomic > 0,
        "{id}: ordinary price and approval remain required"
    );
    assert_eq!(
        plan.cap_atomic,
        plan.steps
            .iter()
            .map(|step| plan.step_total(step))
            .sum::<i64>()
    );
    let step = &plan.steps[0];
    assert_eq!(step.service.api_slug, case.provider);
    assert_eq!(step.service.contract.capability_slug, case.operation);
    assert_eq!(step.service.method, case.method);
    assert_eq!(step.service.path, case.path);
    assert_eq!(Some(step.entity_ref), intent.subjects[0].entity_ref);
    assert_eq!(
        step.service
            .inputs(&task, step.entity_ref, &catalog.vocabulary)
            .unwrap(),
        case.inputs,
        "{id}: literal inputs and reviewed adapter parameters must be bound before approval"
    );
    assert_eq!(
        task.capability_intake
            .as_ref()
            .unwrap()
            .parsed_request
            .capabilities[0]
            .outputs,
        case.outputs,
        "{id}: the reviewed plan must retain every requested field"
    );
}

#[tokio::test]
async fn kvk_registered_company_is_priced() {
    complete_plan("kvk").await;
}
#[tokio::test]
async fn tenderned_cleaning_search_is_priced() {
    complete_plan("tenderned").await;
}
#[tokio::test]
async fn crunchbase_description_and_link_are_priced() {
    complete_plan("crunchbase").await;
}
#[tokio::test]
async fn glassdoor_rating_and_review_count_are_priced() {
    complete_plan("glassdoor").await;
}
#[tokio::test]
async fn vies_validity_and_check_date_are_priced() {
    complete_plan("vies").await;
}
#[tokio::test]
async fn ecb_rate_and_observation_date_are_priced() {
    complete_plan("ecb").await;
}

#[test]
fn extra_requirements_cannot_be_dropped_to_fit_a_homepage_example() {
    let starters: Vec<Starter> =
        serde_json::from_str(include_str!("../tests/fixtures/homepage-starters.json")).unwrap();
    for case in starters {
        let richer = format!(
            "{} Also include a credit-risk assessment and a PDF report.",
            case.question
        );
        assert!(crate::capability_intake::literal_request(&richer, "homepage-gate").is_none(),
            "{}: added user requirements must reach full interpretation instead of reusing the narrower starter", case.id);
    }
}

#[tokio::test]
async fn stale_or_changed_named_source_scope_cannot_shortcut_to_a_narrower_plan() {
    let catalog = catalog();
    let starters: Vec<Starter> =
        serde_json::from_str(include_str!("../tests/fixtures/homepage-starters.json")).unwrap();
    for case in starters
        .iter()
        .filter(|case| matches!(case.id.as_str(), "crunchbase" | "glassdoor"))
    {
        for change in ["subject", "outputs", "deliverable", "question_hash"] {
            let parser = Parser::new(
                reqwest::Client::new(),
                "offline://model-calls-forbidden".into(),
                String::new(),
                "homepage-gate".into(),
            );
            let task = Task::default();
            let mut intake = parser
                .ask_intake(&case.question, &task, &catalog, None)
                .await
                .unwrap();
            match change {
                "subject" => intake.parsed_request.subjects[0].label = "Another company".into(),
                "outputs" => intake.parsed_request.capabilities[0]
                    .outputs
                    .push("credit_risk_score".into()),
                "deliverable" => intake.parsed_request.deliverable.format = "pdf".into(),
                "question_hash" => intake.parsed_request.meta.text_sha256 = "old-question".into(),
                _ => unreachable!(),
            }
            let reply = parser
                .chat_from_capabilities(&case.question, None, &task, &catalog, &intake)
                .await;
            assert!(reply.as_ref().map_or(true, |reply| !reply.discover && reply.intent.is_none()),
                "{} / {change}: stale or broader scope must not silently become the original one-source plan: {reply:?}", case.id);
        }
    }
}

#[tokio::test]
async fn source_search_location_filters_require_the_full_planner() {
    let catalog = catalog();
    let parser = Parser::new(
        reqwest::Client::new(),
        "offline://model-calls-forbidden".into(),
        String::new(),
        "homepage-gate".into(),
    );
    for question in [
        "Search Crunchbase for Starbucks in Seattle and show its company description and profile link.",
        "Search Glassdoor for Starbucks in Seattle and show its employer rating and review count.",
    ] {
        let intake = parser.ask_intake(question, &Task::default(), &catalog, None).await.unwrap();
        assert_eq!(intake.parsed_request.subjects[0].identifiers["location"], "Seattle");
        assert!(!crate::capability_intake::named_source_fields::matches_intake(question, &intake),
            "a search contract without a location input must not bypass planning merely because it has the requested output fields");
    }
}
