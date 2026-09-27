use super::*;
use crate::{
    catalog::{KnownSource, Snapshot},
    parser::Parser,
};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

fn raw() -> Value {
    json!({"language":"NL","subjects":[{"id":"original","label":"Coolblue","type":"Company","identifiers":{"kvk_number":"12345678","vat_number":"invented"}}],
        "capabilities":[{"slug":"company.financial-statements","name":"Company financial statements","description":"Retrieve company financial statements.","domain":"company","inputs":["KVK Number"],"outputs":["financial_statements"],"subject_id":"original","source_hint":"kvk","confidence":75}],
        "deliverable":{"format":"Excel","operations":["export"]}})
}
const QUESTION: &str = "Geef de jaarrekening van Coolblue KVK 12345678 als Excel";
fn catalog() -> Catalog {
    Catalog::build(
        serde_json::from_str::<Snapshot>(include_str!(
            "../../tests/fixtures/integration-catalog.json"
        ))
        .unwrap(),
    )
}

#[test]
fn webgpu_shape_normalizes_literals_slug_aliases_and_adds_app_metadata() {
    let p = normalize(&raw().to_string(), QUESTION, "fixture-model").unwrap();
    assert_eq!(p.language, "nl");
    assert_eq!(p.subjects[0].id, "s1");
    assert_eq!(p.subjects[0].identifiers.len(), 1);
    assert_eq!(p.capabilities[0].slug, "company.financial.statements");
    assert_eq!(p.capabilities[0].subject_id.as_deref(), Some("s1"));
    assert_eq!(p.capabilities[0].confidence, 0.75);
    assert_eq!(p.deliverable.format, "xlsx");
    assert_eq!(
        p.meta.text_sha256,
        hex::encode(Sha256::digest(QUESTION.as_bytes()))
    );
    assert_eq!(p.meta.model, "fixture-model");
    assert!(chrono::DateTime::parse_from_rfc3339(&p.meta.parsed_at).is_ok());
}
#[test]
fn malformed_output_is_not_a_successful_empty_parse() {
    for (field, value) in [
        ("subjects", json!({})),
        ("capabilities", json!([])),
        ("deliverable", json!(null)),
    ] {
        let mut input = raw();
        input[field] = value;
        assert!(normalize(&input.to_string(), QUESTION, "model").is_err());
    }
    let mut input = raw();
    input["capabilities"][0]["inputs"] = json!("kvk_number");
    assert!(normalize(&input.to_string(), QUESTION, "model").is_err());
    assert!(normalize("{\"subjects\":[", QUESTION, "model").is_err());
    let wrapped = format!("```json\n{},\n```", raw());
    assert!(normalize(&wrapped, QUESTION, "model").is_ok());
    assert_eq!(
        extract_json("text {\"a\":\"x,}\",\"b\":[1,],} trailing {\"ignored\":1}").unwrap(),
        json!({"a":"x,}","b":[1]})
    );
}
#[test]
fn query_uses_generic_fields_and_scrubs_subjects_even_if_model_copies_them() {
    let mut p = normalize(&raw().to_string(), QUESTION, "model").unwrap();
    assert_eq!(
        capability_query(&p.capabilities[0], &p, &Task::default()),
        "Company financial statements. Retrieve company financial statements. kvk number. kvk"
    );
    p.capabilities[0].description = "Retrieve COOLBLUE statements for 12345678.".into();
    let q = capability_query(&p.capabilities[0], &p, &Task::default());
    assert!(!q.to_lowercase().contains("coolblue") && !q.contains("12345678"));
    assert!(!q.contains("financial_statements")); // output concepts aren't query fields
    assert!(q.chars().count() <= 300);
}
#[test]
fn greetings_never_search_and_greeting_plus_task_still_parses() {
    let p = normalize("invalid output", "hoe gaat het met jou?", "model").unwrap();
    assert_eq!(p.capabilities[0].slug, "general.conversation");
    assert!(queries(&p, &Task::default()).is_empty());
    assert!(conversation_language("Hoi, geef de jaarrekening van Coolblue").is_none());
}
#[test]
fn all_37_directory_sources_and_pulse_members_are_searchable() {
    let mut c = catalog();
    c.known_sources = (0..36)
        .map(|i| KnownSource {
            slug: format!("source-{i}"),
            name: format!("Source {i}"),
            block_reason: None,
            metadata: Default::default(),
        })
        .collect();
    for i in 0..76 {
        c.known_sources.push(KnownSource {
            slug: format!("pulse-{i}"),
            name: format!("Pulse {i}"),
            block_reason: None,
            metadata: crate::readiness::Metadata {
                provider_slug: Some("pulsenetwork".into()),
                ..Default::default()
            },
        });
    }
    let p = normalize(&raw().to_string(), QUESTION, "model").unwrap();
    let input = search_input(&p, &Task::default(), &c);
    assert_eq!(source_count(&c), 37);
    assert_eq!(input["sources"].as_array().unwrap().len(), 112);
    assert!(input["operations"]["company.accounts"].is_array());
    assert!(!input.to_string().contains("Coolblue"));
}
#[test]
fn semantic_matching_accepts_only_supplied_operations_and_accounts_for_every_capability() {
    let p = normalize(&raw().to_string(), QUESTION, "model").unwrap();
    let input = search_input(&p, &Task::default(), &catalog());
    for response in [
        json!({"matches":[]}),
        json!({"matches":[{"slug":"company.financial.statements","capabilities":["invented.operation"]}]}),
        json!({"matches":[{"slug":"unknown","capabilities":[]}]}),
    ] {
        assert!(decode_matches(&response.to_string(), &input).is_err());
    }
    assert!(decode_matches(
        &json!({"matches":[{"slug":"company.financial.statements","capabilities":[]}]}).to_string(),
        &input
    )
    .is_ok());
}
#[test]
fn bazaar_filters_bad_urls_types_duplicates_and_does_not_compare_different_assets() {
    let results = coinbase_candidates(json!({"resources":[
        {"resource":"javascript:alert(1)"}, {"resource":"https://example.com/socket","type":"websocket"},
        {"resource":"https://user:pass@example.com/private"},
        {"resource":"https://example.com/data","accepts":[{"amount":"1","extra":{"name":"Other"}},{"amount":"3000","extra":{"name":"USD Coin"},"network":"eip155:8453"}]},
        {"resource":"https://example.com/data"}, {"resource":"https://unknown.example/data","accepts":[{"amount":"1","extra":{"name":"Other"}}]}
    ]})).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].price.as_deref(), Some("0.003000 USD"));
    assert_eq!(results[0].availability, "discovery_only");
    assert!(results[0].endpoint_id.is_none());
    assert!(results[1].price.is_none());
}

