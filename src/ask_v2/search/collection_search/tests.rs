use super::*;
use crate::capability_intake::ParsedRequest;
use serde_json::{json, Value};

fn catalog() -> Catalog {
    Catalog::build(
        serde_json::from_str(include_str!(
            "../../../../tests/fixtures/search/company-name-collections.json"
        ))
        .unwrap(),
    )
}

fn request() -> ParsedRequest {
    serde_json::from_value(json!({
        "language":"en", "subjects":[{"id":"s1","type":"company","label":"Example Company", "identifiers":{"company_name":"Example Company"}}],
        "capabilities":[{"slug":"company.search", "domain":"company", "name":"Company Search",
            "description":"Find companies matching the given name", "inputs":["company_name"],
            "outputs":["company_matches"], "subject_id":"s1", "source_hint":null,"confidence":1,"missing":[]}],
        "deliverable":{"format":"chat","operations":[]},
        "meta":{"model":"fixture","parsed_at":"2026-10-13T00:00:00Z","text_sha256":"fixture"}
    })).unwrap()
}

#[test]
fn generic_company_matches_find_typed_name_searches_without_a_provider() {
    let catalog = catalog();
    assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
    let found = super::super::search(&request(), &catalog);
    assert!(
        found[0].operations.contains("company_registry.search"),
        "{:?}",
        found[0].apiosk
    );
    assert!(
        found[0].operations.contains("company.legal-entity-search"),
        "{:?}",
        found[0].apiosk
    );
    assert!(!found[0].operations.contains("company.registry-profile"));
    assert!(!found[0].operations.contains("company.watchlist-search"));
}

#[test]
fn name_search_discovery_does_not_require_one_parser_spelling() {
    let catalog = catalog();
    for input in ["name", "company_name"] {
        for output in ["company", "companies", "matching_companies", "company_matches", "company_results", "company_records", "company_collection", "company_name", "company_id", "company_identifier"] {
            let mut parsed = request();
            let cap = &mut parsed.capabilities[0];
            cap.slug = "company.nameSearch".into();
            cap.inputs = vec![input.into()];
            cap.outputs = vec![output.into()];
            let found = super::super::search(&parsed, &catalog);
            assert!(found[0].operations.contains("company_registry.search"), "{input}/{output}: {:?}", found[0].operations);
            assert!(!found[0].operations.contains("company.registry-profile"));
            assert!(!found[0].operations.contains("company.watchlist-search"));
        }
    }
}

#[test]
fn generic_bucket_uses_the_linked_entity_only_for_a_matching_name_collection_search() {
    let catalog = catalog();
    for (input, output) in [("name", "companies"), ("company_name", "matching_companies"), ("name", "company_name"), ("name", "company_id")] {
        let mut parsed = request();
        let cap = &mut parsed.capabilities[0];
        cap.slug = "general.search".into();
        cap.domain = "general".into();
        cap.name = "Search".into();
        cap.inputs = vec![input.into()];
        cap.outputs = vec![output.into()];
        cap.source_hint = Some("kvk".into());
        let unchanged = serde_json::to_value(&parsed).unwrap();
        assert_eq!(request_domain(&parsed.capabilities[0], &parsed).as_deref(), Some("company"));
        let found = super::super::search(&parsed, &catalog);
        assert!(found[0].operations.contains("company_registry.search"), "{input}/{output}: {:?}",found[0].operations);
        assert!(!found[0].operations.contains("company.registry-profile"));
        assert!(!found[0].operations.contains("company.watchlist-search"));
        assert_eq!(serde_json::to_value(&parsed).unwrap(), unchanged, "discovery does not rewrite interpreted intent");
    }
}

