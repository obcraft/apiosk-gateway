use super::*;
use crate::{ask_v2::search::search, capability_intake::ParsedRequest, catalog::Snapshot};
use serde_json::json;

fn catalog(schema: Value) -> Catalog {
    let mut value: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/integration-catalog.json"
    ))
    .unwrap();
    let mut service = value["services"][0].clone();
    service["name"] = json!("Vendor");
    service["description"] = json!("Retrieve one company record.");
    service["contract"]["capability_slug"] = json!("company.vendor-record");
    service["contract"]["cardinality"] = json!("one");
    service["contract"]["identifies"] = Value::Null;
    service["contract"]["output_map"] =
        json!({"base":"$","produces":{"company.record":{"path":"$.data"}}});
    service["sample_payload"] = json!({"data":{}});
    let mut other = service.clone();
    other["id"] = json!("963fa799-db8f-4484-9288-21d760d7f323");
    other["endpoint_id"] = json!("7cd248cf-ac7a-4ec3-85dc-536dc3d39f22");
    other["contract"]["capability_slug"] = json!("company.vendor-lookup");
    other["contract"]["output_map"]["produces"] = json!({"company.unrelated":{"path":"$.data"}});
    value["services"] = json!([service, other]);
    value["vocabulary"] = json!([
        {"slug":"company.name","value_schema":{"type":"string"}},
        {"slug":"company.record","value_schema":schema},
        {"slug":"company.unrelated","value_schema":{"type":"object","properties":{"registration_id":{"type":"string"}}}}
    ]);
    let catalog = Catalog::build(serde_json::from_value::<Snapshot>(value).unwrap());
    assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
    catalog
}

fn request() -> ParsedRequest {
    serde_json::from_value(json!({
        "language":"nl","subjects":[{"id":"s1","label":"Example","type":"company","identifiers":{}}],
        "capabilities":[{"slug":"company.employee.details","name":"Company employee details",
            "description":"Retrieve employee information for the company.","domain":"company",
            "inputs":[],"outputs":["employee_details"],"subject_id":"s1","source_hint":null,"confidence":1,"missing":[]}],
        "deliverable":{"format":"chat","operations":[]},"meta":{"model":"fixture","parsed_at":"2026-10-13T00:00:00Z","text_sha256":"fixture"}
    })).unwrap()
}

#[test]
fn provider_free_employee_request_finds_reviewed_output_alias_only_on_its_endpoint() {
    let catalog = catalog(json!({"type":"object","properties":{"staff":{
        "type":"array","items":{"type":"object","properties":{"name":{"type":"string"},"role":{"type":"string"}}},
        "$comment":"{\"apiosk_output_aliases_v1\":[\"employee_details\"]}"
    }}}));
    let found = search(&request(), &catalog);
    assert_eq!(found[0].apiosk.len(), 1, "{:?}", found[0].apiosk);
    assert_eq!(
        found[0].apiosk[0].capability.as_deref(),
        Some("company.vendor-record")
    );
    let selected = catalog
        .services
        .iter()
        .find(|service| service.contract.capability_slug == "company.vendor-record")
        .unwrap();
    assert!(crate::parser::plan_coverage::outputs_cover(
        &request().capabilities[0],
        selected,
        &catalog
    ));
}

#[test]
fn descriptions_and_nested_field_names_contribute_without_inventing_output_coverage() {
    let catalog = catalog(json!({"type":"object","properties":{"staff":{
        "type":"array","items":{"type":"object","properties":{"employee_name":{"type":"string","description":"Employee name and professional role"}}}
    }}}));
    let mut parsed = request();
    parsed.capabilities[0].outputs = vec!["employee_name".into()];
    let found = search(&parsed, &catalog);
    assert!(found[0]
        .apiosk
        .iter()
        .any(|candidate| candidate.capability.as_deref() == Some("company.vendor-record")));
    let selected = catalog
        .services
        .iter()
        .find(|service| service.contract.capability_slug == "company.vendor-record")
        .unwrap();
    assert!(
        !crate::parser::plan_coverage::outputs_cover(
            &request().capabilities[0],
            selected,
            &catalog
        ),
        "discovery evidence does not claim an undeclared aggregate output"
    );
}

#[test]
fn opaque_and_unverified_outputs_do_not_supply_schema_evidence() {
    let catalog = catalog(json!({"type":"object","properties":{
        "blob":{"type":"object","description":"Employee details","$comment":"{\"apiosk_output_aliases_v1\":[\"employee_details\"]}"},
        "unknown":{"description":"Employee details"}
    }}));
    assert!(search(&request(), &catalog)[0].apiosk.is_empty());
    let mut service = catalog.services[0].clone();
    service.unverified = true;
    assert!(metadata(&service, &catalog).text.is_empty());
}

#[test]
fn reviewed_nullable_fields_are_searchable_but_compositions_are_not() {
    let schema = json!({"type":"object","properties":{
        "employee_name":{"type":["string","null"]},
        "unsafe":{"type":"object","oneOf":[{"properties":{"invented_field":{"type":"string"}}}],
            "description":"invented coverage","$comment":"{\"apiosk_output_aliases_v1\":[\"invented_details\"]}"}
    }});
    let mut terms = BTreeSet::new();
    collect(&schema, "", 0, &mut terms, &mut BTreeSet::new());
    assert!(terms.contains("employee_name"));
    assert!(!terms.iter().any(|term| term.contains("invented")));
}