#[derive(Default)]
struct Mock {
    queue: Mutex<VecDeque<Value>>,
    events: Mutex<Vec<String>>,
    inputs: Mutex<Vec<Value>>,
    bazaar_fail: bool,
}
async fn fixture(
    responses: Vec<Value>,
    fail: bool,
) -> (Parser, Arc<Mock>, String, tokio::task::JoinHandle<()>) {
    let state = Arc::new(Mock {
        queue: Mutex::new(responses.into()),
        bazaar_fail: fail,
        ..Default::default()
    });
    let app = Router::new().route("/model", post(|State(state): State<Arc<Mock>>, Json(body): Json<Value>| async move {
        let input:Value = serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
        let stage = if input["queries"].is_object() { "search" } else if input["parsed_request"].is_object() { "plan" } else { "parse" };
        state.events.lock().unwrap().push(stage.into());
        state.inputs.lock().unwrap().push(input);
        let next = state.queue.lock().unwrap().pop_front().expect("unexpected model call");
        Json(json!({"choices":[{"message":{"content":next.to_string()}}]}))
    })).route("/bazaar", get(|State(state): State<Arc<Mock>>, Query(query): Query<BTreeMap<String,String>>| async move {
        state.events.lock().unwrap().push("bazaar".into());
        assert_eq!(query["limit"], "3");
        assert!(!query["query"].to_lowercase().contains("coolblue"));
        assert!(!query["query"].contains("12345678"));
        (if state.bazaar_fail { StatusCode::SERVICE_UNAVAILABLE } else { StatusCode::OK }, Json(json!({"resources":[{"resource":"https://external.example/accounts","serviceName":"External accounts"}]})))
    })).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        Parser::new(
            reqwest::Client::new(),
            format!("{base}/model"),
            "fixture".into(),
            "fixture".into(),
        ),
        state,
        format!("{base}/bazaar"),
        server,
    )
}
fn matched() -> Value {
    json!({"matches":[{"slug":"company.financial.statements","capabilities":["company.accounts"]}]})
}
fn planned() -> Value {
    json!({"message":"Ik zoek de jaarrekening op en toon eerst de prijs.","discover":true,"intent":{"subjects":[{"entity_ref":null,"label":"Coolblue","values":{"company.name":"Coolblue"}}],"goals":[{"capability":"company.accounts","subject":0,"constraints":{}}],"answer_operations":[],"unresolved_references":[]}})
}

