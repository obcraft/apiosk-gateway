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
        for output in ["companies", "matching_companies", "company_matches", "company_results"] {
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
fn generic_name_search_keeps_requested_result_and_identifier_scope() {
    let mut cap = request().capabilities.remove(0);
    cap.inputs = vec!["name".into()];
    for output in ["company_name", "company_count", "company_details", "company_sanctions_matches", "person_matches"] {
        cap.outputs = vec![output.into()];
        assert!(!requested(&cap), "{output} is not a generic company collection");
    }
    cap.outputs = vec!["company_matches".into()];
    cap.inputs = vec!["person_name".into()];
    assert!(!requested(&cap), "a related person's name is not a supplied company name");
    cap.inputs = vec!["name".into()];
    cap.slug = "company.profile".into();
    cap.name = "Company profile".into();
    assert!(!requested(&cap), "a profile request does not imply a collection search");
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
