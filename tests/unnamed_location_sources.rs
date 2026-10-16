//! Discovery regression: a user supplies information needs, not API names.
use apiosk_gateway_v2::{
    ask_v2::{search::search, Client, DEFAULT_MODEL},
    capability_intake::ParsedRequest,
    catalog::{Catalog, Snapshot},
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn catalog() -> Catalog {
    let mut services = BTreeMap::new();
    let mut vocabulary = BTreeMap::new();
    for text in [
        include_str!("fixtures/workflows/catalog.json"),
        include_str!("fixtures/dutch-links/catalog.json"),
        include_str!("fixtures/dutch-public-sources/catalog.json"),
    ] {
        let snapshot: Snapshot = serde_json::from_str(text).unwrap();
        for service in snapshot.services {
            services.insert(service.contract.capability_slug.clone(), service);
        }
        for fact in snapshot.vocabulary {
            vocabulary.insert(fact.slug.clone(), fact);
        }
    }
    Catalog::build(Snapshot {
        services: services.into_values().collect(),
        vocabulary: vocabulary.into_values().collect(),
        known_sources: vec![],
    })
}

fn check(row: &Value) {
    let expected_address = row["address"].as_str().unwrap();
    let parsed: ParsedRequest = serde_json::from_value(row["parsed"].clone()).unwrap();
    assert!(parsed.subjects.iter().any(|s| s.label == expected_address), "wrong target: {row}");
    assert!(parsed.subjects.iter().flat_map(|s| s.identifiers.values())
        .all(|v| !v.starts_with("ind ") && !v.ends_with(" in")), "instruction leaked into input: {row}");
    assert!(parsed.capabilities.iter().all(|c| c.source_hint.is_none()), "invented source: {row}");
    let matches = search(&parsed, &catalog());
    let candidates: Vec<_> = matches.iter().flat_map(|m| &m.apiosk).collect();
    assert!(candidates.iter().any(|c| c.capability.as_deref() == Some("government.publication.search")),
        "missing publication search: {row}");
    let location_requested = row["location_requested"].as_bool().unwrap();
    assert_eq!(candidates.iter().any(|c| c.capability.as_deref() == Some("location.property-search")),
        location_requested, "address retrieval must match user scope: {row}");
    assert_eq!(parsed.capabilities.iter().any(|c| c.outputs.iter().any(|o| o == "address_details")), location_requested,
        "address scope was lost or invented: {row}");
}

#[test]
fn recorded_needs_find_sources_without_provider_names_and_preserve_scope() {
    // Captured from the configured model, not hand-built ideal capabilities.
    // Replay normalization and discovery without network or source execution.
    let rows: Vec<Value> = serde_json::from_str(include_str!(
        "fixtures/unnamed-location-sources/recorded-intakes.json"
    )).unwrap();
    for mut row in rows {
        let parsed = apiosk_gateway_v2::capability_intake::normalize(
            &row["parsed"].to_string(), row["question"].as_str().unwrap(), "recorded-fixture",
        ).unwrap();
        row["parsed"] = serde_json::to_value(parsed).unwrap();
        check(&row);
    }
}

#[tokio::test]
#[ignore = "live intent model only; recorded catalog; no source purchases"]
async fn live_unnamed_location_sources() {
    dotenvy::dotenv().ok();
    let _ = tracing_subscriber::fmt().with_env_filter("apiosk_gateway_v2::ask_v2=warn").try_init();
    let key = std::env::var("OPENROUTER_API_KEY")
        .or_else(|_| std::env::var("INTENT_API_KEY")).unwrap();
    let client = Client::new(reqwest::Client::new(), key, DEFAULT_MODEL.into());
    let questions = [
        ("Find Damrak 1, Amsterdam and search publications for that address, with sources.", "Damrak 1, Amsterdam", true),
        ("ind Damrak 1, Amsterdam in and search publications for that address, with sources.", "Damrak 1, Amsterdam", true),
        ("Zoek het adres Damrak 1, Amsterdam en zoek publicaties over dat adres, met bronnen.", "Damrak 1, Amsterdam", true),
        ("Locate Coolsingel 40, Rotterdam and find official publications about that address.", "Coolsingel 40, Rotterdam", true),
        ("Search publications for Damrak 1, Amsterdam, with sources.", "Damrak 1, Amsterdam", false),
        ("Zoek publicaties over Coolsingel 40, Rotterdam, met bronnen.", "Coolsingel 40, Rotterdam", false),
    ];
    let mut rows = vec![];
    for (question, address, location_requested) in questions {
        let result = client.interpret(question, &[]).await.unwrap();
        let row = json!({"question":question,"address":address,"location_requested":location_requested,
            "parsed":result["parsed_request"],"attempts":result["attempts"]});
        println!("{}", serde_json::to_string_pretty(&row).unwrap());
        rows.push(row);
    }
    if let Ok(path) = std::env::var("ASK_TEST_EVIDENCE") {
        std::fs::write(path, serde_json::to_vec_pretty(&rows).unwrap()).unwrap();
    }
    for row in rows { check(&row); }
}
