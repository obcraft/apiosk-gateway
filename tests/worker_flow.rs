use apiosk_gateway_v2::{
    auth::Principal,
    catalog::CatalogCache,
    engine::Engine,
    execution::Executor,
    parser::Parser,
    protocol::{DiscoverRequest, ExecuteRequest, Status},
    state::Signer,
    store::Store,
};
use axum::{extract::State, routing::post, Json, Router};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use uuid::Uuid;
struct Counts {
    pool: sqlx::PgPool,
    active: AtomicUsize,
    peak: AtomicUsize,
    calls: AtomicUsize,
    parses: AtomicUsize,
    slow_mode: AtomicBool,
    slow_active: AtomicBool,
    dependency_overlapped: AtomicBool,
}
async fn model(State(c): State<Arc<Counts>>, Json(body): Json<Value>) -> Json<Value> {
    c.parses.fetch_add(1, Ordering::SeqCst);
    let input: Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    let ambiguous = input["question"] == "Ambiguous";
    let labels = if ambiguous {
        vec!["Ambiguous"]
    } else {
        vec!["FirstCo", "SecondCo"]
    };
    Json(
        json!({"choices":[{"message":{"content":json!({"subjects":labels.iter().map(|name|json!({"entity_ref":null,"label":name,"values":{"company.name":name}})).collect::<Vec<_>>(),"goals":(0..labels.len()).map(|subject|json!({"capability":"company.accounts","subject":subject,"constraints":{}})).collect::<Vec<_>>(),"answer_operations":[],"unresolved_references":[]}).to_string()}}]}),
    )
}
async fn provider(State(c): State<Arc<Counts>>, Json(body): Json<Value>) -> Json<Value> {
    let active = c.active.fetch_add(1, Ordering::SeqCst) + 1;
    c.peak.fetch_max(active, Ordering::SeqCst);
    c.calls.fetch_add(1, Ordering::SeqCst);
    let slow = c.slow_mode.load(Ordering::SeqCst) && body["inputs"]["company.name"] == "SecondCo";
    if slow {
        c.slow_active.store(true, Ordering::SeqCst)
    }
    if body["inputs"]["company.id"] == "123" && c.slow_active.load(Ordering::SeqCst) {
        c.dependency_overlapped.store(true, Ordering::SeqCst)
    }
    tokio::time::sleep(Duration::from_millis(if slow { 700 } else { 200 })).await;
    if slow {
        c.slow_active.store(false, Ordering::SeqCst)
    }
    let search = body["inputs"].get("company.name").is_some();
    let result = if search {
        if body["inputs"]["company.name"] == "Ambiguous" {
            json!({"companies":[{"id":"123"},{"id":"456"}]})
        } else {
            json!({"companies":[{"id":if body["inputs"]["company.name"]=="SecondCo"{"456"}else{"123"}}]})
        }
    } else {
        json!({"accounts":body["inputs"]["company.id"]})
    };
    sqlx::query("INSERT INTO public.balance_charges(id,user_id,idempotency_key,status,charged_micro_usd,provider_price_micro_usd,fee_micro_usd) SELECT $1,owner_id,id::text,'charged',$2,$3,$4 FROM gateway_v2.executions WHERE id=$5")
  .bind(Uuid::new_v4()).bind(if search{23i64}else{115}).bind(if search{20i64}else{100}).bind(if search{3i64}else{15}).bind(body["idempotency_key"].as_str().unwrap().parse::<Uuid>().unwrap()).execute(&c.pool).await.unwrap();
    c.active.fetch_sub(1, Ordering::SeqCst);
    Json(
        json!({"ok":true,"result":result,"charged_micro_usd":if search{23}else{115},"provider_micro_usd":if search{20}else{100},"fee_micro_usd":if search{3}else{15}}),
    )
}
async fn settled(
    engine: &Engine,
    p: &Principal,
    id: Uuid,
) -> apiosk_gateway_v2::protocol::Response {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if !engine.worker_active(id).await.unwrap() {
                return engine.view(p, id, Uuid::new_v4(), None).await.unwrap();
            }
            tokio::time::sleep(Duration::from_millis(30)).await
        }
    })
    .await
    .expect("worker must settle")
}
#[tokio::test]
#[ignore = "isolated support-db only"]
async fn one_approval_parallel_execution_and_ambiguity_resume() {
    let pool = PgPoolOptions::new()
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
    let counts = Arc::new(Counts {
        pool: pool.clone(),
        active: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
        calls: AtomicUsize::new(0),
        parses: AtomicUsize::new(0),
        slow_mode: AtomicBool::new(false),
        slow_active: AtomicBool::new(false),
        dependency_overlapped: AtomicBool::new(false),
    });
    let app = Router::new()
        .route("/chat", post(model))
        .route("/functions/v1/use-selection", post(provider))
        .with_state(counts.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let store = Store { pool: pool.clone() };
    let http = reqwest::Client::new();
    let engine = Arc::new(Engine {
        store: store.clone(),
        catalog: CatalogCache::new(pool.clone()),
        parser: Parser::new(
            http.clone(),
            format!("{base}/chat"),
            "fixture".into(),
            "fixture".into(),
        ),
        signer: Signer::new(vec![3; 32]).unwrap(),
        executor: Executor {
            store: store.clone(),
            http,
            supabase_url: base,
            enabled: true,
        },
        app_url: "http://localhost".into(),
    });
    let p = Principal {
        owner: Uuid::new_v4(),
        connection: None,
    };
    let r = engine
        .discover(
            &p,
            DiscoverRequest {
                request_id: Uuid::new_v4(),
                question: "Compare FirstCo and SecondCo".into(),
                state: None,
                context_delta: Default::default(),
            },
        )
        .await
        .unwrap();
    assert!(r.proposal.is_some(), "{r:?}");
    let id = r.state.as_ref().unwrap().state_ref;
    let quote = r.proposal.as_ref().unwrap();
    engine
        .start_worker(p.clone(), "fixture".into(), id)
        .await
        .unwrap();
    assert_eq!(
        counts.calls.load(Ordering::SeqCst),
        0,
        "no execution before consent"
    );
    engine
        .executor
        .approve(&p, id, quote.quote_ref, &quote.max_total_atomic)
        .await
        .unwrap();
    let start = Instant::now();
    engine
        .start_worker(p.clone(), "fixture".into(), id)
        .await
        .unwrap();
    engine
        .start_worker(p.clone(), "fixture".into(), id)
        .await
        .unwrap();
    let done = settled(&engine, &p, id).await;
    assert_eq!(done.status, Status::Succeeded, "{done:?}");
    assert_eq!(counts.calls.load(Ordering::SeqCst), 4);
    assert!(
        counts.peak.load(Ordering::SeqCst) >= 2,
        "independent providers must overlap"
    );
    assert_eq!(done.billing.as_ref().unwrap()["total_charged"], "276");
    assert_eq!(done.proposal.as_ref().unwrap().quote_ref, quote.quote_ref);
    assert_eq!(counts.parses.load(Ordering::SeqCst), 1);
    engine
        .start_worker(p.clone(), "fixture".into(), id)
        .await
        .unwrap();
    assert_eq!(
        counts.calls.load(Ordering::SeqCst),
        4,
        "completed work cannot be bought twice"
    );
    println!(
        "parallel request: {}ms, 4 calls, 1 approval, peak concurrency {}",
        start.elapsed().as_millis(),
        counts.peak.load(Ordering::SeqCst)
    );
    let r = engine
        .discover(
            &p,
            DiscoverRequest {
                request_id: Uuid::new_v4(),
                question: "Ambiguous".into(),
                state: None,
                context_delta: Default::default(),
            },
        )
        .await
        .unwrap();
    let id = r.state.as_ref().unwrap().state_ref;
    let quote = r.proposal.as_ref().unwrap().clone();
    engine
        .executor
        .approve(&p, id, quote.quote_ref, &quote.max_total_atomic)
        .await
        .unwrap();
    engine
        .start_worker(p.clone(), "fixture".into(), id)
        .await
        .unwrap();
    let choice = settled(&engine, &p, id).await;
    assert_eq!(choice.status, Status::NeedsSelection);
    assert_eq!(counts.calls.load(Ordering::SeqCst), 5);
    let action = choice
        .next_actions
        .iter()
        .find(|a| a.kind == "select_entity")
        .unwrap();
    let selected = choice.context_view["candidates"][1]["entity_ref"].clone();
    let resumed = engine
        .execute(
            &p,
            "fixture",
            ExecuteRequest {
                request_id: Uuid::new_v4(),
                action_id: action.action_id,
                idempotency_key: action.action_id,
                state: choice.state.unwrap(),
                quote_ref: Some(quote.quote_ref),
                authorization_ref: None,
                input: json!({"entity_ref":selected}),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        resumed.proposal.as_ref().unwrap().quote_ref,
        quote.quote_ref
    );
    engine
        .start_worker(p.clone(), "fixture".into(), id)
        .await
        .unwrap();
    let done = settled(&engine, &p, id).await;
    assert_eq!(done.status, Status::Succeeded, "{done:?}");
    assert_eq!(done.result.unwrap()["data"]["accounts"], "456");
    assert_eq!(
        counts.calls.load(Ordering::SeqCst),
        6,
        "selection must not repeat search"
    );
    assert_eq!(
        counts.parses.load(Ordering::SeqCst),
        2,
        "selection must not replan"
    );
    assert_eq!(done.billing.unwrap()["total_charged"], "138");
    let approvals: i64 = sqlx::query_scalar("SELECT count(*) FROM authorizations WHERE task_id=$1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(approvals, 1);
    counts.slow_mode.store(true, Ordering::SeqCst);
    let r = engine
        .discover(
            &p,
            DiscoverRequest {
                request_id: Uuid::new_v4(),
                question: "Compare FirstCo and SecondCo".into(),
                state: None,
                context_delta: Default::default(),
            },
        )
        .await
        .unwrap();
    let id = r.state.as_ref().unwrap().state_ref;
    let quote = r.proposal.as_ref().unwrap();
    engine
        .executor
        .approve(&p, id, quote.quote_ref, &quote.max_total_atomic)
        .await
        .unwrap();
    engine
        .start_worker(p.clone(), "fixture".into(), id)
        .await
        .unwrap();
    let done = settled(&engine, &p, id).await;
    assert_eq!(done.status, Status::Succeeded, "{done:?}");
    assert!(
        counts.dependency_overlapped.load(Ordering::SeqCst),
        "financials must start before the unrelated slow search finishes"
    );
    counts.slow_mode.store(false, Ordering::SeqCst);
    let baseline = counts.calls.load(Ordering::SeqCst);
    let r = engine
        .discover(
            &p,
            DiscoverRequest {
                request_id: Uuid::new_v4(),
                question: "Compare FirstCo and SecondCo".into(),
                state: None,
                context_delta: Default::default(),
            },
        )
        .await
        .unwrap();
    let id = r.state.as_ref().unwrap().state_ref;
    let quote = r.proposal.as_ref().unwrap();
    engine
        .executor
        .approve(&p, id, quote.quote_ref, &quote.max_total_atomic)
        .await
        .unwrap();
    engine
        .start_worker(p.clone(), "fixture".into(), id)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while counts.active.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await
        }
    })
    .await
    .unwrap();
    let latest = engine.view(&p, id, Uuid::new_v4(), None).await.unwrap();
    engine
        .execute(
            &p,
            "fixture",
            ExecuteRequest {
                request_id: Uuid::new_v4(),
                action_id: id,
                idempotency_key: id,
                state: latest.state.unwrap(),
                quote_ref: Some(quote.quote_ref),
                authorization_ref: None,
                input: Value::Null,
            },
        )
        .await
        .unwrap();
    let cancelled = settled(&engine, &p, id).await;
    assert_eq!(cancelled.status, Status::Partial);
    assert_eq!(
        counts.calls.load(Ordering::SeqCst),
        baseline + 2,
        "cancellation stops dependent calls but preserves in-flight outcomes"
    );
    let r = engine
        .discover(
            &p,
            DiscoverRequest {
                request_id: Uuid::new_v4(),
                question: "Compare FirstCo and SecondCo".into(),
                state: None,
                context_delta: Default::default(),
            },
        )
        .await
        .unwrap();
    let id = r.state.as_ref().unwrap().state_ref;
    let quote = r.proposal.as_ref().unwrap();
    engine
        .executor
        .approve(&p, id, quote.quote_ref, &quote.max_total_atomic)
        .await
        .unwrap();
    sqlx::query("UPDATE authorizations SET cap_atomic=23 WHERE task_id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let baseline = counts.calls.load(Ordering::SeqCst);
    engine
        .start_worker(p.clone(), "fixture".into(), id)
        .await
        .unwrap();
    let _ = settled(&engine, &p, id).await;
    let reserved: i64 = sqlx::query_scalar(
        "SELECT coalesce(sum(reserved_atomic),0)::bigint FROM executions WHERE task_id=$1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(reserved, 23);
    assert_eq!(
        counts.calls.load(Ordering::SeqCst),
        baseline + 1,
        "parallel admission cannot exceed the saved authorization"
    );
    let exposed:bool=sqlx::query_scalar("SELECT has_table_privilege('authenticated','gateway_v2.execution_leases','SELECT') OR has_table_privilege('anon','gateway_v2.execution_leases','INSERT')").fetch_one(&pool).await.unwrap();
    assert!(!exposed);
    println!("slow branch, cancellation, parallel budget ceiling and private lease access passed");
    server.abort();
    pool.close().await;
}
