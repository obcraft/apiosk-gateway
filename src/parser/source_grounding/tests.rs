use super::*;
use crate::{
    parser::{Goal, Intent, Subject},
    state::Task,
};

#[test]
fn captured_name_only_request_can_quote_two_independent_sources() {
    let captured: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/name-search-alternative.json"
    )).unwrap();
    let catalog = Catalog::build(serde_json::from_str(include_str!(
        "../../../tests/fixtures/homepage-starters/catalog.json"
    )).unwrap());
    let parsed_request = serde_json::from_value(captured["parsed_request"].clone()).unwrap();
    let matches = crate::ask_v2::search::search(&parsed_request, &catalog);
    let intake = Intake { selected_capabilities:matches.iter().flat_map(|found| found.operations.iter().cloned()).collect(),
        parsed_request, matches, apiosk_sources_searched:2, catalog_version:catalog.version.clone() };
    let mut intent = Intent { subjects:vec![Subject { entity_ref:None, label:intake.parsed_request.subjects[0].label.clone(),
        values:BTreeMap::from([("company.name".into(),json!(intake.parsed_request.subjects[0].label))]) }],
        goals:vec![], answer_operations:vec![],unresolved_references:vec![] };
    let mut bindings = Vec::new();
    for (requirement, capability) in ["company_registry.search", "company.sanctions-screen"].into_iter().enumerate() {
        let service = candidates(&intake, requirement, &catalog).into_iter()
            .find(|service| service.contract.capability_slug == capability).expect("discovery must offer the name-based call");
        intent.goals.push(Goal { capability:capability.into(),subject:0,constraints:BTreeMap::from([
            ("source".into(),json!(service.api_slug)),("endpoint_id".into(),json!(service.endpoint_id))]) });
        // A valid semantic planner response over the real admitted contracts.
        // This is a replay of the planning boundary, not a live model/source call.
        bindings.push(Binding { requirement,output:intake.parsed_request.capabilities[requirement].outputs[0].clone(),
            fields:inventory(service,&catalog).into_values().take(10).map(|field|field.id).collect(),
            interpretation:if requirement == 0 {"Summarize the returned candidate names and registration numbers."}
                else {"Summarize the returned screening matches."}.into(),
            limitation:Some("Candidate matches do not confirm identity; this is not a complete registration profile.".into()) });
    }
    validate(&bindings,&intake,&catalog).unwrap();
    crate::parser::plan_coverage::prepare_grounded(&mut intent,&intake,&catalog,&bindings).unwrap();
    let mut task = Task { capability_intake:Some(intake.clone()),
        output_grounding:Grounding::new(&intake,&catalog,bindings.clone()), ..Default::default() };
    task.record_turn(captured["question"].as_str().unwrap(),None);
    intent.validate(captured["question"].as_str().unwrap(),&task,&catalog).unwrap();
    verify_plan(&intent,&task,&intake,&catalog,&bindings).unwrap();
    crate::engine::bind_subjects(&mut task,&catalog,&mut intent).unwrap();
    let crate::resolver::Resolution::Plan(plan) = crate::resolver::resolve(&catalog,&task,&intent) else {
        panic!("both calls must be ready using only the supplied company name");
    };
    assert_eq!(plan.steps.len(),2);
    assert!(plan.independent_calls);
    assert_eq!(plan.steps.iter().map(|step| &step.service.api_slug).collect::<BTreeSet<_>>().len(),2);
    assert!(crate::resolver::describe_steps(&plan,&[]).iter().all(|step| step["dependencies"] == json!([])));
    plan.validate_independent_inputs(&task,&catalog.vocabulary).unwrap();
    assert!(task.pending_dispatches.is_empty() && task.completed.is_empty());
}

