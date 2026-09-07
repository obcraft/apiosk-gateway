use apiosk_gateway_v2::{
    auth::{Auth, Principal},
    catalog::CatalogCache,
    engine::Engine,
    execution::Executor,
    parser::Parser,
    protocol::{DiscoverRequest, ExecuteRequest, Response, Status},
    server::{router, ServerState},
    state::{Signer, Task},
    store::Store,
};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, Row};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use uuid::Uuid;

#[derive(Default)]
struct Counters {
    parser: AtomicU64,
    provider: AtomicU64,
}
async fn model(State(c): State<Arc<Counters>>, Json(body): Json<Value>) -> Json<Value> {
    c.parser.fetch_add(1, Ordering::Relaxed);
    let input: Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    if input["surface"] == "ask-chat" {
        return Json(
            json!({"choices":[{"message":{"content":json!({"message":"Hallo! Waar kan ik je mee helpen?","discover":input["question"] != "Hallo"}).to_string()}}]}),
        );
    }
    let entity = input["context"]["entities"][0]["entity_ref"].clone();
    let label = if input["question"].as_str().unwrap().contains("Ambiguous") {
        "Ambiguous"
    } else if input["question"].as_str().unwrap().contains("Partial") {
        "Partial"
    } else {
        "Example"
    };
    let subject = if entity.is_string() {
        json!({"entity_ref":entity,"label":"Example","values":{}})
    } else {
        json!({"entity_ref":null,"label":label,"values":{"company.name":label}})
    };
    let capability = if input["question"]
        .as_str()
        .unwrap()
        .starts_with("Search only")
    {
        "company.search"
    } else {
        "company.accounts"
    };
    Json(
        json!({"choices":[{"message":{"content":json!({"subjects":[subject],"goals":[{"capability":capability,"subject":0,"constraints":{}}],"answer_operations":[],"unresolved_references":[]}).to_string()}}]}),
    )
}
async fn provider(State(c): State<Arc<Counters>>, Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(body["deterministic"], true);
    if body["inputs"]["company.id"] == "no-balance" {
        return Json(
            json!({"ok":false,"error":"insufficient_funds","stage":"checking_balance","balance_micro_usd":0}),
        );
    }
    c.provider.fetch_add(1, Ordering::Relaxed);
    if body["inputs"]["company.id"] == "timeout" {
        return Json(json!({"ok":false,"error":"unknown_after_dispatch"}));
    }
    let search = body["inputs"].get("company.name").is_some();
    let result = if search {
        if body["inputs"]["company.name"] == "Ambiguous" {
            json!({"companies":[{"id":"123"},{"id":"456"}]})
        } else if body["inputs"]["company.name"] == "Partial" {
            json!({"companies":[{"id":"partial"}]})
        } else {
            json!({"companies":[{"id":"123"}]})
        }
    } else if body["inputs"]["company.id"] == "partial" {
        json!({"available":false})
    } else {
        json!({"accounts":"2025"})
    };
    Json(
        json!({"ok":true,"charged_micro_usd":if search{23}else{115},"provider_micro_usd":if search{20}else{100},"fee_micro_usd":if search{3}else{15},"result":result,"result_truncated":false}),
    )
}
async fn fixture_user(headers: HeaderMap) -> (StatusCode, Json<Value>) {
    if headers.get("authorization").and_then(|v| v.to_str().ok()) == Some("Bearer fixture-human") {
        (
            StatusCode::OK,
            Json(json!({"id":"00000000-0000-4000-8000-000000000001"})),
        )
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"fixture authentication refused"})),
        )
    }
}
fn execution(r: &Response) -> ExecuteRequest {
    let a = r
        .next_actions
        .iter()
        .find(|a| a.kind == "execute_quoted_step")
        .unwrap();
    ExecuteRequest {
        request_id: Uuid::new_v4(),
        action_id: a.action_id,
        state: r.state.clone().unwrap(),
        quote_ref: r.proposal.as_ref().map(|p| p.quote_ref),
        authorization_ref: None,
        idempotency_key: a.action_id,
        input: Value::Null,
    }
}

