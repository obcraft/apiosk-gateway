use super::*;
use crate::catalog::{Catalog, Snapshot};
use catalog_core::facts::contract::{Cardinality, FactType};

fn catalog(schema: Value) -> Catalog {
    let mut snapshot: Snapshot = serde_json::from_str(include_str!(
        "../../../tests/fixtures/integration-catalog.json"
    ))
    .unwrap();
    let mut service = snapshot.services.remove(0);
    service.api_slug = "kvk".into();
    service.name = "KVK".into();
    service.contract.capability_slug = "company.registry-profile".into();
    service.contract.cardinality = Cardinality::One;
    service.contract.identifies = None;
    service.contract.output_map =
        serde_json::from_value(json!({"base":"$","produces":{"company.profile":{"path":"$"}}}))
            .unwrap();
    service.optional_outputs.clear();
    service.sample_payload = Some(json!({"legal_name":"Coolblue B.V."}));
    snapshot.services = vec![service];
    snapshot
        .vocabulary
        .retain(|fact| fact.slug != "company.profile");
    snapshot.vocabulary.push(FactType {
        slug: "company.profile".into(),
        value_schema: schema,
    });
    let result = Catalog::build(snapshot);
    assert_eq!(result.services.len(), 1, "{:?}", result.rejected);
    result
}

fn profile_schema() -> Value {
    json!({"type":"object","properties":{
        "legal_name":{"type":"string"},
        "activities":{"type":"array","items":{"type":"object","properties":{"code":{"type":"string"}}}},
        "registered_on":{"type":"string","$comment":"{\"apiosk_output_aliases_v1\":[\"registration_date\"]}"}
    }})
}

fn response(required: Value, optional: Value) -> Value {
    json!({"schema_version":1,"model":"fixture","timing":{"parse_ms":1},"usage":{"cost_usd":0.001},
        "intent":{"language":"en","subjects":[{"id":"s1","type":"company","label":"Coolblue B.V.","identifiers":{}}],
        "capabilities":[],"requests":[{"id":"r1","capability_slug":"company.registry-profile","subject_ids":["s1"],
        "kind":"lookup","goal":"Read the company registration","required_fields":required,"optional_fields":optional,
        "filters":{},"sources":["KVK"]}],"output":{"format":"chat","operations":[]}}})
}

#[test]
fn canonical_fields_preserve_unsupported_requirements_and_cost() {
    let catalog = catalog(profile_schema());
    let mut output = response(
        json!(["legal_name", "registration_details", "registration_date"]),
        json!(["activities", "employee_count"]),
    );
    ground_response(&mut output, &catalog).unwrap();
    assert_eq!(output["schema_version"], 2);
    assert_eq!(output["usage"]["cost_usd"], 0.001);
    assert_eq!(output["intent"]["field_catalog_version"], catalog.version);
    let request = &output["intent"]["requests"][0];
    assert_eq!(
        request["required_fields"],
        json!([
            "company.profile#/properties/legal_name",
            "company.profile#/properties/registered_on"
        ])
    );
    assert_eq!(
        request["optional_fields"],
        json!(["company.profile#/properties/activities"])
    );
    assert_eq!(
        request["unresolved_fields"],
        json!({"required":[{"field":"registration_details","reason":"undocumented"}],"optional":[{"field":"employee_count","reason":"undocumented"}]})
    );
    assert_eq!(request["field_resolution"], "unresolved");
    for field in output["intent"]["field_definitions"].as_array().unwrap() {
        let fact = field["fact_type"].as_str().unwrap();
        assert!(catalog.vocabulary[fact]
            .value_schema
            .pointer(field["schema_pointer"].as_str().unwrap())
            .is_some());
    }
}

#[test]
fn unrelated_capabilities_and_named_sources_cannot_lend_field_evidence() {
    let catalog = catalog(profile_schema());
    for (key, value) in [
        ("capability_slug", json!("company.sanctions-screen")),
        ("sources", json!(["OpenSanctions"])),
    ] {
        let mut output = response(json!(["legal_name"]), json!([]));
        output["intent"]["requests"][0][key] = value;
        ground_response(&mut output, &catalog).unwrap();
        assert_eq!(
            output["intent"]["requests"][0]["required_fields"],
            json!([])
        );
        assert_eq!(
            output["intent"]["requests"][0]["unresolved_fields"]["required"][0]["field"],
            "legal_name"
        );
    }
}

