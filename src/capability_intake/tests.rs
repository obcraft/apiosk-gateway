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
fn source_hint_recovers_only_a_source_literally_named_in_the_question() {
    let mut input = raw();
    input["capabilities"][0]["source_hint"] = json!("companies_house");
    let parsed = normalize(&input.to_string(), &format!("{QUESTION} via Companies House"), "fixture").unwrap();
    assert_eq!(parsed.capabilities[0].source_hint.as_deref(), Some("companies house"));
    assert!(normalize(&input.to_string(), QUESTION, "fixture").unwrap().capabilities[0].source_hint.is_none());
}

#[test]
fn separate_named_sources_survive_same_slug_and_subject_deduplication() {
    let mut input = raw();
    let first = &mut input["capabilities"][0];
    first["slug"] = json!("company.ratings");
    first["source_hint"] = json!("glassdoor");
    first["outputs"] = json!(["rating", "review_count"]);
    let mut second = first.clone();
    second["source_hint"] = json!("yelp");
    input["capabilities"].as_array_mut().unwrap().extend([second.clone(), second]);
    let parsed = normalize(&input.to_string(), "Show Coolblue ratings and review counts from Glassdoor and Yelp", "fixture").unwrap();
    assert_eq!(parsed.capabilities.len(), 2);
    assert_eq!(parsed.capabilities.iter().filter_map(|cap| cap.source_hint.as_deref()).collect::<Vec<_>>(), ["glassdoor", "yelp"]);
}

#[test]
fn missing_inputs_preserve_identifier_dependencies_without_borrowing_other_subjects() {
    let mut input = raw();
    input["subjects"][0]["identifiers"] = json!({});
    input["capabilities"][0]["inputs"] = json!(["company_name", "kvk_number", "postcode"]);
    input["capabilities"][0]["missing"] = json!(["model_supplied_garbage"]);
    let parsed = normalize(&input.to_string(), QUESTION, "fixture").unwrap();
    assert_eq!(parsed.capabilities[0].missing, ["kvk_number", "postcode"]);
    input["subjects"] = json!([]);
    assert!(normalize(&input.to_string(), QUESTION, "fixture").is_err(), "a removed subject cannot leave a dangling reference");
    input["capabilities"][0]["subject_id"] = Value::Null;
    assert!(normalize(&input.to_string(), QUESTION, "fixture").is_err(), "a subjectless name is not supplied");
    input["subjects"] = json!([
        {"id":"original","label":"Coolblue","type":"company","identifiers":{}},
        {"id":"second","label":"KVK","type":"company","identifiers":{"kvk_number":"12345678"}}
    ]);
    input["capabilities"][0]["subject_id"] = json!("original");
    assert_eq!(normalize(&input.to_string(), QUESTION, "fixture").unwrap().capabilities[0].missing, ["kvk_number", "postcode"], "another subject's identifier cannot satisfy this one");
    input["capabilities"][0]["subject_id"] = Value::Null;
    assert!(normalize(&input.to_string(), QUESTION, "fixture").is_err(), "a null link cannot borrow another subject's name");
    input["capabilities"][0]["inputs"] = json!(["kvk_number", "postcode"]);
    assert_eq!(normalize(&input.to_string(), QUESTION, "fixture").unwrap().capabilities[0].missing, ["kvk_number", "postcode"]);
}

