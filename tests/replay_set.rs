//! Replays the versioned replay set (eval/replay/v<N>) offline: production
//! planning, source selection, answer and presentation code, fed the
//! stored source documents and a tape of model responses. No source can be
//! called. Semantic outputs are compared with each case's golden; only
//! `equal` passes.
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

#[path = "support/tape.rs"]
mod tape;

use apiosk_gateway_v2::{catalog::Catalog, replay};
use serde_json::json;
use std::{collections::BTreeMap, path::Path};
use tape::{latest_version, read_json, replay_root, Mode};

#[tokio::test]
async fn replay_set_matches_its_goldens() {
    let mode = tape::mode();
    let version = std::env::var("REPLAY_VERSION").ok().and_then(|v| v.trim_start_matches('v').parse().ok()).unwrap_or_else(latest_version);
    let set = replay_root().join(format!("v{version}"));
    let manifest = read_json(&set.join("manifest.json"));
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let (server, parser, intent_model, analysis_model) = tape::serve(mode, &manifest).await;

    let (mut compared, mut regressions, mut not_comparable, mut blessed) = (vec![], vec![], vec![], vec![]);
    let mut recorded = BTreeMap::new();
    let mut cases: Vec<_> = std::fs::read_dir(set.join("cases")).unwrap().map(|entry| entry.unwrap().path()).collect();
    cases.sort();
    for dir in cases {
        let case: replay::Case = serde_json::from_value(read_json(&dir.join("case.json"))).unwrap();
        let stored: Vec<replay::StoredStep> = serde_json::from_value(read_json(&dir.join("sources.json"))["steps"].clone()).unwrap();
        let catalog_text = std::fs::read_to_string(repo.join(&case.catalog)).unwrap();
        if manifest["catalog_fixtures"][&case.catalog] != replay::sha256(&catalog_text).as_str() {
            not_comparable.push(format!("{}: catalog fixture changed", case.id));
            continue;
        }
        let catalog = Catalog::build(serde_json::from_str(&catalog_text).unwrap());
        tape::load(&server, &dir);
        let outcome = replay::run(&parser, &catalog, &case, &stored).await;
        let (misses, exchanges) = tape::finish(&server, &dir);
        if !misses.is_empty() {
            not_comparable.push(format!("{}: {}", case.id, misses.join(", ")));
            continue;
        }
        let actual = replay::normalize(&outcome.unwrap_or_else(|error| panic!("{}: {error}", case.id)));
        recorded.insert(case.id.clone(), (dir.clone(), exchanges, actual.clone()));
        let golden_path = dir.join("golden.json");
        if !golden_path.exists() {
            if server.bless {
                std::fs::write(&golden_path, serde_json::to_string_pretty(&actual).unwrap() + "\n").unwrap();
                blessed.push(case.id.clone());
            } else {
                regressions.push(format!("{}: no golden; review and bless:\n{}", case.id, serde_json::to_string_pretty(&actual).unwrap()));
            }
            continue;
        }
        let verdicts = replay::compare(&read_json(&golden_path), &actual);
        let summary: Vec<_> = verdicts.iter().map(|(section, verdict)| format!("{section}={}", verdict.as_str())).collect();
        compared.push(format!("{}: {}", case.id, summary.join(" ")));
        if verdicts.values().any(|verdict| *verdict != replay::Verdict::Equal) {
            regressions.push(format!("{}: {}\n{}", case.id, summary.join(" "), serde_json::to_string_pretty(&actual).unwrap()));
        }
    }
    println!("replay v{version}: {} compared, {} not comparable, {} blessed", compared.len(), not_comparable.len(), blessed.len());
    for line in compared.iter().chain(&not_comparable).chain(&blessed) {
        println!("  {line}");
    }
    if mode == Mode::Live {
        let tape = server.tape.lock().unwrap();
        println!("live model calls {}, known cost {} USD, unpriced responses {}", tape.calls, tape.cost_nanousd as f64 / 1e9, tape.unpriced);
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
    assert!(regressions.is_empty(), "replay regressions:\n{}", regressions.join("\n"));
    assert!(!comparable_required || not_comparable.is_empty(), "not comparable: {not_comparable:?}");
    assert!(!compared.is_empty() || !not_comparable.is_empty() || !blessed.is_empty(), "the replay set is empty");
}