#[test]
fn ambiguous_names_are_not_arbitrarily_bound_and_array_paths_are_stable() {
    let catalog = catalog(json!({"type":"object","properties":{
        "legal_name":{"type":"string"},
        "officers":{"type":"array","items":{"type":"object","properties":{"name":{"type":"string"}}}},
        "owner":{"type":"object","properties":{"name":{"type":"string"},"a/b~c":{"type":"string"}}}
    }}));
    let mut output = response(json!(["name", "a_b_c"]), json!([]));
    ground_response(&mut output, &catalog).unwrap();
    assert_eq!(
        output["intent"]["requests"][0]["unresolved_fields"]["required"],
        json!([{"field":"name","reason":"ambiguous"}])
    );
    assert_eq!(
        output["intent"]["requests"][0]["required_fields"],
        json!(["company.profile#/properties/owner/properties/a~1b~0c"])
    );
    let fields = Fields::for_request(&output["intent"]["requests"][0], &catalog).unwrap();
    assert!(fields
        .definitions
        .contains_key("company.profile#/properties/officers/items/properties/name"));
}

#[test]
fn opaque_records_prose_and_invalid_aliases_never_define_fields() {
    let catalog = catalog(
        json!({"type":"object","description":"Includes sanctions matches and registration details","properties":{
            "legal_name":{"type":"string"},
            "opaque":{"type":"object","$comment":"{\"apiosk_output_aliases_v1\":[\"registration_details\"]}"},
            "unknown":{},
            "wrong":{"type":"string","$comment":"{\"apiosk_output_aliases_v1\":[\"sanctions_matches\",\"sanctions_matches\"]}"}
        }}),
    );
    let mut output = response(
        json!([
            "registration_details",
            "sanctions_matches",
            "opaque",
            "unknown"
        ]),
        json!([]),
    );
    ground_response(&mut output, &catalog).unwrap();
    assert_eq!(
        output["intent"]["requests"][0]["required_fields"],
        json!([])
    );
    assert_eq!(
        output["intent"]["requests"][0]["unresolved_fields"]["required"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
}

#[test]
fn canonical_alias_overlap_preserves_optional_and_deduplicates() {
    let catalog = catalog(profile_schema());
    let mut output = response(
        json!(["registered_on", "legal_name", "legal_name"]),
        json!(["registration_date"]),
    );
    ground_response(&mut output, &catalog).unwrap();
    let request = &output["intent"]["requests"][0];
    assert_eq!(
        request["required_fields"],
        json!(["company.profile#/properties/legal_name"])
    );
    assert_eq!(
        request["optional_fields"],
        json!(["company.profile#/properties/registered_on"])
    );
    assert_eq!(request["field_resolution"], "resolved");
    assert_eq!(
        output["intent"]["field_definitions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn unverified_services_cannot_ground_fields_and_drafts_stay_unresolved() {
    let mut catalog = catalog(profile_schema());
    catalog.services[0].unverified = true;
    let mut output = response(json!(["legal_name"]), json!([]));
    ground_response(&mut output, &catalog).unwrap();
    assert_eq!(
        output["intent"]["requests"][0]["required_fields"],
        json!([])
    );
    assert_eq!(
        output["intent"]["requests"][0]["field_resolution"],
        "unresolved"
    );
}

#[test]
fn actual_uk_registry_reviewed_alias_resolves_to_schema_property() {
    let catalog = Catalog::build(
        serde_json::from_str(include_str!(
            "../../../tests/fixtures/uk-registry-profile/catalog.json"
        ))
        .unwrap(),
    );
    let mut output = response(json!(["legal_name"]), json!([]));
    output["intent"]["requests"][0]["sources"] = json!([]);
    ground_response(&mut output, &catalog).unwrap();
    let request = &output["intent"]["requests"][0];
    assert_eq!(request["field_resolution"], "resolved", "{output}");
    assert_eq!(request["required_fields"].as_array().unwrap().len(), 1);
    assert!(request["required_fields"][0]
        .as_str()
        .unwrap()
        .ends_with("/properties/company_name"));
}

#[test]
fn empty_fields_remain_valid_without_claiming_requested_data() {
    let catalog = catalog(profile_schema());
    let mut output = response(json!([]), json!([]));
    output["intent"]["requests"][0]["kind"] = json!("answer");
    ground_response(&mut output, &catalog).unwrap();
    assert_eq!(output["intent"]["field_definitions"], json!([]));
    assert_eq!(
        output["intent"]["requests"][0]["field_resolution"],
        "resolved"
    );
    assert!(
        ground_response(&mut output, &catalog).is_err(),
        "do not reinterpret a saved v2 intent"
    );
}
