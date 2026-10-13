//! Opt-in, analysis-only check against a configured model. No executor, database,
//! provider requests, or approvals. Inputs must be supplied explicitly.
use apiosk_gateway_v2::parser::Parser;
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[tokio::test]
#[ignore = "requires model credentials and ANSWER_PRESENTATION_FIXTURE / ANSWER_PRESENTATION_OUTPUT"]
async fn saved_results_choose_the_requested_answer_form() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let fixture = std::env::var("ANSWER_PRESENTATION_FIXTURE")?;
    let output = std::path::PathBuf::from(std::env::var("ANSWER_PRESENTATION_OUTPUT")?);
    let saved: Value = serde_json::from_slice(&std::fs::read(fixture)?)?;
    let parser = Parser::new(
        reqwest::Client::new(),
        std::env::var("INTENT_API_URL")?,
        std::env::var("INTENT_API_KEY")?,
        std::env::var("INTENT_MODEL")?,
    )
    .with_routing(std::env::var("ANALYSIS_MODEL").ok(), None, None)?;
    let example = json!({"result_ref":"fixture","document":{"source":{"name":"Test fixture"},"data":{"items":[{"name":"Item A","price":12,"currency":"EUR","billing_period":"monthly"},{"name":"Item B","price":20,"currency":"EUR","billing_period":"monthly"}]}}});
    let cases = [
        (
            "tenders",
            "Which publications mention charging stations?",
            "table",
            saved.clone(),
        ),
        (
            "closed",
            "How many records were returned in this page? Answer in one sentence.",
            "text",
            saved.clone(),
        ),
        (
            "document",
            "Maak een document met een overzicht van deze publicaties.",
            "document",
            saved,
        ),
        (
            "comparison",
            "Vergelijk de prijzen van Item A en Item B naast elkaar.",
            "comparison",
            example.clone(),
        ),
        (
            "chart",
            "Toon de maandprijzen van Item A en Item B in een grafiek.",
            "chart",
            example,
        ),
    ];
    std::fs::create_dir_all(&output)?;
    for (name, question, expected, source) in cases {
        let answer = parser
            .analyze(&json!([question]), std::slice::from_ref(&source), &BTreeMap::new(), &[])
            .await?;
        std::fs::write(
            output.join(format!("{name}.json")),
            serde_json::to_vec_pretty(
                &json!({"question":question,"analysis":answer,"sources":[source]}),
            )?,
        )?;
        assert_eq!(
            answer["presentation"]["kind"], expected,
            "answer form for {name}"
        );
        if name == "tenders" {
            assert!(!answer["presentation"]["rows"]
                .as_array()
                .unwrap()
                .is_empty());
        }
    }
    // A relevance check, not just a row-count cap: the only matching record is
    // at the end of a large, entirely synthetic source response.
    let source = json!({"result_ref":"synthetic-large","document":{"source":{"name":"Synthetic test data"},"data":{"items":(0..2000).map(|i| json!({"title":format!("Record {i}"),"topic":if i == 1999 {"charging stations"} else {"office supplies"},"url":format!("https://example.com/{i}"),"debug":"omit"})).collect::<Vec<_>>()}}});
    let question =
        "Which records are about charging stations? Show only their title and source link.";
    let answer = parser
        .analyze(&json!([question]), std::slice::from_ref(&source), &BTreeMap::new(), &[])
        .await?;
    std::fs::write(
        output.join("selective-large.json"),
        serde_json::to_vec_pretty(
            &json!({"question":question,"analysis":answer,"input_records":2000}),
        )?,
    )?;
    assert_eq!(answer["presentation"]["kind"], "table");
    let rows = answer["presentation"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "only the relevant record should be selected");
    assert_eq!(
        rows[0].as_array().unwrap().len(),
        2,
        "only requested title and source link"
    );
    assert!(rows[0]
        .as_array()
        .unwrap()
        .iter()
        .all(|cell| cell["pointer"]
            .as_str()
            .unwrap()
            .starts_with("/data/items/1999/")));
    Ok(())
}