fn fixture() -> (Intake, Catalog) {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/unsupported-glassdoor-starter.json"
    ))
    .unwrap();
    let catalog = Catalog::build(
        serde_json::from_str(include_str!(
            "../../../tests/fixtures/homepage-starters/catalog.json"
        ))
        .unwrap(),
    );
    let parsed = crate::capability_intake::normalize(
        &fixture["model_output"].to_string(),
        fixture["question"].as_str().unwrap(),
        "fixture",
    )
    .unwrap();
    let matches = crate::ask_v2::search::search(&parsed, &catalog);
    (
        Intake {
            selected_capabilities: matches
                .iter()
                .flat_map(|found| found.operations.iter().cloned())
                .collect(),
            parsed_request: parsed,
            matches,
            apiosk_sources_searched: 1,
            catalog_version: catalog.version.clone(),
        },
        catalog,
    )
}

fn glassdoor(catalog: &Catalog) -> &Service {
    catalog
        .services
        .iter()
        .find(|service| service.contract.capability_slug.contains("glassdoor"))
        .unwrap()
}

#[tokio::test]
async fn unrecognized_wording_reaches_dynamic_planning_instead_of_a_fixed_refusal() {
    let (mut intake, catalog) = fixture();
    let question = "Explain the supplied employer's compensation experience.";
    use sha2::Digest;
    intake.parsed_request.meta.text_sha256 = hex::encode(sha2::Sha256::digest(question.as_bytes()));
    for cap in &mut intake.parsed_request.capabilities {
        cap.outputs = vec![format!("novel_meaning_{}", Uuid::new_v4().simple())];
    }
    let parser = crate::parser::Parser::new(reqwest::Client::new(), "offline://no-model-transport".into(), String::new(), "fixture".into());
    let error = parser.chat_from_capabilities(question, None, &Task::default(), &catalog, &intake).await.unwrap_err();
    assert!(matches!(error.downcast_ref::<crate::parser::ModelFailure>(), Some(crate::parser::ModelFailure::Unavailable)),
        "unknown wording must reach model planning; an unavailable model is a retryable error, not a false source mismatch");
}

#[test]
fn removed_fields_invalidate_semantic_bindings() {
    let (intake, mut catalog) = fixture();
    let link = binding(&intake, &catalog, 0, "pay_rating", "/document/data/*/compensation_and_benefits_rating");
    validate(std::slice::from_ref(&link), &intake, &catalog).unwrap();
    fn remove(value: &mut Value) {
        match value {
            Value::Object(object) => { object.remove("compensation_and_benefits_rating"); for value in object.values_mut() { remove(value); } }
            Value::Array(values) => for value in values { remove(value); },
            _ => {}
        }
    }
    let service = catalog.services.iter_mut().find(|service| service.contract.capability_slug.contains("glassdoor")).unwrap();
    remove(service.sample_payload.as_mut().unwrap());
    remove(&mut catalog.vocabulary.get_mut("company.glassdoor_search_results").unwrap().value_schema);
    assert!(validate(&[link], &intake, &catalog).is_err());
}

fn binding(
    intake: &Intake,
    catalog: &Catalog,
    requirement: usize,
    output: &str,
    path: &str,
) -> Binding {
    assert!(intake.parsed_request.capabilities[requirement]
        .outputs
        .iter()
        .any(|name| name == output));
    let fields = inventory(glassdoor(catalog), catalog);
    Binding {
        requirement,
        output: output.into(),
        fields: vec![fields
            .values()
            .find(|field| field.path == path)
            .expect(path)
            .id
            .clone()],
        interpretation: "Use the returned rating for this requested category.".into(),
        limitation: Some("Category ratings may be unavailable for an employer.".into()),
    }
}

#[test]
fn observed_extracted_fields_support_semantics_without_disclosing_sample_answers() {
    let (intake, catalog) = fixture();
    let fields = inventory(glassdoor(&catalog), &catalog);
    assert!(fields.values().any(|field| field.path
        == "/document/data/*/compensation_and_benefits_rating"
        && field.observed));
    let view = view(&intake, &catalog);
    assert!(
        !view["sources"].to_string().contains("Starbucks"),
        "sample answer values must not enter the planning context"
    );
    for source in view["sources"].as_array().unwrap() {
        for field in source["fields"].as_array().unwrap() {
            assert!(field.get("value").is_none());
        }
    }
    let link = binding(
        &intake,
        &catalog,
        0,
        "pay_rating",
        "/document/data/*/compensation_and_benefits_rating",
    );
    validate(std::slice::from_ref(&link), &intake, &catalog).unwrap();
    assert!(covered(
        &[link],
        0,
        "pay_rating",
        &[glassdoor(&catalog)],
        &catalog
    ));
}

