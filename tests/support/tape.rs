//! The model tape server shared by the replay and quality runners: it
//! answers the gateway's model requests from a case's recorded tape, or
//! forwards them to a live model under a spending cap and records them.

use apiosk_gateway_v2::{parser::Parser, replay};
use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Strict,
    ByPosition,
    Live,
}

pub struct Live {
    pub url: String,
    pub key: String,
    pub max_calls: u64,
    pub max_nanousd: u64,
}

#[derive(Default)]
pub struct Tape {
    pub entries: Vec<Value>,
    pub cursor: HashMap<String, usize>,
    pub misses: Vec<String>,
    pub recorded: Vec<Value>,
    pub filled: bool,
    pub calls: u64,
    pub cost_nanousd: u64,
    pub unpriced: u64,
}

pub struct Server {
    pub mode: Mode,
    pub bless: bool,
    pub live: Option<Live>,
    pub http: reqwest::Client,
    pub tape: Mutex<Tape>,
}

fn miss(reason: String) -> (StatusCode, Json<Value>) {
    (StatusCode::CONFLICT, Json(json!({"error":{"message":reason}})))
}

async fn model(State(server): State<Arc<Server>>, Json(request): Json<Value>) -> (StatusCode, Json<Value>) {
    let (key, prompt_hash, values) = replay::tape_key(&request);
    let surface = replay::surface_of(&request);
    if let Some(live) = &server.live {
        {
            let tape = server.tape.lock().unwrap();
            if tape.calls >= live.max_calls || tape.cost_nanousd > live.max_nanousd {
                return miss(format!("{surface}: live spending cap reached"));
            }
        }
        let Ok(response) = server.http.post(&live.url).bearer_auth(&live.key).json(&request).send().await else {
            return miss(format!("{surface}: live model unreachable"));
        };
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        let mut tape = server.tape.lock().unwrap();
        tape.calls += 1;
        match body["usage"]["cost"].as_f64() {
            Some(cost) => tape.cost_nanousd += (cost * 1e9).round() as u64,
            None => tape.unpriced += 1,
        }
        if status.is_success() {
            tape.recorded.push(replay::tape_entry(surface, &key, &prompt_hash, &replay::record(&body, &values)));
        }
        return (StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY), Json(body));
    }
    let mut tape = server.tape.lock().unwrap();
    let found = match server.mode {
        Mode::ByPosition => {
            let position = *tape.cursor.get(surface).unwrap_or(&0);
            tape.cursor.insert(surface.to_owned(), position + 1);
            tape.entries.iter().enumerate().filter(|(_, entry)| entry["surface"] == surface).nth(position).map(|(index, _)| index)
        }
        _ => tape.entries.iter().position(|entry| entry["key"] == key.as_str()),
    };
    match found {
        Some(index) => {
            if server.bless && tape.entries[index]["key"].is_null() {
                tape.entries[index]["key"] = json!(key);
                tape.entries[index]["prompt_hash"] = json!(prompt_hash);
                tape.filled = true;
            }
            (StatusCode::OK, Json(replay::restore(&tape.entries[index]["response"], &values)))
        }
        None => {
            let prompt_changed = tape.entries.iter().any(|entry| entry["surface"] == surface && entry["prompt_hash"] != prompt_hash.as_str());
            let reason = format!("{surface}: {}", if prompt_changed { "prompt changed" } else { "input changed" });
            tape.misses.push(reason.clone());
            miss(reason)
        }
    }
}

pub fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

pub fn replay_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("eval/replay")
}

pub fn latest_version() -> u32 {
    std::fs::read_dir(replay_root()).unwrap()
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.strip_prefix('v')?.parse().ok())
        .max()
        .expect("eval/replay has a version")
}

/// The tape mode, from REPLAY_TAPE (`strict` by default).
pub fn mode() -> Mode {
    match std::env::var("REPLAY_TAPE").as_deref() {
        Ok("by-position") => Mode::ByPosition,
        Ok("live") => Mode::Live,
        Ok("strict") | Err(_) => Mode::Strict,
        Ok(other) => panic!("unknown REPLAY_TAPE {other}"),
    }
}

/// Serves model requests on a loopback port and returns a parser pointed
/// at it: the manifest's models on a tape, INTENT_MODEL (and optional
/// ANALYSIS_MODEL) live.
pub async fn serve(mode: Mode, manifest: &Value) -> (Arc<Server>, Parser, String, Option<String>) {
    let live = (mode == Mode::Live).then(|| {
        let required = |key: &str| std::env::var(key).unwrap_or_else(|_| panic!("live replay needs {key}"));
        Live {
            url: required("INTENT_API_URL"),
            key: required("INTENT_API_KEY"),
            max_calls: required("REPLAY_MAX_MODEL_CALLS").parse().unwrap(),
            max_nanousd: (required("REPLAY_MAX_MODEL_USD").parse::<f64>().unwrap() * 1e9) as u64,
        }
    });
    let (intent_model, analysis_model) = match mode {
        Mode::Live => (std::env::var("INTENT_MODEL").unwrap(), std::env::var("ANALYSIS_MODEL").ok()),
        _ => (manifest["models"]["intent"].as_str().unwrap().to_owned(), manifest["models"]["analysis"].as_str().map(str::to_owned)),
    };
    let bless = mode != Mode::Live && std::env::var("REPLAY_BLESS").as_deref() == Ok("1");
    let server = Arc::new(Server { mode, bless, live, http: reqwest::Client::new(), tape: Mutex::new(Tape::default()) });
    let app = Router::new().route("/chat", post(model)).with_state(server.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/chat", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let parser = Parser::new(reqwest::Client::new(), url, "replay".into(), intent_model.clone())
        .with_routing(analysis_model.clone(), None, None).unwrap();
    (server, parser, intent_model, analysis_model)
}

/// Loads one case's tape before running it.
pub fn load(server: &Server, case: &Path) {
    let mut tape = server.tape.lock().unwrap();
    tape.entries = std::fs::read_to_string(case.join("tape.jsonl")).unwrap_or_default().lines()
        .filter(|line| !line.trim().is_empty()).map(|line| serde_json::from_str(line).unwrap()).collect();
    tape.cursor.clear();
}

/// What a finished case left: tape misses and live exchanges. With
/// REPLAY_BLESS, keys filled in by position are written back.
pub fn finish(server: &Server, case: &Path) -> (Vec<String>, Vec<Value>) {
    let mut tape = server.tape.lock().unwrap();
    if std::mem::take(&mut tape.filled) {
        let lines: Vec<String> = tape.entries.iter().map(Value::to_string).collect();
        std::fs::write(case.join("tape.jsonl"), lines.join("\n") + "\n").unwrap();
    }
    (std::mem::take(&mut tape.misses), std::mem::take(&mut tape.recorded))
}