#[test]
fn supplied_input_concepts_are_grounded_to_their_own_subject() {
    let question = "Find records for catering in Nederland and transport in Belgium.";
    let mut input = json!({"language":"en","subjects":[
        {"id":"a","label":"catering","type":"topic","identifiers":{"sector":"catering","country":"Nederland"}},
        {"id":"b","label":"transport","type":"topic","identifiers":{"service":"transport","country":"Belgium"}}
    ],"capabilities":[{"slug":"research.records","name":"Matching records","description":"Find matching records.","domain":"research","inputs":["sector","country","topic"],"outputs":["records"],"subject_id":"a","source_hint":null,"confidence":1}],
        "deliverable":{"format":"chat","operations":[]}});
    let parsed = normalize(&input.to_string(),question,"fixture").unwrap();
    assert!(parsed.capabilities[0].missing.is_empty());
    for missing in ["record_type", "service", "period"] {
        input["capabilities"][0]["inputs"] = json!(["sector","country",missing]);
        let error = normalize(&input.to_string(),question,"fixture").unwrap_err().to_string();
        assert!(error.contains("input concepts require supplied identifiers or the literal subject") && error.contains(missing),"{error}");
    }
    input["capabilities"][0]["inputs"] = json!(["sector","country","record_type"]);
    input["subjects"][0]["identifiers"]["record_type"] = json!("records");
    assert!(normalize(&input.to_string(),question,"fixture").unwrap().capabilities[0].missing.is_empty(), "explicit literal type criteria remain valid");
    input["subjects"][0]["identifiers"]["record_type"] = json!("invented");
    assert!(normalize(&input.to_string(),question,"fixture").is_err(), "filtered invented values cannot ground an input");

    let mut company = raw();
    company["capabilities"][0]["inputs"] = json!(["company_name"]);
    assert!(normalize(&company.to_string(),QUESTION,"fixture").unwrap().capabilities[0].missing.is_empty());
    company["capabilities"][0]["inputs"] = json!(["topic"]);
    assert!(normalize(&company.to_string(),QUESTION,"fixture").is_err(), "a company label does not imply an unspecified topic");
}