#[test]
fn generic_bucket_recovery_requires_agreement_between_operation_inputs_output_and_subject() {
    let mut parsed = request();
    let cap = &mut parsed.capabilities[0];
    cap.slug = "general.search".into();
    cap.domain = "general".into();
    cap.name = "Search".into();
    cap.inputs = vec!["name".into()];
    for output in ["person_matches", "company_profile", "person_name", "company_count", "company_rating", "company_sanctions_matches", "name", "id", "identifier"] {
        parsed.capabilities[0].outputs = vec![output.into()];
        assert!(request_domain(&parsed.capabilities[0], &parsed).is_none(), "{output}");
    }
    parsed.capabilities[0].outputs = vec!["companies".into()];
    parsed.capabilities[0].inputs = vec!["person_name".into()];
    assert!(request_domain(&parsed.capabilities[0], &parsed).is_none());
    parsed.capabilities[0].inputs = vec!["name".into()];
    parsed.capabilities[0].slug = "general.profile".into();
    parsed.capabilities[0].name = "Profile".into();
    assert!(request_domain(&parsed.capabilities[0], &parsed).is_none());
    parsed.capabilities[0].slug = "general.search".into();
    parsed.capabilities[0].name = "Search".into();
    parsed.capabilities[0].subject_id = None;
    assert!(request_domain(&parsed.capabilities[0], &parsed).is_none());
}

#[test]
fn entity_domain_recovery_has_no_company_or_provider_specific_rule() {
    for (entity, collection) in [("person", "person_matches"),("product", "products"),("building", "building_records")] {
        let mut parsed = request();
        parsed.subjects[0].kind = entity.into();
        let cap = &mut parsed.capabilities[0];
        cap.domain = "general".into();
        cap.slug = "general.search".into();
        cap.name = "Search".into();
        cap.inputs = vec![format!("{entity}_name")];
        cap.outputs = vec![collection.into()];
        assert_eq!(request_domain(&parsed.capabilities[0], &parsed).as_deref(), Some(entity));
    }
}

#[test]
fn generic_name_search_keeps_requested_result_and_identifier_scope() {
    let mut cap = request().capabilities.remove(0);
    cap.inputs = vec!["name".into()];
    for output in ["company_count", "company_details", "company_rating", "company_sanctions_matches", "person_matches"] {
        cap.outputs = vec![output.into()];
        assert!(!requested(&cap), "{output} is not a generic company collection");
    }
    cap.outputs = vec!["company_matches".into()];
    cap.inputs = vec!["person_name".into()];
    assert!(!requested(&cap), "a related person's name is not a supplied company name");
    cap.inputs = vec!["name".into()];
    cap.slug = "company.profile".into();
    cap.name = "Company profile".into();
    cap.outputs = vec!["company_name".into(),"company_id".into()];
    assert!(!requested(&cap), "a profile request does not imply a collection search");
}

#[test]
fn identity_search_outputs_cannot_hide_an_additional_profile_count_or_rating() {
    let mut cap=request().capabilities.remove(0);
    cap.inputs=vec!["name".into()];
    cap.outputs=vec!["company_name".into(),"company_id".into()];
    assert!(requested(&cap),"qualified identity fields describe the requested search results");
    for additional in ["company_profile","company_details","company_count","company_rating","person_name","company_sanctions_matches"] {
        cap.outputs=vec!["company_name".into(),"company_id".into(),additional.into()];
        assert!(!requested(&cap),"{additional} must retain its own coverage requirements");
    }
}

