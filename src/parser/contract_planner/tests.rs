use super::*;
use crate::{capability_intake, catalog::Snapshot};
use uuid::Uuid;

#[tokio::test]
async fn rdw_registration_fields_compile_without_a_second_model_request() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/rdw-registration-planning.json"
    )).unwrap();
    let catalog = Catalog::build(serde_json::from_value(fixture["catalog"].clone()).unwrap());
    assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
    for (identifier, compound) in [("plate_number",false),("plate",true),("registration_plate",false),("license_plate",true)] {
        let mut request = fixture["intake"].clone();
        request["parsed_request"]["subjects"][0]["identifiers"] = json!({(identifier):"RZ874R"});
        request["parsed_request"]["capabilities"][0]["inputs"] = json!([identifier]);
        if compound { request["parsed_request"]["capabilities"][0]["outputs"] = json!(["make_model","first_registration","apk_expiry","recall_indicators"]); }
        let mut intake: Intake = serde_json::from_value(request).unwrap();
        intake.catalog_version = catalog.version.clone();
        let question = fixture["question"].as_str().unwrap();
        let reply = compile(question, question, &Task::default(), &catalog, &intake);
        assert!(ready(question, &reply, &Task::default(), &catalog, &intake), "{identifier}/{compound}: {reply:?}");
        let parser=crate::parser::Parser::new(reqwest::Client::new(),"offline://no-model".into(),String::new(),"fixture".into());
        let reply=parser.chat_from_capabilities(question,None,&Task::default(),&catalog,&intake).await.unwrap();
        let model=reply.output_bindings.iter().find(|binding|binding.output==if compound {"make_model"} else {"model"}).unwrap();
        assert!(model.interpretation.contains("commercial designation"));
        if compound { assert_eq!(model.fields.len(),2,"make_model must bind both manufacturer and commercial designation"); }
        let dates=reply.output_bindings.iter().find(|binding|binding.output=="first_registration").unwrap();
        assert_eq!(dates.fields.len(),2,"first admission and Dutch registration stay distinct");
        let mut intent=reply.intent.unwrap();
        let mut task=Task {capability_intake:Some(intake.clone()),output_grounding:source_grounding::Grounding::new(&intake,&catalog,reply.output_bindings),..Default::default()};
        task.record_turn(question,None);
        crate::engine::bind_subjects(&mut task,&catalog,&mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan)=crate::resolver::resolve(&catalog,&task,&intent) else {panic!("RDW plan must be ready");};
        assert_eq!(plan.steps.len(),1);
        assert_eq!(plan.steps[0].service.api_slug,"rdw");
        assert_eq!(plan.steps[0].service.inputs(&task,plan.steps[0].entity_ref,&catalog.vocabulary).unwrap()["kenteken"],"RZ874R");
        assert!(plan.cap_atomic>0 && task.completed.is_empty(),"the source still needs price review and approval");
        for unsupported in ["recall_history","recall_details","owner_identity"] {
            let mut broader=intake.clone();
            broader.parsed_request.capabilities[0].outputs.push(unsupported.into());
            let reply=compile(question,question,&Task::default(),&catalog,&broader);
            assert!(!ready(question,&reply,&Task::default(),&catalog,&broader),"{unsupported} cannot be covered by an indicator");
        }
    }
}

