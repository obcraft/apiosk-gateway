//! Production TenderNed contract and a captured Dutch intake. These tests
//! exercise proposal/price construction without any provider or model call.
use apiosk_gateway_v2::{
    ask_v2,
    capability_intake::{Intake, ParsedRequest},
    catalog::Catalog,
    engine::bind_subjects,
    parser::connected_planning,
    resolver::{resolve, Resolution},
    state::Task,
};
use serde_json::json;
use sha2::{Digest, Sha256};

const QUESTION: &str = "Via Apiosk: Zoek nieuwe aanbestedingen in de energiesector.";

fn fixture(output: &str) -> (Catalog, Intake) {
    let catalog = Catalog::build(serde_json::from_str(include_str!("fixtures/connected-procurement/catalog.json")).unwrap());
    let parsed_request: ParsedRequest = serde_json::from_value(json!({
        "language":"nl",
        "subjects":[{"id":"s1","label":"energiesector","type":"sector","identifiers":{"sector":"energiesector"}}],
        "capabilities":[{"slug":"procurement.tenders.search","name":"Search new tenders",
            "description":"Search for new procurement tenders in the energy sector", "domain":"procurement",
            "inputs":["sector"],"outputs":[output],"missing":[],"subject_id":"s1","source_hint":null,"confidence":0.9}],
        "deliverable":{"format":"chat","operations":[]},
        "meta":{"model":"captured-production","parsed_at":"2026-10-22T18:13:19Z","text_sha256":hex::encode(Sha256::digest(QUESTION.as_bytes()))},
    })).unwrap();
    let matches = ask_v2::search::search(&parsed_request, &catalog);
    let intake = Intake {
        selected_capabilities: matches.iter().flat_map(|found| found.operations.iter().cloned()).collect(),
        parsed_request, matches, apiosk_sources_searched: 1, catalog_version: catalog.version.clone(),
    };
    (catalog, intake)
}

#[test]
fn captured_energy_question_prices_the_runnable_source_without_an_identifier_lookup() {
    for output in ["tenders", "tender_notices", "procurement_records", "tender_records", "new_tenders", "recent_tenders"] {
        let (catalog, intake) = fixture(output);
        let prepared = connected_planning::compile(QUESTION, &Task::default(), &catalog, &intake)
            .unwrap_or_else(|| panic!("runnable procurement request ({output}): rejected={:?}; matches={:?}", catalog.rejected, intake.matches));
        assert!(prepared.coverage.gaps.is_empty());
        if output.starts_with("new_") || output.starts_with("recent_") {
            assert!(prepared.coverage.notices().iter().any(|notice| notice.contains("No publication window was supplied")));
        }
        let mut task = Task { capability_intake:Some(prepared.intake), ..Task::default() };
        task.record_turn(QUESTION, None);
        let mut intent = prepared.reply.intent.unwrap();
        bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else { panic!("expected priced plan") };
        plan.validate_independent_inputs(&task, &catalog.vocabulary).unwrap();
        assert_eq!(plan.steps.len(), 1);
        let step = &plan.steps[0];
        assert_eq!(step.service.api_slug, "tenderned");
        assert_eq!(step.service.contract.capability_slug, "procurement.nl-public-contracts");
        assert_eq!(step.service.inputs(&task, step.entity_ref, &catalog.vocabulary).unwrap(),
            json!({"page":0,"size":20,"sort":"publicatieDatum,desc","search":"energiesector"}));
        assert_eq!(plan.cap_atomic, 54_348);
    }
}

#[test]
fn a_source_listing_cannot_claim_unreturned_ownership_or_awarded_contract_amounts() {
    for output in ["beneficial_owners", "guaranteed_awarded_contract_value", "procurement_awarded_records", "open_tenders"] {
        let (catalog, intake) = fixture(output);
        assert!(connected_planning::compile(QUESTION, &Task::default(), &catalog, &intake).is_none(), "{output}");
    }
}

#[test]
fn newest_wording_requires_the_actual_selected_call_to_order_publication_dates() {
    let (mut catalog, intake) = fixture("new_tenders");
    for service in &mut catalog.services {
        service.fixed_params.as_object_mut().unwrap().remove("sort");
    }
    assert!(connected_planning::compile(QUESTION, &Task::default(), &catalog, &intake).is_none());
}

