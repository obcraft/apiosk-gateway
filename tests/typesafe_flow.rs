//! Real Gateway state/quote/authorization/worker/recovery with local provider
//! and model fixtures. This verifies plumbing, not live model quality or speed.
#[path = "support/typesafe.rs"]
mod support;
use apiosk_gateway_v2::{
    auth::Principal,
    catalog::CatalogCache,
    engine::Engine,
    execution::Executor,
    parser::Parser,
    protocol::{DiscoverRequest, ExecuteRequest, Status},
    state::Signer,
    store::Store,
    typesafe::{Config, Mode},
};
use serde_json::Value;
use std::{
    sync::{atomic::Ordering::Relaxed, Arc},
    time::Duration,
};
use uuid::Uuid;

#[tokio::test]
#[ignore = "isolated support-db only; local model, provider and billing fixtures"]
async fn typesafe_question_quote_approval_worker_answer_and_recovery() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SET search_path TO public,gateway_v2")
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let counts = Arc::new(support::Fixture::default());
    let (base, server) = support::server(counts.clone()).await;
    let http = reqwest::Client::new();
    let store = Store { pool: pool.clone() };
    let mut config = Config::new("fixture".into(), Mode::On);
    config.url = format!("{base}/typesafe");
    let engine = Arc::new(Engine {
        store: store.clone(),
        catalog: CatalogCache::new(pool.clone()),
        parser: Parser::new(
            http.clone(),
            format!("{base}/chat"),
            "fixture".into(),
            "baseline".into(),
        )
        .with_typesafe(Some(config))
        .unwrap(),
        signer: Signer::new(vec![9; 32]).unwrap(),
        executor: Executor {
            store,
            http,
            supabase_url: base,
            enabled: true,
        },
        app_url: "http://localhost:5173".into(),
    });
    let person = Principal {
        owner: Uuid::new_v4(),
        connection: None,
        workspace: None,
    };
    // Exercise both entry points used by the App and MCP against durable state.
    for app in [true, false] {
        let before = counts.provider_calls.load(Relaxed);
        let request = DiscoverRequest {
            request_id: Uuid::new_v4(),
            question: "What year does the available filing for company 123 cover?".into(),
            state: None,
            context_delta: Default::default(),
        };
        let ready = if app {
            engine.chat(&person, request.clone()).await
        } else {
            engine.discover(&person, request.clone()).await
        }
        .unwrap();
        assert_eq!(ready.status, Status::Ready, "{ready:?}");
        let quote = ready.proposal.as_ref().unwrap();
        assert_eq!(quote.max_total_atomic, "115");
        assert_eq!(quote.steps.len(), 1);
        assert_eq!(counts.provider_calls.load(Relaxed), before);
        let task = ready.state.as_ref().unwrap().state_ref;
        let action = ready
            .next_actions
            .iter()
            .find(|a| a.kind == "execute_quoted_step")
            .unwrap();
        let unapproved = engine
            .execute(
                &person,
                "fixture",
                ExecuteRequest {
                    request_id: Uuid::new_v4(),
                    action_id: action.action_id,
                    state: ready.state.clone().unwrap(),
                    quote_ref: Some(quote.quote_ref),
                    authorization_ref: None,
                    idempotency_key: action.action_id,
                    input: Value::Null,
                },
            )
            .await
            .unwrap();
        assert_eq!(unapproved.status, Status::RequiresApproval);
        assert_eq!(counts.provider_calls.load(Relaxed), before);
        engine
            .executor
            .approve(&person, task, quote.quote_ref, &quote.max_total_atomic)
            .await
            .unwrap();
        engine
            .start_worker(person.clone(), "fixture".into(), task)
            .await
            .unwrap();
        let done = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let response = engine
                    .view(&person, task, Uuid::new_v4(), None)
                    .await
                    .unwrap();
                let (_, saved, _) = engine.store.load(person.owner, task).await.unwrap();
                if saved
                    .analysis
                    .as_ref()
                    .is_some_and(|analysis| analysis["status"] == "completed")
                {
                    break (response, saved);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(done.0.status, Status::Succeeded, "{:?}", done.0);
        let analysis = done.1.analysis.unwrap();
        assert_eq!(
            analysis["observations"][0]["text"],
            "According to the source: 2025"
        );
        assert_eq!(
            analysis["observations"][0]["evidence"][0]["pointer"],
            "/data/accounts"
        );
        assert_eq!(counts.provider_calls.load(Relaxed), before + 1);
        let replay = if app {
            engine.chat(&person, request).await
        } else {
            engine.discover(&person, request).await
        }
        .unwrap();
        assert_eq!(replay.state.unwrap().state_ref, task);
        engine
            .view(&person, task, Uuid::new_v4(), None)
            .await
            .unwrap();
        assert_eq!(
            counts.provider_calls.load(Relaxed),
            before + 1,
            "recovery must not buy the result again"
        );
        let (approvals, charged): (i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM authorizations WHERE task_id=$1), (SELECT coalesce(sum(actual_atomic),0)::bigint FROM executions WHERE task_id=$1)")
            .bind(task).fetch_one(&pool).await.unwrap();
        assert_eq!((approvals, charged), (1, 115));
    }
    assert_eq!(
        counts.typesafe_calls.load(Relaxed),
        10,
        "three intent calls and two answer calls, on both surfaces"
    );
    assert_eq!(counts.baseline_calls.load(Relaxed), 0);
    pool.close().await;
    server.abort();
}
