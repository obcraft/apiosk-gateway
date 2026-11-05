use super::*;
use crate::catalog::Snapshot;
use crate::parser::{Goal, Subject};

fn fixture() -> (Catalog, Task, Intent) {
    let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../tests/fixtures/integration-catalog.json")).unwrap();
    // Distinct, arbitrary providers: no named-source behavior may be needed.
    for (index, service) in snapshot.services.iter_mut().enumerate() {
        service.api_slug = format!("provider-{}", Uuid::new_v4());
        service.name = format!("Source {index}");
    }
    let catalog = Catalog::build(snapshot);
    assert!(catalog.rejected.is_empty());
    let intake = serde_json::from_value(json!({
        "parsed_request":{"language":"en","subjects":[],"capabilities":[],
            "deliverable":{"format":"chat","operations":[]},
            "meta":{"model":"fixture","parsed_at":"2026-10-13T00:00:00Z","text_sha256":"fixture"}},
        "matches":[],"selected_capabilities":[],"catalog_version":catalog.version,"apiosk_sources_searched":0
    })).unwrap();
    let mut task = Task { capability_intake: Some(intake), ..Default::default() };
    let mut intent = Intent {
        subjects: vec![Subject { entity_ref: None, label: "Supplied company".into(), values: BTreeMap::from([("company.name".into(), json!("Supplied company"))]) }],
        goals: ["company.search", "company.accounts"].into_iter().map(|capability| Goal { capability: capability.into(), subject: 0, constraints: BTreeMap::new() }).collect(),
        answer_operations: vec![], unresolved_references: vec![],
    };
    crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
    (catalog, task, intent)
}

#[test]
fn independent_sources_request_missing_input_even_when_another_call_produces_it() {
    let (catalog, task, intent) = fixture();
    assert!(matches!(resolve(&catalog, &task, &intent), Resolution::Missing { kind, .. } if kind == "company.id"));
    assert!(task.proposal.is_none() && task.pending_dispatches.is_empty());
    // Removing the lookup provider must not change which input is requested.
    let mut only_report = intent;
    only_report.goals.remove(0);
    assert!(matches!(resolve(&catalog, &task, &only_report), Resolution::Missing { kind, .. } if kind == "company.id"));
}

#[test]
fn explicit_same_subject_dependencies_work_without_authorizing_extra_lookups() {
    let (catalog, mut task, intent) = fixture();
    let question = "Search Supplied company and get its accounts using the returned company ID";
    let parsed_request = crate::capability_intake::normalize(&json!({"language":"en",
        "subjects":[{"id":"s1","label":"Supplied company","type":"company","identifiers":{"name":"Supplied company"}}],
        "capabilities":[
            {"slug":"company.search","name":"Search","description":"Find the company ID","domain":"company","inputs":["company.name"],"outputs":["company.id"],"subject_id":"s1","confidence":1},
            {"slug":"company.accounts","name":"Accounts","description":"Fetch the accounts using the found company ID","domain":"company","inputs":["company.id"],"outputs":["company.accounts"],"missing":["company.id"],"subject_id":"s1","confidence":1}],
        "deliverable":{"format":"chat","operations":[]}}).to_string(), question, "fixture").unwrap();
    let matches = parsed_request.capabilities.iter().map(|cap| {
        let service = catalog.services.iter().find(|service| service.contract.capability_slug == cap.slug).unwrap();
        serde_json::from_value(json!({"slug":cap.slug,"query":cap.name,"operations":[cap.slug],
            "apiosk":[{"origin":"apiosk","name":service.name,"description":"Fixture source","method":"GET","availability":"supported","capability":cap.slug,"endpoint_id":service.endpoint_id,"source_slug":service.api_slug}],
            "coinbase":[],"coinbase_status":"ready"})).unwrap()
    }).collect();
    task.capability_intake = Some(crate::capability_intake::Intake { parsed_request, matches,
        selected_capabilities: intent.goals.iter().map(|goal|goal.capability.clone()).collect(),
        apiosk_sources_searched: 2, catalog_version: catalog.version.clone() });
    let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else { panic!("explicit dependency should resolve"); };
    assert_eq!(plan.steps.len(), 2);
    assert!(!plan.independent_calls);
    assert_eq!(plan.steps[0].service.contract.capability_slug, "company.search");
    let saved = task.capability_intake.clone();
    task.capability_intake.as_mut().unwrap().parsed_request.capabilities[0].subject_id = Some("another-company".into());
    assert!(matches!(resolve(&catalog, &task, &intent), Resolution::Missing { .. }), "never borrow another company's identifier");
    task.capability_intake = saved;
    task.capability_intake.as_mut().unwrap().parsed_request.capabilities[0].outputs.clear();
    assert!(matches!(resolve(&catalog, &task, &intent), Resolution::Missing { .. }), "a lookup must explicitly promise the required identifier");
}

