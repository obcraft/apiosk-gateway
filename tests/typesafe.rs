#[path = "support/typesafe.rs"]
mod support;
use apiosk_gateway_v2::{
    catalog::Catalog,
    parser::Parser,
    state::Task,
    typesafe::{Config, Mode},
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{atomic::Ordering::Relaxed, Arc},
    time::{Duration, Instant},
};
use uuid::Uuid;

fn catalog() -> Catalog {
    Catalog::build(serde_json::from_str(include_str!("fixtures/integration-catalog.json")).unwrap())
}
fn parser(base: &str, mode: Mode) -> Parser {
    let mut config = Config::new("fixture".into(), mode);
    config.url = format!("{base}/typesafe");
    config.timeout = Duration::from_millis(500);
    Parser::new(
        reqwest::Client::new(),
        format!("{base}/chat"),
        "fixture".into(),
        "baseline".into(),
    )
    .with_typesafe(Some(config))
    .unwrap()
}

#[tokio::test]
async fn app_and_mcp_use_verified_typed_intents_and_mcp_cache() {
    let counts = Arc::new(support::Fixture::default());
    let (base, server) = support::server(counts.clone()).await;
    let parser = parser(&base, Mode::On);
    let owner = Uuid::new_v4();
    let question = "Get the available accounts for Example";
    for _ in 0..2 {
        let intent = parser
            .parse(owner, question, &Task::default(), &catalog())
            .await
            .unwrap();
        assert_eq!(intent.subjects[0].label, "Example");
        assert_eq!(intent.subjects[0].values["company.name"], "Example");
        assert_eq!(intent.answer_operations, vec![question]);
    }
    assert_eq!(
        counts.typesafe_calls.load(Relaxed),
        3,
        "selection, binding and verification cached together"
    );
    let chat = parser
        .chat(question, &Task::default(), &catalog())
        .await
        .unwrap();
    assert!(chat.discover && chat.intent.is_some());
    assert_eq!(counts.typesafe_calls.load(Relaxed), 6);
    assert_eq!(counts.baseline_calls.load(Relaxed), 0);
    let usage = parser.typesafe_usage();
    assert!(usage.cost_complete);
    assert_eq!(usage.input_tokens, 600);
    assert!((usage.known_cost_usd - 0.0000252).abs() < 1e-10);
    server.abort();
}

#[tokio::test]
async fn uncertain_constraints_and_invalid_distributions_fall_back_without_losing_latest() {
    for fixture in [
        support::Fixture {
            reject_guard: Some("no_extra_constraints".into()),
            ..Default::default()
        },
        support::Fixture {
            reject_guard: Some("constraints_complete".into()),
            ..Default::default()
        },
        support::Fixture {
            malformed: true,
            ..Default::default()
        },
        support::Fixture {
            http_failure: true,
            ..Default::default()
        },
    ] {
        let counts = Arc::new(fixture);
        let (base, server) = support::server(counts.clone()).await;
        let parser = parser(&base, Mode::On);
        let reply = parser
            .chat(
                "Get latest accounts for Example",
                &Task::default(),
                &catalog(),
            )
            .await
            .unwrap();
        assert_eq!(
            reply.intent.unwrap().goals[0].constraints["filing_selection"],
            "latest"
        );
        assert_eq!(counts.baseline_calls.load(Relaxed), 1);
        assert!(
            parser.typesafe_usage().requests > 0,
            "fallback cost must remain counted"
        );
        if counts.http_failure {
            assert!(!parser.typesafe_usage().cost_complete);
        }
        server.abort();
    }
}

#[tokio::test]
async fn shadow_preserves_baseline_and_context_bypasses_typesafe() {
    let counts = Arc::new(support::Fixture::default());
    let (base, server) = support::server(counts.clone()).await;
    let parser = parser(&base, Mode::Shadow);
    let chat = parser
        .chat("Get accounts for Example", &Task::default(), &catalog())
        .await
        .unwrap();
    assert_eq!(chat.message, "Baseline reply");
    assert_eq!(counts.typesafe_calls.load(Relaxed), 3);
    assert_eq!(counts.baseline_calls.load(Relaxed), 1);
    let mut task = Task::default();
    task.record_turn("Get accounts for Example", Some("Need more detail".into()));
    parser
        .chat("Get latest accounts for Example", &task, &catalog())
        .await
        .unwrap();
    assert_eq!(
        counts.typesafe_calls.load(Relaxed),
        3,
        "do not strip follow-up context to force eligibility"
    );
    server.abort();
}

