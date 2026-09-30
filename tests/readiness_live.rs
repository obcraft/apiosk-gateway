use apiosk_gateway_v2::{catalog::CatalogCache,readiness};
use sqlx::postgres::PgPoolOptions;

async fn catalog_pool() -> sqlx::PgPool {
    PgPoolOptions::new()
        .max_connections(1)
        .after_connect(|connection, _| Box::pin(async move {
            // Match the runtime's schema resolution after the schema split.
            sqlx::query("SET search_path TO public, gateway_v2, kyb, providers, catalog, actions, billing, agents, wallets, research, integrations, observability, support, content, operations, identity, extensions")
                .execute(&mut *connection).await?;
            sqlx::query("SET default_transaction_read_only = on")
                .execute(connection).await?;
            Ok(())
        }))
        .connect(&std::env::var("DATABASE_URL").unwrap()).await.unwrap()
}
#[tokio::test]
#[ignore = "reads the production catalog only; no provider purchases"]
async fn production_readiness_matches_planner_and_runtime_contracts() {
    dotenvy::dotenv().ok();
    let pool=catalog_pool().await;
    let c=CatalogCache::new(pool).fresh().await.unwrap();
    let report=readiness::public_report(&c);
    let parser=c.parser_view();
    assert!(!c.services.is_empty(), "production must contain admitted operations");
    for source in report["sources"].as_array().unwrap() {
        let s=parser["sources"].as_array().unwrap().iter().find(|s|s["slug"]==source["slug"]).unwrap();
        assert_eq!(s["readiness"],source["readiness"]);
        if source["readiness"]["status"]=="blocked" || source["readiness"]["status"]=="discovery-only" {assert_eq!(s["available"],false);}
    }
    for slug in ["eodhd","x402-endpoints","x402-seller","koop","rvo","rdw","ted"] {
        if let Some(source)=report["sources"].as_array().unwrap().iter().find(|s|s["slug"]==slug) {
            println!("{}: {}",slug,source["readiness"]);
        } else {
            println!("{slug}: not in the current published catalog");
        }
    }
    println!("sources={} accepted_contracts={} rejected_contracts={}",c.known_sources.len(),c.services.len(),c.rejected.len());
}

#[tokio::test]
#[ignore = "reads production Ask search contracts only; no model or provider purchases"]
async fn production_ask_matches_reference_admitted_operations() {
    use apiosk_gateway_v2::{ask_v2::search, capability_intake};
    use serde_json::json;
    dotenvy::dotenv().ok();
    let catalog = CatalogCache::new(catalog_pool().await).fresh().await.unwrap();
    let mut supported = 0;
    for (slug, description, subject, kind, hint) in [
        ("procurement.tenders", "Find public tenders for IT services in the Netherlands", "Netherlands", "country", None),
        ("company.registry", "Retrieve official company registration details", "Coolblue B.V.", "company", None),
        ("company.financial.statements", "Retrieve the latest filed company annual accounts", "Coolblue B.V.", "company", None),
        ("vehicle.technical.profile", "Retrieve technical vehicle registration data by registration plate", "RZ874R", "vehicle", None),
        ("location.building", "Find building information for an address", "Damrak 1, Amsterdam", "address", None),
        ("finance.stock.prices", "Retrieve stock prices", "AAPL.US", "company", Some("EODHD")),
    ] {
        let raw = json!({"language":"en","subjects":[{"id":"s1","label":subject,"type":kind,"identifiers":{}}],
            "capabilities":[{"slug":slug,"name":description,"description":description,"domain":slug.split('.').next().unwrap(),
                "inputs":[],"outputs":[],"subject_id":"s1","source_hint":hint,"confidence":1}],
            "deliverable":{"format":"chat","operations":[]}});
        let question = format!("{description} for {subject}");
        let parsed = capability_intake::normalize(&raw.to_string(), &question, "contract-audit").unwrap();
        let matches = search::search(&parsed, &catalog);
        let mut admitted = 0;
        for candidate in matches.iter().flat_map(|found| &found.apiosk) {
            if candidate.availability != "supported" { continue; }
            let id: uuid::Uuid = candidate.endpoint_id.as_deref().expect("supported candidate needs endpoint id").parse().unwrap();
            let service = catalog.services.iter().find(|service|
                service.endpoint_id == id && Some(service.contract.capability_slug.as_str()) == candidate.capability.as_deref()
            ).expect("supported candidate must reference an admitted endpoint and operation");
            assert!(!service.unverified);
            assert_eq!(candidate.source_slug.as_deref(), Some(service.api_slug.as_str()));
            assert_eq!(candidate.method, service.method);
            assert!(candidate.price.is_some());
            assert!(service.total() >= 0);
            for required in &service.contract.requires {
                assert!(catalog.vocabulary.contains_key(&required.fact_type));
                assert!(service.contract.input_map.contains_key(&required.fact_type));
            }
            assert_ne!(service.api_slug, "eodhd", "blocked source cannot be executable");
            admitted += 1;
        }
        if hint.is_none() { assert!(admitted > 0, "no admitted matches for {slug}"); }
        else { assert_eq!(admitted, 0, "explicit blocked source cannot be substituted"); }
        println!("{slug}: {admitted} supported matches with admitted contracts");
        supported += admitted;
    }
    assert!(supported > 0);
}

#[tokio::test]
#[ignore = "production catalog and three model calls; no data-source purchases"]
async fn live_model_respects_readiness_and_still_selects_supported_sources() {
    use apiosk_gateway_v2::{parser::Parser,state::Task};
    dotenvy::dotenv().ok();
    let pool=catalog_pool().await;
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
