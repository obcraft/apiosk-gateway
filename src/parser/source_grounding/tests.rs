use super::*;
use crate::{
    parser::{Goal, Intent, Subject},
    state::Task,
};

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

#[test]
fn deterministic_mappings_require_the_current_field_and_source() {
    let (intake,mut catalog)=fixture();
    assert!(compile(&intake,&catalog,&[glassdoor(&catalog)]).iter().any(|b|b.output=="pay_rating"));
    let service=catalog.services.iter_mut().find(|service|service.contract.capability_slug.contains("glassdoor")).unwrap();
    fn remove(value:&mut Value) {
        match value {
            Value::Object(object)=>{object.remove("compensation_and_benefits_rating");for value in object.values_mut() {remove(value);}},
            Value::Array(values)=>for value in values {remove(value);},
            _=>{},
        }
    }
    remove(service.sample_payload.as_mut().unwrap());
    remove(&mut catalog.vocabulary.get_mut("company.glassdoor_search_results").unwrap().value_schema);
    assert!(!compile(&intake,&catalog,&[glassdoor(&catalog)]).iter().any(|b|b.output=="pay_rating"),"a static alias cannot manufacture a removed field");
    assert!(compile(&intake,&catalog,&[]).is_empty(),"an unselected source cannot establish coverage");
}

#[test]
fn reviewed_mapping_metadata_is_bounded_and_contains_no_sample_values() {
    let mappings:Vec<Mapping>=serde_json::from_str(include_str!("../../../contracts/source-output-mappings.json")).unwrap();
    for mapping in mappings {
        assert!(!mapping.outputs.is_empty() && !mapping.fields.is_empty() && mapping.fields.len()<=12);
        assert!(!mapping.interpretation.trim().is_empty() && mapping.interpretation.chars().count()<=500);
        assert!(mapping.limitation.as_ref().is_none_or(|note|!note.trim().is_empty() && note.chars().count()<=300));
        for field in mapping.fields {assert!(!field.types.is_empty() && !field.fact.is_empty() && !field.source.is_empty());}
    }
}

#[test]
fn procurement_parse_variants_preserve_query_and_notice_classification_limits() {
    let fixtures: Value = serde_json::from_str(include_str!("../../../tests/fixtures/homepage-starters.json")).unwrap();
    let fixture = fixtures.as_array().unwrap().iter().find(|row| row["question"].as_str().unwrap().contains("TenderNed")).unwrap();
    let catalog = Catalog::build(serde_json::from_str(include_str!("../../../tests/fixtures/homepage-starters/catalog.json")).unwrap());
    let mut raw = fixture["model_output"].clone();
    raw["capabilities"][0]["outputs"] = json!(["contract_status"]);
    raw["capabilities"][0]["inputs"] = json!(["country", "query"]);
    raw["subjects"][0]["identifiers"] = json!({"country":"Dutch", "query":"cleaning services"});
    let parsed_request = crate::capability_intake::normalize(&raw.to_string(), fixture["question"].as_str().unwrap(), "fixture").unwrap();
    let matches = crate::ask_v2::search::search(&parsed_request, &catalog);
    let intake = Intake {selected_capabilities:matches.iter().flat_map(|found|found.operations.iter().cloned()).collect(),
        parsed_request,matches,apiosk_sources_searched:1,catalog_version:catalog.version.clone()};
    let sources: Vec<_> = catalog.services.iter().filter(|service|service.contract.capability_slug=="procurement.nl-public-contracts").collect();
    let bindings = compile(&intake,&catalog,&sources);
    let binding = bindings.iter().find(|binding|binding.output=="contract_status").unwrap();
    assert!(binding.interpretation.contains("opportunity versus award notices"));
    assert!(binding.limitation.as_ref().unwrap().contains("does not by itself establish that bidding is still open"));
    assert!(compile(&intake,&catalog,&[]).is_empty());
    let question = fixture["question"].as_str().unwrap();
    let reply = crate::parser::contract_planner::compile(question,question,&Task::default(),&catalog,&intake);
    assert!(reply.discover,"{}",reply.message);
    assert_eq!(reply.intent.unwrap().subjects[0].values["company_intel.input.search"],json!("cleaning services"));
    let mut whole_question = intake.clone();
    whole_question.parsed_request.subjects[0].identifiers.insert("query".into(),question.into());
    assert!(!crate::parser::contract_planner::compile(question,question,&Task::default(),&catalog,&whole_question).discover,
        "the complete instruction cannot substitute for a supplied search phrase");
    let mut extra_filter = intake;
    extra_filter.parsed_request.subjects[0].identifiers.insert("maximum_budget".into(),"50000".into());
    assert!(!crate::parser::contract_planner::compile(question,question,&Task::default(),&catalog,&extra_filter).discover,
        "a usable query must not hide an unsupported filter");
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
    validate(&[link.clone()], &intake, &catalog).unwrap();
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
        &[link.clone()],
        0,
        "pay_rating",
        &[unrelated],
        &catalog
    ));
    assert!(
        !covered(
            &[link.clone()],
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
