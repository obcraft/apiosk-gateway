//! Opt-in canonical model checks. No source discovery, task, quote, approval,
//! payment, or provider-data request is made by this test.
use apiosk_gateway_v2::ask_v2::{Client, DEFAULT_MODEL};
use serde_json::{json, Value};

#[tokio::test]
#[ignore = "uses configured live model credentials; does not purchase source data"]
async fn company_profile_wording_and_explicit_scope_live() {
    dotenvy::dotenv().ok();
    let key = std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .or_else(|| std::env::var("INTENT_API_KEY").ok())
        .expect("configured model credentials required");
    let client = Client::new(reqwest::Client::new(), key, DEFAULT_MODEL.into());
    let questions = [
        "Kun je het beschikbare bedrijfsprofiel ophalen voor KVK 87560488?",
        "Wat staat er bij de Kamer van Koophandel over nummer 30204462? Toon het bedrijfsprofiel.",
        "Please retrieve the available registered company profile for KVK number 00123456.",
        "Haal het bedrijfsprofiel van KVK 87560488 op en geef ook het e-mailadres en de jaarrekeningen.",
        "Toon de bedrijfsprofielen voor KVK 87560488 en KVK 30204462.",
    ];
    let mut rows = Vec::new();
    for question in questions {
        rows.push(match client.interpret(question, &[]).await {
            Ok(value) => json!({"question": question, "parsed_request": value["parsed_request"],
                "model": value["model"], "attempts": value["attempts"]}),
            Err(_) => json!({"question": question, "error": "canonical interpretation failed"}),
        });
    }
    let profile_checks: Vec<_> = ["87560488", "30204462", "00123456"]
        .iter().enumerate().map(|(index, number)| {
            let parsed = &rows[index]["parsed_request"];
            let subjects = parsed["subjects"].as_array();
            let caps = parsed["capabilities"].as_array();
            subjects.is_some_and(|subjects| subjects.len() == 1
                && subjects[0]["identifiers"]["kvk_number"] == *number)
                && caps.is_some_and(|caps| caps.len() == 1 && caps[0]["outputs"] == json!(["profile"]))
                && parsed["deliverable"]["format"] == "chat"
                && parsed["deliverable"]["operations"] == json!([])
        }).collect();
    let rich_outputs: Vec<&str> = rows[3]["parsed_request"]["capabilities"].as_array()
        .into_iter().flatten().flat_map(|cap| cap["outputs"].as_array().into_iter().flatten())
        .filter_map(Value::as_str).collect();
    let rich_scope_retained = rich_outputs.iter().any(|name| name.contains("email"))
        && rich_outputs.iter().any(|name| name.contains("financial") || name.contains("annual") || name.contains("accounts"));
    let multiple = rows[4]["parsed_request"]["subjects"].as_array();
    let two_identifiers_retained = multiple.is_some_and(|subjects| subjects.len() == 2
        && ["87560488", "30204462"].iter().all(|number| subjects.iter()
            .any(|subject| subject["identifiers"]["kvk_number"] == *number)));
    let passed = profile_checks.iter().all(|passed| *passed) && rich_scope_retained && two_identifiers_retained;
    if let Ok(path) = std::env::var("ASK_V2_LIVE_EVIDENCE") {
        std::fs::write(path, serde_json::to_vec_pretty(&json!({
            "scope": "actual canonical model interpretation only; no source calls or paid end-to-end proof",
            "model": DEFAULT_MODEL, "rows": rows, "checks": {
                "broad_profiles": profile_checks, "explicit_extra_scope_retained": rich_scope_retained,
                "two_identifiers_retained": two_identifiers_retained
            }, "passed": passed
        })).unwrap()).unwrap();
    }
    assert!(passed, "canonical model scope checks failed; inspect sanitized evidence");
}
