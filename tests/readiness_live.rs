use apiosk_gateway_v2::{catalog::CatalogCache,readiness};
use sqlx::postgres::PgPoolOptions;
#[tokio::test]
#[ignore = "reads the production catalog only; no provider purchases"]
async fn production_readiness_matches_planner_and_runtime_contracts() {
    dotenvy::dotenv().ok();
    let pool=PgPoolOptions::new().max_connections(1).connect(&std::env::var("DATABASE_URL").unwrap()).await.unwrap();
    let c=CatalogCache::new(pool).fresh().await.unwrap();
    let report=readiness::public_report(&c);
    let parser=c.parser_view();
    for source in report["sources"].as_array().unwrap() {
        let s=parser["sources"].as_array().unwrap().iter().find(|s|s["slug"]==source["slug"]).unwrap();
        assert_eq!(s["readiness"],source["readiness"]);
        if source["readiness"]["status"]=="blocked" || source["readiness"]["status"]=="discovery-only" {assert_eq!(s["available"],false);}
    }
    for slug in ["eodhd","x402-endpoints","x402-seller","koop","rvo","rdw","ted"] {
        let source=report["sources"].as_array().unwrap().iter().find(|s|s["slug"]==slug).unwrap();
        println!("{}: {}",slug,source["readiness"]);
    }
    println!("sources={} accepted_contracts={} rejected_contracts={}",c.known_sources.len(),c.services.len(),c.rejected.len());
}

#[tokio::test]
#[ignore = "production catalog and three model calls; no data-source purchases"]
async fn live_model_respects_readiness_and_still_selects_supported_sources() {
    use apiosk_gateway_v2::{parser::Parser,state::Task};
    dotenvy::dotenv().ok();
    let pool=PgPoolOptions::new().max_connections(1).connect(&std::env::var("DATABASE_URL").unwrap()).await.unwrap();
    let c=CatalogCache::new(pool).fresh().await.unwrap();
    let parser=Parser::new(reqwest::Client::new(),std::env::var("INTENT_API_URL").unwrap(),std::env::var("INTENT_API_KEY").unwrap(),std::env::var("INTENT_MODEL").unwrap());
    for q in ["Buy EODHD prices for AAPL.US.","Use x402 Seller to buy a company report."] {
        let reply=parser.chat(q,&Task::default(),&c).await.unwrap();
        assert!(!reply.discover,"{reply:?}");println!("{q}: {}",reply.message);
    }
    let intent=parser.parse(uuid::Uuid::new_v4(),"Get RDW technical vehicle data for plate RZ874R.",&Task::default(),&c).await.unwrap();
    assert!(intent.goals.iter().any(|g|g.capability=="vehicle.technical-profile"));
    println!("RDW selected: {:?}",intent.goals);
}
