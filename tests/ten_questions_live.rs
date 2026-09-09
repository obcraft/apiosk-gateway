//! Planning-only live test; no task creation, approval, payment or source calls.
use apiosk_gateway_v2::{catalog::{Catalog,Snapshot},parser::Parser,resolver::{resolve,Resolution},state::{Entity,Fact,Task}};
use chrono::Utc;
use serde_json::{json,Value};
use uuid::Uuid;
#[tokio::test]
#[ignore="reads production contracts and calls configured intent model; no provider purchases"]
async fn plan_ten_natural_language_questions() {
    dotenvy::dotenv().ok();
    let pool=sqlx::postgres::PgPoolOptions::new().max_connections(1).connect(&std::env::var("DATABASE_URL").unwrap()).await.unwrap();
    let value:Value=sqlx::query_scalar(include_str!("../src/catalog.sql")).fetch_one(&pool).await.unwrap();pool.close().await;
    let catalog=Catalog::build(serde_json::from_value::<Snapshot>(value).unwrap());
    let parser=Parser::new(reqwest::Client::new(),std::env::var("INTENT_API_URL").unwrap(),std::env::var("INTENT_API_KEY").unwrap(),std::env::var("INTENT_MODEL").unwrap());
    let questions=[
        "Haal via KVK de beschikbare jaarrekeningen van HEMA B.V. en AFAS Software B.V. op en vergelijk de financiële cijfers. Vermeld de boekjaren.",
        "Toon het KVK bedrijfsprofiel van Coolblue B.V. en controleer via OpenSanctions of er sanctiematches zijn.",
        "Welke directe dochterbedrijven van ASML Holding N.V. staan in GLEIF?",
        "Toon via Companies House het bedrijfsprofiel, bestuurders en personen met significante zeggenschap van MONZO BANK LIMITED.",
        "Zoek TenderNed-publicaties over laadpalen en vat de resultaten samen.",
        "Toon CBS-faillissementscijfers en leg uit welke periodes, sectoren en eenheden zijn teruggegeven.",
        "Vergelijk via Eurostat het bbp van Nederland en Duitsland in 2023.",
        "Zoek op data.overheid.nl datasets over waterkwaliteit en toon de uitgevers en bronlinks.",
        "Controleer via PDOK het adres Dam 1 Amsterdam en toon postcode, gemeente en buurt.",
        "Toon via Overheid.io de BAG-adressen voor postcode 1012JS.",
    ];
    let mut report=vec![];
    for (index,question) in questions.iter().enumerate() {
        let mut task=Task::default();
        let chat=parser.chat(question,&task,&catalog).await.expect("Chat must route a supported question to the planner");
        let mut row=match parser.parse(Uuid::new_v4(),question,&task,&catalog).await {
            Err(e)=>json!({"question":question,"status":"parse_failed","error":e.to_string()}),
            Ok(mut intent)=>{
                for s in &mut intent.subjects {let id=Uuid::new_v4();s.entity_ref=Some(id);task.entities.push(Entity{entity_ref:id,label:s.label.clone(),selected:true});for (kind,value) in &s.values {task.facts.push(Fact{entity_ref:id,semantic_type:kind.clone(),value:value.clone(),provenance:json!({}),observed_at:Utc::now(),expires_at:Utc::now()+chrono::Duration::hours(1),verification:"user_asserted".into()});}}
                match resolve(&catalog,&task,&intent) {
                    Resolution::Plan(plan)=>json!({"question":question,"status":"ready","intent":intent,"max_total_atomic":plan.cap_atomic,"steps":plan.steps.iter().map(|s|json!({"capability":s.service.contract.capability_slug,"source":s.service.api_slug,"entity_ref":s.entity_ref,"inputs":s.service.inputs(&task,s.entity_ref,&catalog.vocabulary).ok(),"requires":s.service.contract.requires})).collect::<Vec<_>>()}),
                    other=>json!({"question":question,"status":"not_ready","intent":intent,"resolution":format!("{other:?}")}),
                }
            }
        };
        row["chat_discover"]=json!(chat.discover);
        row["chat_message"]=json!(chat.message);
        println!("{}: {} {}",index+1,row["status"],row.get("steps").map(|s|s.to_string()).unwrap_or_else(||row.to_string()));
        report.push(row);
    }
    let dir=std::env::var("DIRECT_AUDIT_DIR").expect("evidence output directory");
    std::fs::write(format!("{dir}/question-planning.json"),serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    assert!(report.iter().all(|r|r["status"]=="ready"),"Some questions failed: {}",json!(report));
    assert!(report.iter().all(|r|r["chat_discover"]==true),"Chat must not reject a supported request before planning: {}",json!(report));
    assert_eq!(report[0]["steps"].as_array().unwrap().len(),4);
    assert!(report[1]["steps"].as_array().unwrap().iter().any(|s|s["capability"]=="company.registry-profile"),"A company search is not a profile");
    for cap in ["company.registry-profile","company.officers","company.significant-control"] {assert!(report[3]["steps"].as_array().unwrap().iter().any(|s|s["capability"]==cap),"Monzo missing {cap}");}
    assert!(report[7]["intent"]["answer_operations"].as_array().unwrap().is_empty(),"Dataset listing uses its native table, not generated publisher claims");
    assert_eq!(report[5]["steps"].as_array().unwrap().len(),4,"CBS needs observations and three codebooks");
    assert_eq!(report[6]["steps"].as_array().unwrap().len(),2);
    for s in report[6]["steps"].as_array().unwrap(){assert_eq!(s["inputs"]["time"],"2023");assert_eq!(s["inputs"]["unit"],"CP_MEUR");}
}
