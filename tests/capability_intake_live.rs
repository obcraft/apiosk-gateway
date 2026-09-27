//! Explicit opt-in: live model + public discovery, using a read-only catalog
//! snapshot. Creates no task, approval, payment, or provider data request.
use apiosk_gateway_v2::{
    capability_intake,
    catalog::{Catalog, Snapshot},
    parser::Parser,
    state::Task,
};
use serde_json::json;

#[tokio::test]
#[ignore = "requires ASK_CATALOG_SNAPSHOT and configured model; public discovery only"]
async fn parse_and_discover_live() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("apiosk_gateway_v2=info")
        .with_test_writer()
        .try_init();
    dotenvy::dotenv().ok();
    let path = std::env::var("ASK_CATALOG_SNAPSHOT").expect("read-only snapshot path");
    let snapshot: Snapshot = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let catalog = Catalog::build(snapshot);
    let parser = Parser::new(
        reqwest::Client::new(),
        std::env::var("INTENT_API_URL").unwrap(),
        std::env::var("INTENT_API_KEY").unwrap(),
        std::env::var("INTENT_MODEL").unwrap(),
    );
    let mut results = vec![];
    for question in [
        "Geef de jaarrekening van Coolblue",
        "Maak een analyse tussen Coolblue en Bol.com",
        "Find public procurement notices about solar panels in the Netherlands",
    ] {
        let start = std::time::Instant::now();
        let result = parser
            .prepare_ask(question, None, &Task::default(), &catalog)
            .await;
        match result {
            Ok(intake) => {
                let planned = parser
                    .chat_from_capabilities(question, None, &Task::default(), &catalog, &intake)
                    .await;
                let plan = match planned {
                    Ok(reply) => {
                        json!({"discover":reply.discover,"message":reply.message,"intent":reply.intent})
                    }
                    Err(error) => json!({"error":error.to_string()}),
                };
                println!("{question}: {} capabilities, {} local matches, {} Coinbase matches, {}ms; plan={}", intake.parsed_request.capabilities.len(), intake.matches.iter().map(|m| m.apiosk.len()).sum::<usize>(), intake.matches.iter().map(|m| m.coinbase.len()).sum::<usize>(), start.elapsed().as_millis(), plan["discover"]);
                results.push(json!({"question":question,"intake":intake,"plan":plan,"elapsed_ms":start.elapsed().as_millis()}));
            }
            Err(error) => {
                println!("{question}: {}", error);
                results.push(json!({"question":question,"error":error.to_string()}));
            }
        }
    }
    let output = std::env::var("ASK_DISCOVERY_REPORT").expect("evidence report path");
    std::fs::write(output, serde_json::to_vec_pretty(&json!({"sources":capability_intake::source_count(&catalog),"accepted_contracts":catalog.services.len(),"results":results})).unwrap()).unwrap();
    assert!(
        results
            .iter()
            .all(|r| r["error"].is_null() && r["plan"]["error"].is_null()),
        "live parsing/discovery/planning failed; inspect evidence report"
    );
    assert!(
        results[0]["intake"]["matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| !m["apiosk"].as_array().unwrap().is_empty()),
        "financial statements need an Apiosk match"
    );
}