#[test]
fn identity_search_fields_use_typed_contracts_for_other_entity_domains_without_rewriting_the_request() {
    for entity in ["person","product"] {
        let mut value:Value=serde_json::from_str(include_str!("../../../../tests/fixtures/search/company-name-collections.json")).unwrap();
        let mut service=value["services"][0].clone();
        let name=format!("{entity}.name");
        let identifier=format!("{entity}.id");
        let operation=format!("{entity}.search");
        service["api_slug"]=json!(format!("registry-{}",uuid::Uuid::new_v4().simple()));
        service["name"]=json!("Typed name search");
        service["contract"]["capability_slug"]=json!(operation);
        service["contract"]["requires"]=json!([{"fact_type":name,"subject_role":"subject","optional":false}]);
        service["contract"]["input_map"]=json!({name.clone():{"in":"query","name":"query"}});
        service["contract"]["output_map"]=json!({"base":"$.records[*]","produces":{
            name.clone():{"path":"$.name"},identifier.clone():{"path":"$.id"}}});
        service["contract"]["identifies"]=json!(identifier);
        service["sample_payload"]=json!({"records":[{"name":"Example record","id":"record-1"}]});
        value["services"]=json!([service]);
        value["known_sources"]=json!([]);
        value["vocabulary"]=json!([{"slug":name,"value_schema":{"type":"string"}},
            {"slug":identifier,"value_schema":{"type":"string"}}]);
        let catalog=Catalog::build(serde_json::from_value(value).unwrap());
        assert!(catalog.rejected.is_empty(),"{:?}",catalog.rejected);
        for (domain, collection) in [entity,"general"].into_iter().flat_map(|domain|[false,true].map(|collection|(domain,collection))) {
            let mut parsed=request();
            parsed.subjects[0].kind=entity.into();
            parsed.subjects[0].label=format!("Example {}",uuid::Uuid::new_v4().simple());
            parsed.subjects[0].identifiers=[("name".into(),parsed.subjects[0].label.clone())].into();
            let cap=&mut parsed.capabilities[0];
            cap.domain=domain.into();cap.slug=format!("{domain}.search");cap.name="Name search".into();
            cap.description=format!("Find {entity} records matching the supplied name");
            cap.inputs=vec!["name".into()];
            cap.outputs=if collection {vec![format!("{entity}_collection")]} else {vec![format!("{entity}_name"),format!("{entity}_identifier")]};
            let unchanged=serde_json::to_value(&parsed).unwrap();
            assert_eq!(request_domain(&parsed.capabilities[0],&parsed).as_deref(),Some(entity));
            assert!(super::super::search(&parsed,&catalog)[0].operations.contains(&operation),"{entity}/{domain}");
            assert_eq!(serde_json::to_value(&parsed).unwrap(),unchanged,"discovery never adds or removes requested outputs");
        }
    }
}

#[test]
fn a_specialized_search_cannot_become_a_generic_entity_search() {
    let catalog = catalog();
    for domain in ["company", "general"] {
        let mut parsed = request();
        let cap = &mut parsed.capabilities[0];
        cap.domain = domain.into();
        cap.slug = format!("{domain}.search");
        cap.name = "Sanctions search".into();
        cap.description = "Find sanctioned companies matching the name".into();
        cap.outputs = vec!["companies".into()];
        let found = super::super::search(&parsed, &catalog);
        assert!(!found[0].operations.contains("company_registry.search"),
            "sanctioned-company scope cannot be discarded for a generic register search");
    }
}

#[test]
fn an_explicit_source_hint_restricts_the_same_generic_collection_search() {
    let catalog = catalog();
    let mut request = request();
    request.capabilities[0].source_hint = Some("kvk".into());
    let found = super::super::search(&request, &catalog);
    assert_eq!(
        found[0].operations,
        ["company_registry.search".into()].into()
    );
    assert!(found[0]
        .apiosk
        .iter()
        .all(|candidate| candidate.source_slug.as_deref() == Some("kvk-dutch-business-register")));
}