#[test]
fn grounding_rejects_invented_fields_other_endpoints_and_changed_requests() {
    let (intake, mut catalog) = fixture();
    let link = binding(
        &intake,
        &catalog,
        0,
        "pay_rating",
        "/document/data/*/compensation_and_benefits_rating",
    );
    let proof = Grounding::new(&intake, &catalog, vec![link.clone()]).unwrap();
    for variant in ["field", "output", "requirement", "duplicate"] {
        let mut bad = link.clone();
        match variant {
            "field" => bad.fields = vec!["invented".into()],
            "output" => bad.output = "private_employee_salary".into(),
            "requirement" => bad.requirement = 88,
            "duplicate" => bad.fields.push(bad.fields[0].clone()),
            _ => unreachable!(),
        }
        assert!(validate(&[bad], &intake, &catalog).is_err(), "{variant}");
    }
    let unrelated = catalog
        .services
        .iter()
        .find(|service| !service.contract.capability_slug.contains("glassdoor"))
        .unwrap();
    assert!(!covered(
        std::slice::from_ref(&link),
        0,
        "pay_rating",
        &[unrelated],
        &catalog
    ));
    assert!(
        !covered(
            std::slice::from_ref(&link),
            1,
            "pay_rating",
            &[glassdoor(&catalog)],
            &catalog
        ),
        "subject requirements cannot share an unassigned binding"
    );
    let mut changed = intake.clone();
    changed.parsed_request.subjects[0].label = "Different company".into();
    assert!(proof.bindings_for(&changed, &catalog).is_empty());
    catalog.version.push_str("changed");
    assert!(proof.bindings_for(&intake, &catalog).is_empty());
    catalog
        .services
        .iter_mut()
        .find(|service| service.contract.capability_slug.contains("glassdoor"))
        .unwrap()
        .unverified = true;
    assert!(validate(&[link], &intake, &catalog).is_err());
}

#[test]
fn final_plan_requires_each_subjects_actual_selected_service() {
    let (mut intake, catalog) = fixture();
    for cap in &mut intake.parsed_request.capabilities {
        cap.outputs = vec!["pay_rating".into()];
    }
    let bindings = (0..2)
        .map(|i| {
            binding(
                &intake,
                &catalog,
                i,
                "pay_rating",
                "/document/data/*/compensation_and_benefits_rating",
            )
        })
        .collect::<Vec<_>>();
    let service = glassdoor(&catalog);
    let input = &service
        .contract
        .requires
        .iter()
        .find(|requirement| !requirement.optional)
        .unwrap()
        .fact_type;
    let mut intent = Intent {
        subjects: intake
            .parsed_request
            .subjects
            .iter()
            .map(|subject| Subject {
                entity_ref: None,
                label: subject.label.clone(),
                values: BTreeMap::from([(input.clone(), json!(subject.label))]),
            })
            .collect(),
        goals: (0..2)
            .map(|subject| Goal {
                capability: service.contract.capability_slug.clone(),
                subject,
                constraints: BTreeMap::new(),
            })
            .collect(),
        answer_operations: vec![],
        unresolved_references: vec![],
    };
    super::super::plan_coverage::prepare_grounded(&mut intent, &intake, &catalog, &bindings)
        .unwrap();
    let mut task = Task {
        capability_intake: Some(intake.clone()),
        output_grounding: Grounding::new(&intake, &catalog, bindings),
        ..Default::default()
    };
    crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
    let crate::resolver::Resolution::Plan(mut plan) =
        crate::resolver::resolve(&catalog, &task, &intent)
    else {
        panic!("two priced retrievals")
    };
    assert_eq!(plan.steps.len(), 2);
    assert!(
        super::super::plan_coverage::uncovered_outputs(&task, &catalog, &intent, &plan).is_empty()
    );
    plan.steps[1].entity_ref = Uuid::new_v4();
    assert_eq!(
        super::super::plan_coverage::uncovered_outputs(&task, &catalog, &intent, &plan).len(),
        1
    );
    assert_eq!(
        task.output_grounding.as_ref().unwrap().notices().len(),
        1,
        "same caveat is shown once"
    );
}