#[test]
fn independent_sources_quote_only_the_requested_calls_after_all_inputs_exist() {
    let (catalog, mut task, mut intent) = fixture();
    intent.subjects[0].values.insert("company.id".into(), json!("001234"));
    crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
    let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else { panic!("complete independent batch"); };
    assert!(plan.independent_calls);
    assert_eq!(plan.steps.len(), 2);
    assert_eq!(plan.cap_atomic, plan.steps.iter().map(|step| plan.step_total(step)).sum::<i64>());
    assert!(plan.steps.iter().all(|step| step.browser_fallback.is_none()));
    assert!(describe_steps(&plan, &[]).iter().all(|step| step["dependencies"] == json!([])));
    plan.validate_independent_inputs(&task, &catalog.vocabulary).unwrap();
    task.facts.retain(|fact| fact.semantic_type != "company.id");
    assert!(plan.validate_independent_inputs(&task, &catalog.vocabulary).is_err(), "no partial batch if an input expired or disappeared");
}

#[test]
fn independent_source_limit_quotes_ten_and_rejects_eleven_without_truncation() {
    let (catalog, task, mut intent) = fixture();
    let template = catalog.services.iter().find(|s| s.contract.capability_slug == "company.search").unwrap();
    let services = (0..11).map(|index| {
        let mut service = template.clone();
        service.id = Uuid::new_v4();
        service.endpoint_id = Uuid::new_v4();
        service.api_slug = format!("provider-{}", Uuid::new_v4());
        service.contract.capability_slug = format!("company.lookup-{index}");
        service
    }).collect();
    let catalog = Catalog::build(Snapshot { services, vocabulary: catalog.vocabulary.into_values().collect(), known_sources: vec![] });
    assert_eq!(catalog.services.len(), 11);
    for count in [10, 11] {
        intent.goals = (0..count).map(|index| Goal {
            capability: format!("company.lookup-{index}"), subject: 0, constraints: BTreeMap::new(),
        }).collect();
        match resolve(&catalog, &task, &intent) {
            Resolution::Plan(plan) if count == 10 => {
                assert_eq!(plan.steps.len(), 10);
                assert_eq!(plan.steps.iter().map(|step| &step.service.api_slug).collect::<BTreeSet<_>>().len(), 10);
                plan.validate_independent_inputs(&task, &catalog.vocabulary).unwrap();
            }
            Resolution::Unsupported(reason) if count == 11 => assert_eq!(reason, crate::protocol::SOURCE_LIMIT_MESSAGE),
            result => panic!("{count} calls: unexpected {result:?}"),
        }
        assert_eq!(intent.goals.len(), count, "all requested sources are retained for clarification");
        assert!(task.pending_dispatches.is_empty());
    }
}

#[test]
fn independent_model_context_contains_no_implicit_lookup_contracts() {
    let (catalog, _, _) = fixture();
    let view = catalog.independent_parser_view(&BTreeSet::from(["company.accounts".into()]));
    assert_eq!(view["capabilities"], json!(["company.accounts"]));
    assert!(!view["input_types"].as_array().unwrap().contains(&json!("company.name")));
    assert!(view["input_types"].as_array().unwrap().contains(&json!("company.id")));
}
