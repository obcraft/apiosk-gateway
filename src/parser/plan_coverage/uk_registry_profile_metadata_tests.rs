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
fn recorded_broad_uk_company_requests_collect_profile_inputs_before_quoting() {
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
        let reply=crate::parser::contract_planner::compile(question,question,&Default::default(),&after,&intake);
        let mut intent=reply.intent.expect("supported profile retains a draft for input collection");
        let mut task=crate::state::Task {capability_intake:Some(intake),..Default::default()};task.record_turn(question,None);
        crate::engine::bind_subjects(&mut task,&after,&mut intent).unwrap();
        let crate::resolver::Resolution::Missing {kind,..}=crate::resolver::resolve(&after,&task,&intent) else {panic!("ask for the actual profile identifier")};
        assert!(after.vocabulary.contains_key(&kind));
        assert_eq!(intent.goals.len(),1,"no implicit name lookup");
        assert!(task.proposal.is_none());

    }
}
