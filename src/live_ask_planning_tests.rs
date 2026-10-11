//! Opt-in production-model verification. Reads catalog metadata and builds
//! ephemeral quotes only: source execution and task persistence are disabled.
use serde_json::{json, Value};

#[tokio::test]
#[ignore = "uses configured model credentials and a read-only production catalog; no source purchases"]
async fn live_questions_use_ai_and_reach_complete_quotes() {
    dotenvy::dotenv().ok();
    let _ = tracing_subscriber::fmt().with_env_filter("warn,sqlx=error").try_init();
    let questions: Vec<String> = serde_json::from_slice(&std::fs::read(
        std::env::var("ASK_PLANNING_QUESTIONS").expect("question JSON file required")
    ).unwrap()).unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1)
        .connect(&std::env::var("DATABASE_URL").unwrap()).await.unwrap();
    let catalog = crate::catalog::CatalogCache::new(pool.clone()).get().await.unwrap();
    pool.close().await;
    let key = std::env::var("OPENROUTER_API_KEY").ok().filter(|s| !s.is_empty())
        .unwrap_or_else(|| std::env::var("INTENT_API_KEY").unwrap());
    let http = reqwest::Client::new();
    let parser = crate::parser::Parser::new(http.clone(), "https://openrouter.ai/api/v1/chat/completions".into(),
        key.clone(), std::env::var("ASK_PLANNING_TEST_MODEL").unwrap_or_else(|_| crate::parser::MODEL.into()))
        .with_ask_v2(crate::ask_v2::Client::new(http.clone(), key, crate::ask_v2::DEFAULT_MODEL.into()));
    let unreachable = sqlx::postgres::PgPoolOptions::new().connect_lazy("postgresql://postgres@127.0.0.1:1/unused").unwrap();
    let store = crate::store::Store { pool: unreachable.clone() };
    let engine = crate::engine::Engine {
        store: store.clone(), catalog: crate::catalog::CatalogCache::new(unreachable), parser,
        signer: crate::state::Signer::new(vec![8;32]).unwrap(), app_url: "http://localhost".into(),
        executor: crate::execution::Executor { store, http, supabase_url: "http://127.0.0.1:1".into(), enabled: false },
    };
    let mut reports = Vec::<Value>::new();
    for question in questions {
        let started = std::time::Instant::now();
        let intake = engine.parser.ask_intake(&question, &crate::state::Task::default(), &catalog, None).await;
        let report = match intake {
            Ok(intake) => {
                let parsed = json!(intake.parsed_request);
                let matches = json!(intake.matches);
                match engine.guest_plan(&question, intake, &catalog).await {
                    Ok(quote) => {
                        use base64::Engine;
                        // Inspect only our ephemeral locally signed quote's
                        // literal inputs; never persist its approval token.
                        let task = quote["preview_token"].as_str().and_then(|token| token.split_once('.'))
                            .and_then(|(payload,_)| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).ok())
                            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                            .map(|preview| preview["task"].clone()).unwrap_or(Value::Null);
                        json!({"question":question,"parsed":parsed,"matches":matches,"status":quote["status"],
                            "sources":quote["sources"],"total_atomic":quote["total_atomic"],"message":quote["message"],
                            "intent":task["intent"],"facts":task["facts"]})
                    },
                    Err(error) => json!({"question":question,"parsed":parsed,"matches":matches,"error":error.to_string()}),
                }
            },
            Err(error) => json!({"question":question,"error":error.to_string()}),
        };
        let mut report = report;
        report["elapsed_ms"] = json!(started.elapsed().as_millis());
        println!("{}: {} ({}ms)", question, report["status"], report["elapsed_ms"]);
        reports.push(report);
        std::fs::write(std::env::var("ASK_PLANNING_EVIDENCE").expect("evidence path required"),
            serde_json::to_vec_pretty(&reports).unwrap()).unwrap();
    }
    assert!(reports.iter().all(|row| row["status"] == "ready"), "Inspect saved evidence for incomplete plans");
}
