use super::*;

#[test]
fn explicit_record_categories_require_structured_direct_fields_or_aliases() {
    for kind in ["string", "number", "integer", "boolean"] {
        let direct=json!({"type":"object","properties":{"employee_details":{"type":kind}}});
        let alias=json!({"type":"object","properties":{"employees":{"type":kind,
            "$comment":"{\"apiosk_output_aliases_v1\":[\"employee_details\"]}"}}});
        assert!(!schema_covers(&direct,"employee_details",false),"direct {kind}");
        assert!(!schema_covers(&alias,"employee_details",false),"alias {kind}");
    }
    for field in [
        json!({"type":"object","properties":{"name":{"type":"string"}}}),
        json!({"type":"array","items":{"type":"object","properties":{"name":{"type":"string"}}}}),
    ] {
        assert!(schema_covers(&json!({"type":"object","properties":{"employee_details":field}}),"employee_details",false));
    }
}

#[test]
fn scalar_fields_do_not_become_records_from_plural_spelling() {
    for field in ["status","business","earnings","employee_count","company_overview","company_information","company_info"] {
        assert!(!requires_record_shape(field));
        assert!(schema_covers(&json!({"type":"object","properties":{(field):{"type":"number"}}}),field,false),"{field}");
    }
    // With no typed request output, a bare noun cannot safely decide whether
    // employees means a count, collection or another meaning. Intake must
    // preserve a personnel-record request as an explicit category.
    assert!(!requires_record_shape("employees"));
}

#[test]
fn a_fact_suffix_cannot_bypass_record_shape_validation() {
    let mut snapshot:crate::catalog::Snapshot=serde_json::from_str(include_str!(
        "../../../tests/fixtures/integration-catalog.json")).unwrap();
    let mut service=snapshot.services.remove(0);
    service.contract.identifies=None;
    service.contract.cardinality=serde_json::from_value(json!("one")).unwrap();
    service.sample_payload=Some(json!({"employees":12}));
    service.contract.output_map.base="$".into();
    service.contract.output_map.produces=serde_json::from_value(json!({"company.employee_details":{"path":"$.employees"}})).unwrap();
    snapshot.services=vec![service];
    snapshot.vocabulary.push(serde_json::from_value(json!({"slug":"company.employee_details","value_schema":{"type":"number"}})).unwrap());
    let catalog=Catalog::build(snapshot);
    assert!(!catalog.services.is_empty(),"{:?}",catalog.rejected);
    assert!(!output_covered("employee_details",&catalog.services[0],&catalog,false));
}