#[tokio::test]
#[ignore = "requires local tests/support-db.mjs on 54329; no live database"]
async fn durable_flow_and_authorization_boundary() {
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&std::env::var("TEST_DATABASE_URL").unwrap_or_else(|_| {
            "postgresql://postgres@127.0.0.1:54329/postgres?sslmode=disable".into()
        }))
        .await
        .unwrap();
    let counts = Arc::new(Counters::default());
    let mock = Router::new()
        .route("/chat", post(model))
        .route("/functions/v1/use-selection", post(provider))
        .route("/functions/v1/agent-gateway/v1/run", post(provider))
        .route("/auth/v1/user", get(fixture_user))
        .with_state(counts.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let store = Store { pool: pool.clone() };
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
            http: http.clone(),
            supabase_url: base.clone(),
            enabled: true,
        },
        app_url: "http://localhost:5173".into(),
    });
    let person = Principal {
        owner: Uuid::new_v4(),
        connection: None,
    };
    let loaded = engine.catalog.get().await.unwrap();
    assert_eq!(loaded.services.len(), 2, "rejected: {:?}", loaded.rejected);
    let request = DiscoverRequest {
        request_id: Uuid::new_v4(),
        question: "Get accounts for Example".into(),
        state: None,
        context_delta: Default::default(),
    };
    let start = std::time::Instant::now();
    let r = engine.discover(&person, request.clone()).await.unwrap();
    assert_eq!(r.status, Status::Ready, "{r:?}");
    assert_eq!(r.proposal.as_ref().unwrap().max_total_atomic, "138");
    assert_eq!(counts.provider.load(Ordering::Relaxed), 0);
    let replay = engine.discover(&person, request).await.unwrap();
    assert_eq!(
        replay.state.unwrap().state_ref,
        r.state.as_ref().unwrap().state_ref
    );
    assert_eq!(counts.parser.load(Ordering::Relaxed), 1);
    let no_consent = engine
        .execute(&person, "fixture", execution(&r))
        .await
        .unwrap();
    assert_eq!(no_consent.status, Status::RequiresApproval);
    assert_eq!(counts.provider.load(Ordering::Relaxed), 0);
    let task = r.state.as_ref().unwrap().state_ref;
    let proposal = r.proposal.as_ref().unwrap();
    let agent = Principal {
        owner: person.owner,
        connection: Some(Uuid::new_v4()),
    };
    assert!(engine
        .executor
        .approve(&agent, task, proposal.quote_ref, "138")
        .await
        .is_err());
    assert!(engine
        .executor
        .approve(&person, task, proposal.quote_ref, "139")
        .await
        .is_err());
    engine
        .executor
        .approve(&person, task, proposal.quote_ref, "138")
        .await
        .unwrap();
    let run = execution(&r);
    let first = engine
        .execute(&person, "fixture", run.clone())
        .await
        .unwrap();
    assert_eq!(first.status, Status::Ready, "{first:?}");
    let again = engine.execute(&person, "fixture", run).await.unwrap();
    assert_eq!(again.status, Status::Ready);
    assert_eq!(counts.provider.load(Ordering::Relaxed), 1);
    let done = engine
        .execute(&person, "fixture", execution(&first))
        .await
        .unwrap();
    assert_eq!(done.status, Status::Succeeded, "{done:?}");
    assert_eq!(counts.provider.load(Ordering::Relaxed), 2);
    assert_eq!(counts.parser.load(Ordering::Relaxed), 1);
    let money: i64 = sqlx::query_scalar(
        "SELECT sum(actual_atomic)::bigint FROM gateway_v2.executions WHERE task_id=$1",
    )
    .bind(task)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(money, 138);
    let read = done
        .next_actions
        .iter()
        .find(|a| a.kind == "read_result")
        .unwrap();
    let read_req = ExecuteRequest {
        request_id: Uuid::new_v4(),
        action_id: read.action_id,
        state: done.state.clone().unwrap(),
        quote_ref: None,
        authorization_ref: None,
        idempotency_key: read.action_id,
        input: Value::Null,
    };
    let read_result = engine.execute(&person, "fixture", read_req).await.unwrap();
    assert!(read_result.result.is_some());
    assert_eq!(counts.provider.load(Ordering::Relaxed), 2);
    let follow = engine
        .discover(
            &person,
            DiscoverRequest {
                request_id: Uuid::new_v4(),
                question: "Get accounts again".into(),
                state: done.state.clone(),
                context_delta: Default::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(follow.proposal.as_ref().unwrap().steps.len(), 1);
    assert_eq!(counts.parser.load(Ordering::Relaxed), 2);
    let other = Principal {
        owner: Uuid::new_v4(),
        connection: None,
    };
    assert!(engine
        .view(&other, task, Uuid::new_v4(), None)
        .await
        .is_err());
    let (rev, t, _) = store.load(person.owner, task).await.unwrap();
    store.save(person.owner, task, rev, &t).await.unwrap();
    assert!(store.save(person.owner, task, rev, &t).await.is_err());
    let altered = execution(&follow);
    let mut forged = altered;
    forged.state.focus.entity_refs.push(Uuid::new_v4());
    let rejected = engine.execute(&person, "fixture", forged).await.unwrap();
    assert_eq!(rejected.status, Status::Failed);
    // Ambiguous results are selected from stored candidates, without another
    // parser call or a second search purchase.
    let amb = engine
        .discover(
            &person,
            DiscoverRequest {
                request_id: Uuid::new_v4(),
                question: "Accounts for Ambiguous".into(),
                state: None,
                context_delta: Default::default(),
            },
        )
        .await
        .unwrap();
    let ambtask = amb.state.as_ref().unwrap().state_ref;
    let q = amb.proposal.as_ref().unwrap();
    engine
        .executor
        .approve(&person, ambtask, q.quote_ref, &q.max_total_atomic)
        .await
        .unwrap();
    let choices = engine
        .execute(&person, "fixture", execution(&amb))
        .await
        .unwrap();
    assert_eq!(choices.status, Status::NeedsSelection, "{choices:?}");
    let pick = &choices
        .next_actions
        .iter()
        .find(|a| a.kind == "select_entity")
        .unwrap();
    let selection = choices.context_view["candidates"][1]["entity_ref"].clone();
    let before = counts.provider.load(Ordering::Relaxed);
    let selected = engine
        .execute(
            &person,
            "fixture",
            ExecuteRequest {
                request_id: Uuid::new_v4(),
                action_id: pick.action_id,
                state: choices.state.clone().unwrap(),
                quote_ref: None,
                authorization_ref: None,
                idempotency_key: pick.action_id,
                input: json!({"entity_ref":selection}),
            },
        )
        .await
        .unwrap();
    assert_eq!(selected.status, Status::Ready);
    assert_eq!(counts.provider.load(Ordering::Relaxed), before);
    assert_eq!(counts.parser.load(Ordering::Relaxed), 3);
    let finished = engine
        .execute(&person, "fixture", execution(&selected))
        .await
        .unwrap();
    assert_eq!(finished.status, Status::Succeeded);
    // Persisted source data must also be attached to the task before a new
    // intent may replace its recovery plan (crash between ingest and CAS).
    sqlx::query(
        "UPDATE gateway_v2.tasks SET document=jsonb_set(document,'{result_refs}','[]') WHERE id=$1",
    )
    .bind(ambtask)
    .execute(&pool)
    .await
    .unwrap();
    let parser_before = counts.parser.load(Ordering::Relaxed);
    let blocked = engine
        .discover(
            &person,
            DiscoverRequest {
                request_id: Uuid::new_v4(),
                question: "Get accounts again".into(),
                state: finished.state.clone(),
                context_delta: Default::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(blocked.status, Status::Failed);
    assert_eq!(counts.parser.load(Ordering::Relaxed), parser_before);
    // Simulate an unknown provider outcome, then recover its result solely
    // from the existing charge/run ledger. The provider is never redispatched.
    let timeout_task = store.create(person.owner, None).await.unwrap();
    let mut timeout_state = t.clone();
    for f in &mut timeout_state.facts {
        if f.semantic_type == "company.id" {
            f.value = json!("timeout")
        }
    }
    timeout_state.result_refs.clear();
    store
        .save(person.owner, timeout_task, 0, &timeout_state)
        .await
        .unwrap();
    let pending_start = engine
        .view(&person, timeout_task, Uuid::new_v4(), None)
        .await
        .unwrap();
    let q = pending_start.proposal.as_ref().unwrap();
    engine
        .executor
        .approve(&person, timeout_task, q.quote_ref, &q.max_total_atomic)
        .await
        .unwrap();
    let sent = execution(&pending_start);
    let sent_id = sent.idempotency_key;
    let pending = engine.execute(&person, "fixture", sent).await.unwrap();
    assert_eq!(pending.status, Status::Running, "{pending:?}");
    let journal = sqlx::query(
        "SELECT status,reserved_atomic,actual_atomic FROM gateway_v2.executions WHERE id=$1",
    )
    .bind(sent_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(journal.get::<String, _>("status"), "unresolved");
    assert_eq!(journal.get::<i64, _>("reserved_atomic"), 115);
    assert!(journal.get::<Option<i64>, _>("actual_atomic").is_none());
    let charge = Uuid::new_v4();
    sqlx::query("INSERT INTO public.balance_charges(id,user_id,idempotency_key,status,charged_micro_usd,actual_provider_micro_usd,provider_price_micro_usd,fee_micro_usd) VALUES($1,$2,$3,'charged',115,100,100,15)")
        .bind(charge)
        .bind(person.owner)
        .bind(sent_id.to_string())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO public.call_runs(id,charge_id,user_id,status,output,output_truncated) VALUES($1,$2,$3,'succeeded',$4,false)").bind(Uuid::new_v4()).bind(charge).bind(person.owner).bind(json!({"accounts":"2025"})).execute(&pool).await.unwrap();
    let poll = pending
        .next_actions
        .iter()
        .find(|a| a.kind == "poll")
        .unwrap();
    let before = counts.provider.load(Ordering::Relaxed);
    let recovered = engine
        .execute(
            &person,
            "fixture",
            ExecuteRequest {
                request_id: Uuid::new_v4(),
                action_id: poll.action_id,
                state: pending.state.clone().unwrap(),
                quote_ref: None,
                authorization_ref: None,
                idempotency_key: poll.action_id,
                input: Value::Null,
            },
        )
        .await
        .unwrap();
    assert_eq!(recovered.status, Status::Succeeded, "{recovered:?}");
    let bill = recovered.billing.as_ref().unwrap();
    assert_eq!(bill["status"], "captured");
    assert_eq!(bill["total_charged"], "115");
    assert_eq!(bill["provider_cost"], "100");
    assert_eq!(bill["apiosk_fee"], "15");
    assert_eq!(bill["executions"][0]["receipt_ref"], charge.to_string());
    sqlx::query("INSERT INTO public.user_balances VALUES($1,885)")
        .bind(person.owner)
        .execute(&pool)
        .await
        .unwrap();
    let fresh_bill = engine
        .view(&person, timeout_task, Uuid::new_v4(), None)
        .await
        .unwrap();
    assert_eq!(fresh_bill.billing.unwrap()["balance_available"], "885");
    // A later refund changes financial state even if the source result remains.
    sqlx::query("UPDATE public.balance_charges SET status='refunded' WHERE id=$1")
        .bind(charge)
        .execute(&pool)
        .await
        .unwrap();
    let refund = engine
        .view(&person, timeout_task, Uuid::new_v4(), None)
        .await
        .unwrap();
    assert_eq!(refund.billing.as_ref().unwrap()["status"], "refunded");
    assert_eq!(refund.billing.as_ref().unwrap()["total_charged"], "0");
    assert_eq!(counts.provider.load(Ordering::Relaxed), before);
    // A lower authorization cap cannot cause a partial overspend.
    let empty_task = store.create(person.owner, None).await.unwrap();
    let mut empty_state = timeout_state.clone();
    for action in &mut empty_state.actions {
        action["action_id"] = json!(Uuid::new_v4());
    }
    for fact in &mut empty_state.facts {
        if fact.semantic_type == "company.id" {
            fact.value = json!("no-balance");
        }
    }
    store
        .save(person.owner, empty_task, 0, &empty_state)
        .await
        .unwrap();
    let empty = engine
        .view(&person, empty_task, Uuid::new_v4(), None)
        .await
        .unwrap();
    let q = empty.proposal.as_ref().unwrap();
    engine
        .executor
        .approve(&person, empty_task, q.quote_ref, &q.max_total_atomic)
        .await
        .unwrap();
    let denied = engine
        .execute(&person, "fixture", execution(&empty))
        .await
        .unwrap();
    assert_eq!(denied.status, Status::Failed);
    assert_eq!(denied.errors[0]["code"], "insufficient_balance");
    assert_eq!(denied.billing.as_ref().unwrap()["status"], "released");
    assert_eq!(denied.billing.as_ref().unwrap()["total_charged"], "0");
    assert!(!denied.next_actions.iter().any(|a| a.kind == "poll"));
    assert_eq!(counts.provider.load(Ordering::Relaxed), before);
    let cap_task = store.create(person.owner, None).await.unwrap();
    for action in &mut timeout_state.actions {
        action["action_id"] = json!(Uuid::new_v4());
    }
    store
        .save(person.owner, cap_task, 0, &timeout_state)
        .await
        .unwrap();
    let capped = engine
        .view(&person, cap_task, Uuid::new_v4(), None)
        .await
        .unwrap();
    let q = capped.proposal.as_ref().unwrap();
    let auth = engine
        .executor
        .approve(&person, cap_task, q.quote_ref, &q.max_total_atomic)
        .await
        .unwrap();
    sqlx::query("UPDATE gateway_v2.authorizations SET cap_atomic=1 WHERE id=$1")
        .bind(auth)
        .execute(&pool)
        .await
        .unwrap();
    let refused = engine
        .execute(&person, "fixture", execution(&capped))
        .await
        .unwrap();
    assert_eq!(refused.status, Status::Failed);
    assert_eq!(counts.provider.load(Ordering::Relaxed), before);
    let cancelled = engine
        .execute(
            &person,
            "fixture",
            ExecuteRequest {
                request_id: Uuid::new_v4(),
                action_id: cap_task,
                state: capped.state.clone().unwrap(),
                quote_ref: None,
                authorization_ref: None,
                idempotency_key: Uuid::new_v4(),
                input: Value::Null,
            },
        )
        .await
        .unwrap();
    assert_eq!(cancelled.status, Status::Partial);
    assert!(!cancelled
        .next_actions
        .iter()
        .any(|a| a.kind == "execute_quoted_step"));
    assert_eq!(counts.provider.load(Ordering::Relaxed), before);
    let partial = engine
        .discover(
            &person,
            DiscoverRequest {
                request_id: Uuid::new_v4(),
                question: "Accounts for Partial".into(),
                state: None,
                context_delta: Default::default(),
            },
        )
        .await
        .unwrap();
    let q = partial.proposal.as_ref().unwrap();
    engine
        .executor
        .approve(
            &person,
            partial.state.as_ref().unwrap().state_ref,
            q.quote_ref,
            &q.max_total_atomic,
        )
        .await
        .unwrap();
    let partial = engine
        .execute(&person, "fixture", execution(&partial))
        .await
        .unwrap();
    let partial = engine
        .execute(&person, "fixture", execution(&partial))
        .await
        .unwrap();
    assert_eq!(partial.status, Status::Partial);
    let last = partial
        .next_actions
        .iter()
        .find(|a| a.kind == "read_result")
        .unwrap()
        .action_id;
    let source = store
        .result(
            person.owner,
            partial.state.as_ref().unwrap().state_ref,
            last,
        )
        .await
        .unwrap();
    assert_eq!(source["coverage"], "partial");
    assert!(!source["missing_fields"].as_array().unwrap().is_empty());
    assert_eq!(source["charged_atomic"], "115");
    let rls=sqlx::query("SELECT relrowsecurity FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='gateway_v2' AND c.relkind='r'").fetch_all(&pool).await.unwrap();
    assert!(rls.iter().all(|r| r.get::<bool, _>("relrowsecurity")));
    sqlx::query("SET ROLE gateway_v2_runtime")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(engine.catalog.fresh().await.unwrap().services.len(), 2);
    assert!(store.load(person.owner, task).await.is_ok());
    assert!(
        sqlx::query("SELECT provider_api_key_encrypted FROM public.apis")
            .fetch_all(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE public.balance_charges SET status='refunded' WHERE false")
            .execute(&pool)
            .await
            .is_err()
    );
    sqlx::query("RESET ROLE").execute(&pool).await.unwrap();
    sqlx::query("SET ROLE anon").execute(&pool).await.unwrap();
    assert!(sqlx::query("SELECT id FROM gateway_v2.tasks")
        .fetch_all(&pool)
        .await
        .is_err());
    sqlx::query("RESET ROLE").execute(&pool).await.unwrap();
    let blank = Task::default();
    assert!(blank.bounded());
    // Real MCP adapter -> actual Rust HTTP router/auth -> SQLx state ->
    // fixture provider, with separate human authorization through HTTP.
    let api = router(
        ServerState {
            engine: engine.clone(),
            auth: Arc::new(Auth {
                pool: pool.clone(),
                http,
                supabase_url: base,
                publishable_key: "fixture".into(),
            }),
            permits: Arc::new(tokio::sync::Semaphore::new(4)),
        },
        "http://localhost:5173",
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api_url = format!("http://{}", listener.local_addr().unwrap());
    let api_server = tokio::spawn(async move { axum::serve(listener, api).await.unwrap() });
    let smoke = tokio::process::Command::new("node")
        .arg("tests/mcp-http-smoke.mjs")
        .env("FIXTURE_GATEWAY_URL", &api_url)
        .output()
        .await
        .unwrap();
    assert!(
        smoke.status.success(),
        "MCP HTTP smoke failed: {} {}",
        String::from_utf8_lossy(&smoke.stdout),
        String::from_utf8_lossy(&smoke.stderr)
    );
    println!("{}", String::from_utf8_lossy(&smoke.stdout));
    let chats_before = counts.parser.load(Ordering::Relaxed);
    assert!(engine
        .chat(
            &agent,
            DiscoverRequest {
                request_id: Uuid::new_v4(),
                question: "Hallo".into(),
                state: None,
                context_delta: Default::default()
            }
        )
        .await
        .is_err());
    assert_eq!(
        counts.parser.load(Ordering::Relaxed),
        chats_before,
        "MCP callers must not enter the App chat layer"
    );
    let before = counts.provider.load(Ordering::Relaxed);
    let greeting = engine
        .chat(
            &person,
            DiscoverRequest {
                request_id: Uuid::new_v4(),
                question: "Hallo".into(),
                state: None,
                context_delta: Default::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(greeting.status, Status::NeedsInput);
    assert!(greeting.errors.is_empty());
    assert!(greeting.proposal.is_none());
    assert!(greeting.next_actions.is_empty());
    assert_eq!(
        greeting.context_view["conversation"][0]["reply"],
        "Hallo! Waar kan ik je mee helpen?"
    );
    let restored = engine
        .view(
            &person,
            greeting.state.as_ref().unwrap().state_ref,
            Uuid::new_v4(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(restored.status, Status::NeedsInput);
    assert_eq!(
        restored.context_view["conversation"],
        greeting.context_view["conversation"]
    );
    let history = store.list_tasks(person.owner).await.unwrap();
    let greeting_id = greeting.state.as_ref().unwrap().state_ref;
    let summary = history
        .iter()
        .find(|row| row["id"] == greeting_id.to_string())
        .unwrap();
    assert_eq!(summary["title"], "Hallo");
    assert_eq!(
        summary.as_object().unwrap().len(),
        3,
        "history must contain summaries only"
    );
    assert!(store.list_tasks(Uuid::new_v4()).await.unwrap().is_empty());
    let empty_id = store.create(person.owner, None).await.unwrap();
    assert!(!store
        .list_tasks(person.owner)
        .await
        .unwrap()
        .iter()
        .any(|row| row["id"] == empty_id.to_string()));
    sqlx::query("UPDATE gateway_v2.tasks SET expires_at=now()-interval '1 day' WHERE id=$1")
        .bind(greeting_id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(!store
        .list_tasks(person.owner)
        .await
        .unwrap()
        .iter()
        .any(|row| row["id"] == greeting_id.to_string()));
    sqlx::query("UPDATE gateway_v2.tasks SET expires_at=now()+interval '1 day' WHERE id=$1")
        .bind(greeting_id)
        .execute(&pool)
        .await
        .unwrap();
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .get(format!("{api_url}/v2/tasks"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(format!("{api_url}/v2/tasks"))
            .bearer_auth("apk_access_local_fixture")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let response = client
        .get(format!("{api_url}/v2/tasks"))
        .bearer_auth("fixture-human")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert!(
        !body["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == greeting_id.to_string()),
        "a different authenticated person must not see this chat"
    );
    let planned = engine
        .chat(
            &person,
            DiscoverRequest {
                request_id: Uuid::new_v4(),
                question: "Get accounts for Example".into(),
                state: restored.state,
                context_delta: Default::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(planned.status, Status::Ready);
    assert_eq!(planned.proposal.unwrap().max_total_atomic, "138");
    let chatting = engine
        .chat(
            &person,
            DiscoverRequest {
                request_id: Uuid::new_v4(),
                question: "Hallo".into(),
                state: planned.state,
                context_delta: Default::default(),
            },
        )
        .await
        .unwrap();
    assert!(chatting.proposal.is_none());
    assert!(
        chatting.next_actions.is_empty(),
        "conversation must not retain a stale purchase action"
    );
    assert_eq!(counts.provider.load(Ordering::Relaxed), before);
    // Actual fixture purchase -> related service -> fresh approval -> second result.
    let lookup = engine
        .discover(
            &person,
            DiscoverRequest {
                request_id: Uuid::new_v4(),
                question: "Search only Example".into(),
                state: None,
                context_delta: Default::default(),
            },
        )
        .await
        .unwrap();
    let lookup_plan = lookup.proposal.as_ref().unwrap();
    engine
        .executor
        .approve(
            &person,
            lookup.state.as_ref().unwrap().state_ref,
            lookup_plan.quote_ref,
            &lookup_plan.max_total_atomic,
        )
        .await
        .unwrap();
    let found = engine
        .execute(&person, "fixture", execution(&lookup))
        .await
        .unwrap();
    assert_eq!(found.status, Status::Succeeded);
    let suggestions = found.context_view["related_services"].as_array().unwrap();
    assert_eq!(suggestions.len(), 1);
    assert_eq!(suggestions[0]["title"], "accounts");
    assert_eq!(suggestions[0]["total_atomic"], "115");
    let followup_request = crate_followup_request(&found, &suggestions[0]);
    let provider_before_preview = counts.provider.load(Ordering::Relaxed);
    let parser_before_preview = counts.parser.load(Ordering::Relaxed);
    let preview = engine.followup(&person, followup_request).await.unwrap();
    assert_eq!(preview.status, Status::Ready);
    assert_eq!(preview.proposal.as_ref().unwrap().steps.len(), 1);
    assert_eq!(preview.proposal.as_ref().unwrap().max_total_atomic, "115");
    assert_ne!(
        preview.proposal.as_ref().unwrap().quote_ref,
        lookup_plan.quote_ref
    );
    assert_eq!(
        counts.provider.load(Ordering::Relaxed),
        provider_before_preview
    );
    assert_eq!(counts.parser.load(Ordering::Relaxed), parser_before_preview);
    let blocked = engine
        .execute(&person, "fixture", execution(&preview))
        .await
        .unwrap();
    assert_eq!(
        blocked.status,
        Status::RequiresApproval,
        "earlier approval cannot authorize the follow-up"
    );
    assert_eq!(
        counts.provider.load(Ordering::Relaxed),
        provider_before_preview
    );
    let quote = preview.proposal.as_ref().unwrap();
    engine
        .executor
        .approve(
            &person,
            preview.state.as_ref().unwrap().state_ref,
            quote.quote_ref,
            &quote.max_total_atomic,
        )
        .await
        .unwrap();
    let followup_done = engine
        .execute(&person, "fixture", execution(&preview))
        .await
        .unwrap();
    assert_eq!(followup_done.status, Status::Succeeded);
    assert_eq!(
        counts.provider.load(Ordering::Relaxed),
        provider_before_preview + 1
    );
    assert!(
        followup_done.context_view["related_services"]
            .as_array()
            .unwrap()
            .is_empty(),
        "do not sell the same endpoint again"
    );
    assert!(engine
        .followup(&agent, crate_followup_request(&found, &suggestions[0]))
        .await
        .is_err());
    let cross_account = engine
        .followup(&other, crate_followup_request(&found, &suggestions[0]))
        .await
        .unwrap();
    assert_eq!(cross_account.status, Status::Failed);
    let stale = engine
        .followup(&person, crate_followup_request(&found, &suggestions[0]))
        .await
        .unwrap();
    assert_eq!(stale.status, Status::StateConflict);
    api_server.abort();
    println!(
        "fixture scenarios elapsed_ms={} parser_calls={} provider_calls={}; initial two-step cap/charge=138 atomic",
        start.elapsed().as_millis(),counts.parser.load(Ordering::Relaxed),counts.provider.load(Ordering::Relaxed)
    );
    server.abort();
    pool.close().await;
}

fn crate_followup_request(
    response: &apiosk_gateway_v2::protocol::Response,
    suggestion: &Value,
) -> apiosk_gateway_v2::engine::FollowupRequest {
    apiosk_gateway_v2::engine::FollowupRequest {
        request_id: Uuid::new_v4(),
        state: response.state.clone().unwrap(),
        service_ref: suggestion["service_ref"].as_str().unwrap().parse().unwrap(),
        entity_ref: suggestion["entity_ref"].as_str().unwrap().parse().unwrap(),
    }
}
