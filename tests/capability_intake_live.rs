//! Explicit opt-in: live model + public discovery, using a read-only catalog
//! snapshot. Creates no task, approval, payment, or provider data request.
use apiosk_gateway_v2::{
    capability_intake,
    catalog::{Catalog, Snapshot},
    parser::Parser,
    state::Task,
};
use serde_json::json;

#[test]
#[ignore = "requires a local read-only ASK_CATALOG_SNAPSHOT; no network access"]
fn inspect_catalog_locally() {
    let path = std::env::var("ASK_CATALOG_SNAPSHOT").unwrap();
    let snapshot: Snapshot = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let catalog = Catalog::build(snapshot);
    assert_eq!(capability_intake::source_count(&catalog), 37);
    for (slug, name, description, expected) in [
        (
            "company.financial.statements",
            "Company financial statements",
            "Retrieve annual accounts for a company.",
            "company.financial-statements",
        ),
        (
            "procurement.notices",
            "Public procurement notices",
            "Search public procurement notices by topic and jurisdiction.",
            "procurement.nl-public-contracts",
        ),
        (
            "compliance.sanctions.screening",
            "Company sanctions screening",
            "Screen companies against current sanctions lists and return matching records, match scores and source links.",
            "company.sanctions-screen",
        ),
    ] {
        let raw = json!({"language":"en","subjects":[],"capabilities":[{"slug":slug,"name":name,"description":description,"domain":slug.split('.').next().unwrap(),"inputs":[],"outputs":[],"subject_id":null,"source_hint":null,"confidence":1}],"deliverable":{"format":"chat","operations":[]}});
        let parsed =
            capability_intake::normalize(&raw.to_string(), "synthetic request", "fixture").unwrap();
        let input = capability_intake::search_input(&parsed, &Task::default(), &catalog);
        assert!(
            input["operations"].get(expected).is_some(),
            "missing expected operation {expected}"
        );
        assert_eq!(input["sources"].as_array().unwrap().len(), 112);
        println!("{slug}: scanned {} accepted operations, retained {} operation groups; {} bytes of search context",catalog.services.len(),input["operations"].as_object().unwrap().len(),input.to_string().len());
        assert!(input.to_string().len() < 75_000);
        let compact = capability_intake::search_model_input(&input);
        println!("Model search input: {} bytes", compact.to_string().len());
        assert!(compact.to_string().len() < 20_000);
        assert!(compact["candidates"].as_object().unwrap().values().any(|c|c["capability"]==expected));
    }
}

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
    let mut model_url = std::env::var("INTENT_API_URL").unwrap();
    let mut capture_server = None;
    if std::env::var("ASK_CAPTURE_SYNTHETIC").as_deref() == Ok("1") {
        assert!(
            catalog
                .known_sources
                .iter()
                .all(|s| s.name.starts_with("Synthetic source ")),
            "raw diagnostics require synthetic source metadata"
        );
        let upstream = model_url.clone();
        let key = std::env::var("INTENT_API_KEY").unwrap();
        let app = axum::Router::new().route(
            "/model",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let upstream = upstream.clone();
                let key = key.clone();
                async move {
                    let reply = reqwest::Client::new()
                        .post(upstream)
                        .bearer_auth(key)
                        .json(&body)
                        .send()
                        .await
                        .unwrap();
                    let status = reply.status();
                    let result: serde_json::Value = reply.json().await.unwrap();
                    let input: serde_json::Value =
                        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap())
                            .unwrap();
                    let stage = if input["queries"].is_object() {
                        "search"
                    } else if input["parsed_request"].is_object() {
                        "plan"
                    } else {
                        "parse"
                    };
                    println!(
                        "SYNTHETIC_DIAGNOSTIC {stage}: {}",
                        result
                            .pointer("/choices/0/message/content")
                            .unwrap_or(&serde_json::Value::Null)
                    );
                    (status, axum::Json(result))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        model_url = format!("http://{}/model", listener.local_addr().unwrap());
        capture_server = Some(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap()
        }));
    }
    let parser = Parser::new(
        reqwest::Client::new(),
        model_url,
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
    if let Some(server) = capture_server {
        server.abort();
    }
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
    assert_eq!(
        results[0]["plan"]["discover"], true,
        "a supported request must reach planning"
    );
    assert_eq!(
        results[1]["plan"]["discover"], false,
        "an unspecified comparison needs clarification"
    );
    if catalog.services.len() == 2 && catalog.by_capability.contains_key("company.accounts") {
        assert_eq!(
            results[2]["plan"]["discover"], false,
            "a company-only fixture cannot supply procurement notices"
        );
        assert!(results[2]["intake"]["matches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["apiosk"].as_array().unwrap().is_empty()));
    }
}
