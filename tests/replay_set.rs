//! Replays the versioned replay set (eval/replay/v<N>) offline: production
//! planning, source selection, answer and presentation code, fed the
//! stored source documents and a tape of model responses served here. No
//! source can be called. Semantic outputs are compared with each case's
//! golden; only `equal` passes.
//!
//! REPLAY_TAPE: `strict` (default) answers only a request whose key
//! matches; a miss is reported not comparable and fails only with
//! REPLAY_REQUIRE_COMPARABLE=1. `by-position` replays the n-th response per
//! surface even when a prompt changed. `live` calls INTENT_API_URL with
//! INTENT_API_KEY and INTENT_MODEL (optional ANALYSIS_MODEL) within
//! REPLAY_MAX_MODEL_CALLS and REPLAY_MAX_MODEL_USD; with REPLAY_RECORD=1
//! it writes a new eval/replay/v<N+1> for human review, never overwriting.
//! REPLAY_BLESS=1 fills what a hand-authored case leaves out (a tape
//! entry's key and prompt hash, served by position; a missing golden) and
//! never replaces anything recorded.

use apiosk_gateway_v2::{catalog::Catalog, parser::Parser, replay};
use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Strict,
    ByPosition,
    Live,
}

struct Live {
    url: String,
    key: String,
    max_calls: u64,
    max_nanousd: u64,
}

#[derive(Default)]
struct Tape {
    entries: Vec<Value>,
    cursor: HashMap<String, usize>,
    misses: Vec<String>,
    recorded: Vec<Value>,
    filled: bool,
    calls: u64,
    cost_nanousd: u64,
    unpriced: u64,
}

struct Server {
    mode: Mode,
    bless: bool,
    live: Option<Live>,
    http: reqwest::Client,
    tape: Mutex<Tape>,
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

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn replay_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("eval/replay")
}

fn latest_version() -> u32 {
    std::fs::read_dir(replay_root()).unwrap()
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.strip_prefix('v')?.parse().ok())
        .max()
        .expect("eval/replay has a version")
}

struct Report {
    compared: Vec<String>,
    regressions: Vec<String>,
    not_comparable: Vec<String>,
}

