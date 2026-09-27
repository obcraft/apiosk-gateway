//! Opt-in model/discovery regression with a wholly synthetic large catalog.
//! Uses the reported question, but no production catalog, account or source calls.
use apiosk_gateway_v2::{
    capability_intake,
    catalog::{Catalog, Snapshot},
    parser::Parser,
    resolver,
    state::Task,
};
use serde_json::{json, Value};
use uuid::Uuid;

const QUESTION: &str = "Check Coolblue B.V. against current sanctions lists and show any matching records, match scores and source links.";

fn synthetic_catalog() -> Catalog {
    let mut snapshot: Value =
        serde_json::from_str(include_str!("fixtures/integration-catalog.json")).unwrap();
    let template = snapshot["services"][1].clone();
    let mut services = vec![];
    for i in 0..600u128 {
        let mut service = template.clone();
        service["id"] = json!(Uuid::from_u128(i * 2 + 1));
        service["endpoint_id"] = json!(Uuid::from_u128(i * 2 + 2));
        service["api_slug"] = json!(format!("synthetic-{}", i % 37));
        service["name"] = json!(format!("Synthetic source {}", i % 37));
        let (slug, description) = match i {
            0 => ("company.sanctions-screen".to_owned(), "Screen a company name against sanctions lists. Return candidate records, match scores and source links.".to_owned()),
            1 => ("company.sanctions-alternative".to_owned(), "Company watchlist screening by name with matching records, scores and original source links.".to_owned()),
            n if n < 25 => (format!("operation.synthetic-{n}.sanctions-reference-{n:08x}"), "Catalogue of sanctions programs and policy documents by jurisdiction. Returns list descriptions and links; does not screen companies or return matching entity records.".repeat(3)),
            n => (format!("operation.synthetic-{n}.weather-forecast-{n:08x}"), "Weather forecast for a city. Returns temperature and precipitation data.".repeat(3)),
        };
        service["description"] = json!(description);
        service["contract"]["capability_slug"] = json!(slug);
        service["contract"]["requires"] =
            json!([{"fact_type":"company.name","subject_role":"subject","optional":false}]);
        service["contract"]["input_map"] = json!({"company.name":{"name":"name"}});
        services.push(service);
    }
    snapshot["services"] = json!(services);
    snapshot["known_sources"] = json!((0..37).map(|i|json!({"slug":format!("synthetic-{i}"),"name":format!("Synthetic source {i}"),"block_reason":null,"metadata":{"description":"Synthetic catalogue source for model regression tests.".repeat(20)}})).collect::<Vec<_>>());
    Catalog::build(serde_json::from_value::<Snapshot>(snapshot).unwrap())
}

#[test]
fn large_catalog_keeps_screening_candidates_and_compacts_model_context() {
    let catalog = synthetic_catalog();
    assert_eq!(catalog.services.len(), 600);
    assert_eq!(capability_intake::source_count(&catalog), 37);
    let raw = json!({"language":"en","subjects":[{"id":"s1","label":"Coolblue B.V.","type":"company","identifiers":{}}],"capabilities":[{"slug":"compliance.sanctions.screening","name":"Company sanctions screening","description":"Screen companies against sanctions lists and return matching records, scores and source links.","domain":"compliance","inputs":["company_name"],"outputs":["matching_records"],"subject_id":"s1","source_hint":null,"confidence":1}],"deliverable":{"format":"chat","operations":["list"]}});
    let parsed = capability_intake::normalize(&raw.to_string(), QUESTION, "fixture").unwrap();
    let index = capability_intake::search_input(&parsed, &Task::default(), &catalog);
    let compact = capability_intake::search_model_input(&index);
    assert!(compact.to_string().len() < 18_000);
    assert!(compact["candidates"]
        .as_object()
        .unwrap()
        .values()
        .any(|c| c["capability"] == "company.sanctions-screen"));
}

#[tokio::test]
#[ignore = "live configured model and public Bazaar; synthetic metadata only, no purchases"]
async fn exact_coolblue_sanctions_question_reaches_a_supported_plan() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("apiosk_gateway_v2=info")
        .with_test_writer()
        .try_init();
    dotenvy::dotenv().ok();
    let catalog = synthetic_catalog();
    let parser = Parser::new(
        reqwest::Client::new(),
        std::env::var("INTENT_API_URL").unwrap(),
        std::env::var("INTENT_API_KEY").unwrap(),
        std::env::var("INTENT_MODEL").unwrap(),
    );
    let mut task = Task::default();
    let intake = parser
        .prepare_ask(QUESTION, None, &task, &catalog)
        .await
        .unwrap();
    assert!(!intake.matches.is_empty());
    assert!(intake
        .selected_capabilities
        .iter()
        .any(|c| c == "company.sanctions-screen" || c == "company.sanctions-alternative"));
    assert!(
        intake
            .selected_capabilities
            .iter()
            .all(|c| c == "company.sanctions-screen" || c == "company.sanctions-alternative"),
        "policy catalogues are not entity screening: {:?}",
        intake.selected_capabilities
    );
    let reply = parser
        .chat_from_capabilities(QUESTION, None, &task, &catalog, &intake)
        .await
        .unwrap();
    assert!(reply.discover, "{}", reply.message);
    let mut intent = reply.intent.unwrap();
    apiosk_gateway_v2::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
    let resolution = resolver::resolve(&catalog, &task, &intent);
    assert!(
        matches!(resolution, resolver::Resolution::Plan(_)),
        "{resolution:?}"
    );
    println!("Exact reported question parsed, matched and compiled against 600 synthetic operations / 37 sources; no approval or provider execution.");
}