#[tokio::test]
async fn strict_sequence_is_parse_then_apiosk_then_bazaar_then_plan() {
    let (parser, mock, bazaar, server) = fixture(vec![raw(), matched(), planned()], false).await;
    let c = catalog();
    let task = Task::default();
    let intake = parser
        .prepare_ask_with_bazaar(QUESTION, None, &task, &c, &bazaar)
        .await
        .unwrap();
    assert_eq!(
        *mock.events.lock().unwrap(),
        vec!["parse", "search", "bazaar"]
    );
    assert_eq!(intake.matches[0].apiosk[0].origin, "apiosk");
    assert_eq!(intake.matches[0].coinbase[0].origin, "coinbase");
    let reply = parser
        .chat_from_capabilities(QUESTION, None, &task, &c, &intake)
        .await
        .unwrap();
    assert!(reply.discover);
    assert_eq!(
        *mock.events.lock().unwrap(),
        vec!["parse", "search", "bazaar", "plan"]
    );
    assert_eq!(
        mock.inputs.lock().unwrap()[2]["parsed_request"]["meta"]["text_sha256"],
        intake.parsed_request.meta.text_sha256
    );
    assert!(mock.inputs.lock().unwrap()[2]["catalog"]["capabilities"]
        .as_array()
        .unwrap()
        .contains(&json!("company.search"))); // dependency retained
    server.abort();
}
#[tokio::test]
async fn invalid_parse_retries_once_and_never_searches() {
    let (parser, mock, bazaar, server) = fixture(vec![json!({}), json!({})], false).await;
    assert!(parser
        .prepare_ask_with_bazaar(QUESTION, None, &Task::default(), &catalog(), &bazaar)
        .await
        .is_err());
    assert_eq!(*mock.events.lock().unwrap(), vec!["parse", "parse"]);
    server.abort();
}
#[tokio::test]
async fn coinbase_failure_leaves_apiosk_matches_available() {
    let (parser, mock, bazaar, server) = fixture(vec![raw(), matched()], true).await;
    let intake = parser
        .prepare_ask_with_bazaar(QUESTION, None, &Task::default(), &catalog(), &bazaar)
        .await
        .unwrap();
    assert!(!intake.matches[0].apiosk.is_empty());
    assert_eq!(intake.matches[0].coinbase_status, "unavailable");
    assert!(intake.matches[0].coinbase.is_empty());
    assert_eq!(mock.events.lock().unwrap().len(), 3);
    server.abort();
}
#[tokio::test]
async fn greetings_skip_all_source_searches() {
    let (parser, mock, bazaar, server) = fixture(vec![], false).await;
    let intake = parser
        .prepare_ask_with_bazaar("Hoi!", None, &Task::default(), &catalog(), &bazaar)
        .await
        .unwrap();
    assert!(intake.matches.is_empty());
    assert!(mock.events.lock().unwrap().is_empty());
    server.abort();
}
#[tokio::test]
async fn external_only_match_cannot_become_an_apiosk_quote() {
    let empty = json!({"matches":[{"slug":"company.financial.statements","capabilities":[]}]});
    let (parser, _, bazaar, server) =
        fixture(vec![raw(), empty, planned(), planned()], false).await;
    let c = catalog();
    let task = Task::default();
    let intake = parser
        .prepare_ask_with_bazaar(QUESTION, None, &task, &c, &bazaar)
        .await
        .unwrap();
    assert!(!intake.matches[0].coinbase.is_empty());
    assert!(parser
        .chat_from_capabilities(QUESTION, None, &task, &c, &intake)
        .await
        .is_err());
    server.abort();
}

#[tokio::test]
async fn generic_parsed_slug_cannot_replace_the_discovered_operation() {
    let mut wrong = planned();
    wrong["intent"]["goals"][0]["capability"] = json!("company.financial.statements");
    let (parser, mock, bazaar, server) =
        fixture(vec![raw(), matched(), wrong, planned()], false).await;
    let c = catalog();
    let task = Task::default();
    let intake = parser
        .prepare_ask_with_bazaar(QUESTION, None, &task, &c, &bazaar)
        .await
        .unwrap();
    let reply = parser
        .chat_from_capabilities(QUESTION, None, &task, &c, &intake)
        .await
        .unwrap();
    assert_eq!(
        reply.intent.unwrap().goals[0].capability,
        "company.accounts"
    );
    assert_eq!(
        *mock.events.lock().unwrap(),
        vec!["parse", "search", "bazaar", "plan", "plan"]
    );
    server.abort();
}

#[tokio::test]
async fn a_dependency_goal_cannot_silently_replace_the_requested_retrieval() {
    let mut dependency = planned();
    dependency["intent"]["goals"][0]["capability"] = json!("company.search");
    let (parser, _, bazaar, server) = fixture(
        vec![raw(), matched(), dependency.clone(), dependency],
        false,
    )
    .await;
    let c = catalog();
    let task = Task::default();
    let intake = parser
        .prepare_ask_with_bazaar(QUESTION, None, &task, &c, &bazaar)
        .await
        .unwrap();
    assert!(parser
        .chat_from_capabilities(QUESTION, None, &task, &c, &intake)
        .await
        .is_err());
    server.abort();
}

#[test]
fn a_shared_search_verb_does_not_match_unrelated_data() {
    let mut input = raw();
    input["capabilities"][0] = json!({"slug":"procurement.notices","name":"Public procurement notices","description":"Search public procurement notices by topic and jurisdiction.","domain":"procurement","inputs":["topic","jurisdiction"],"outputs":["procurement_notices"],"subject_id":null,"source_hint":null,"confidence":1});
    let parsed = normalize(&input.to_string(), QUESTION, "fixture").unwrap();
    let search = search_input(&parsed, &Task::default(), &catalog());
    assert!(search["operations"].as_object().unwrap().is_empty());
}
