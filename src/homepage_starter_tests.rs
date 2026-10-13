//! Published homepage examples must reach complete, priced plans; historical
//! starter regressions remain covered after the visible questions are refreshed.
//! Model interpretation uses a local HTTP fixture; intake, discovery, coverage,
//! subject binding and pricing run their production implementations against
//! recorded source contracts. No prepared plan is stubbed and no external model,
//! source, database or payment is contacted.
use crate::{
    catalog::{Catalog, Snapshot},
    parser::Parser,
    resolver::Resolution,
    state::Task,
};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

struct ModelFixture {
    parser: Parser,
    calls: Arc<Mutex<Vec<Value>>>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for ModelFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn model_reply_fixture(question: &str) -> Value {
    // Historical sentence helpers are used only to DECLARE a model-output
    // fixture here, never to bypass the production HTTP interpretation path.
    let parsed =
        crate::capability_intake::literal_request(question, "declared-model-fixture").unwrap();
    let mut output = serde_json::to_value(parsed).unwrap();
    output.as_object_mut().unwrap().remove("meta");
    // The interpretation model returns semantic identifiers, not the old
    // recognizer's duplicate copy of the entire request.
    for subject in output["subjects"].as_array_mut().unwrap() {
        subject["identifiers"]
            .as_object_mut()
            .unwrap()
            .remove("request");
    }
    for capability in output["capabilities"].as_array_mut().unwrap() {
        capability.as_object_mut().unwrap().remove("missing");
    }
    output
}

async fn model_fixture(question: &str, output: Value) -> ModelFixture {
    recorded_model_fixture(question, output, None).await
}

async fn recorded_model_fixture(
    question: &str,
    output: Value,
    planner_reply: Option<Value>,
) -> ModelFixture {
    use axum::response::IntoResponse;
    use axum::{routing::post, Json, Router};
    let question = question.to_owned();
    let has_planner = planner_reply.is_some();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let capture = calls.clone();
    let app = Router::new().route(
        "/",
        post(move |Json(body): Json<Value>| {
            let output = output.clone();
            let question = question.clone();
            let capture = capture.clone();
            let planner_reply = planner_reply.clone();
            async move {
                if body["stream"] != true {
                    let input: Value =
                        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap())
                            .unwrap();
                    assert_eq!(input["question"], question);
                    capture.lock().unwrap().push(body);
                    let reply = planner_reply.expect("no additional model call was declared");
                    return Json(serde_json::json!({
                        "choices":[{"finish_reason":"stop","message":{"content":reply.to_string()}}]
                    }))
                    .into_response();
                }
                assert_eq!(
                    body["messages"][1]["content"], question,
                    "the actual user question must reach the interpretation model"
                );
                assert_eq!(body["stream"], true);
                capture.lock().unwrap().push(body);
                let event =
                    serde_json::json!({"choices":[{"delta":{"content":output.to_string()}}]});
                (
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    format!("data: {event}\n\ndata: [DONE]\n\n"),
                )
                    .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let http = reqwest::Client::new();
    let client = crate::ask_v2::Client::new(
        http.clone(),
        "fixture-key".into(),
        "fixture-interpretation-model".into(),
    )
    .with_local_endpoint(endpoint.clone())
    .unwrap();
    let parser = Parser::new(
        http,
        if has_planner {
            endpoint
        } else {
            "offline://additional-model-calls-forbidden".into()
        },
        if has_planner { "fixture-key".into() } else { String::new() },
        "fixture-planner".into(),
    )
    .with_ask_v2(client);
    ModelFixture {
        parser,
        calls,
        server,
    }
}

async fn planner_engine_fixture(
    output: Value,
) -> (
    crate::engine::Engine,
    Arc<Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    use axum::{routing::post, Json, Router};
    let calls = Arc::new(Mutex::new(Vec::new()));
    let capture = calls.clone();
    let app = Router::new().route("/", post(move |Json(body): Json<Value>| {
        let capture = capture.clone();
        let output = output.clone();
        async move {
            capture.lock().unwrap().push(body);
            Json(serde_json::json!({"choices":[{"finish_reason":"stop","message":{"content":output.to_string()}}]}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let http = reqwest::Client::new();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgresql://postgres@127.0.0.1:1/unused")
        .unwrap();
    let store = crate::store::Store { pool: pool.clone() };
    let engine = crate::engine::Engine {
        store: store.clone(),
        catalog: crate::catalog::CatalogCache::new(pool),
        parser: Parser::new(
            http.clone(),
            endpoint,
            "fixture-key".into(),
            "fixture-planner".into(),
        ),
        signer: crate::state::Signer::new(vec![8; 32]).unwrap(),
        app_url: "http://localhost".into(),
        executor: crate::execution::Executor {
            store,
            http,
            supabase_url: "http://127.0.0.1:1".into(),
            enabled: false,
        },
    };
    (engine, calls, server)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Starter {
    id: String,
    question: String,
    subject: String,
    source_hint: String,
    outputs: Vec<String>,
    provider: String,
    operation: String,
    method: String,
    path: String,
    inputs: Value,
}

fn catalog() -> Catalog {
    // Include competing unrelated operations as well as the expected routes;
    // a one-service fixture could conceal incorrect discovery and selection.
    let mut services = BTreeMap::new();
    let mut vocabulary = BTreeMap::new();
    let mut known_sources = BTreeMap::new();
    for text in [
        include_str!("../tests/fixtures/workflows/catalog.json"),
        include_str!("../tests/fixtures/profession-output-coverage/catalog.json"),
        include_str!("../tests/fixtures/tenderned-browse.json"),
        include_str!("../tests/fixtures/frankfurter/catalog.json"),
        include_str!("../tests/fixtures/company-reputation/catalog.json"),
    ] {
        let snapshot: Snapshot = serde_json::from_str(text).unwrap();
        for service in snapshot.services {
            services.insert(
                (
                    service.api_slug.clone(),
                    service.contract.capability_slug.clone(),
                ),
                service,
            );
        }
        for fact in snapshot.vocabulary {
            vocabulary.insert(fact.slug.clone(), fact);
        }
        for source in snapshot.known_sources {
            known_sources.insert(source.slug.clone(), source);
        }
    }
    // These reviewed production schemas already accompany the saved profession
    // catalog, whose historical opaque payload fields predate their migration.
    let reviewed: Value = serde_json::from_str(include_str!(
        "../tests/fixtures/profession-output-coverage/reviewed-schemas.json"
    ))
    .unwrap();
    for (slug, fact) in &mut vocabulary {
        if let Some(schema) = reviewed["schemas"].get(slug) {
            fact.value_schema = schema.clone();
        }
    }
    let catalog = Catalog::build(Snapshot {
        services: services.into_values().collect(),
        vocabulary: vocabulary.into_values().collect(),
        known_sources: known_sources.into_values().collect(),
    });
    assert!(
        catalog.rejected.is_empty(),
        "rejected recorded contracts: {:?}",
        catalog.rejected
    );
    catalog
}

async fn complete_plan(id: &str) {
    let starters: Vec<Starter> = serde_json::from_str(include_str!(
        "../tests/fixtures/homepage-starters-historical.json"
    ))
    .unwrap();
    assert_eq!(
        starters.len(),
        6,
        "keep all six historical examples as planning regressions"
    );
    let case = starters.iter().find(|case| case.id == id).unwrap();
    let catalog = catalog();
    let model = model_fixture(&case.question, model_reply_fixture(&case.question)).await;
    let parser = &model.parser;
    let task = Task::default();
    let intake = parser
        .ask_intake(&case.question, &task, &catalog, None)
        .await
        .unwrap();
    let requested = &intake.parsed_request;
    assert_eq!(
        model.calls.lock().unwrap().len(),
        1,
        "every historical example must be interpreted by the model"
    );
    assert_eq!(requested.meta.model, "fixture-interpretation-model");
    assert_eq!(
        requested.meta.text_sha256,
        hex::encode(Sha256::digest(case.question.as_bytes()))
    );
    assert_eq!(requested.language, "en");
    assert_eq!(
        requested.subjects.len(),
        1,
        "{id}: no dropped or invented subject"
    );
    assert_eq!(requested.subjects[0].label, case.subject);
    assert_eq!(
        requested.capabilities.len(),
        1,
        "{id}: preserve the complete retrieval scope"
    );
    let capability = &requested.capabilities[0];
    assert_eq!(
        capability.outputs, case.outputs,
        "{id}: no requested field may disappear before coverage checking"
    );
    assert_eq!(
        capability.source_hint.as_deref(),
        Some(case.source_hint.as_str())
    );
    assert!(
        capability.missing.is_empty(),
        "{id}: the historical example supplies its required inputs"
    );
    assert_eq!(
        capability.subject_id.as_deref(),
        Some(requested.subjects[0].id.as_str())
    );
    assert_eq!(requested.deliverable.format, "chat");
    if id == "tenderned" {
        assert_eq!(
            requested.subjects[0].identifiers["country"],
            "the Netherlands"
        );
    }
    assert!(
        intake.selected_capabilities.contains(&case.operation),
        "{id}: expected executable operation must be discovered"
    );

    let reply = parser
        .chat_from_capabilities(&case.question, None, &task, &catalog, &intake)
        .await
        .unwrap_or_else(|error| panic!("{id}: {error}; semantic intake: {intake:?}"));
    assert!(reply.discover, "{id}: {}", reply.message);
    let mut intent = reply
        .intent
        .expect("a priced plan requires complete intent");
    assert!(
        intent.unresolved_references.is_empty(),
        "{id}: no unresolved subject"
    );
    assert_eq!(intent.subjects.len(), 1);
    assert_eq!(intent.subjects[0].label, case.subject);
    assert_eq!(
        intent.goals.len(),
        1,
        "{id}: exactly one requested source operation"
    );
    assert_eq!(intent.goals[0].capability, case.operation);
    assert_eq!(intent.goals[0].subject, 0);
    assert_eq!(intent.goals[0].constraints["source"], case.provider);

    let mut task = Task {
        capability_intake: Some(intake),
        ..task
    };
    crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
    let resolution = crate::resolver::resolve(&catalog, &task, &intent);
    let Resolution::Plan(plan) = resolution else {
        panic!("{id}: expected complete plan, got {resolution:?}")
    };
    assert_eq!(
        plan.steps.len(),
        1,
        "{id}: no unrelated or duplicate purchase"
    );
    assert!(
        !plan.quote_ref.is_nil(),
        "{id}: the plan must have a reviewable quote"
    );
    assert!(
        plan.cap_atomic > 0,
        "{id}: ordinary price and approval remain required"
    );
    assert_eq!(
        plan.cap_atomic,
        plan.steps
            .iter()
            .map(|step| plan.step_total(step))
            .sum::<i64>()
    );
    let step = &plan.steps[0];
    assert_eq!(step.service.api_slug, case.provider);
    assert_eq!(step.service.contract.capability_slug, case.operation);
    assert_eq!(step.service.method, case.method);
    assert_eq!(step.service.path, case.path);
    assert_eq!(Some(step.entity_ref), intent.subjects[0].entity_ref);
    assert_eq!(
        step.service
            .inputs(&task, step.entity_ref, &catalog.vocabulary)
            .unwrap(),
        case.inputs,
        "{id}: literal inputs and reviewed adapter parameters must be bound before approval"
    );
    assert_eq!(
        task.capability_intake
            .as_ref()
            .unwrap()
            .parsed_request
            .capabilities[0]
            .outputs,
        case.outputs,
        "{id}: the reviewed plan must retain every requested field"
    );
}

#[tokio::test]
async fn kvk_registered_company_is_priced() {
    complete_plan("kvk").await;
}
#[tokio::test]
async fn tenderned_cleaning_search_is_priced() {
    complete_plan("tenderned").await;
}
#[tokio::test]
async fn crunchbase_description_and_link_are_priced() {
    complete_plan("crunchbase").await;
}
#[tokio::test]
async fn glassdoor_rating_and_review_count_are_priced() {
    complete_plan("glassdoor").await;
}
#[tokio::test]
async fn vies_validity_and_check_date_are_priced() {
    complete_plan("vies").await;
}
#[tokio::test]
async fn ecb_rate_and_observation_date_are_priced() {
    complete_plan("ecb").await;
}

#[tokio::test]
async fn model_returned_extra_requirements_are_preserved_for_every_homepage_example() {
    let catalog = catalog();
    let starters: Vec<Starter> = serde_json::from_str(include_str!(
        "../tests/fixtures/homepage-starters-historical.json"
    ))
    .unwrap();
    for case in starters {
        let question = format!(
            "{} Also include a credit-risk assessment and a PDF report.",
            case.question
        );
        let mut output = model_reply_fixture(&case.question);
        output["capabilities"][0]["outputs"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("credit_risk_score"));
        output["deliverable"]["format"] = serde_json::json!("pdf");
        let model = model_fixture(&question, output).await;
        let intake = model
            .parser
            .ask_intake(&question, &Task::default(), &catalog, None)
            .await
            .unwrap();
        assert_eq!(model.calls.lock().unwrap().len(), 1);
        assert!(intake.parsed_request.capabilities[0]
            .outputs
            .iter()
            .any(|field| field == "credit_risk_score"));
        assert_eq!(intake.parsed_request.deliverable.format, "pdf");
        let reply = model
            .parser
            .chat_from_capabilities(&question, None, &Task::default(), &catalog, &intake)
            .await;
        assert!(reply.as_ref().map_or(true, |reply| !reply.discover && reply.intent.is_none()),
            "{}: model-returned extra requirements cannot be discarded to reuse a narrower starter plan: {reply:?}", case.id);
    }
}

#[tokio::test]
async fn stale_or_changed_named_source_scope_cannot_shortcut_to_a_narrower_plan() {
    let catalog = catalog();
    let starters: Vec<Starter> = serde_json::from_str(include_str!(
        "../tests/fixtures/homepage-starters-historical.json"
    ))
    .unwrap();
    for case in starters
        .iter()
        .filter(|case| matches!(case.id.as_str(), "crunchbase" | "glassdoor"))
    {
        for change in ["subject", "outputs", "question_hash"] {
            let model = model_fixture(&case.question, model_reply_fixture(&case.question)).await;
            let parser = &model.parser;
            let task = Task::default();
            let mut intake = parser
                .ask_intake(&case.question, &task, &catalog, None)
                .await
                .unwrap();
            match change {
                "subject" => intake.parsed_request.subjects[0].label = "Another company".into(),
                "outputs" => intake.parsed_request.capabilities[0]
                    .outputs
                    .push("credit_risk_score".into()),
                "question_hash" => intake.parsed_request.meta.text_sha256 = "old-question".into(),
                _ => unreachable!(),
            }
            let reply = parser
                .chat_from_capabilities(&case.question, None, &task, &catalog, &intake)
                .await;
            assert!(reply.as_ref().map_or(true, |reply| !reply.discover && reply.intent.is_none()),
                "{} / {change}: stale or broader scope must not silently become the original one-source plan: {reply:?}", case.id);
        }
    }
}

#[tokio::test]
async fn source_search_location_filters_cannot_be_discarded_after_model_interpretation() {
    let catalog = catalog();
    for question in [
        "Search Crunchbase for Starbucks in Seattle and show its company description and profile link.",
        "Search Glassdoor for Starbucks in Seattle and show its employer rating and review count.",
    ] {
        let model = model_fixture(question, model_reply_fixture(question)).await;
        let intake = model.parser.ask_intake(question, &Task::default(), &catalog, None).await.unwrap();
        assert_eq!(model.calls.lock().unwrap().len(), 1);
        assert_eq!(intake.parsed_request.subjects[0].identifiers["location"], "Seattle");
        let reply = model.parser.chat_from_capabilities(question, None, &Task::default(), &catalog, &intake).await;
        assert!(reply.as_ref().map_or(true, |reply| !reply.discover && reply.intent.is_none()),
            "a name-only source contract cannot silently discard the model-extracted location: {reply:?}");
    }
}

#[tokio::test]
async fn contract_planner_cannot_quote_a_plan_that_discards_a_parsed_location() {
    let question = "Search Crunchbase for Starbucks in Seattle and show its company description and profile link.";
    let catalog = catalog();
    let interpretation = model_fixture(question, model_reply_fixture(question)).await;
    let intake = interpretation
        .parser
        .ask_intake(question, &Task::default(), &catalog, None)
        .await
        .unwrap();
    assert_eq!(
        intake.parsed_request.subjects[0].identifiers["location"],
        "Seattle"
    );
    let subject = &intake.parsed_request.subjects[0].id;
    let raw = serde_json::json!({"message":"I will retrieve the requested company details.","discover":true,"decline_reason":"none",
        "intent":{"subjects":[{"subject_ref":subject,"values":{"company.crunchbase_query":{"ref":format!("{subject}.label")}}}],
            "goals":[{"subject":0,"capability":"company.crunchbase-search","constraints":{}}],
            "answer_operations":[],"unresolved_references":[]}});
    let (engine, calls, server) = planner_engine_fixture(raw).await;
    let quote = engine.guest_plan(question, intake, &catalog).await.unwrap();
    server.abort();
    assert_eq!(
        calls.lock().unwrap().len(),
        0,
        "the planner must not ask a model to reinterpret the filters"
    );
    assert_ne!(
        quote["status"], "ready",
        "source output coverage cannot excuse a discarded location"
    );
    assert!(quote["total_atomic"].is_null());
    assert!(
        quote["preview_token"].is_null(),
        "no payable quote may be minted from the incomplete plan"
    );
}

#[tokio::test]
async fn dynamic_compiler_prices_both_company_subjects_with_their_own_bound_names() {
    let question =
        "Find the company descriptions and profile links for Starbucks and Mollie on Crunchbase.";
    let catalog = catalog();
    let output = serde_json::json!({"language":"en",
        "subjects":[{"id":"coffee","label":"Starbucks","type":"company","identifiers":{"company_name":"Starbucks"}},
            {"id":"payments","label":"Mollie","type":"company","identifiers":{"company_name":"Mollie"}}],
        "capabilities":(["coffee","payments"].map(|subject| serde_json::json!({"slug":"company.description","name":"Company description and profile link",
            "description":"Search the named company and retrieve its description and profile URL.","domain":"company","inputs":["company_name"],
            "outputs":["description","profile_url"],"subject_id":subject,"source_hint":"crunchbase","confidence":1}))),
        "deliverable":{"format":"chat","operations":[]}});
    let interpretation = model_fixture(question, output).await;
    let intake = interpretation
        .parser
        .ask_intake(question, &Task::default(), &catalog, None)
        .await
        .unwrap();
    assert_eq!(
        interpretation.calls.lock().unwrap().len(),
        1,
        "AI interpretation still owns both requirements"
    );
    let ids: Vec<_> = intake
        .parsed_request
        .subjects
        .iter()
        .map(|subject| subject.id.as_str())
        .collect();
    let raw = serde_json::json!({"message":"I will show the total for both company searches.","discover":true,"decline_reason":"none",
        "intent":{"subjects":(ids.iter().map(|subject| serde_json::json!({"subject_ref":subject,
            "values":{"company.crunchbase_query":{"ref":format!("{subject}.identifiers.company_name")}}})).collect::<Vec<_>>()),
            "goals":([0,1].map(|subject| serde_json::json!({"subject":subject,"capability":"company.crunchbase-search","constraints":{}}))),
            "answer_operations":[],"unresolved_references":[]}});
    let (engine, calls, server) = planner_engine_fixture(raw).await;
    let quote = engine
        .guest_plan(question, intake, &catalog)
        .await
        .unwrap_or_else(|error| {
            panic!(
                "planner rejected the complete fixture: {error}; requests: {:?}",
                calls.lock().unwrap()
            )
        });
    server.abort();
    assert_eq!(
        calls.lock().unwrap().len(),
        0,
        "complete interpreted requirements compile without a second planning model call"
    );
    assert_eq!(
        quote["status"], "ready",
        "both complete source bindings must remain quotable: {quote}"
    );
    assert_eq!(quote["sources"].as_array().unwrap().len(), 2);
    let total: i64 = quote["total_atomic"].as_str().unwrap().parse().unwrap();
    assert!(total > 0);
    assert_eq!(
        total,
        quote["sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|source| source["price_atomic"]
                .as_str()
                .unwrap()
                .parse::<i64>()
                .unwrap())
            .sum::<i64>()
    );
    let preview = crate::engine::guest_preview::Preview::open(
        &engine.signer,
        quote["preview_token"].as_str().unwrap(),
    )
    .unwrap();
    let saved = serde_json::to_value(preview).unwrap();
    let task: Task = serde_json::from_value(saved["task"].clone()).unwrap();
    let intent: crate::parser::Intent =
        serde_json::from_value(task.intent.clone().unwrap()).unwrap();
    assert_eq!(intent.subjects.len(), 2);
    for name in ["Starbucks", "Mollie"] {
        let subject = intent
            .subjects
            .iter()
            .find(|subject| subject.label == name)
            .unwrap();
        assert_eq!(
            crate::catalog::fact_value(
                &task.facts,
                subject.entity_ref.unwrap(),
                "company.crunchbase_query"
            )
            .unwrap(),
            Some(serde_json::json!(name))
        );
    }
}

fn semantic_name_profile_intake(
    question: &str,
    name: &str,
    slug: &str,
    catalog: &Catalog,
) -> crate::capability_intake::Intake {
    // Model-shaped semantic extraction, deliberately no provider hint and no
    // registration number. The test does not depend on a starter sentence.
    let parsed_request = crate::capability_intake::normalize(&serde_json::json!({
        "language":"nl",
        "subjects":[{"id":"s1","label":name,"type":"company","identifiers":{"company_name":name}}],
        "capabilities":[{"slug":slug,"name":"Company profile","description":"Retrieve the registered profile information for a company.",
            "domain":"company","inputs":["company_name"],"outputs":["profile"],"subject_id":"s1","source_hint":null,"confidence":0.96}],
        "deliverable":{"format":"chat","operations":[]}
    }).to_string(), question, "semantic-profile-fixture").unwrap();
    assert!(
        parsed_request.capabilities[0].source_hint.is_none(),
        "name lookup must not invent a named provider"
    );
    let matches = crate::ask_v2::search::search(&parsed_request, catalog);
    crate::capability_intake::Intake {
        selected_capabilities: matches
            .iter()
            .flat_map(|found| found.operations.iter().cloned())
            .collect(),
        parsed_request,
        matches,
        catalog_version: catalog.version.clone(),
        apiosk_sources_searched: crate::capability_intake::source_count(catalog),
    }
}

fn profile_catalog_with_unrelated_directory_entries() -> Catalog {
    let original = catalog();
    let mut known_sources = original.known_sources.clone();
    // Simulate a growing directory where "profile" becomes common, without
    // changing a real registry's contracts, response schema, inputs or price.
    // These artist directories have no executable operation and cannot satisfy
    // a request for an official company registration profile.
    for index in 0..24 {
        known_sources.push(
            serde_json::from_value(serde_json::json!({
                "slug":format!("unrelated-profile-directory-{index}"),
                "name":format!("Artist profile directory {index}"),
                "block_reason":null,
                "metadata":{"description":"Artist profile and musician biography information."}
            }))
            .unwrap(),
        );
    }
    let catalog = Catalog::build(Snapshot {
        services: original.services,
        vocabulary: original.vocabulary.into_values().collect(),
        known_sources,
    });
    assert!(catalog.rejected.is_empty());
    catalog
}

#[tokio::test]
async fn explicit_profile_fields_require_the_identifier_before_pricing() {
    let catalog = profile_catalog_with_unrelated_directory_entries();
    for (name, question, slug) in [
        (
            "Mollie B.V.",
            "Show the legal form and SBI codes for Mollie B.V.",
            "company.profile",
        ),
        (
            "Atelier De Blauwe Maan B.V.",
            "Please retrieve the legal form and SBI codes for Atelier De Blauwe Maan B.V.",
            "company.registry.profile",
        ),
        (
            "Coolblue B.V.",
            "Can you find the legal form and SBI codes of Coolblue B.V.?",
            "company.profile",
        ),
    ] {
        let mut intake = semantic_name_profile_intake(question, name, slug, &catalog);
        intake.parsed_request.capabilities[0].description =
            "Retrieve the legal form and SBI codes of a registered company.".into();
        intake.parsed_request.capabilities[0].outputs = vec!["legal_form".into(), "sbi_codes".into()];
        intake.matches = crate::ask_v2::search::search(&intake.parsed_request, &catalog);
        intake.selected_capabilities = intake.matches.iter()
            .flat_map(|found| found.operations.iter().cloned()).collect();
        assert!(intake.matches.iter().flat_map(|found| &found.apiosk).any(|candidate|
            candidate.source_slug.as_deref() == Some("kvk-dutch-business-register")
                && candidate.capability.as_deref() == Some("company.registry-profile")),
            "{question}: semantic company profile discovery must retain the registry profile even without a source hint; {:?}", intake.matches);
        let parser = Parser::new(
            reqwest::Client::new(),
            "offline://model-calls-forbidden".into(),
            String::new(),
            "semantic-profile-fixture".into(),
        );
        let mut task = Task {
            capability_intake: Some(intake.clone()),
            ..Default::default()
        };
        let reply = parser
            .chat_from_capabilities(question, None, &task, &catalog, &intake)
            .await
            .unwrap();
        assert!(reply.discover, "{question}: {}", reply.message);
        let mut intent = reply
            .intent
            .expect("semantic registered profile must retain its retrieval intent");
        assert_eq!(intent.subjects.len(), 1);
        assert_eq!(
            intent.subjects[0].values.get("company.name"),
            Some(&serde_json::json!(name))
        );
        assert!(
            !intent.subjects[0]
                .values
                .contains_key("company_registry.kvknummer"),
            "a company name does not authorize inventing its registration number"
        );
        crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        let resolution = crate::resolver::resolve(&catalog, &task, &intent);
        assert!(matches!(resolution, Resolution::Missing { kind, .. } if kind == "company_registry.kvknummer"),
            "a profile cannot be priced while its identifier is missing: {}", serde_json::to_string(&intent).unwrap());
        assert_eq!(intent.goals.len(), 1, "no implicit paid name-search dependency");
        assert!(task.independent_sources());
        assert!(task.facts.iter().all(|fact| fact.semantic_type != "company_registry.kvknummer"));

    }
}

#[test]
fn semantic_name_only_profile_intake_does_not_pin_other_countries_to_kvk() {
    let catalog = catalog();
    for name in [
        "Example Ltd.",
        "Example GmbH",
        "Example S.A.R.L.",
        "Example Company",
    ] {
        let question = format!("Retrieve the registered company profile of {name}");
        let intake = semantic_name_profile_intake(&question, name, "company.profile", &catalog);
        assert!(intake.parsed_request.capabilities[0].source_hint.is_none());
        assert_eq!(intake.parsed_request.subjects[0].identifiers.len(), 1);
        assert_eq!(
            intake.parsed_request.subjects[0].identifiers["company_name"],
            name
        );
        if matches!(name, "Example S.A.R.L." | "Example GmbH") {
            assert!(intake.matches.iter().flat_map(|found| &found.apiosk).all(|candidate|
                candidate.source_slug.as_deref() != Some("kvk-dutch-business-register")),
                "a foreign legal form cannot be routed to the Dutch registry because of the parser language");
        }
    }
}

#[tokio::test]
async fn semantic_name_only_profile_scope_is_never_narrowed_or_rebound() {
    let catalog = catalog();
    let question = "Show the registered company details for Mollie B.V.";
    let intake = semantic_name_profile_intake(question, "Mollie B.V.", "company.profile", &catalog);
    for change in [
        "extra_output",
        "unknown_identifier",
        "different_name",
        "country_filter",
        "stale_question",
    ] {
        let mut changed = intake.clone();
        let parsed = &mut changed.parsed_request;
        match change {
            "extra_output" => parsed.capabilities[0].outputs.push("turnover".into()),
            "unknown_identifier" => {
                parsed.subjects[0]
                    .identifiers
                    .insert("ticker".into(), "EXAMPLE".into());
            }
            "different_name" => {
                parsed.subjects[0]
                    .identifiers
                    .insert("company_name".into(), "Another Company B.V.".into());
            }
            "country_filter" => {
                parsed.subjects[0]
                    .identifiers
                    .insert("country".into(), "Belgium".into());
            }
            "stale_question" => parsed.meta.text_sha256 = "different-question".into(),
            _ => unreachable!(),
        }
        let before = serde_json::to_value(&changed).unwrap();
        let parser = Parser::new(
            reqwest::Client::new(),
            "offline://additional-model-calls-forbidden".into(),
            String::new(),
            "fixture-planner".into(),
        );
        let reply = parser
            .chat_from_capabilities(question, None, &Task::default(), &catalog, &changed)
            .await;
        assert!(reply.as_ref().map_or(true, |reply| !reply.discover && reply.intent.is_none()),
            "{change}: a broader, filtered or stale request must not be narrowed to a plain profile: {reply:?}");
        assert_eq!(
            serde_json::to_value(&changed).unwrap(),
            before,
            "planning must not rewrite requested scope"
        );
    }
}

#[tokio::test]
async fn model_requested_answer_format_survives_planning() {
    let catalog = catalog();
    let starters: Vec<Starter> = serde_json::from_str(include_str!(
        "../tests/fixtures/homepage-starters-historical.json"
    ))
    .unwrap();
    let case = starters
        .iter()
        .find(|case| case.id == "crunchbase")
        .unwrap();
    let question = format!("{} Return a PDF.", case.question);
    let mut output = model_reply_fixture(&case.question);
    output["deliverable"]["format"] = serde_json::json!("pdf");
    let model = model_fixture(&question, output).await;
    let intake = model
        .parser
        .ask_intake(&question, &Task::default(), &catalog, None)
        .await
        .unwrap();
    assert_eq!(model.calls.lock().unwrap().len(), 1);
    assert_eq!(intake.parsed_request.deliverable.format, "pdf");
    let reply = model
        .parser
        .chat_from_capabilities(&question, None, &Task::default(), &catalog, &intake)
        .await
        .unwrap();
    assert!(reply.discover, "{}", reply.message);
    let intent = reply.intent.unwrap();
    assert!(
        intent
            .answer_operations
            .iter()
            .any(|operation| operation.to_ascii_lowercase().contains("pdf")),
        "the requested file format must remain part of the generated plan"
    );
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishedStarter {
    id: String,
    question: String,
    model_output: Value,
    planner_reply: Option<Value>,
    cap_atomic: i64,
    steps: Vec<PublishedStep>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishedStep {
    source: String,
    capability: String,
    subject: String,
    method: String,
    path: String,
    inputs: Option<Value>,
    dependencies: Vec<Value>,
}

fn semantic_output(parsed: &crate::capability_intake::ParsedRequest) -> Value {
    let mut output = serde_json::to_value(parsed).unwrap();
    output.as_object_mut().unwrap().remove("meta");
    for capability in output["capabilities"].as_array_mut().unwrap() {
        capability.as_object_mut().unwrap().remove("missing");
    }
    output
}

#[tokio::test]
async fn published_homepage_questions_preserve_complete_priced_plans() {
    // Recorded interpretations are replayed through the actual contract planner.
    // No example recognizer or second model supplies a plan. The assertions
    // cover complete scope, literal inputs, dependencies and one total price.
    let cases: Vec<PublishedStarter> =
        serde_json::from_str(include_str!("../tests/fixtures/homepage-starters.json")).unwrap();
    assert_recorded_plans(cases).await;
}

#[tokio::test]
async fn unpublished_multilingual_questions_preserve_complete_priced_plans() {
    let cases:Vec<PublishedStarter>=serde_json::from_str(include_str!("../tests/fixtures/general-planning.json")).unwrap();
    assert_recorded_plans(cases).await;
}

async fn assert_recorded_plans(cases:Vec<PublishedStarter>) {
    assert!(!cases.is_empty(), "publish verified examples without padding the list");
    let mut snapshot: Snapshot = serde_json::from_str(include_str!(
        "../tests/fixtures/homepage-starters/catalog.json"
    ))
    .unwrap();
    for raw in [include_str!("../tests/fixtures/general-planning-domain-catalog.json")] {
        let additional:Snapshot=serde_json::from_str(raw).unwrap();
        for service in additional.services {if !snapshot.services.iter().any(|existing|existing.id==service.id) {snapshot.services.push(service);}}
        for fact in additional.vocabulary {if !snapshot.vocabulary.iter().any(|existing|existing.slug==fact.slug) {snapshot.vocabulary.push(fact);}}
        snapshot.known_sources.extend(additional.known_sources);
    }
    let catalog = Catalog::build(snapshot);
    assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);

    for case in cases {
        let id = &case.id;
        let model = recorded_model_fixture(
            &case.question,
            case.model_output.clone(),
            case.planner_reply,
        )
        .await;
        let mut task = Task::default();
        let intake = model
            .parser
            .ask_intake(&case.question, &task, &catalog, None)
            .await
            .unwrap_or_else(|error| panic!("{id}: interpretation failed: {error}"));
        assert_eq!(model.calls.lock().unwrap().len(), 1, "{id}: one AI intake");
        assert_eq!(
            intake.parsed_request.meta.model,
            "fixture-interpretation-model"
        );
        assert_eq!(
            intake.parsed_request.meta.text_sha256,
            hex::encode(Sha256::digest(case.question.as_bytes()))
        );
        assert_eq!(
            semantic_output(&intake.parsed_request),
            case.model_output,
            "{id}: preserve every model-extracted subject, field and answer instruction"
        );
        let reply = model
            .parser
            .chat_from_capabilities(&case.question, None, &task, &catalog, &intake)
            .await
            .unwrap_or_else(|error| panic!("{id}: planning failed: {error}"));
        assert!(reply.discover, "{id}: {}", reply.message);
        let mut intent = reply.intent.expect("a complete plan requires an intent");
        assert!(intent.unresolved_references.is_empty(), "{id}");
        assert!(
            model.calls.lock().unwrap().len() == 1,
            "{id}: only intake may call a model; planning must be deterministic"
        );
        task.record_turn(&case.question, None);
        task.output_grounding = crate::parser::source_grounding::Grounding::new(&intake,&catalog,reply.output_bindings);
        task.capability_intake = Some(intake);
        crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        let resolution = crate::resolver::resolve(&catalog, &task, &intent);
        let Resolution::Plan(plan) = resolution else {
            panic!("{id}: expected a complete plan, got {resolution:?}")
        };
        // The resolver can price a route whose contracts do not cover the
        // requested fields. Match the final Engine::plan refusal checks too.
        assert_eq!(
            crate::parser::plan_coverage::uncovered_inputs(&task, &catalog, &intent, &plan),
            Vec::<String>::new(), "{id}: all supplied inputs must survive planning"
        );
        assert_eq!(
            crate::parser::plan_coverage::uncovered_outputs(&task, &catalog, &intent, &plan),
            Vec::<String>::new(), "{id}: every requested field needs confirmed source coverage"
        );
        assert_eq!(
            plan.steps.len(),
            case.steps.len(),
            "{id}: full scope, no extra purchase"
        );
        assert!(!plan.quote_ref.is_nil(), "{id}: reviewable quote");
        assert!(plan.cap_atomic > 0, "{id}: normal approval required");
        assert_eq!(
            plan.cap_atomic, case.cap_atomic,
            "{id}: recorded complete price"
        );
        assert_eq!(
            plan.cap_atomic,
            plan.steps
                .iter()
                .map(|step| plan.step_total(step))
                .sum::<i64>(),
            "{id}: one total includes every dependency and requested source"
        );
        let shown = crate::resolver::describe_steps(&plan, &[]);
        for (index, (step, expected)) in plan.steps.iter().zip(&case.steps).enumerate() {
            assert_eq!(step.service.api_slug, expected.source, "{id}: step {index}");
            assert_eq!(
                step.service.contract.capability_slug, expected.capability,
                "{id}: step {index}"
            );
            assert_eq!(step.service.method, expected.method, "{id}: step {index}");
            assert_eq!(step.service.path, expected.path, "{id}: step {index}");
            let subject = intent
                .subjects
                .iter()
                .find(|subject| subject.entity_ref == Some(step.entity_ref))
                .unwrap();
            assert_eq!(
                subject.label, expected.subject,
                "{id}: step {index} subject binding"
            );
            let inputs = step
                .service
                .inputs(&task, step.entity_ref, &catalog.vocabulary);
            assert_eq!(
                inputs.ok(),
                expected.inputs,
                "{id}: step {index} literal input binding"
            );
            let dependencies: Vec<Value> = shown[index]["dependencies"]
                .as_array()
                .unwrap()
                .iter()
                .map(|dependency| {
                    serde_json::json!({
                        "step":dependency["step"], "field":dependency["field"]
                    })
                })
                .collect();
            assert_eq!(
                dependencies, expected.dependencies,
                "{id}: step {index} dependency binding"
            );
            if expected.inputs.is_none() {
                assert!(
                    !dependencies.is_empty(),
                    "{id}: unresolved inputs need a quoted prior dependency"
                );
            }
        }
        assert_eq!(
            semantic_output(&task.capability_intake.as_ref().unwrap().parsed_request),
            case.model_output,
            "{id}: planning must never rewrite or reduce the interpreted request"
        );
    }
}

#[tokio::test]
async fn unsupported_glassdoor_comparison_cannot_be_published_as_a_priced_example() {
    let case: PublishedStarter = serde_json::from_str(include_str!(
        "../tests/fixtures/unsupported-glassdoor-starter.json"
    )).unwrap();
    let catalog = Catalog::build(serde_json::from_str(include_str!(
        "../tests/fixtures/homepage-starters/catalog.json"
    )).unwrap());
    let model = model_fixture(&case.question, case.model_output).await;
    let intake = model.parser.ask_intake(&case.question, &Task::default(), &catalog, None).await.unwrap();
    let (engine, _, server) = planner_engine_fixture(case.planner_reply.unwrap()).await;
    let quote = engine.guest_plan(&case.question, intake, &catalog).await.unwrap();
    assert_eq!(quote["status"], "needs_input");
    assert!(quote["preview_token"].is_null());
    assert!(quote["total_atomic"].is_null());
    let message = quote["message"].as_str().unwrap();
    assert!(message.starts_with("I couldn’t match these details"), "{message}");
    assert_eq!(message.matches("strongest category").count(), 1, "{message}");
    assert!(!message.contains("overall rating"), "registered rating coverage remains available");
    assert!(message.chars().count() < 250, "{message}");
    server.abort();
}
