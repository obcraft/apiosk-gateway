use super::*;
use serde_json::{json, Value};
use uuid::Uuid;

#[test]
fn employee_records_survive_a_shortlist_crowded_by_headcount_profile_prose() {
    let mut value: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/crunchbase/official-mcp.json"
    ))
    .unwrap();
    let profile = value["services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|service| service["contract"]["capability_slug"] == "company.crunchbase-profile")
        .unwrap()
        .clone();
    value["services"] = json!([profile.clone()]);
    value["vocabulary"]
        .as_array_mut()
        .unwrap()
        .push(json!({"slug":"company.headcount_record",
        "value_schema":{"type":"object","properties":{"employee_count":{"type":"integer"}}}}));
    for index in 0..4 {
        let mut service = profile.clone();
        service["id"] = json!(Uuid::new_v4());
        service["endpoint_id"] = json!(Uuid::new_v4());
        service["api_slug"] = json!(format!("synthetic-count-provider-{index}"));
        service["name"] = json!("Company employee profile registry");
        service["description"] = json!("Employee profile and company information for research into employee names and job titles. Employee count is the only returned workforce field.");
        service["contract"]["capability_slug"] = json!(format!("company.registry-profile-{index}"));
        service["contract"]["output_map"] =
            json!({"base":"$","produces":{"company.headcount_record":{"path":"$"}}});
        service["sample_payload"] = json!({"employee_count":10});
        value["services"].as_array_mut().unwrap().push(service);
    }
    let catalog = Catalog::build(serde_json::from_value(value).unwrap());
    assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
    let request: ParsedRequest = serde_json::from_value(json!({
        "language":"en","subjects":[{"id":"s1","label":"Example Company","type":"company","identifiers":{"company_name":"Example Company"}}],
        "capabilities":[{"slug":"company.employees.profile","name":"Employee profile",
            "description":"Retrieve employee names and job titles for a company","domain":"company",
            "inputs":["company_name"],"outputs":["employee_names","job_titles"],"subject_id":"s1","source_hint":null,"confidence":1,"missing":[]}],
        "deliverable":{"format":"chat","operations":[]},
        "meta":{"model":"fixture","parsed_at":"2026-10-13T00:00:00Z","text_sha256":"fixture"}
    })).unwrap();
    let found = search(&request, &catalog);
    assert_eq!(
        found[0].apiosk[0].capability.as_deref(),
        Some("company.crunchbase-profile"),
        "{:?}",
        found[0].apiosk
    );
}