#[tokio::test]
async fn replay_set_matches_its_goldens() {
    let mode = match std::env::var("REPLAY_TAPE").as_deref() {
        Ok("by-position") => Mode::ByPosition,
        Ok("live") => Mode::Live,
        Ok("strict") | Err(_) => Mode::Strict,
        Ok(other) => panic!("unknown REPLAY_TAPE {other}"),
    };
    let version = std::env::var("REPLAY_VERSION").ok().and_then(|v| v.trim_start_matches('v').parse().ok()).unwrap_or_else(latest_version);
    let set = replay_root().join(format!("v{version}"));
    let manifest = read_json(&set.join("manifest.json"));
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
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

    let mut report = Report { compared: vec![], regressions: vec![], not_comparable: vec![] };
    let mut recorded = BTreeMap::new();
    let mut cases: Vec<_> = std::fs::read_dir(set.join("cases")).unwrap().map(|entry| entry.unwrap().path()).collect();
    cases.sort();
    for dir in cases {
        let case: replay::Case = serde_json::from_value(read_json(&dir.join("case.json"))).unwrap();
        let stored: Vec<replay::StoredStep> = serde_json::from_value(read_json(&dir.join("sources.json"))["steps"].clone()).unwrap();
        let catalog_text = std::fs::read_to_string(repo.join(&case.catalog)).unwrap();
        if manifest["catalog_fixtures"][&case.catalog] != replay::sha256(&catalog_text).as_str() {
            report.not_comparable.push(format!("{}: catalog fixture changed", case.id));
            continue;
        }
        let catalog = Catalog::build(serde_json::from_str(&catalog_text).unwrap());
        {
            let mut tape = server.tape.lock().unwrap();
            tape.entries = std::fs::read_to_string(dir.join("tape.jsonl")).unwrap_or_default().lines()
                .filter(|line| !line.trim().is_empty()).map(|line| serde_json::from_str(line).unwrap()).collect();
            tape.cursor.clear();
        }
        let outcome = replay::run(&parser, &catalog, &case, &stored).await;
        let (misses, exchanges, entries, filled) = {
            let mut tape = server.tape.lock().unwrap();
            (std::mem::take(&mut tape.misses), std::mem::take(&mut tape.recorded), std::mem::take(&mut tape.entries), std::mem::take(&mut tape.filled))
        };
        if filled {
            let lines: Vec<String> = entries.iter().map(Value::to_string).collect();
            std::fs::write(dir.join("tape.jsonl"), lines.join("\n") + "\n").unwrap();
        }
        if !misses.is_empty() {
            report.not_comparable.push(format!("{}: {}", case.id, misses.join(", ")));
            continue;
        }
        let actual = replay::normalize(&outcome.unwrap_or_else(|error| panic!("{}: {error}", case.id)));
        recorded.insert(case.id.clone(), (dir.clone(), exchanges, actual.clone()));
        let golden_path = dir.join("golden.json");
        if !golden_path.exists() {
            if bless {
                std::fs::write(&golden_path, serde_json::to_string_pretty(&actual).unwrap() + "\n").unwrap();
            } else {
                report.regressions.push(format!("{}: no golden; review and bless:\n{}", case.id, serde_json::to_string_pretty(&actual).unwrap()));
            }
            continue;
        }
        let verdicts = replay::compare(&read_json(&golden_path), &actual);
        let summary: Vec<_> = verdicts.iter().map(|(section, verdict)| format!("{section}={}", verdict.as_str())).collect();
        report.compared.push(format!("{}: {}", case.id, summary.join(" ")));
        if verdicts.values().any(|verdict| *verdict != replay::Verdict::Equal) {
            report.regressions.push(format!("{}: {}\n{}", case.id, summary.join(" "), serde_json::to_string_pretty(&actual).unwrap()));
        }
    }
    println!("replay v{version}: {} compared, {} not comparable", report.compared.len(), report.not_comparable.len());
    if mode == Mode::Live {
        let tape = server.tape.lock().unwrap();
        println!("live model calls {}, known cost {} USD, unpriced responses {}", tape.calls, tape.cost_nanousd as f64 / 1e9, tape.unpriced);
    }
    for line in report.compared.iter().chain(&report.not_comparable) {
        println!("  {line}");
    }
    if mode == Mode::Live && std::env::var("REPLAY_RECORD").as_deref() == Ok("1") {
        let next = replay_root().join(format!("v{}", latest_version() + 1));
        assert!(!next.exists(), "a recording never overwrites {}", next.display());
        let mut prompt_hashes = BTreeMap::new();
        for (id, (dir, exchanges, actual)) in &recorded {
            let target = next.join("cases").join(id);
            std::fs::create_dir_all(&target).unwrap();
            for file in ["case.json", "sources.json"] {
                std::fs::copy(dir.join(file), target.join(file)).unwrap();
            }
            let tape: Vec<String> = exchanges.iter().map(|entry| {
                prompt_hashes.insert(entry["surface"].as_str().unwrap().to_owned(), entry["prompt_hash"].clone());
                entry.to_string()
            }).collect();
            std::fs::write(target.join("tape.jsonl"), tape.join("\n") + "\n").unwrap();
            std::fs::write(target.join("golden.json"), serde_json::to_string_pretty(actual).unwrap() + "\n").unwrap();
        }
        let mut next_manifest = manifest.clone();
        next_manifest["version"] = json!(latest_version() + 1);
        next_manifest["created_at"] = json!(chrono::Utc::now().to_rfc3339());
        next_manifest["gateway_commit"] = json!(std::env::var("GATEWAY_GIT_SHA").unwrap_or_else(|_| "unknown".into()));
        next_manifest["models"] = json!({"intent": intent_model, "analysis": analysis_model.unwrap_or(intent_model.clone())});
        next_manifest["prompt_hashes"] = json!(prompt_hashes);
        next_manifest["review"] = json!({"status":"recorded","by":null,"at":null});
        std::fs::write(next.join("manifest.json"), serde_json::to_string_pretty(&next_manifest).unwrap() + "\n").unwrap();
        println!("recorded {} for human review", next.display());
        return;
    }
    let comparable_required = std::env::var("REPLAY_REQUIRE_COMPARABLE").as_deref() == Ok("1");
    assert!(report.regressions.is_empty(), "replay regressions:\n{}", report.regressions.join("\n"));
    assert!(!comparable_required || report.not_comparable.is_empty(), "not comparable: {:?}", report.not_comparable);
    assert!(!report.compared.is_empty() || !report.not_comparable.is_empty(), "the replay set is empty");
}
