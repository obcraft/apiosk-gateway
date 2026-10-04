//! Re-run synthesis of exported, already-saved sources; never executes a source.
use apiosk_gateway_v2::{
    answer_blocks,
    parser::{self, Parser},
    report_assets,
};
use axum::{routing::post, Json, Router};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Instant,
};

#[allow(dead_code)]
#[path = "../src/completeness.rs"]
mod completeness;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter("apiosk_gateway_v2::parser=info")
        .with_writer(std::io::stderr)
        .init();
    let path = std::env::args().nth(1).expect("saved input JSON path");
    let request_only = std::env::args().any(|arg| arg == "--request-only");
    let out = std::env::var("ANALYSIS_AUDIT_OUT")
        .unwrap_or_else(|_| "/private/tmp/apiosk-analysis-reproduction".into());
    std::fs::create_dir_all(&out)?;
    std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o700))?;
    let input: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let key = std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("INTENT_API_KEY").ok())
        .or_else(|| request_only.then(String::new))
        .expect("model key");
    let url = std::env::var("INTENT_API_URL")
        .unwrap_or_else(|_| "https://openrouter.ai/api/v1/chat/completions".into());
    let http = reqwest::Client::new();
    let counter = Arc::new(AtomicUsize::new(0));
    let saved_out = out.clone();
    let proxy = Router::new().route(
        "/",
        post(move |Json(payload): Json<Value>| {
            let (http, key, url, out, counter) = (
                http.clone(),
                key.clone(),
                url.clone(),
                saved_out.clone(),
                counter.clone(),
            );
            async move {
                let n = counter.fetch_add(1, Ordering::Relaxed) + 1;
                save(
                    format!("{out}/request-{n}.json"),
                    serde_json::to_vec_pretty(&payload).unwrap(),
                )
                .unwrap();
                // Inspect the exact production projection locally before
                // authorizing any outbound model request.
                if request_only {
                    return (reqwest::StatusCode::BAD_REQUEST, [("content-type", "application/json")], b"{\"request_only\":true}".to_vec().into());
                }
                let start = Instant::now();
                let response = http
                    .post(url)
                    .bearer_auth(key)
                    .json(&payload)
                    .send()
                    .await
                    .unwrap();
                let status = response.status();
                let body = response.bytes().await.unwrap();
                save(format!("{out}/response-{n}.json"), &body).unwrap();
                eprintln!(
                    "model attempt {n}: HTTP {status} {}ms",
                    start.elapsed().as_millis()
                );
                (status, [("content-type", "application/json")], body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let serving = tokio::spawn(async move { axum::serve(listener, proxy).await.unwrap() });
    let model = parser::pinned_model("ANALYSIS_MODEL");
    let parser = Parser::new(
        reqwest::Client::new(),
        format!("http://{addr}/"),
        "audit-relay".into(),
        model,
    );
    let mut ops = input["intent"]["answer_operations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    ops.insert(0,json!(format!("Answer this question directly using only the saved source fields, in the same language: {}",input["question"].as_str().unwrap())));
    let intent: parser::Intent = serde_json::from_value(input["intent"].clone())?;
    let reqs = completeness::requirements(&intent);
    let failures: BTreeMap<String, Value> = serde_json::from_value(input["failed_steps"].clone())?;
    let started = Instant::now();
    let result = parser
        .analyze(
            &json!(ops),
            input["sources"].as_array().unwrap(),
            &failures,
            &reqs,
        )
        .await;
    let report = match result {
        Ok(value) => json!({"ok":true,"analysis":value}),
        Err(e) => json!({"ok":false,"error":e.to_string()}),
    };
    save(
        format!("{out}/validated.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!(
        "analysis ok={} elapsed={}ms",
        report["ok"],
        started.elapsed().as_millis()
    );
    serving.abort();
    Ok(())
}

fn save(path: impl AsRef<std::path::Path>, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    let path = path.as_ref();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes.as_ref())
}