#[test]
fn generic_search_words_do_not_promote_single_records_opaque_outputs_or_other_domains() {
    let catalog = catalog();
    let selected = catalog
        .services
        .iter()
        .find(|service| service.contract.capability_slug == "company_registry.search")
        .unwrap();
    assert!(domains(selected, &catalog).contains("company"));
    let mut single = selected.clone();
    single.contract.cardinality = Cardinality::One;
    assert!(domains(&single, &catalog).is_empty());
    let mut unverified = selected.clone();
    unverified.unverified = true;
    assert!(domains(&unverified, &catalog).is_empty());
    let mut opaque = catalog.clone();
    opaque
        .vocabulary
        .get_mut("company.name")
        .unwrap()
        .value_schema = json!({});
    assert!(domains(selected, &opaque).is_empty());
    let mut other = request();
    other.capabilities[0].domain = "person".into();
    other.capabilities[0].slug = "person.search".into();
    other.capabilities[0].inputs = vec!["person_name".into()];
    other.capabilities[0].outputs = vec!["person_matches".into()];
    assert!(super::super::search(&other, &catalog)[0].apiosk.is_empty());
}

#[test]
fn an_exact_product_search_contract_survives_a_different_collection_envelope() {
    let mut value: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/search/company-name-collections.json"
    )).unwrap();
    let mut service = value["services"][0].clone();
    service["contract"]["capability_slug"] = json!("product.search");
    service["contract"]["cardinality"] = json!("one");
    service["contract"]["identifies"] = Value::Null;
    service["contract"]["requires"] = json!([{"fact_type":"product.query","subject_role":"subject","optional":false}]);
    service["contract"]["input_map"] = json!({"product.query":{"in":"query","name":"query"}});
    service["contract"]["output_map"] = json!({"base":"$","produces":{"product.results":{"path":"$.results"}}});
    service["sample_payload"] = json!({"results":[{"name":"Example"}]});
    value["services"] = json!([service]);
    value["vocabulary"] = json!([
        {"slug":"product.query","value_schema":{"type":"string"}},
        {"slug":"product.results","value_schema":{"type":"array","items":{"type":"object","properties":{"name":{"type":"string"}}}}}
    ]);
    let catalog = Catalog::build(serde_json::from_value(value.clone()).unwrap());
    assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
    assert!(domains(&catalog.services[0], &catalog).is_empty(), "this contract uses neither *.name facts nor many-subject cardinality");
    let mut parsed = request();
    parsed.subjects[0].kind = "product".into();
    let cap = &mut parsed.capabilities[0];
    cap.slug = "product.search".into();
    cap.domain = "product".into();
    cap.name = "Product search".into();
    cap.description = "Find matching products.".into();
    cap.inputs = vec!["product_name".into()];
    cap.outputs = vec!["product_matches".into()];
    assert!(requested(cap));
    assert!(super::super::search(&parsed, &catalog)[0].operations.contains("product.search"));
    value["services"][0]["contract"]["capability_slug"] = json!("product.profile");
    let profile = Catalog::build(serde_json::from_value(value.clone()).unwrap());
    assert!(profile.rejected.is_empty());
    assert!(super::super::search(&parsed, &profile)[0].apiosk.is_empty(),
        "a shared product domain is insufficient evidence for a collection search");
    value["vocabulary"][1]["value_schema"] = json!({"type":"object","properties":{"name":{"type":"string"}},
        "$comment":"{\"apiosk_output_aliases_v1\":[\"product_details\"]}"});
    value["services"][0]["sample_payload"] = json!({"results":{"name":"Example"}});
    let singleton = Catalog::build(serde_json::from_value(value.clone()).unwrap());
    assert!(singleton.rejected.is_empty());
    assert!(super::super::search(&parsed, &singleton)[0].apiosk.is_empty(),
        "a documented singleton product category is not a product collection");
    value["vocabulary"][1]["value_schema"] = json!({"type":"array","items":{"type":"object","properties":{"name":{"type":"string"}}},
        "$comment":"{\"apiosk_output_aliases_v1\":[\"product_matches\"]}"});
    value["services"][0]["sample_payload"] = json!({"results":[{"name":"Example"}]});
    let collection = Catalog::build(serde_json::from_value(value).unwrap());
    assert!(collection.rejected.is_empty());
    assert!(super::super::search(&parsed, &collection)[0].operations.contains("product.profile"),
        "reviewed array output evidence can establish a differently named operation");
}
