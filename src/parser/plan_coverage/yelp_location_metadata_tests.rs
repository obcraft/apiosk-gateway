use super::*;
use serde_json::json;

fn reviewed() -> Value {serde_json::from_str(include_str!("../../../tests/fixtures/yelp-location-record/reviewed-schema.json")).unwrap()}

#[test]
fn yelp_record_alias_preserves_validation_without_claiming_street_addresses() {
    let fixture=reviewed();
    let schema=&fixture["after"];
    catalog_core::facts::schema::check_schema(schema).unwrap();
    for output in ["location","business_location","customer_rating","review_count"] {
        assert!(schema_covers(schema,output,false),"{output}");
    }
    for output in ["address","street_address","geocoded_address","latitude","longitude","employer_rating"] {
        assert!(!schema_covers(schema,output,false),"{output}");
    }
    for data in [json!([]),json!([{"id":"literal-id","name":"Literal business","business_page_link":"https://www.yelp.com/biz/literal","rating":4.0,"review_count":12}])] {
        let payload=json!({"document":{"status":"OK","data":data}});
        assert_eq!(catalog_core::facts::schema::validate(&fixture["before"],&payload).is_ok(),catalog_core::facts::schema::validate(schema,&payload).is_ok());
        catalog_core::facts::schema::validate(schema,&payload).unwrap();
    }
    let mut stripped=schema.clone();stripped.pointer_mut("/properties/document/properties/data/items").unwrap().as_object_mut().unwrap().remove("$comment");
    assert_eq!(stripped,fixture["before"]);
}

#[tokio::test]
async fn recorded_combined_location_rating_request_compiles_all_three_sources() {
    let mut snapshot:crate::catalog::Snapshot=serde_json::from_str(include_str!("../../../tests/fixtures/company-reputation/catalog.json")).unwrap();
    let inputs:Value=serde_json::from_str(include_str!("../../../tests/fixtures/ecb-split-currency/reviewed-schemas.json")).unwrap();
    for fact in &mut snapshot.vocabulary {
        if fact.slug=="location.yelp_search_results" {fact.value_schema=reviewed()["after"].clone();}
        if let Some(row)=inputs["after"].as_array().unwrap().iter().find(|row|row["slug"]==fact.slug) {fact.value_schema=row["value_schema"].clone();}
    }
    let catalog=Catalog::build(snapshot);assert!(catalog.rejected.is_empty());
    let recorded:Value=serde_json::from_str(include_str!("../../../tests/fixtures/yelp-location-record/combined-intake.json")).unwrap();
    let question=recorded["question"].as_str().unwrap();
    let parsed_request=crate::capability_intake::normalize(&recorded["parsed_request"].to_string(),question,"recorded-fixture").unwrap();
    let matches=crate::ask_v2::search::search(&parsed_request,&catalog);
    let intake=Intake {selected_capabilities:matches.iter().flat_map(|m|m.operations.iter().cloned()).collect(),parsed_request,matches,apiosk_sources_searched:3,catalog_version:catalog.version.clone()};
    let parser=crate::parser::Parser::new(reqwest::Client::new(),"offline://no-model".into(),String::new(),"fixture".into());
    let reply=parser.chat_from_capabilities(question,None,&Default::default(),&catalog,&intake).await.unwrap();
    let mut intent=reply.intent.expect("all three source requirements must compile");
    let mut task=crate::state::Task {capability_intake:Some(intake),..Default::default()};task.record_turn(question,None);
    crate::engine::bind_subjects(&mut task,&catalog,&mut intent).unwrap();
    let crate::resolver::Resolution::Plan(plan)=crate::resolver::resolve(&catalog,&task,&intent) else {panic!("complete plan")};
    assert_eq!(plan.steps.len(),3);assert_eq!(plan.cap_atomic,106900);
    assert!(uncovered_outputs(&task,&catalog,&intent,&plan).is_empty());
    assert!(uncovered_inputs(&task,&catalog,&intent,&plan).is_empty());
    for output in ["address","street_address"] {
        task.capability_intake.as_mut().unwrap().parsed_request.capabilities[2].outputs.push(output.into());
        assert!(!uncovered_outputs(&task,&catalog,&intent,&plan).is_empty(),"{output}");
        task.capability_intake.as_mut().unwrap().parsed_request.capabilities[2].outputs.pop();
    }
}