#[tokio::test]
async fn ungrounded_inputs_get_one_transported_model_correction_and_keep_the_supplied_scope() {
    let question = "Find records for catering in Nederland.";
    let invalid = json!({"language":"en","subjects":[{"id":"s1","label":"catering","type":"topic","identifiers":{"sector":"catering","country":"Nederland"}}],
        "capabilities":[{"slug":"research.records","name":"Matching records","description":"Find matching records.","domain":"research","inputs":["sector","country","record_type"],"outputs":["records"],"subject_id":"s1","source_hint":null,"confidence":1}],
        "deliverable":{"format":"chat","operations":[]}});
    for corrected in [true, false] {
        let mut second = invalid.clone();
        if corrected {second["capabilities"][0]["inputs"] = json!(["sector","country"]);}
        let queue = Arc::new(Mutex::new(VecDeque::from([invalid.clone(),second])));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let capture = calls.clone();
        let app = Router::new().route("/",post(move |Json(body):Json<Value>| {
            capture.lock().unwrap().push(body);
            let next = queue.lock().unwrap().pop_front().expect("at most two model calls");
            async move {
                let event = json!({"choices":[{"delta":{"content":next.to_string()}}]});
                ([(axum::http::header::CONTENT_TYPE,"text/event-stream")],format!("data: {event}\n\ndata: [DONE]\n\n"))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/",listener.local_addr().unwrap());
        let server = tokio::spawn(async move {axum::serve(listener,app).await.unwrap()});
        let client = crate::ask_v2::Client::new(reqwest::Client::new(),"fixture".into(),"fixture".into()).with_local_endpoint(endpoint).unwrap();
        let result = client.interpret(question,&[]).await;
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(),2);
        assert!(calls[1]["messages"][0]["content"].as_str().unwrap().contains("input concepts require supplied identifiers or the literal subject: record_type"));
        if corrected {
            let parsed = &result.unwrap()["parsed_request"];
            assert_eq!(parsed["subjects"][0]["identifiers"],invalid["subjects"][0]["identifiers"]);
            assert_eq!(parsed["capabilities"][0]["outputs"],invalid["capabilities"][0]["outputs"]);
            assert_eq!(parsed["capabilities"][0]["inputs"],json!(["sector","country"]));
        } else {assert!(result.is_err(), "a repeated invalid extraction is never accepted");}
        server.abort();
    }
}

#[test]
fn clarification_uses_user_literals_but_hashes_the_current_message() {
    let parsed = normalize_with_context(&raw().to_string(), "De jaarrekening", QUESTION, "fixture").unwrap();
    assert_eq!(parsed.subjects[0].label, "Coolblue");
    assert_eq!(parsed.meta.text_sha256, hex::encode(Sha256::digest("De jaarrekening")));
    assert!(normalize(&raw().to_string(), "De jaarrekening", "fixture").is_err());
}

#[test]
fn a_dossier_or_report_does_not_invent_a_file_format() {
    let mut input = raw();
    for inferred in ["pdf", "docx", "Word", "xlsx", "Excel", "json"] {
        input["deliverable"]["format"] = json!(inferred);
        for question in ["Maak één bronvermeld dossier over Coolblue", "Make a sourced report on Coolblue", "Geef de jaarrekening van Coolblue als chat"] {
            assert_eq!(normalize(&input.to_string(), question, "fixture").unwrap().deliverable.format, "chat");
        }
    }
    input["subjects"][0]["label"] = json!("ASML");
    input["deliverable"]["format"] = json!("docx");
    let question = "Ik ben M&A-adviseur. Geef Reuters-nieuws over ASML, het GLEIF-concernverband van ASML Holding N.V. en de huidige EV/Revenue en EV/EBITDA van ASML. Maak één bronvermeld dossier en benoem welke onderdelen ontbreken.";
    assert_eq!(normalize(&input.to_string(), question, "fixture").unwrap().deliverable.format, "chat", "live M&A dossier request does not ask for a Word document");
}

#[test]
fn file_formats_require_a_literal_format_in_the_user_context() {
    let mut input = raw();
    for (inferred, requested, expected) in [
        ("pdf", "PDF", "pdf"), ("docx", "Word-document", "docx"),
        ("Word", "dossier.docx", "docx"), ("xlsx", "spreadsheet", "xlsx"),
        ("Excel", "Excel", "xlsx"), ("json", "JSON", "json"),
    ] {
        input["deliverable"]["format"] = json!(inferred);
        let question = format!("Maak een dossier over Coolblue als {requested}");
        assert_eq!(normalize(&input.to_string(), &question, "fixture").unwrap().deliverable.format, expected);
        assert_eq!(normalize_with_context(&input.to_string(), "Met bronvermelding", &question, "fixture").unwrap().deliverable.format, expected);
    }
    input["deliverable"]["format"] = json!("docx");
    assert_eq!(normalize(&input.to_string(), "Research Coolblue keywords", "fixture").unwrap().deliverable.format, "chat", "a substring does not request a Word file");
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
fn discovery_accepts_the_schema_limit_of_sixteen_distinct_checks() {
    let mut input = raw();
    let base = input["capabilities"][0].clone();
    for count in [9, 16, 17] {
        input["capabilities"] = json!((0..count).map(|index| {
            let mut capability = base.clone();
            capability["slug"] = json!(format!("company.check{index}"));
            capability
        }).collect::<Vec<_>>());
        let parsed = normalize(&input.to_string(), QUESTION, "fixture");
        if count <= 16 {
            assert_eq!(parsed.unwrap().capabilities.len(), count);
        } else {
            assert!(parsed.unwrap_err().to_string().contains("one to sixteen capabilities"));
        }
    }
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
    let model_output = json!({"language":"nl","subjects":[],"capabilities":[{
        "slug":"general.conversation","name":"Conversation","description":"Reply to the greeting.",
        "domain":"general","inputs":[],"outputs":["reply"],"subject_id":null,"source_hint":null,"confidence":0.95
    }],"deliverable":{"format":"chat","operations":[]}});
    let p = normalize(&model_output.to_string(), "hoe gaat het met jou?", "model").unwrap();
    assert_eq!(p.capabilities[0].slug, "general.conversation");
    assert!(queries(&p, &Task::default()).is_empty());
    assert!(normalize("invalid output", "hoe gaat het met jou?", "model").is_err());
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
async fn repeated_duplicate_planning_returns_scoped_clarification_without_an_outage_or_rewrite() {
    let mut bad = planned();
    let goal = bad["intent"]["goals"][0].clone();
    bad["intent"]["goals"].as_array_mut().unwrap().push(goal);
    let (parser, mock, bazaar, server) = fixture(vec![raw(), matched(), bad.clone(), bad], false).await;
    let catalog = catalog();
    let task = Task::default();
    let mut intake = parser.prepare_ask_with_bazaar(QUESTION,None,&task,&catalog,&bazaar).await.unwrap();
    // Two checks used the same retrieval in the live tender failure.
    let mut second = intake.parsed_request.capabilities[0].clone();
    second.slug = "company.registration.status".into();
    second.name = "Company registration status".into();
    intake.parsed_request.capabilities.push(second.clone());
    let mut source_match = intake.matches[0].clone();
    source_match.slug = second.slug;
    intake.matches.push(source_match);
    let reply = parser.chat_from_capabilities(QUESTION,None,&task,&catalog,&intake).await.unwrap();
    assert!(!reply.discover && reply.intent.is_none());
    assert!(reply.decline_reason.is_none(), "no third model may rewrite this factual refusal");
    assert!(reply.message.contains("plan is nog niet gereed") && reply.message.contains("Company registration status"));
    assert!(reply.message.contains("aanvullende diensten gevonden"), "discovered alternatives must not disappear behind a rejected plan");
    assert!(!reply.message.contains("tijdelijk") && !reply.message.contains("uitgevallen"));
    assert_eq!(mock.events.lock().unwrap().iter().filter(|event|*event=="plan").count(),2);
    assert!(mock.queue.lock().unwrap().is_empty());
    server.abort();
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
    let mut intent = reply.intent.unwrap();
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
    assert_eq!(mock.inputs.lock().unwrap()[2]["catalog"]["capabilities"], json!(["company.accounts"]));
    assert!(mock.inputs.lock().unwrap()[2]["catalog"]["input_types"].as_array().unwrap().contains(&json!("company.name")));
    let mut planning_task = task.clone();
    crate::engine::bind_subjects(&mut planning_task, &c, &mut intent).unwrap();
    let crate::resolver::Resolution::Plan(plan) = crate::resolver::resolve(&c, &planning_task, &intent) else { panic!("name still resolves through its lookup dependency"); };
    assert!(plan.steps.iter().any(|step| step.service.contract.capability_slug == "company.search"));
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
    // The request explicitly names KVK. Make the synthetic provider match
    // that source so this test exercises occurrence preservation, without
    // relying on discovery ignoring an unmatched source preference.
    let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../tests/fixtures/integration-catalog.json")).unwrap();
    for service in &mut snapshot.services {
        service.api_slug = "kvk".into();
        service.name = format!("KVK {}", service.name);
    }
    let c = Catalog::build(snapshot);
    let task = Task::default();
    let mut intake = parser.prepare_ask_with_bazaar(&question, None, &task, &c, &bazaar).await.unwrap();
    // The App's local discovery preserves occurrences. The legacy directory
    // query map collapses identical slugs and is deliberately not bindable.
    intake.matches = crate::ask_v2::search::search(&intake.parsed_request, &c);
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
    let response = json!({"language":"nl","subjects":[],"capabilities":[{
        "slug":"general.conversation","name":"Conversation","description":"Reply to the greeting.",
        "domain":"general","inputs":[],"outputs":["reply"],"subject_id":null,"source_hint":null,"confidence":0.95
    }],"deliverable":{"format":"chat","operations":[]}});
    let (parser, mock, bazaar, server) = fixture(vec![response], false).await;
    let intake = parser
        .prepare_ask_with_bazaar("Hoi!", None, &Task::default(), &catalog(), &bazaar)
        .await
        .unwrap();
    assert!(intake.matches.is_empty());
    assert_eq!(*mock.events.lock().unwrap(), ["parse"], "the model interprets the greeting, while source searches remain skipped");
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
    let reply=parser.chat_from_capabilities(QUESTION, None, &task, &c, &intake).await.unwrap();
    assert!(!reply.discover && reply.intent.is_none(), "external discovery cannot become an Apiosk quote");
    assert!(reply.message.contains("aanvullende diensten gevonden") || reply.message.contains("Additional services were found"));
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
    let reply = parser
        .chat_from_capabilities(QUESTION, None, &task, &c, &intake)
        .await
        .unwrap();
    assert!(!reply.discover && reply.intent.is_none(), "the unsupported substitution gets a clarification, never a price");
    assert!(reply.decline_reason.is_none());
    server.abort();
}

#[test]
fn a_shared_search_verb_does_not_match_unrelated_data() {
    let mut input = raw();
    input["capabilities"][0] = json!({"slug":"procurement.notices","name":"Public procurement notices","description":"Search public procurement notices by topic and jurisdiction.","domain":"procurement","inputs":[],"outputs":["procurement_notices"],"subject_id":null,"source_hint":null,"confidence":1});
    let parsed = normalize(&input.to_string(), QUESTION, "fixture").unwrap();
    let search = search_input(&parsed, &Task::default(), &catalog());
    assert!(search["operations"].as_object().unwrap().is_empty());
}
