use super::*;

fn fixture(field:Value,output:&str)->(Intake,Catalog) {
    let mut snapshot:Value=serde_json::from_str(include_str!("../../../tests/fixtures/integration-catalog.json")).unwrap();
    let mut service=snapshot["services"][0].clone();
    service["contract"]["capability_slug"]=json!("company.vendor-record");
    service["contract"]["cardinality"]=json!("one");
    service["contract"]["identifies"]=Value::Null;
    service["contract"]["output_map"]=json!({"base":"$","produces":{"company.record":{"path":"$.data"}}});
    service["sample_payload"]=json!({"data":{}});
    snapshot["services"]=json!([service]);
    snapshot["vocabulary"]=json!([
        {"slug":"company.name","value_schema":{"type":"string"}},
        {"slug":"company.record","value_schema":{"type":"object","properties":{
            "employees":field,"name":{"type":"string"},"address":{"type":"string"},
            "status":{"type":"string"},"business":{"type":"string"},"earnings":{"type":"number"},"company_overview":{"type":"string"}
        }}}
    ]);
    let catalog=Catalog::build(serde_json::from_value(snapshot).unwrap());
    assert!(catalog.rejected.is_empty(),"{:?}",catalog.rejected);
    let parsed=serde_json::from_value(json!({"language":"en","subjects":[{"id":"s1","label":"Example","type":"company","identifiers":{}}],
        "capabilities":[{"slug":"company.vendor-record","domain":"company","name":"Company information","description":"Retrieve the requested information.",
            "inputs":[],"outputs":[output],"subject_id":"s1","source_hint":null,"confidence":1,"missing":[]}],
        "deliverable":{"format":"chat","operations":[]},"meta":{"model":"fixture","parsed_at":"2026-10-13T00:00:00Z","text_sha256":"fixture"}})).unwrap();
    let matches=crate::ask_v2::search::search(&parsed,&catalog);
    let intake=Intake {selected_capabilities:matches.iter().flat_map(|found|found.operations.iter().cloned()).collect(),
        parsed_request:parsed,matches,apiosk_sources_searched:1,catalog_version:catalog.version.clone()};
    assert_eq!(candidates(&intake,0,&catalog).len(),1);
    (intake,catalog)
}

fn alias(mut field:Value)->Value {
    field["$comment"]=json!("{\"apiosk_output_aliases_v1\":[\"employee_details\"]}");
    field
}

#[test]
fn exact_compilation_and_single_field_model_bindings_reject_scalar_record_aliases() {
    for kind in ["number","string"] {
        let (intake,catalog)=fixture(alias(json!({"type":kind})),"employee_details");
        let service=&catalog.services[0];
        assert!(compile(&intake,&catalog,&[service]).is_empty());
        let field=inventory(service,&catalog).into_values().find(|field|field.path=="/employees").unwrap();
        let binding=Binding {requirement:0,output:"employee_details".into(),fields:vec![field.id],
            interpretation:"Report personnel information.".into(),limitation:None};
        assert!(validate(std::slice::from_ref(&binding),&intake,&catalog).is_err());
        assert!(!covered(&[binding],0,"employee_details",&[service],&catalog));
    }
}

#[test]
fn structured_aliases_and_multiple_concrete_field_compositions_remain_supported() {
    for field in [json!({"type":"object","properties":{"name":{"type":"string"}}}),
        json!({"type":"array","items":{"type":"object","properties":{"name":{"type":"string"}}}})] {
        let (intake,catalog)=fixture(alias(field),"employee_details");
        let bindings=compile(&intake,&catalog,&[&catalog.services[0]]);
        assert_eq!(bindings.len(),1);
        validate(&bindings,&intake,&catalog).unwrap();
    }
    let (intake,catalog)=fixture(json!({"type":"number"}),"company_details");
    let service=&catalog.services[0];
    let fields=inventory(service,&catalog).into_values().filter(|field|matches!(field.path.as_str(),"/name"|"/address"))
        .map(|field|field.id).collect();
    let binding=Binding {requirement:0,output:"company_details".into(),fields,
        interpretation:"Compose company details from its name and address.".into(),limitation:None};
    validate(std::slice::from_ref(&binding),&intake,&catalog).unwrap();
    assert!(covered(&[binding],0,"company_details",&[service],&catalog));
}

#[test]
fn explicit_scalar_fields_remain_exactly_compilable() {
    for output in ["status","business","earnings","company_overview"] {
        let (intake,catalog)=fixture(json!({"type":"number"}),output);
        assert_eq!(compile(&intake,&catalog,&[&catalog.services[0]]).len(),1,"{output}");
    }
}
