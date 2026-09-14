use apiosk_gateway_v2::{catalog::{Catalog, Snapshot}, parser::Parser, resolver::{resolve, Resolution}, state::{Entity, Fact, Task}};
use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

#[tokio::test]
#[ignore = "calls the configured OpenRouter model with saved public fixtures; no paid data source"]
async fn live_multi_company_parser_and_saved_report_analysis() {
    dotenvy::dotenv().ok();
    let parser = Parser::new(reqwest::Client::new(), std::env::var("INTENT_API_URL").unwrap(), std::env::var("INTENT_API_KEY").unwrap(), std::env::var("INTENT_MODEL").unwrap());
    let input=std::env::var("MULTI_COMPANY_CATALOG").expect("read-only catalog snapshot or database");
    let snapshot: Snapshot = if input == "database" {
        let pool=sqlx::postgres::PgPoolOptions::new().max_connections(1).connect(&std::env::var("DATABASE_URL").unwrap()).await.unwrap();
        let value:Value=sqlx::query_scalar(include_str!("../src/catalog.sql")).fetch_one(&pool).await.unwrap();
        pool.close().await; serde_json::from_value(value).unwrap()
    } else { serde_json::from_slice(&std::fs::read(input).unwrap()).unwrap() };
    let catalog = Catalog::build(snapshot);
    let mut task = Task::default();
    let question = "Haal de twee beschikbare jaarrekeningen van HEMA en AFAS op en maak een analyse tussen de twee.";
    let mut intent = parser.parse(Uuid::new_v4(), question, &task, &catalog).await.unwrap();
    assert_eq!(intent.subjects.len(),2); assert_eq!(intent.goals.len(),2); assert!(!intent.answer_operations.is_empty());
    for subject in &mut intent.subjects {
        let id=Uuid::new_v4(); subject.entity_ref=Some(id);
        task.entities.push(Entity {entity_ref:id,label:subject.label.clone(),selected:true});
        for (kind,value) in &subject.values { task.facts.push(Fact {entity_ref:id,semantic_type:kind.clone(),value:value.clone(),provenance:json!({}),observed_at:Utc::now(),expires_at:Utc::now()+chrono::Duration::hours(1),verification:"user_asserted".into()}); }
    }
    let resolution=resolve(&catalog,&task,&intent);
    let Resolution::Plan(plan)=resolution else {panic!("unexpected plan: {resolution:?}; intent={intent:?}")};
    assert_eq!(plan.steps.len(),4);
    assert!(plan.steps[..2].iter().all(|s|matches!(s.service.contract.capability_slug.as_str(),"company.registry-search"|"company_registry.search")));
    assert!(plan.steps[2..].iter().all(|s|s.service.contract.capability_slug=="company.financial-statements"));
    println!("HEMA/AFAS: two searches, two accounts, one cap {}; operations={:?}",plan.cap_atomic,intent.answer_operations);
    // Synthetic identifiers exercise reuse without executing any provider.
    for (index,subject) in intent.subjects.iter().enumerate() {
        let id=subject.entity_ref.unwrap(); task.focus.entity_refs.push(id);
        let kind=plan.steps.iter().find(|s|s.entity_ref==id && s.service.contract.capability_slug=="company.financial-statements").unwrap().service.contract.requires.iter().find(|r|!r.optional).unwrap().fact_type.clone();
        task.facts.push(Fact {entity_ref:id,semantic_type:kind,value:json!(if index==0 {"12345678"} else {"87654321"}),provenance:json!({"kind":"test_fixture"}),observed_at:Utc::now(),expires_at:Utc::now()+chrono::Duration::hours(1),verification:"source_observed".into()});
    }
    let followup=parser.parse(Uuid::new_v4(),"Haal de beschikbare jaarrekeningen van HEMA en AFAS nogmaals op.",&task,&catalog).await.unwrap();
    let Resolution::Plan(reused)=resolve(&catalog,&task,&followup) else {panic!("identifiers were not reused: {followup:?}")};
    assert_eq!(reused.steps.len(),2);
    assert!(reused.steps.iter().all(|s|s.service.contract.capability_slug=="company.financial-statements"));
    println!("Follow-up: both saved company identifiers reused; zero new searches.");
    let dir=std::env::var("SAVED_REPORT_DIR").expect("saved public source responses");
    let mut sources=Vec::new();
    for (name,file) in [("Jortt","jortt-64073319-20260909.json"),("Moneybird","moneybird-58209344-20260909.json")] {
        let data:Value=serde_json::from_slice(&std::fs::read(format!("{dir}/{file}")).unwrap()).unwrap();
        sources.push(json!({"result_ref":Uuid::new_v4(),"document":{"subject":{"label":name},"source":{"name":"KVK","path":apiosk_gateway_v2::annual_report::ENDPOINT},"data":data}}));
    }
    let analysis=parser.analyze(question,&json!(["Vergelijk de beschikbare jaarrekeningen van Jortt en Moneybird. Vermeld de beperkingen van deze vergelijking."]),&sources,&Default::default()).await.unwrap();
    assert!(!analysis["observations"].as_array().unwrap().is_empty());
    println!("Saved-source analysis: {}",serde_json::to_string_pretty(&analysis).unwrap());
}
