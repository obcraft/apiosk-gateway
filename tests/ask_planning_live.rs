//! Opt-in production-catalog model regression, stopping before task creation or payment.
use apiosk_gateway_v2::{
    catalog::{Catalog, Snapshot},
    engine,
    parser::Parser,
    resolver,
    state::Task,
};
use serde_json::json;

#[tokio::test]
#[ignore = "requires authorized catalog metadata and configured model; no purchases"]
async fn production_questions_compile_after_capability_discovery() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("apiosk_gateway_v2=info")
        .with_test_writer()
        .try_init();
    dotenvy::dotenv().ok();
    let snapshot: Snapshot = serde_json::from_slice(
        &std::fs::read(std::env::var("ASK_CATALOG_SNAPSHOT").unwrap()).unwrap(),
    )
    .unwrap();
    let catalog = Catalog::build(snapshot);
    let parser = Parser::new(
        reqwest::Client::new(),
        std::env::var("INTENT_API_URL").unwrap(),
        std::env::var("INTENT_API_KEY").unwrap(),
        std::env::var("INTENT_MODEL").unwrap(),
    );
    let questions = std::env::var("ASK_QUESTION").map(|q| vec![q]).unwrap_or_else(|_| vec![
        "What are the current KVK number, registered name, status and address of Coolblue B.V.? Include the KVK source.".into(),
        "Check Coolblue B.V. against current sanctions lists and show any matching records, match scores and source links.".into(),
    ]);
    let mut results = vec![];
    for question in questions {
        let mut task = Task::default();
        let intake = parser
            .prepare_ask(&question, None, &task, &catalog)
            .await
            .unwrap();
        let reply = parser
            .chat_from_capabilities(&question, None, &task, &catalog, &intake)
            .await
            .unwrap();
        assert!(reply.discover, "{}", reply.message);
        let mut intent = reply.intent.unwrap();
        engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        let resolution = resolver::resolve(&catalog, &task, &intent);
        assert!(
            matches!(resolution, resolver::Resolution::Plan(_)),
            "{resolution:?}"
        );
        results.push(json!({"question":question,"sources":intake.apiosk_sources_searched,"selected":intake.selected_capabilities,"intent":intent,"plan_ready":true}));
    }
    if let Ok(path) = std::env::var("ASK_PLANNING_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&results).unwrap()).unwrap();
    }
    println!("{} questions reached a valid plan with the supplied catalog; no task, approval, purchase or provider execution.", results.len());
}

#[tokio::test]
#[ignore = "live configured model with checked-in synthetic catalog only; no purchases"]
async fn ambiguous_comparison_clarifies_then_binds_both_original_companies() {
    let _ = tracing_subscriber::fmt().with_env_filter("apiosk_gateway_v2=info").with_test_writer().try_init();
    dotenvy::dotenv().ok();
    let catalog = Catalog::build(serde_json::from_str(include_str!("fixtures/integration-catalog.json")).unwrap());
    let parser = Parser::new(reqwest::Client::new(), std::env::var("INTENT_API_URL").unwrap(),
        std::env::var("INTENT_API_KEY").unwrap(), std::env::var("INTENT_MODEL").unwrap());
    let mut task = Task::default();
    let question = "Maak een analyse tussen Coolblue en Bol.com";
    let intake = parser.prepare_ask(question, None, &task, &catalog).await.unwrap();
    let clarification = parser.chat_from_capabilities(question, None, &task, &catalog, &intake).await.unwrap();
    assert!(!clarification.discover, "a comparison without a topic needs a question: {}", clarification.message);
    assert!(clarification.intent.is_none());
    task.record_turn(question, Some(clarification.message));
    let answer = "Vergelijk hun jaarrekeningen in Excel.";
    let intake = parser.prepare_ask(answer, None, &task, &catalog).await.unwrap();
    assert_eq!(intake.parsed_request.deliverable.format, "xlsx");
    let reply = parser.chat_from_capabilities(answer, None, &task, &catalog, &intake).await.unwrap();
    assert!(reply.discover, "{}", reply.message);
    let mut intent = reply.intent.unwrap();
    for name in ["Coolblue", "Bol.com"] {
        assert!(intent.goals.iter().any(|goal| intent.subjects[goal.subject].label.eq_ignore_ascii_case(name)), "both companies need retrieval goals: {intent:?}");
    }
    engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
    assert!(matches!(resolver::resolve(&catalog, &task, &intent), resolver::Resolution::Plan(_)));
    println!("Ambiguous comparison asked a question; the follow-up preserved both companies and Excel and reached a plan. Synthetic catalog only; no payment.");
}