#[test]
fn a_cheaper_unsorted_endpoint_with_the_same_fact_cannot_take_the_newest_quote() {
    let (_, mut intake) = fixture("new_tenders");
    let mut snapshot: serde_json::Value = serde_json::from_str(include_str!("fixtures/connected-procurement/catalog.json")).unwrap();
    let original_endpoint = snapshot["services"][0]["endpoint_id"].as_str().unwrap().to_owned();
    let mut cheaper = snapshot["services"][0].clone();
    cheaper["id"] = json!("e7441a09-e4d5-4f67-bc27-b66b5493820a");
    cheaper["endpoint_id"] = json!("4c020080-142f-4ac4-8b0f-1523bf664c07");
    cheaper["list_atomic"] = json!(1);
    cheaper["fixed_params"].as_object_mut().unwrap().remove("sort");
    snapshot["services"].as_array_mut().unwrap().insert(0, cheaper);
    let catalog = Catalog::build(serde_json::from_value(snapshot).unwrap());
    assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
    intake.matches = ask_v2::search::search(&intake.parsed_request, &catalog);
    intake.selected_capabilities = intake.matches.iter().flat_map(|found| found.operations.iter().cloned()).collect();
    intake.catalog_version = catalog.version.clone();
    let prepared = connected_planning::compile(QUESTION, &Task::default(), &catalog, &intake).expect("sorted candidate remains available");
    let mut task = Task { capability_intake:Some(prepared.intake), ..Task::default() };
    task.record_turn(QUESTION, None);
    let mut intent = prepared.reply.intent.unwrap();
    bind_subjects(&mut task, &catalog, &mut intent).unwrap();
    let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else { panic!("expected priced plan") };
    assert_eq!(plan.steps.len(), 1);
    assert_eq!(plan.steps[0].service.endpoint_id.to_string(), original_endpoint);
    assert_eq!(plan.steps[0].service.inputs(&task, plan.steps[0].entity_ref, &catalog.vocabulary).unwrap()["sort"], "publicatieDatum,desc");
    assert_eq!(plan.cap_atomic, 54_348);
}

#[test]
fn an_explicit_publication_window_cannot_disappear_from_a_partial_proposal() {
    let (catalog, mut intake) = fixture("tenders");
    let question = "Via Apiosk: Zoek aanbestedingen in de energiesector gepubliceerd in de laatste 7 dagen.";
    intake.parsed_request.meta.text_sha256 = hex::encode(Sha256::digest(question.as_bytes()));
    intake.parsed_request.subjects[0].identifiers.insert("publication_window".into(), "last 7 days".into());
    intake.parsed_request.capabilities[0].inputs.push("publication_window".into());
    assert!(connected_planning::compile(question, &Task::default(), &catalog, &intake).is_none());
}

#[test]
fn captured_generic_domain_recovers_a_priced_plan_with_its_typed_sector_literal() {
    let (catalog, mut intake) = fixture("tenders");
    intake.parsed_request = serde_json::from_str(include_str!("fixtures/connected-procurement/generic-production-intake.json")).unwrap();
    let canonical = serde_json::to_value(&intake.parsed_request).unwrap();
    intake.matches = ask_v2::search::search(&intake.parsed_request, &catalog);
    intake.selected_capabilities.clear();
    assert!(intake.matches[0].operations.is_empty());
    assert!(connected_planning::refresh_discovery(&mut intake, &catalog));
    assert_eq!(serde_json::to_value(&intake.parsed_request).unwrap(), canonical);
    assert_eq!(intake.selected_capabilities.iter().map(String::as_str).collect::<Vec<_>>(), ["procurement.nl-public-contracts"]);
    let prepared = connected_planning::compile(QUESTION, &Task::default(), &catalog, &intake).expect("captured generic parse must yield the same literal filtered quote");
    assert!(prepared.coverage.gaps.is_empty());
    assert_eq!(serde_json::to_value(&prepared.coverage.original_intake.parsed_request).unwrap(), canonical);
    let mut task = Task { capability_intake:Some(prepared.intake), ..Task::default() };
    task.record_turn(QUESTION, None);
    let mut intent = prepared.reply.intent.unwrap();
    bind_subjects(&mut task, &catalog, &mut intent).unwrap();
    let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else { panic!("expected priced plan") };
    plan.validate_independent_inputs(&task, &catalog.vocabulary).unwrap();
    assert_eq!(plan.steps.len(), 1);
    let step = &plan.steps[0];
    assert_eq!(step.service.api_slug, "tenderned");
    assert_eq!(step.service.inputs(&task, step.entity_ref, &catalog.vocabulary).unwrap(),
        json!({"page":0,"size":20,"sort":"publicatieDatum,desc","search":"energiesector"}));
    assert_eq!(plan.cap_atomic, 54_348);
}

