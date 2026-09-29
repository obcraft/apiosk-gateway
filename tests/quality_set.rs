//! Scores the quality set (eval/quality/v<N>) per dimension: each case with
//! a recorded replay runs offline through production code and is scored on
//! entity, source, capability, period, required fields, completeness,
//! evidence, cost, data condition and query class. Scores go to
//! output/eval/; a change fails only when an approved case drops a
//! dimension from 1 to 0 against the human-accepted baseline. Cases without
//! a recorded replay are listed as not run until a live recording exists.

#[path = "support/tape.rs"]
mod tape;

use apiosk_gateway_v2::{
    catalog::Catalog,
    replay::{self, score},
    telemetry::QueryClass,
};
use serde_json::{json, Value};
use std::{collections::{BTreeMap, BTreeSet}, path::Path};
use tape::{read_json, replay_root};

#[tokio::test]
async fn quality_set_scores_every_replayed_case() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let set = repo.join("eval/quality/v1");
    let manifest = read_json(&set.join("manifest.json"));
    let cases: Vec<score::QualityCase> = std::fs::read_to_string(set.join("cases.jsonl")).unwrap().lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{error}: {line}")))
        .collect();
    assert_eq!(cases.len(), manifest["case_count"].as_u64().unwrap() as usize);
    let ids: BTreeSet<_> = cases.iter().map(|case| case.id.as_str()).collect();
    assert_eq!(ids.len(), cases.len(), "case ids are unique");
    let classes: BTreeSet<_> = QueryClass::ALL.iter().map(|class| class.as_str()).collect();
    for case in &cases {
        assert!(classes.contains(case.query_class.as_str()), "{}: query class", case.id);
        assert!(score::CONDITIONS.contains(&case.data_condition.as_str()), "{}: data condition", case.id);
        assert!(case.expect.must_not.iter().all(|rule| score::MUST_NOT.contains(&rule.as_str())), "{}: must_not", case.id);
        match case.review.status.as_str() {
            "draft" => {}
            "approved" => assert!(case.review.by.is_some() && case.review.at.is_some(), "{}: an approval names its reviewer and date", case.id),
            other => panic!("{}: unknown review status {other}", case.id),
        }
    }
    assert!(["nl", "en"].iter().all(|lang| cases.iter().any(|case| case.lang == *lang)), "Dutch and English questions");

    let replay_version = manifest["replay_version"].as_u64().unwrap() as u32;
    assert!(replay_version <= tape::latest_version(), "the quality set names a recorded replay version");
    let replays = replay_root().join(format!("v{replay_version}"));
    let replay_manifest = read_json(&replays.join("manifest.json"));
    let (server, parser, ..) = tape::serve(tape::mode(), &replay_manifest).await;
    let mut scores = BTreeMap::new();
    let mut not_run = Vec::new();
    for case in &cases {
        let Some(dir) = case.replay.as_ref().map(|id| replays.join("cases").join(id)).filter(|dir| dir.exists()) else {
            not_run.push(case.id.clone());
            continue;
        };
        let replayed: replay::Case = serde_json::from_value(read_json(&dir.join("case.json"))).unwrap();
        assert_eq!((replayed.question.as_str(), replayed.query_class.as_str()), (case.question.as_str(), case.query_class.as_str()), "{}: replay case matches", case.id);
        let stored: Vec<replay::StoredStep> = serde_json::from_value(read_json(&dir.join("sources.json"))["steps"].clone()).unwrap();
        let catalog = Catalog::build(serde_json::from_str(&std::fs::read_to_string(repo.join(&replayed.catalog)).unwrap()).unwrap());
        tape::load(&server, &dir);
        let outcome = replay::run(&parser, &catalog, &replayed, &stored).await;
        // A tape miss (a changed prompt or input) fails the model call that
        // needed it: the case was not run, as in replay_set. Only a case whose
        // tape answered every request can fail on its own outcome.
        let (misses, _) = tape::finish(&server, &dir);
        if !misses.is_empty() {
            not_run.push(format!("{} (tape: {})", case.id, misses.join(", ")));
            continue;
        }
        let outcome = outcome.unwrap_or_else(|error| panic!("{}: {error}", case.id));
        scores.insert(case.id.clone(), score::score(case, &outcome));
    }

    let cell = |value: &Option<f64>| value.map_or("-".to_owned(), |value| format!("{value:.2}"));
    let dimensions: Vec<&str> = scores.values().next().map(|dims| dims.keys().copied().collect()).unwrap_or_default();
    println!("quality v{}: {} scored, {} not run", manifest["version"], scores.len(), not_run.len());
    println!("  case {}", dimensions.join(" "));
    for (id, dims) in &scores {
        println!("  {id} {}", dims.values().map(cell).collect::<Vec<_>>().join(" "));
    }
    println!("  not run (no recorded replay): {}", not_run.join(", "));
    let report = json!({"quality_version": manifest["version"], "replay_version": manifest["replay_version"], "scores": scores, "not_run": not_run});
    let output = repo.join("output/eval");
    std::fs::create_dir_all(&output).unwrap();
    std::fs::write(output.join(format!("quality-v{}-{}.json", manifest["version"], chrono::Utc::now().timestamp())), serde_json::to_string_pretty(&report).unwrap()).unwrap();

    for (id, dims) in &scores {
        assert_eq!(dims["evidence"], Some(1.0), "{id}: every cited pointer resolves in its saved document");
        assert_eq!(dims["class"], Some(1.0), "{id}: telemetry::classify agrees with the declared query class");
    }
    let baseline: Value = read_json(&set.join("baseline.json"));
    let regressions = score::regressions(&cases, &baseline, &scores);
    assert!(regressions.is_empty(), "approved cases regressed: {regressions:?}");
}
