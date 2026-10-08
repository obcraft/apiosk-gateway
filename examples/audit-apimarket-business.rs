//! Read-only production audit of catalog admission and planning discovery.
//! No models, provider requests, purchases, or customer-state writes.
use apiosk_gateway_v2::{ask_v2::search::search, capability_intake::ParsedRequest, catalog::CatalogCache, sources::{browse, SourceQuery}};
use serde_json::{json, Value};
use std::collections::BTreeSet;

fn request(name: &str, description: &str, hint: Option<&str>) -> ParsedRequest {
    serde_json::from_value(json!({
        "language":"en","subjects":[],
        "capabilities":[{"slug":"general.resource.lookup","name":name,"description":description,
            "domain":"general","inputs":[],"outputs":["source_information"],"subject_id":null,"source_hint":hint,"confidence":1}],
        "deliverable":{"format":"chat","operations":["list"]},
        "meta":{"model":"read-only-catalog-audit","parsed_at":"2026-10-08T00:00:00Z","text_sha256":"public-api-market-metadata"}
    })).unwrap()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let pool=sqlx::postgres::PgPoolOptions::new().max_connections(1)
        .after_connect(|connection,_|Box::pin(async move {
            sqlx::query("SET default_transaction_read_only=on").execute(&mut *connection).await?;
            sqlx::query("SET statement_timeout='30s'").execute(&mut *connection).await?;
            sqlx::query("SET search_path TO public,gateway_v2,kyb,providers,catalog,actions,billing,agents,wallets,research,integrations,observability,support,content,operations,identity,extensions").execute(&mut *connection).await?;
            Ok(())
        })).connect(&std::env::var("DATABASE_URL")?).await?;
    let readonly:String=sqlx::query_scalar("SHOW transaction_read_only").fetch_one(&pool).await?;
    anyhow::ensure!(readonly=="on","audit must be read-only");
    let cache=CatalogCache::new(pool.clone());
    let catalog=cache.fresh().await?;
    let sources=cache.sources().await?;
    let manifest:Vec<Value>=serde_json::from_str(include_str!("../tests/fixtures/apimarket/business-sources.json"))?;
    let expected:BTreeSet<_>=manifest.iter().map(|row|row["slug"].as_str().unwrap()).collect();
    let registered:Vec<_>=catalog.known_sources.iter().filter(|row|expected.contains(row.slug.as_str())).collect();
    anyhow::ensure!(registered.len()==expected.len(),"not all reviewed business sources were admitted");
    anyhow::ensure!(!catalog.services.iter().any(|row|expected.contains(row.api_slug.as_str())),"metadata-only sources must not become executable");
    let parser=catalog.parser_view();
    let planner:Vec<_>=parser["sources"].as_array().unwrap().iter().filter(|row|expected.contains(row["slug"].as_str().unwrap_or(""))).collect();
    anyhow::ensure!(planner.len()==expected.len()&&planner.iter().all(|row|row["available"]==false),"planner discovery/availability mismatch");
    let directory=browse(&sources,&catalog,&SourceQuery{search:Some("api-market".into()),limit:Some(50),..Default::default()});
    let listed:BTreeSet<_>=directory["sources"].as_array().unwrap().iter().filter_map(|row|row["slug"].as_str()).filter(|slug|expected.contains(slug)).collect();
    anyhow::ensure!(listed==expected,"Sources directory is missing imported business listings");
    let mut named_matches=0;
    for source in &registered {
        let matches=search(&request(&source.name,&source.metadata.description,Some(&source.slug)),&catalog);
        let candidate=matches.iter().flat_map(|m|&m.apiosk).find(|candidate|candidate.source_slug.as_deref()==Some(source.slug.as_str()));
        anyhow::ensure!(candidate.is_some_and(|candidate|candidate.availability=="discovery_only"&&candidate.endpoint_id.is_none()&&candidate.price.is_none()),"source search availability mismatch for {}",source.slug);
        named_matches+=1;
    }
    let mut probes=vec![];
    for (name,description) in [
        ("Supplier payment practices","Average days UK companies take to pay suppliers and share of late payments"),
        ("Trademark risk","Trademark name risk score and phonetic visual similarity by jurisdiction"),
        ("Korea business registration","Check Korean business registration number status against the National Tax Service"),
    ] {
        let matches=search(&request(name,description,None),&catalog);
        let found:Vec<_>=matches.iter().flat_map(|m|&m.apiosk).filter(|c|c.source_slug.as_deref().is_some_and(|slug|expected.contains(slug)))
            .map(|c|json!({"source":c.source_slug,"name":c.name,"availability":c.availability})).collect();
        probes.push(json!({"request":name,"api_market_matches":found}));
    }
    anyhow::ensure!(probes.iter().any(|p|!p["api_market_matches"].as_array().unwrap().is_empty()),"no API.market matches in representative discovery searches");
    println!("{}",serde_json::to_string_pretty(&json!({"read_only":true,"known_business_sources":registered.len(),
        "planner_discovery_resources":planner.len(),"directory_sources":listed.len(),"named_search_matches":named_matches,
        "executable_operations":0,"provider_calls":0,"model_calls":0,"search_probes":probes}))?);
    pool.close().await;
    Ok(())
}
