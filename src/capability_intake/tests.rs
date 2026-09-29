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
    assert!(p.capabilities[0].missing.is_empty());
}

#[test]
fn missing_inputs_follow_webgpu_subject_and_identifier_rules() {
    let mut input = raw();
    input["subjects"][0]["identifiers"] = json!({});
    input["capabilities"][0]["inputs"] = json!(["company_name", "kvk_number", "postcode"]);
    input["capabilities"][0]["missing"] = json!(["model_supplied_garbage"]);
    let parsed = normalize(&input.to_string(), QUESTION, "fixture").unwrap();
    assert_eq!(parsed.capabilities[0].missing, ["kvk_number", "postcode"]);
    input["subjects"] = json!([]);
    let parsed = normalize(&input.to_string(), QUESTION, "fixture").unwrap();
    assert_eq!(parsed.capabilities[0].missing, ["company_name", "kvk_number", "postcode"]);
    input["subjects"] = json!([
        {"id":"original","label":"Coolblue","type":"company","identifiers":{}},
        {"id":"second","label":"KVK","type":"company","identifiers":{"kvk_number":"12345678"}}
    ]);
    assert_eq!(normalize(&input.to_string(), QUESTION, "fixture").unwrap().capabilities[0].missing, ["kvk_number", "postcode"], "another subject's identifier cannot satisfy this one");
    input["capabilities"][0]["subject_id"] = Value::Null;
    assert_eq!(normalize(&input.to_string(), QUESTION, "fixture").unwrap().capabilities[0].missing, ["postcode"]);
}

#[test]
fn clarification_uses_user_literals_but_hashes_the_current_message() {
    let parsed = normalize_with_context(&raw().to_string(), "De jaarrekening", QUESTION, "fixture").unwrap();
    assert_eq!(parsed.subjects[0].label, "Coolblue");
    assert_eq!(parsed.meta.text_sha256, hex::encode(Sha256::digest("De jaarrekening")));
    assert!(normalize(&raw().to_string(), "De jaarrekening", "fixture").is_err());
}