fn fixture(outputs: &[&str]) -> (String, Catalog, Intake) {
    let mut snapshot: Snapshot = serde_json::from_str(include_str!(
        "../../../tests/fixtures/integration-catalog.json"
    )).unwrap();
    let id_output = snapshot.services[0].contract.output_map.produces["company.id"].clone();
    snapshot.services[1].contract.output_map.produces.insert("company.id".into(), id_output);
    let provider = format!("registry-{}", Uuid::new_v4().simple());
    for service in &mut snapshot.services {
        service.api_slug = provider.clone();
    }
    let catalog = Catalog::build(snapshot);
    assert!(catalog.rejected.is_empty());
    let company = format!("Company {}", Uuid::new_v4().simple());
    let question = format!("Summarize {company} with sources.");
    let parsed_request = capability_intake::normalize(&json!({
        "language":"en", "subjects":[{"id":"s1", "label":company,"type":"company","identifiers":{}}],
        "capabilities":[{"slug":"company.details", "name":"Company details", "description":"Company registration details",
            "domain":"company", "inputs":["company_name"], "outputs":outputs, "subject_id":"s1", "source_hint":provider,"confidence":1.0}],
        "deliverable":{"format":"summary","operations":[]}
    }).to_string(), &question, "fixture").unwrap();
    // Deliberately rank the identifier-only endpoint first, regardless of price.
    let apiosk = catalog.services.iter().rev().map(|service| serde_json::from_value(json!({
        "origin":"apiosk", "name":service.name, "description":"Company registration details",
        "source_slug":service.api_slug, "capability":service.contract.capability_slug,
        "endpoint_id":service.endpoint_id, "method":"GET", "availability":"supported"
    })).unwrap()).collect();
    let operations = catalog.services.iter().map(|service| service.contract.capability_slug.clone()).collect();
    let intake = Intake { parsed_request, matches: vec![capability_intake::Match {
        slug:"company.details".into(), query:"Company registration details".into(), operations,
        apiosk, coinbase:vec![],coinbase_status:"complete".into()
    }], selected_capabilities:catalog.services.iter().map(|service| service.contract.capability_slug.clone()).collect(),
        apiosk_sources_searched:1, catalog_version:catalog.version.clone() };
    (question, catalog, intake)
}

#[test]
fn ready_alternative_wins_before_requesting_an_identifier() {
    let (question, catalog, intake) = fixture(&["company.id"]);
    let reply = compile(&question, &question, &Task::default(), &catalog, &intake);
    let mut intent = reply.intent.expect("standalone name search covers the request");
    assert_eq!(intent.goals.len(), 1);
    assert_eq!(intent.goals[0].capability, "company.search");
    let mut task = Task { capability_intake:Some(intake), ..Default::default() };
    crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
    let crate::resolver::Resolution::Plan(plan) = crate::resolver::resolve(&catalog, &task, &intent) else {
        panic!("the supplied company name must suffice");
    };
    assert_eq!(plan.steps.len(), 1);
    assert!(plan.independent_calls);
}

#[test]
fn available_search_does_not_replace_explicit_details_it_cannot_return() {
    let (question, catalog, intake) = fixture(&["company.accounts"]);
    let reply = compile(&question, &question, &Task::default(), &catalog, &intake);
    let mut intent = reply.intent.expect("request the detail endpoint's actual input");
    assert_eq!(intent.goals.len(), 1);
    assert_eq!(intent.goals[0].capability, "company.accounts");
    let mut task = Task { capability_intake:Some(intake), ..Default::default() };
    crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
    assert!(matches!(crate::resolver::resolve(&catalog, &task, &intent),
        crate::resolver::Resolution::Missing { kind, .. } if kind == "company.id"));
}

#[test]
fn missing_inputs_do_not_bypass_output_proof_or_dynamic_planning() {
    let (question, catalog, intake) = fixture(&["unlisted_requested_field"]);
    let reply = compile(&question, &question, &Task::default(), &catalog, &intake);
    assert!(!reply.discover && reply.intent.is_none(), "defer semantic wording to the dynamic planner");
    let intent = Intent { subjects:vec![Subject { entity_ref:None,
        label:intake.parsed_request.subjects[0].label.clone(), values:Default::default() }],
        goals:vec![Goal { subject:0, capability:"company.accounts".into(), constraints:BTreeMap::from([
            ("source".into(),json!(catalog.services[1].api_slug)),
            ("endpoint_id".into(),json!(catalog.services[1].endpoint_id))]) }],
        answer_operations:vec![],unresolved_references:vec![] };
    assert!(source_grounding::verify_plan(&intent, &Task::default(), &intake, &catalog, &[]).is_err(),
        "the model's incomplete draft must prove the same output scope");
}

#[test]
fn an_explicit_endpoint_selection_is_not_replaced_by_a_ready_alternative() {
    let (question, catalog, mut intake) = fixture(&["company.id"]);
    intake.matches[0].apiosk.retain(|candidate| candidate.capability.as_deref() == Some("company.accounts"));
    intake.matches[0].operations = BTreeSet::from(["company.accounts".into()]);
    intake.selected_capabilities = intake.matches[0].operations.clone();
    let reply = compile(&question, &question, &Task::default(), &catalog, &intake);
    assert_eq!(reply.intent.unwrap().goals[0].capability, "company.accounts");
}