#[tokio::test]
async fn timeout_bounds_total_overhead_and_retains_unknown_cost() {
    let counts = Arc::new(support::Fixture {
        slow: true,
        ..Default::default()
    });
    let (base, server) = support::server(counts.clone()).await;
    let parser = parser(&base, Mode::On);
    let start = Instant::now();
    parser
        .chat("Get accounts for Example", &Task::default(), &catalog())
        .await
        .unwrap();
    assert!(start.elapsed() < Duration::from_millis(750));
    assert_eq!(counts.baseline_calls.load(Relaxed), 1);
    assert!(!parser.typesafe_usage().cost_complete);
    server.abort();
}

#[tokio::test]
async fn comparison_reports_rejection_and_http_status_without_substitution() {
    for (fixture, expected) in [
        (
            support::Fixture {
                reject_guard: Some("no_extra_constraints".into()),
                ..Default::default()
            },
            "uncertain_judgment: no_extra_constraints",
        ),
        (
            support::Fixture {
                http_failure: true,
                ..Default::default()
            },
            "typesafe_http_status: 429",
        ),
        (
            support::Fixture {
                slow: true,
                ..Default::default()
            },
            "typesafe_deadline",
        ),
    ] {
        let counts = Arc::new(fixture);
        let (base, server) = support::server(counts.clone()).await;
        let mut config = Config::new("secret-fixture-key".into(), Mode::On);
        config.url = format!("{base}/typesafe");
        config.timeout = Duration::from_millis(500);
        let client = apiosk_gateway_v2::typesafe::Client::new(config).unwrap();
        let result = client
            .parse_detailed("Get accounts for Example", &Task::default(), &catalog())
            .await;
        let error = result.err().expect("fixture should reject").to_string();
        assert!(error.starts_with(expected), "{error}");
        assert!(!error.contains("secret-fixture-key"));
        assert_eq!(counts.baseline_calls.load(Relaxed), 0);
        assert_eq!(client.usage.snapshot().requests, 1);
        server.abort();
    }
}

#[tokio::test]
async fn extractive_answer_uses_saved_value_and_rejected_answer_uses_synthesis() {
    for reject in [None, Some("complete".to_owned())] {
        let counts = Arc::new(support::Fixture {
            reject_guard: reject.clone(),
            ..Default::default()
        });
        let (base, server) = support::server(counts.clone()).await;
        let parser = parser(&base, Mode::On);
        let sources = vec![
            json!({"result_ref":"saved", "document":{"subject":{"label":"Example"},"source":{"provider":"fixture"},"coverage":"source_response","missing_fields":[],"data":{"accounts":"2025"}}}),
        ];
        let answer = parser
            .analyze(
                &json!(["Which year does the filing cover?"]),
                &sources,
                &BTreeMap::new(),
                &[],
            )
            .await
            .unwrap();
        assert_eq!(answer["observations"][0]["evidence"][0]["value"], "2025");
        assert_eq!(
            answer["observations"][0]["evidence"][0]["result_ref"],
            "saved"
        );
        assert_eq!(counts.typesafe_calls.load(Relaxed), 2);
        assert_eq!(
            counts.baseline_calls.load(Relaxed),
            u64::from(reject.is_some())
        );
        // Partial source retrieval must not be presented as a complete answer.
        let failures = BTreeMap::from([("missing".into(), json!({"code":"unavailable"}))]);
        assert_eq!(
            parser
                .analyze(&json!(["Which year?"]), &sources, &failures, &[])
                .await
                .unwrap()["status"],
            "partial"
        );
        assert_eq!(counts.typesafe_calls.load(Relaxed), 2);
        server.abort();
    }
}

#[test]
fn configuration_and_large_requests_fail_closed() {
    let mut config = Config::new("".into(), Mode::On);
    assert!(apiosk_gateway_v2::typesafe::Client::new(config.clone()).is_err());
    config.api_key = "fixture".into();
    config.url = "http://external.example/systemone".into();
    assert!(apiosk_gateway_v2::typesafe::Client::new(config).is_err());
    assert!(apiosk_gateway_v2::typesafe::intent_request(
        &"long ".repeat(23),
        &Task::default(),
        &catalog()
    )
    .is_err());
}