#[test]
fn clarified_analysis_is_a_deliverable_operation_not_an_extra_source() {
    let mut input = raw();
    let analysis = json!({"slug":"general.analysis","name":"Comparative analysis","description":"Compare entities; the aspects to compare are unspecified.","domain":"general","inputs":[],"outputs":["comparison"],"subject_id":null,"source_hint":null,"confidence":0.75});
    input["capabilities"].as_array_mut().unwrap().push(analysis.clone());
    input["deliverable"]["operations"] = json!(["analyze", "compare"]);
    let parsed = normalize(&input.to_string(), QUESTION, "fixture").unwrap();
    assert_eq!(parsed.capabilities.len(), 1);
    assert_eq!(parsed.deliverable.operations, ["analyze", "compare"]);
    assert_eq!(parsed.deliverable.format, "xlsx");
    input["capabilities"] = json!([analysis]);
    assert_eq!(normalize(&input.to_string(), QUESTION, "fixture").unwrap().capabilities[0].slug, "general.analysis", "standalone ambiguity still requires clarification");
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
fn every_grouped_provider_counts_as_one_source() {
    let mut c = catalog();
    let member = |slug: String, provider: &str| KnownSource {
        slug,
        name: "member".into(),
        block_reason: None,
        metadata: crate::readiness::Metadata { provider_slug: Some(provider.into()), ..Default::default() },
    };
    c.known_sources = (0..3).map(|i| member(format!("source-{i}"), "someone")).collect();
    c.known_sources.extend((0..76).map(|i| member(format!("pulse-{i}"), "pulsenetwork")));
    c.known_sources.extend((0..500).map(|i| member(format!("cbz-host-{i}"), "coinbase-bazaar")));
    // three independent sources, Pulse Network and Coinbase Bazaar
    assert_eq!(source_count(&c), 5);
}
#[test]
fn semantic_matching_accepts_only_supplied_operations_and_accounts_for_every_capability() {
    let p = normalize(&raw().to_string(), QUESTION, "model").unwrap();
    let input = search_model_input(&search_input(&p, &Task::default(), &catalog()));
    for response in [
        json!({"matches":[]}),
        json!({"matches":[{"slug":"company.financial.statements","candidate_ids":["invented.operation"]}]}),
        json!({"matches":[{"slug":"unknown","candidate_ids":[]}]}),
    ] {
        assert!(decode_matches(&response.to_string(), &input).is_err());
    }
    assert!(decode_matches(
        &json!({"matches":[{"slug":"company.financial.statements","candidate_ids":[]}]})
            .to_string(),
        &input
    )
    .is_ok());
}

#[test]
fn candidate_ids_resolve_exact_operations_without_exporting_the_full_directory() {
    let slug = "compliance.sanctions.screening";
    let operation = "operation.fixture-screen.get-match-a1b2c3d4";
    let index = json!({"queries":{slug:"Screen companies against sanctions lists."},
        "operations":{operation:[{"source":"fixture-screen","description":"Company sanctions screening with match scores.","inputs":["name"]}]},
        "workflow_presets":[],"sources":[{"slug":"unrelated","description":"This directory description need not enter model context."}]});
    let input = search_model_input(&index);
    assert!(input.get("sources").is_none());
    let raw = json!({"matches":[{"slug":slug,"candidate_ids":["c1"]}]}).to_string();
    assert_eq!(
        decode_matches(&raw, &input).unwrap()[slug],
        BTreeSet::from([operation.to_owned()])
    );
    let schema = search_reply_schema(&input);
    assert_eq!(
        schema.pointer("/properties/matches/items/properties/candidate_ids/items/enum"),
        Some(&json!(["c1"]))
    );
    for id in ["fixture-screen", operation, "c2"] {
        let raw = json!({"matches":[{"slug":slug,"candidate_ids":[id]}]}).to_string();
        assert!(decode_matches(&raw, &input).is_err());
    }
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
    json!({"matches":[{"slug":"company.financial.statements","candidate_ids":["c1"]}]})
}
fn planned() -> Value {
    json!({"message":"Ik zoek de jaarrekening op en toon eerst de prijs.","discover":true,"intent":{"subjects":[{"subject_ref":"s1","values":{"company.name":{"ref":"s1.label"}}}],"goals":[{"capability":"company.accounts","subject":0,"constraints":{}}],"answer_operations":[],"unresolved_references":[]}})
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
    let intent = reply.intent.unwrap();
    assert_eq!(intent.subjects[0].label, intake.parsed_request.subjects[0].label);
    assert_eq!(intent.subjects[0].values["company.name"], intake.parsed_request.subjects[0].label);
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
async fn clarification_answer_keeps_prior_user_subject_and_deliverable() {
    let (parser, _, bazaar, server) = fixture(vec![raw(), matched(),
        json!({"message":"Welke gegevens wil je controleren?","discover":false,"intent":null,"decline_reason":"ambiguous_request"}),
        raw(), matched(), planned()], false).await;
    let c = catalog();
    let mut task = Task::default();
    let intake = parser.prepare_ask_with_bazaar(QUESTION, None, &task, &c, &bazaar).await.unwrap();
    let clarification = parser.chat_from_capabilities(QUESTION, None, &task, &c, &intake).await.unwrap();
    assert!(!clarification.discover);
    assert!(clarification.intent.is_none());
    task.record_turn(QUESTION, Some(clarification.message));
    let followup = "De jaarrekening";
    let intake = parser.prepare_ask_with_bazaar(followup, None, &task, &c, &bazaar).await.unwrap();
    assert_eq!(intake.parsed_request.deliverable.format, "xlsx");
    let reply = parser.chat_from_capabilities(followup, None, &task, &c, &intake).await.unwrap();
    assert!(reply.discover);
    let mut intent = reply.intent.unwrap();
    assert_eq!(intent.subjects[0].label, "Coolblue");
    crate::engine::bind_subjects(&mut task, &c, &mut intent).unwrap();
    assert!(matches!(crate::resolver::resolve(&c, &task, &intent), crate::resolver::Resolution::Plan(_)));
    server.abort();
}

#[tokio::test]
async fn two_companies_cannot_be_reduced_to_one_retrieval() {
    let question = format!("{QUESTION} en Example");
    let mut parsed = raw();
    parsed["subjects"].as_array_mut().unwrap().push(json!({"id":"second","label":"Example","type":"company","identifiers":{}}));
    let mut second = parsed["capabilities"][0].clone();
    second["subject_id"] = json!("second");
    parsed["capabilities"].as_array_mut().unwrap().push(second);
    let mut complete = planned();
    complete["intent"]["subjects"].as_array_mut().unwrap().push(json!({"subject_ref":"s2","values":{"company.name":{"ref":"s2.label"}}}));
    complete["intent"]["goals"].as_array_mut().unwrap().push(json!({"capability":"company.accounts","subject":1,"constraints":{}}));
    let (parser, mock, bazaar, server) = fixture(vec![parsed, matched(), planned(), complete], false).await;
    let c = catalog();
    let task = Task::default();
    let intake = parser.prepare_ask_with_bazaar(&question, None, &task, &c, &bazaar).await.unwrap();
    let reply = parser.chat_from_capabilities(&question, None, &task, &c, &intake).await.unwrap();
    assert_eq!(reply.intent.unwrap().goals.len(), 2);
    assert_eq!(*mock.events.lock().unwrap(), vec!["parse", "search", "bazaar", "plan", "plan"]);
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
    let empty = json!({"matches":[{"slug":"company.financial.statements","candidate_ids":[]}]});
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