#[test]
fn generic_discovery_keeps_source_and_country_restrictions_and_requires_schema_evidence() {
    for (source, country, strip_alias, output) in [
        (Some("different-provider"), None, false, "tenders"),
        (None, Some("Spain"), false, "tenders"),
        (None, None, true, "tenders"),
        (None, None, false, "content"),
        (None, None, false, "beneficial_owners"),
    ] {
        let (mut catalog, mut intake) = fixture(output);
        intake.parsed_request.capabilities[0].domain = "general".into();
        intake.parsed_request.capabilities[0].slug = "general.search".into();
        intake.parsed_request.capabilities[0].source_hint = source.map(str::to_owned);
        if let Some(country) = country { intake.parsed_request.subjects[0].identifiers.insert("country".into(), country.into()); }
        if strip_alias {
            catalog.vocabulary.get_mut("tenderned.publication_page").unwrap().value_schema["properties"]["content"].as_object_mut().unwrap().remove("$comment");
        }
        intake.matches = ask_v2::search::search(&intake.parsed_request, &catalog);
        intake.matches[0].apiosk.clear();
        intake.matches[0].operations.clear();
        intake.selected_capabilities.clear();
        assert!(!connected_planning::refresh_discovery(&mut intake, &catalog), "source={source:?} country={country:?} strip_alias={strip_alias} output={output}");
        assert!(intake.selected_capabilities.is_empty());
    }
}

#[test]
fn typed_sector_overlay_requires_the_declared_input_and_same_literal_name() {
    for mismatch in ["different_name", "not_requested", "no_input_alias"] {
        let (mut catalog, mut intake) = fixture("tenders");
        intake.parsed_request = serde_json::from_str(include_str!("fixtures/connected-procurement/generic-production-intake.json")).unwrap();
        intake.matches = ask_v2::search::search(&intake.parsed_request, &catalog);
        assert!(connected_planning::refresh_discovery(&mut intake, &catalog));
        match mismatch {
            "different_name" => { intake.parsed_request.subjects[0].identifiers.insert("name".into(), "other sector".into()); },
            "not_requested" => intake.parsed_request.capabilities[0].inputs.clear(),
            "no_input_alias" => { catalog.vocabulary.get_mut("company_intel.input.search").unwrap().value_schema.as_object_mut().unwrap().remove("$comment"); },
            _ => unreachable!(),
        }
        let prepared_inputs = connected_planning::input_context(&intake, &catalog);
        assert_eq!(serde_json::to_value(&prepared_inputs.parsed_request).unwrap(), serde_json::to_value(&intake.parsed_request).unwrap(), "{mismatch}");
    }
}

#[test]
fn alternate_contract_namespaces_for_one_endpoint_are_not_ambiguous_source_meanings() {
    for same_endpoint in [true, false] {
        let (_, mut intake) = fixture("tenders");
        intake.parsed_request = serde_json::from_str(include_str!("fixtures/connected-procurement/generic-production-intake.json")).unwrap();
        let mut snapshot: serde_json::Value = serde_json::from_str(include_str!("fixtures/connected-procurement/catalog.json")).unwrap();
        let mut alternate = snapshot["services"][0].clone();
        alternate["id"] = json!("53d7bfbc-00fa-48a8-9839-86b076b70a1f");
        alternate["contract"]["capability_slug"] = json!("company_intel.search");
        alternate["fixed_params"] = json!({});
        alternate["list_atomic"] = json!(1);
        if !same_endpoint { alternate["endpoint_id"] = json!("ab9494c4-7f94-499c-8583-ec23f8e53ff3"); }
        snapshot["services"].as_array_mut().unwrap().push(alternate);
        let catalog = Catalog::build(serde_json::from_value(snapshot).unwrap());
        assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
        intake.matches = ask_v2::search::search(&intake.parsed_request, &catalog);
        assert!(intake.matches[0].operations.is_empty());
        assert_eq!(connected_planning::refresh_discovery(&mut intake, &catalog), same_endpoint);
        if same_endpoint {
            let prepared = connected_planning::compile(QUESTION, &Task::default(), &catalog, &intake).unwrap();
            assert!(prepared.coverage.notices().iter().any(|notice| notice.contains("No publication window was supplied")));
            let mut task = Task { capability_intake:Some(prepared.intake), ..Task::default() };
            task.record_turn(QUESTION, None);
            let mut intent = prepared.reply.intent.unwrap();
            bind_subjects(&mut task, &catalog, &mut intent).unwrap();
            let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else { panic!("expected priced plan") };
            assert_eq!(plan.steps.len(), 1);
            assert_eq!(plan.steps[0].service.contract.capability_slug, "procurement.nl-public-contracts");
            assert_eq!(plan.steps[0].service.inputs(&task, plan.steps[0].entity_ref, &catalog.vocabulary).unwrap(),
                json!({"page":0,"size":20,"sort":"publicatieDatum,desc","search":"energiesector"}));
            assert_eq!(plan.cap_atomic, 54_348);
        }
    }
}
