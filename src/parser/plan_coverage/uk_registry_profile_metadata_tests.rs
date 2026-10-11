use super::*;

fn reviewed() -> Value {
    serde_json::from_str(include_str!("../../../tests/fixtures/uk-registry-profile/reviewed-schema.json")).unwrap()
}

fn catalog(updated: bool) -> Catalog {
    let mut snapshot: crate::catalog::Snapshot = serde_json::from_str(include_str!("../../../tests/fixtures/uk-registry-profile/catalog.json")).unwrap();
    if updated {
        snapshot.vocabulary.iter_mut().find(|fact|fact.slug=="company.uk_registry_record").unwrap().value_schema=reviewed()["after"].clone();
    }
    let catalog=Catalog::build(snapshot);
    assert!(catalog.rejected.is_empty(),"{:?}",catalog.rejected);
    catalog
}

#[test]
fn reviewed_uk_record_alias_preserves_validation_and_field_scope() {
    let fixture=reviewed();
    let schema=&fixture["after"];
    catalog_core::facts::schema::check_schema(schema).unwrap();
    assert!(!schema_covers(&fixture["before"],"profile",false));
    for output in ["profile","company_profile","registry_profile","registered_details","legal_name","registered_address","registration_number"] {
        assert!(schema_covers(schema,output,false),"{output}");
    }
    for output in ["employees","employer_rating","funding_rounds","investors","revenue"] {
        assert!(!schema_covers(schema,output,false),"{output}");
    }
    let mut stripped=schema.clone();stripped.as_object_mut().unwrap().remove("$comment");
    assert_eq!(stripped,fixture["before"]);
    let catalog=catalog(true);
    let service=catalog.services.iter().find(|service|service.contract.capability_slug=="company.registry-profile").unwrap();
    // Shared capability prose mentions Dutch fields. Only the typed contract
    // proves this endpoint's GB jurisdiction, independently of that prose.
    assert_eq!(crate::ask_v2::search::fixed_endpoint_country(service,None).as_deref(),Some("GB"));
    catalog_core::facts::schema::validate(schema,service.sample_payload.as_ref().unwrap()).unwrap();
}

#[test]
fn recorded_broad_uk_company_requests_compile_a_complete_name_lookup_and_profile() {
    let before=catalog(false);let after=catalog(true);
    let recorded:Vec<Value>=serde_json::from_str(include_str!("../../../tests/fixtures/uk-registry-profile/recorded-intakes.json")).unwrap();
    assert_eq!(recorded.len(),2);
    for row in recorded {
        let question=row["question"].as_str().unwrap();
        let parsed_request=crate::capability_intake::normalize(&row["parsed_request"].to_string(),question,"recorded-fixture").unwrap();
        let intake_for=|catalog:&Catalog| {
            let matches=crate::ask_v2::search::search(&parsed_request,catalog);
            Intake {selected_capabilities:matches.iter().flat_map(|found|found.operations.iter().cloned()).collect(),parsed_request:parsed_request.clone(),matches,apiosk_sources_searched:1,catalog_version:catalog.version.clone()}
        };
        assert!(single_retrieval_reply(&intake_for(&before),&before,question,question,&Default::default()).is_none(),"unreviewed aggregate must fail output proof");
        let intake=intake_for(&after);
        let reply=single_retrieval_reply(&intake,&after,question,question,&Default::default()).expect("reviewed profile must compile");
        let mut intent=reply.intent.unwrap();
        let mut task=crate::state::Task {capability_intake:Some(intake),..Default::default()};task.record_turn(question,None);
        crate::engine::bind_subjects(&mut task,&after,&mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan)=crate::resolver::resolve(&after,&task,&intent) else {panic!("complete profile plan")};
        assert_eq!(plan.steps.len(),2);
        assert!(plan.steps.iter().all(|step|step.service.api_slug=="companies-house"));
        assert!(uncovered_outputs(&task,&after,&intent,&plan).is_empty());
        assert!(uncovered_inputs(&task,&after,&intent,&plan).is_empty());
        task.capability_intake.as_mut().unwrap().parsed_request.subjects[0].identifiers.insert("country".into(),"France".into());
        assert!(!uncovered_inputs(&task,&after,&intent,&plan).is_empty(),"UK plan cannot prove another country");
    }
}
