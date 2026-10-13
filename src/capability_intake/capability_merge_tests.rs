use super::*;

fn request() -> Value {
    json!({"language":"en","subjects":[
        {"id":"s1","label":"Example Ltd","type":"company","identifiers":{}},
        {"id":"s2","label":"Other Ltd","type":"company","identifiers":{}}
    ],"capabilities":[
        {"slug":"company.employees","name":"Employee details","description":"Retrieve employee names.","domain":"company","inputs":["company_name"],"outputs":["employee_name"],"subject_id":"s1","source_hint":null,"confidence":0.95},
        {"slug":"company.employees","name":"Employee roles","description":"Retrieve employee job titles.","domain":"company","inputs":["company_name"],"outputs":["job_title"],"subject_id":"s1","source_hint":null,"confidence":0.9}
    ],"deliverable":{"format":"chat","operations":[]}})
}

#[test]
fn repeated_record_capability_retains_all_requested_fields() {
    let parsed = normalize(&request().to_string(),
        "Give employee names and job titles for Example Ltd and Other Ltd.", "fixture").unwrap();
    assert_eq!(parsed.capabilities.len(), 1);
    let capability = &parsed.capabilities[0];
    assert_eq!(capability.outputs, ["employee_name", "job_title"]);
    assert_eq!(capability.inputs, ["company_name"]);
    assert!(capability.description.contains("names") && capability.description.contains("job titles"));
    assert!(capability.source_hint.is_none() && capability.missing.is_empty());
}

#[test]
fn repeated_capabilities_do_not_merge_different_companies_or_source_restrictions() {
    let mut raw = request();
    raw["capabilities"][1]["subject_id"] = json!("s2");
    let mut third = raw["capabilities"][0].clone();
    third["source_hint"] = json!("directory");
    raw["capabilities"].as_array_mut().unwrap().push(third);
    let parsed = normalize(&raw.to_string(),
        "Give employee names and job titles for Example Ltd and Other Ltd, and employee names from Directory for Example Ltd.", "fixture").unwrap();
    assert_eq!(parsed.capabilities.len(), 3);
    assert_eq!(parsed.capabilities[0].subject_id.as_deref(), Some("s1"));
    assert_eq!(parsed.capabilities[1].subject_id.as_deref(), Some("s2"));
    assert_eq!(parsed.capabilities[2].source_hint.as_deref(), Some("directory"));
}

#[test]
fn combined_record_still_enforces_output_bound() {
    let mut raw = request();
    raw["capabilities"][0]["outputs"] = json!((0..32).map(|index| format!("field_{index}")).collect::<Vec<_>>());
    assert!(normalize(&raw.to_string(), "Example Ltd and Other Ltd", "fixture")
        .unwrap_err().to_string().contains("too many input or output concepts"));
}
