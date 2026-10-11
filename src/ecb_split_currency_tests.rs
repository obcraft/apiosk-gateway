//! Recorded AI scope, real catalog selection/binding/price proof. No model,
//! source, database or payment request is made by these metadata regressions.
use crate::{capability_intake::{self, Intake}, catalog::{Catalog, Snapshot}, parser::{Intent, Parser}, resolver::{Plan, Resolution}, state::Task};
use serde_json::{json, Value};

fn catalog() -> Catalog {
    let mut snapshot: Snapshot = serde_json::from_str(include_str!("../tests/fixtures/frankfurter/catalog.json")).unwrap();
    let reviewed: Value = serde_json::from_str(include_str!("../tests/fixtures/ecb-split-currency/reviewed-schemas.json")).unwrap();
    for fact in &mut snapshot.vocabulary {
        let row = reviewed["after"].as_array().unwrap().iter().find(|row| row["slug"] == fact.slug).unwrap();
        fact.value_schema = row["value_schema"].clone();
    }
    let catalog = Catalog::build(snapshot);
    assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
    catalog
}

fn make_intake(base: &str, quote: &str, quote_key: &str, observation: Option<&str>, catalog: &Catalog) -> (String, Intake) {
    let question = format!("Get the {}official ECB reference exchange rate for {base}/{quote}, including the observation date, via Frankfurter.", observation.map(|value| format!("{value} ")).unwrap_or("latest ".into()));
    // The actual Luna interpretation split EUR/USD into these supplied values.
    let mut identifiers = json!({"base_currency":base,"rate_type":"official ECB reference"});
    identifiers[quote_key] = json!(quote);
    let mut inputs = vec!["base_currency", quote_key, "rate_type"];
    if let Some(date) = observation { identifiers["observation_date"] = json!(date); inputs.push("observation_date"); }
    let raw = json!({"language":"en","subjects":[{"id":"pair","label":format!("{base}/{quote}"),"type":"currency_pair","identifiers":identifiers}],
        "capabilities":[{"slug":"finance.exchangeRate","name":"Exchange rate","description":"Provide the latest reference exchange rate for a currency pair and its observation date.",
            "domain":"finance","inputs":inputs,"outputs":["exchange_rate","observation_date"],"subject_id":"pair","source_hint":"frankfurter","confidence":0.97}],
        "deliverable":{"format":"chat","operations":[]}});
    let parsed_request = capability_intake::normalize(&raw.to_string(), &question, "recorded-ai-fixture").unwrap();
    let matches = crate::ask_v2::search::search(&parsed_request, catalog);
    let selected_capabilities = matches.iter().flat_map(|found| found.operations.iter().cloned()).collect();
    (question, Intake {parsed_request, matches, selected_capabilities, apiosk_sources_searched:1, catalog_version:catalog.version.clone()})
}

fn resolve(question: &str, intake: Intake, catalog: &Catalog, mut intent: Intent) -> (Task, Intent, Plan) {
    let mut task = Task {capability_intake:Some(intake), ..Default::default()};
    task.record_turn(question, None);
    crate::engine::bind_subjects(&mut task, catalog, &mut intent).unwrap();
    let Resolution::Plan(plan) = crate::resolver::resolve(catalog, &task, &intent) else { panic!("expected exact ECB route") };
    (task, intent, plan)
}

#[tokio::test]
async fn split_currency_metadata_supports_ai_scope_and_rejects_reverse_or_historical_quotes() {
    let catalog = catalog();
    let parser = Parser::new(reqwest::Client::new(), "offline://no-extra-model".into(), String::new(), "fixture-planner".into());
    for (quote_key, reference_source) in [("quote_currency", false), ("target_currency", false), ("quote_currency", true)] {
        let (question, mut intake) = make_intake("EUR", "USD", quote_key, None, &catalog);
        if reference_source {
            intake.parsed_request.subjects[0].identifiers.remove("rate_type");
            intake.parsed_request.subjects[0].identifiers.insert("reference_source".into(), "ECB".into());
            for input in &mut intake.parsed_request.capabilities[0].inputs {
                if input == "rate_type" { *input = "reference_source".into(); }
            }
        }
        let reply = parser.chat_from_capabilities(&question, None, &Task::default(), &catalog, &intake).await
            .expect("reviewed metadata must compile the complete recorded AI scope without another model");
        let (task, intent, plan) = resolve(&question, intake, &catalog, reply.intent.unwrap());
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].service.api_slug, "frankfurter");
        assert_eq!(plan.steps[0].service.path, "/v2/providers/ecb/rate/EUR/{quote}");
        assert_eq!(intent.subjects[0].values["fx.ecb.quote"], json!("USD"));
        assert!(plan.cap_atomic > 0);
        assert!(crate::parser::plan_coverage::uncovered_inputs(&task, &catalog, &intent, &plan).is_empty());
        assert!(crate::parser::plan_coverage::uncovered_outputs(&task, &catalog, &intent, &plan).is_empty());
        for (base, quote, date) in [("USD", "EUR", None), ("EUR", "USD", Some("2024-01-01"))] {
            let (question, invalid) = make_intake(base, quote, quote_key, date, &catalog);
            let mut narrowed = intent.clone();
            narrowed.subjects[0].entity_ref = None;
            narrowed.subjects[0].label = invalid.parsed_request.subjects[0].label.clone();
            narrowed.subjects[0].values.insert("fx.ecb.quote".into(), json!(quote));
            let (task, intent, plan) = resolve(&question, invalid, &catalog, narrowed);
            assert!(!crate::parser::plan_coverage::uncovered_inputs(&task, &catalog, &intent, &plan).is_empty(),
                "a model-authored plan cannot price {base}/{quote} at {date:?} as latest EUR-base data");
        }
    }
}

#[test]
fn reviewed_quote_aliases_keep_literal_values_and_schema_constraints() {
    let catalog = catalog();
    let schema = &catalog.vocabulary["fx.ecb.quote"].value_schema;
    for key in ["quote_currency", "target_currency"] {
        let subject = capability_intake::Subject {id:"s1".into(), label:"exchange rate".into(), kind:"rate".into(),
            identifiers: [(key.to_owned(), "USD".into())].into_iter().collect()};
        assert_eq!(crate::ask_v2::input_values::documented_literal("fx.ecb.quote", schema, &subject), Some(json!("USD")));
    }
    assert!(catalog_core::facts::schema::validate(schema, &json!("USD")).is_ok());
    assert!(catalog_core::facts::schema::validate(schema, &json!("BTC")).is_err());
}

#[test]
fn reviewed_yelp_inputs_keep_company_name_separate_from_city() {
    let reviewed: Value = serde_json::from_str(include_str!("../tests/fixtures/ecb-split-currency/reviewed-schemas.json")).unwrap();
    let subject = capability_intake::Subject {id:"s1".into(),label:"Starbucks in Seattle".into(),kind:"company".into(),
        identifiers:[("company_name".into(),"Starbucks".into()),("city".into(),"Seattle".into())].into_iter().collect()};
    for (field, expected) in [("query","Starbucks"),("location","Seattle")] {
        let slug = format!("source_input.eeddb418e721e589b9ea21083dee98afd.{field}");
        let row = reviewed["after"].as_array().unwrap().iter().find(|row| row["slug"] == slug).unwrap();
        assert_eq!(crate::ask_v2::input_values::documented_literal(&slug, &row["value_schema"], &subject), Some(json!(expected)));
    }
}