#[test]
fn field_inventory_follows_arbitrary_provider_fields_and_validated_extraction() {
    let (_, mut catalog) = fixture();
    let mut service = glassdoor(&catalog).clone();
    service.sample_payload = None;
    let fact = service
        .contract
        .output_map
        .produces
        .keys()
        .next()
        .unwrap()
        .clone();
    catalog.vocabulary.get_mut(&fact).unwrap().value_schema = json!({"type":"object","required":["metingen"],"properties":{
        "metingen":{"type":"array","items":{"type":"object","properties":{"energie_kwh":{"type":"number"}}}}
    }});
    let fields = inventory(&service, &catalog);
    assert!(fields
        .values()
        .any(|field| field.path == "/metingen/*/energie_kwh" && !field.observed));
    assert!(
        !fields.values().any(|field| field.path.contains("rating")),
        "not a hardcoded employer mapping"
    );
    service.sample_payload = Some(json!({"private_salary":123456}));
    assert!(
        !inventory(&service, &catalog)
            .values()
            .any(|field| field.path.contains("private_salary")),
        "raw unextracted response fields cannot establish coverage"
    );
}

#[test]
fn customized_sources_get_semantic_matching_and_keep_their_pinned_endpoints() {
    use crate::ask_v2::source_selection::{Context, SelectedSource, Selection};
    let (mut intake, catalog) = fixture();
    for cap in &mut intake.parsed_request.capabilities {
        cap.outputs = vec!["pay_rating".into()];
    }
    let options =
        crate::ask_v2::source_options::options(&intake.parsed_request, &intake.matches, &catalog);
    let service = glassdoor(&catalog);
    let input = service
        .contract
        .requires
        .iter()
        .find(|requirement| !requirement.optional)
        .unwrap()
        .fact_type
        .clone();
    let sources = intake
        .parsed_request
        .subjects
        .iter()
        .map(|subject| SelectedSource {
            endpoint_id: service.endpoint_id,
            capability: service.contract.capability_slug.clone(),
            subject_id: subject.id.clone(),
            input: BTreeMap::from([(input.clone(), json!(subject.label))]),
        })
        .collect();
    let context = Context {
        planning_reply: None,
        question: "Compare Starbucks and McDonalds pay ratings on Glassdoor.".into(),
        original_intake: intake,
        options,
        selection: Selection {
            sources,
            ..Default::default()
        },
        search_id: "fixture".into(),
        discovery_id: None,
    };
    assert!(
        context.compile(&catalog).unwrap().is_none(),
        "a field-name mismatch reaches semantic planning instead of a final refusal"
    );
    let expanded = context.expanded(&catalog).unwrap();
    let bindings = (0..expanded.parsed_request.capabilities.len())
        .map(|i| {
            binding(
                &expanded,
                &catalog,
                i,
                "pay_rating",
                "/document/data/*/compensation_and_benefits_rating",
            )
        })
        .collect::<Vec<_>>();
    let mut intent = Intent {
        subjects: expanded
            .parsed_request
            .subjects
            .iter()
            .map(|s| Subject {
                entity_ref: None,
                label: s.label.clone(),
                values: BTreeMap::from([(input.clone(), json!(s.label))]),
            })
            .collect(),
        goals: (0..2)
            .map(|subject| Goal {
                capability: service.contract.capability_slug.clone(),
                subject,
                constraints: BTreeMap::new(),
            })
            .collect(),
        answer_operations: vec![],
        unresolved_references: vec![],
    };
    super::super::plan_coverage::prepare_grounded(&mut intent, &expanded, &catalog, &bindings)
        .unwrap();
    let task = Task {
        source_selection: Some(context),
        ..Default::default()
    };
    verify_plan(&intent, &task, &expanded, &catalog, &bindings).unwrap();
}
