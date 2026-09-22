//! Local comparison UI. Reads catalog contracts; never creates tasks or buys data.
use apiosk_gateway_v2::{
    catalog::{Catalog, Snapshot},
    parser::{Intent, Parser},
    resolver::{resolve, Resolution},
    state::Task,
    typesafe::{Client, Config, Mode, UsageSnapshot},
};
use axum::{
    extract::{DefaultBodyLimit, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use uuid::Uuid;

struct App {
    catalog: Catalog,
    fixture: bool,
    port: u16,
    permits: Semaphore,
}

// Re-read local credentials so adding the TypeSafe key does not require a restart.
fn settings() -> BTreeMap<String, String> {
    let mut values: BTreeMap<_, _> = dotenvy::from_path_iter(".env")
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .collect();
    values.extend(std::env::vars());
    values
}
fn configured(values: &BTreeMap<String, String>, key: &str) -> bool {
    values.get(key).is_some_and(|v| !v.trim().is_empty())
}
fn allowed(headers: &HeaderMap, port: u16) -> bool {
    let hosts = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    let Some(host) = headers.get(header::HOST).and_then(|h| h.to_str().ok()) else {
        return false;
    };
    hosts.iter().any(|allowed| host == allowed)
        && headers.get(header::ORIGIN).is_none_or(|origin| {
            origin
                .to_str()
                .is_ok_and(|origin| origin == format!("http://{host}"))
        })
}
async fn index(State(app): State<Arc<App>>, headers: HeaderMap) -> impl IntoResponse {
    if !allowed(&headers, app.port) {
        return (StatusCode::FORBIDDEN, Html("Forbidden"));
    }
    (StatusCode::OK, Html(include_str!("typesafe-compare.html")))
}
async fn status(State(app): State<Arc<App>>, headers: HeaderMap) -> (StatusCode, Json<Value>) {
    if !allowed(&headers, app.port) {
        return (StatusCode::FORBIDDEN, Json(json!({})));
    }
    let env = settings();
    (
        StatusCode::OK,
        Json(
            json!({"baseline_ready":configured(&env,"INTENT_API_KEY"),"typesafe_ready":configured(&env,"TYPESAFE_API_KEY"),
        "catalog":if app.fixture {"sample catalog"} else {"configured Apiosk catalog"},"capabilities":app.catalog.by_capability.len()}),
        ),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Run {
    question: String,
    method: String,
}

fn plan(intent: &Intent, catalog: &Catalog) -> Value {
    let mut task = Task::default();
    let mut intent = intent.clone();
    if let Err(error) = apiosk_gateway_v2::engine::bind_subjects(&mut task, catalog, &mut intent) {
        return json!({"status":"unsupported","message":error.to_string()});
    }
    match resolve(catalog, &task, &intent) {
        Resolution::Plan(plan) => {
            json!({"status":"ready","source_price_usd":plan.cap_atomic as f64 / 1e6,
            "steps":plan.steps.iter().map(|s|json!({"source":s.service.name,"capability":s.service.contract.capability_slug})).collect::<Vec<_>>()})
        }
        Resolution::Missing { kind, .. } => {
            json!({"status":"needs_input","message":format!("Missing input: {kind}")})
        }
        Resolution::Unsupported(reason) => json!({"status":"unsupported","message":reason}),
    }
}
fn response(intent: &Intent, catalog: &Catalog) -> Value {
    let mut lines = vec![];
    for subject in &intent.subjects {
        lines.push(format!("Subject: {}", subject.label));
    }
    for goal in &intent.goals {
        lines.push(format!("Lookup: {}", goal.capability));
        for (key, value) in &goal.constraints {
            lines.push(format!(
                "{key}: {}",
                value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string())
            ));
        }
    }
    let plan = plan(intent, catalog);
    if let Some(steps) = plan["steps"].as_array() {
        lines.push(String::new());
        for (i, step) in steps.iter().enumerate() {
            lines.push(format!(
                "{}. {} — {}",
                i + 1,
                step["source"].as_str().unwrap_or_default(),
                step["capability"].as_str().unwrap_or_default()
            ));
        }
    } else if let Some(message) = plan["message"].as_str() {
        lines.push(message.to_owned());
    }
    json!({"text":lines.join("\n"),"intent":intent,"plan":plan})
}
async fn run(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(run): Json<Run>,
) -> (StatusCode, Json<Value>) {
    if !allowed(&headers, app.port) {
        return (StatusCode::FORBIDDEN, Json(json!({"error":"Forbidden"})));
    }
    let Ok(_permit) = app.permits.try_acquire() else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error":"A comparison is already running."})),
        );
    };
    let question = run.question.trim();
    if question.is_empty()
        || question.len() > 2000
        || !matches!(run.method.as_str(), "current" | "typesafe")
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"Enter a question of up to 2,000 bytes."})),
        );
    }
    let env = settings();
    let started = Instant::now();
    let (result, usage, model): (Value, UsageSnapshot, String) = if run.method == "typesafe" {
        if !configured(&env, "TYPESAFE_API_KEY") {
            return (
                StatusCode::OK,
                Json(
                    json!({"status":"unavailable","text":"Add TYPESAFE_API_KEY to gateway/.env, then run the comparison again.","elapsed_ms":0}),
                ),
            );
        }
        if let Err(reason) =
            apiosk_gateway_v2::typesafe::intent_request(question, &Task::default(), &app.catalog)
        {
            let reason = match reason.to_string().as_str() {
                "question_span_limit" => "This first TypeSafe parser handles questions of up to twenty-two words. Shorten the question to test it.",
                "capability_limit" | "input_type_limit" | "source_limit" | "catalog_limit" => "This catalog exceeds the current TypeSafe prototype's bounded selection limits.",
                _ => "This request needs the full contextual parser.",
            };
            return (
                StatusCode::OK,
                Json(
                    json!({"status":"not_accepted","text":reason,"elapsed_ms":started.elapsed().as_millis(),"usage":UsageSnapshot::default()}),
                ),
            );
        }
        let mut config = Config::new(env["TYPESAFE_API_KEY"].clone(), Mode::On);
        config.model = env
            .get("TYPESAFE_MODEL")
            .cloned()
            .unwrap_or_else(|| "jev-1.13.0".into());
        // A local experiment allows the full configured request deadline.
        if let Some(ms) = env.get("TYPESAFE_TIMEOUT_MS").and_then(|s| s.parse().ok()) {
            config.timeout = Duration::from_millis(ms);
        }
        let model = config.model.clone();
        let client = match Client::new(config) {
            Ok(client) => client,
            Err(_) => {
                return (
                    StatusCode::OK,
                    Json(
                        json!({"status":"unavailable","text":"Check the server's TypeSafe settings."}),
                    ),
                )
            }
        };
        let result = match client
            .parse_detailed(question, &Task::default(), &app.catalog)
            .await
        {
            Ok(decision) => {
                let mut output = response(&decision.intent, &app.catalog);
                output["status"] = json!("completed");
                output
            }
            Err(error) => {
                let reason = if error.downcast_ref::<reqwest::Error>().is_some() {
                    "TypeSafe connection failed or timed out.".to_owned()
                } else {
                    error.to_string()
                };
                json!({"status":"not_accepted","text":format!("TypeSafe did not accept this interpretation.\n\n{reason}\n\nNo answer from the other model was substituted.")})
            }
        };
        (result, client.usage.snapshot(), model)
    } else {
        if !configured(&env, "INTENT_API_KEY") {
            return (
                StatusCode::OK,
                Json(
                    json!({"status":"unavailable","text":"Configure INTENT_API_KEY in gateway/.env."}),
                ),
            );
        }
        let model = env
            .get("INTENT_MODEL")
            .cloned()
            .unwrap_or_else(|| "google/gemini-3.8-flash".into());
        let parser = Parser::new(
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            env.get("INTENT_API_URL")
                .cloned()
                .unwrap_or_else(|| "https://openrouter.ai/api/v1/chat/completions".into()),
            env["INTENT_API_KEY"].clone(),
            model.clone(),
        )
        .with_routing(
            None,
            Some(
                env.get("INTENT_PROVIDER_SORT")
                    .cloned()
                    .unwrap_or_else(|| "latency".into()),
            ),
            None,
        );
        let parser = match parser {
            Ok(parser) => parser,
            Err(_) => {
                return (
                    StatusCode::OK,
                    Json(
                        json!({"status":"unavailable","text":"Check the current model's provider settings."}),
                    ),
                )
            }
        };
        let result = match parser
            .parse(Uuid::new_v4(), question, &Task::default(), &app.catalog)
            .await
        {
            Ok(intent) => {
                let mut output = response(&intent, &app.catalog);
                output["status"] = json!("completed");
                output
            }
            Err(error) => json!({"status":"unavailable","text":error.to_string()}),
        };
        (result, parser.usage.snapshot(), model)
    };
    let mut result = result;
    result["elapsed_ms"] = json!(started.elapsed().as_millis());
    result["usage"] = json!(usage);
    result["model"] = json!(model);
    (StatusCode::OK, Json(result))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let env = settings();
    let port: u16 = env
        .get("TYPESAFE_COMPARE_PORT")
        .map(String::as_str)
        .unwrap_or("5280")
        .parse()?;
    let (snapshot, fixture): (Snapshot, bool) = if let Some(path) =
        env.get("TYPESAFE_COMPARE_CATALOG")
    {
        (serde_json::from_slice(&std::fs::read(path)?)?, true)
    } else {
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1).acquire_timeout(Duration::from_secs(10))
            .after_connect(|conn, _|Box::pin(async move {
                for statement in ["SET search_path TO public, gateway_v2, kyb, providers, catalog, actions, billing, agents, wallets, research, integrations, observability, support, content, operations, identity, extensions", "SET default_transaction_read_only=on", "SET statement_timeout='10s'"] {
                    sqlx::query(statement).execute(&mut *conn).await?;
                }
                Ok(())
            })).connect(env.get("DATABASE_URL").ok_or_else(||anyhow::anyhow!("Set DATABASE_URL or TYPESAFE_COMPARE_CATALOG"))?).await?;
        let raw: Value = sqlx::query_scalar(apiosk_gateway_v2::catalog::CATALOG_SQL)
            .fetch_one(&pool)
            .await?;
        pool.close().await;
        (serde_json::from_value(raw)?, false)
    };
    let catalog = Catalog::build(snapshot);
    println!(
        "Catalog: {} capabilities, {} services",
        catalog.by_capability.len(),
        catalog.services.len()
    );
    match apiosk_gateway_v2::typesafe::intent_request(
        "What is the address of Mollie B.V.?",
        &Task::default(),
        &catalog,
    ) {
        Ok(request) => println!(
            "TypeSafe request: {} bytes",
            serde_json::to_vec(&request)?.len()
        ),
        Err(reason) => println!("TypeSafe request needs adjustment: {reason}"),
    }
    let app = Arc::new(App {
        catalog,
        fixture,
        port,
        permits: Semaphore::new(2),
    });
    let router = Router::new()
        .route("/", get(index))
        .route("/api/status", get(status))
        .route("/api/run", post(run))
        .layer(DefaultBodyLimit::max(8192))
        .with_state(app);
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
    println!("TypeSafe comparison: http://127.0.0.1:{port}");
    axum::serve(listener, router).await?;
    Ok(())
}