#[test]
fn employee_counts_and_employer_ratings_do_not_prove_employee_details() {
    for field in ["employee_count", "employee_rating", "salary"] {
        let catalog = catalog(json!({"type":"object","properties":{
            (field):{"type":"number"}
        }}));
        let found = search(&request(), &catalog);
        assert!(found[0].apiosk.is_empty(), "{field}: {:?}", found[0].apiosk);
        let selected = &catalog.services[0];
        assert!(!crate::parser::plan_coverage::outputs_cover(
            &request().capabilities[0],
            selected,
            &catalog
        ));
    }
}

#[test]
fn scalar_plural_fields_do_not_establish_record_categories() {
    for kind in ["number", "string"] {
        // Existing company-enrichment contracts use `employees` for a count
        // or size band. Its name alone does not promise employee records.
        let catalog = catalog(json!({"type":"object","properties":{
            "employees":{"type":kind}
        }}));
        let found = search(&request(), &catalog);
        assert!(found[0].apiosk.is_empty(), "{kind}: {:?}", found[0].apiosk);
        let mut bare_collection = request();
        bare_collection.capabilities[0].slug = "company.employees".into();
        bare_collection.capabilities[0].name = "Personnel information".into();
        bare_collection.capabilities[0].description = "Retrieve personnel information for the company.".into();
        bare_collection.capabilities[0].outputs = vec!["employees".into()];
        assert!(search(&bare_collection, &catalog)[0].apiosk.is_empty(), "bare collection, {kind}");
        bare_collection.capabilities[0].outputs = vec!["employee_names".into()];
        assert!(search(&bare_collection, &catalog)[0].apiosk.is_empty(), "employee names, {kind}");
        let output = metadata(&catalog.services[0], &catalog);
        assert!(output.text.contains("employees"), "keep scalar lexical discovery");
        assert!(!output.concepts.contains(&concept("employee_details")));
        assert!(!output.concepts.contains(&concept("employees")));
    }
}

#[test]
fn structured_collections_establish_record_categories_without_special_case_routing() {
    for category in ["employee", "product"] {
        let field = format!("{category}s");
        let catalog = catalog(json!({"type":"object","properties":{
            (field):{"type":"array","items":{"type":"object","properties":{
                "name":{"type":"string"},"description":{"type":"string"}
            }}}
        }}));
        let mut parsed = request();
        parsed.capabilities[0].slug = format!("company.{category}.details");
        parsed.capabilities[0].name = format!("Company {category} details");
        parsed.capabilities[0].description = format!("Retrieve {category} details.");
        parsed.capabilities[0].outputs = vec![format!("{category}_details")];
        assert_eq!(search(&parsed, &catalog)[0].apiosk.len(), 1, "{category}");
    }
}

#[test]
fn scalar_measurement_aliases_keep_their_exact_discovery_evidence() {
    let catalog = catalog(json!({"type":"object","properties":{
        "employees":{"type":"number","$comment":"{\"apiosk_output_aliases_v1\":[\"employee_count\"]}"}
    }}));
    let mut parsed = request();
    parsed.capabilities[0].slug = "company.employee.count".into();
    parsed.capabilities[0].name = "Employee count".into();
    parsed.capabilities[0].description = "Retrieve the number of employees.".into();
    parsed.capabilities[0].outputs = vec!["employee_count".into()];
    assert_eq!(search(&parsed, &catalog)[0].apiosk.len(), 1);
    assert!(metadata(&catalog.services[0], &catalog).concepts.contains(&concept("employee_count")));
}

#[test]
fn company_employee_fields_do_not_turn_into_an_individual_profile_lookup() {
    let catalog = catalog(json!({"type":"object","properties":{"employees":{
        "type":"array","items":{"type":"object","properties":{"name":{"type":"string"}}},
        "$comment":"{\"apiosk_output_aliases_v1\":[\"employee_details\"]}"
    }}}));
    let mut parsed = request();
    parsed.subjects[0].kind = "person".into();
    parsed.capabilities[0].slug = "company.employee.profile".into();
    parsed.capabilities[0].name = "Individual employee profile".into();
    parsed.capabilities[0].outputs = vec!["profile".into(), "employee_details".into()];
    assert!(search(&parsed, &catalog)[0].apiosk.is_empty());
}

#[test]
fn generic_record_names_and_urls_do_not_establish_employee_coverage() {
    let catalog = catalog(json!({"type":"object","properties":{
        "name":{"type":"string"},"url":{"type":"string"},"description":{"type":"string"}
    }}));
    for output in ["name", "url", "description"] {
        let mut parsed = request();
        parsed.capabilities[0].outputs = vec![output.into()];
        assert!(search(&parsed, &catalog)[0].apiosk.is_empty(), "{output}");
    }
}

#[test]
fn actual_crunchbase_employee_cards_are_found_without_provider_or_profile_url() {
    let catalog = Catalog::build(
        serde_json::from_str(include_str!(
            "../../../../tests/fixtures/crunchbase/official-mcp.json"
        ))
        .unwrap(),
    );
    let found = search(&request(), &catalog);
    assert!(
        found[0]
            .apiosk
            .iter()
            .any(|candidate| candidate.capability.as_deref() == Some("company.crunchbase-profile")),
        "{:?}",
        found[0].apiosk
    );
    assert!(
        !found[0]
            .apiosk
            .iter()
            .any(|candidate| candidate.capability.as_deref() == Some("person.crunchbase-profile")),
        "{:?}",
        found[0].apiosk
    );
    let selected = catalog
        .services
        .iter()
        .find(|service| service.contract.capability_slug == "company.crunchbase-profile")
        .unwrap();
    assert!(metadata(selected, &catalog).concepts.contains(&concept("employees")));
    assert!(
        !crate::parser::plan_coverage::outputs_cover(
            &request().capabilities[0],
            selected,
            &catalog
        ),
        "employee_details still requires semantic grounding in the declared cards before quote"
    );
}
