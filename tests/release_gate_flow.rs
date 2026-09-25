//! The release gate: the ways a real session goes wrong, through the real
//! router, worker and payment journal on a fresh PGlite, with a fake model
//! and a fake Edge provider that keeps a ledger with per-key idempotency like
//! `use-selection`. Each scenario ends with the four invariants of
//! `support/gate.rs` on every task it created. Run by
//! scripts/test-release-gate.mjs (PGTEST_PORT, default 54350), one scenario
//! at a time on the one PGlite connection.

#[path = "support/gate.rs"]
mod gate;

use apiosk_gateway_v2::{
    auth::{Auth, Principal},
    catalog::CatalogCache,
    engine::Engine,
    execution::Executor,
    parser::Parser,
    server::{router, ServerState},
    state::Signer,
    store::Store,
};
use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering::SeqCst},
        Arc, Mutex,
    },
    time::Duration,
};
use uuid::Uuid;

/// The model and the Edge provider, with counters.
struct Fake {
    pool: PgPool,
    chat: AtomicU64,
    analysis: AtomicU64,
    /// The largest analysis input the model received, in bytes.
    analysis_input: AtomicU64,
    provider: AtomicU64,
    replayed: AtomicU64,
    /// Calls whose connection dropped before the Edge function settled.
    parked: Mutex<Vec<Parked>>,
}

/// A paid call the Edge function has not settled yet.
struct Parked {
    owner: Uuid,
    key: String,
    result: Value,
    charged: i64,
    cost: i64,
    fee: i64,
}

impl Fake {
    fn counts(&self) -> (u64, u64, u64) {
        (self.chat.load(SeqCst), self.analysis.load(SeqCst), self.provider.load(SeqCst))
    }
    /// The Edge function finishes a parked call and writes its ledger entry.
    async fn settle_parked(&self) {
        let parked = std::mem::take(&mut *self.parked.lock().unwrap());
        for call in parked {
            record(&self.pool, call.owner, &call.key, &call.result, call.charged, call.cost, call.fee).await;
        }
    }
}

fn bearer_owner(headers: &HeaderMap) -> Option<Uuid> {
    headers.get("authorization")?.to_str().ok()?.strip_prefix("Bearer gate-")?.parse().ok()
}

async fn user(headers: HeaderMap) -> (StatusCode, Json<Value>) {
    match bearer_owner(&headers) {
        Some(owner) => (StatusCode::OK, Json(json!({"id":owner}))),
        None => (StatusCode::UNAUTHORIZED, Json(json!({}))),
    }
}

async fn model(State(fake): State<Arc<Fake>>, Json(body): Json<Value>) -> Json<Value> {
    let input: Value = serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    if input.get("answer_operations").is_some() {
        fake.analysis.fetch_add(1, SeqCst);
        fake.analysis_input.fetch_max(body["messages"][1]["content"].as_str().unwrap().len() as u64, SeqCst);
        let question = input["answer_operations"][0].as_str().unwrap_or_default();
        let sources = input["sources"].as_array().unwrap();
        // A ledger line named in the question, wherever the evidence selection kept it.
        let named = sources.iter().find_map(|source| {
            let rows: Vec<(String, &Value)> = match &source["document"]["data"]["rows"] {
                Value::Object(kept) => kept.iter().map(|(index, row)| (index.clone(), row)).collect(),
                Value::Array(all) => all.iter().enumerate().map(|(index, row)| (index.to_string(), row)).collect(),
                _ => return None,
            };
            rows.into_iter().find(|(_, row)| row["line"].as_str().is_some_and(|line| question.contains(line)))
                .map(|(index, _)| (source["result_ref"].clone(), index))
        });
        let cited: Vec<_> = sources.iter().filter(|source| source["document"]["data"]["accounts"].is_string()).collect();
        let answer = match (named, cited.first()) {
            (Some((result, index)), _) => json!({"observations":[{"text":"The ledger reports {{a}} for {{b}}.","evidence":[
                {"result_ref":result,"pointer":format!("/data/rows/{index}/value_eur")},{"result_ref":result,"pointer":format!("/data/rows/{index}/line")}]}],"limitations":[]}),
            (None, Some(source)) => json!({"observations":[{"text":"The registry reports accounts for {{a}}.",
                "evidence":[{"result_ref":source["result_ref"],"pointer":"/data/accounts"}]}],"limitations":[]}),
            (None, None) => json!({"observations":[],"limitations":["No saved source reports accounts."]}),
        };
        return Json(json!({"choices":[{"message":{"content":answer.to_string()}}]}));
    }
    fake.chat.fetch_add(1, SeqCst);
    // "Jaarrekening van <Label>": the label names the fixture behaviour.
    let label = input["question"].as_str().unwrap().rsplit(' ').next().unwrap().to_owned();
    let intent = json!({"subjects":[{"entity_ref":null,"label":label,"values":{"company.name":label}}],
        "goals":[{"capability":"company.accounts","subject":0,"constraints":{}}],"answer_operations":[],"unresolved_references":[]});
    let content = if input["surface"] == "ask-chat" {
        json!({"message":"Ik zoek de jaarrekening op en toon eerst de prijs.","discover":true,"intent":intent})
    } else {
        intent
    };
    Json(json!({"choices":[{"message":{"content":content.to_string()}}]}))
}

async fn record(pool: &PgPool, owner: Uuid, key: &str, result: &Value, charged: i64, cost: i64, fee: i64) {
    let charge = Uuid::new_v4();
    sqlx::query("INSERT INTO public.balance_charges(id,user_id,idempotency_key,status,charged_micro_usd,actual_provider_micro_usd,provider_price_micro_usd,fee_micro_usd) VALUES($1,$2,$3,'charged',$4,$5,$5,$6)")
        .bind(charge).bind(owner).bind(key).bind(charged).bind(cost).bind(fee).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO public.call_runs(id,charge_id,user_id,status,output,output_truncated) VALUES($1,$2,$3,'succeeded',$4,false)")
        .bind(Uuid::new_v4()).bind(charge).bind(owner).bind(result).execute(pool).await.unwrap();
}

/// A response cut off mid-body: the gateway cannot tell whether it was paid.
fn cut_off() -> Response {
    (StatusCode::OK, [(header::CONTENT_TYPE, "application/json")], "{\"ok\":true,\"charged_micro_usd\":").into_response()
}

/// The Edge `use-selection` contract: one charge per idempotency key; a
/// repeated key replays the saved outcome and charges nothing.
async fn provider(State(fake): State<Arc<Fake>>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    fake.provider.fetch_add(1, SeqCst);
    assert_eq!(body["deterministic"], true);
    let owner = bearer_owner(&headers).expect("the gateway forwards the person's own bearer");
    let key = body["idempotency_key"].as_str().unwrap().to_owned();
    let company = body["inputs"]["company.id"].as_str().unwrap_or_default().to_owned();
    let (result, charged, cost, fee) = if let Some(name) = body["inputs"]["company.name"].as_str() {
        (json!({"companies":[{"id":name.to_lowercase()}]}), 23, 20, 3)
    } else if company == "long" {
        let rows: Vec<_> = (0..5000).map(|row| json!({"row":row,"period":"2025","line":format!("Balance sheet line {row:04}"),"value_eur":row * 1000})).collect();
        (json!({"accounts":"2025","rows":rows}), 115, 100, 15)
    } else {
        (json!({"accounts":"2025"}), 115, 100, 15)
    };
    let saved: Option<(i64, i64, i64)> = sqlx::query_as("SELECT charged_micro_usd,provider_price_micro_usd,fee_micro_usd FROM public.balance_charges WHERE user_id=$1 AND idempotency_key=$2")
        .bind(owner).bind(&key).fetch_optional(&fake.pool).await.unwrap();
    if let Some((charged, cost, fee)) = saved {
        fake.replayed.fetch_add(1, SeqCst);
        return Json(json!({"ok":true,"charged_micro_usd":charged,"provider_micro_usd":cost,"fee_micro_usd":fee,"result":result,"result_truncated":false,"replayed":true})).into_response();
    }
    match company.as_str() {
        // The connection drops while the upstream call is still running.
        "vanished" => {
            fake.parked.lock().unwrap().push(Parked { owner, key, result, charged, cost, fee });
            cut_off()
        }
        // The call is paid and saved, then the response is lost.
        "lost" => {
            record(&fake.pool, owner, &key, &result, charged, cost, fee).await;
            cut_off()
        }
        _ => {
            record(&fake.pool, owner, &key, &result, charged, cost, fee).await;
            Json(json!({"ok":true,"charged_micro_usd":charged,"provider_micro_usd":cost,"fee_micro_usd":fee,"result":result,"result_truncated":false,"replayed":false})).into_response()
        }
    }
}

#[derive(Clone)]
struct Person {
    owner: Uuid,
    bearer: String,
    workspace: Option<Uuid>,
}
impl Person {
    fn new() -> Self {
        let owner = Uuid::new_v4();
        Self { owner, bearer: format!("gate-{owner}"), workspace: None }
    }
    fn principal(&self) -> Principal {
        Principal { owner: self.owner, connection: None, workspace: self.workspace }
    }
    fn in_workspace(&self, workspace: Option<Uuid>) -> Self {
        Self { workspace, ..self.clone() }
    }
}

struct Gate {
    pool: PgPool,
    fake: Arc<Fake>,
    engine: Arc<Engine>,
    gateway: String,
    http: reqwest::Client,
}

impl Gate {
    async fn start() -> Self {
        std::env::set_var("WORKSPACE_CONTEXT_ENABLED", "true");
        let pool = PgPoolOptions::new()
            .after_connect(|conn, _| Box::pin(async move {
                sqlx::query("SET search_path TO public, gateway_v2").execute(conn).await?;
                Ok(())
            }))
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(10))
            .connect(&std::env::var("TEST_DATABASE_URL").unwrap())
            .await
            .unwrap();
        // Every scenario starts from the seeded prices and the workspace fixture.
        sqlx::raw_sql("UPDATE public.api_endpoints SET price=0.00002 WHERE path='/company.search';
            UPDATE public.api_endpoints SET price=0.0001 WHERE path='/company.accounts';
            CREATE TABLE IF NOT EXISTS public.gate_workspace_access(actor uuid,workspace uuid,active boolean);
            CREATE TABLE IF NOT EXISTS public.gate_workspace_bindings(workspace uuid,selection uuid,task uuid,amount bigint);
            CREATE OR REPLACE FUNCTION public.workspace_gateway_context(a uuid,c uuid,w uuid) RETURNS jsonb LANGUAGE plpgsql AS $$ BEGIN
              IF w IS NULL THEN RETURN jsonb_build_object('workspace_id',NULL); END IF;
              IF NOT EXISTS(SELECT 1 FROM public.gate_workspace_access WHERE actor=a AND workspace=w AND active) THEN RAISE EXCEPTION 'forbidden' USING ERRCODE='42501'; END IF;
              RETURN jsonb_build_object('workspace_id',w,'name','Gate workspace','can_spend',true); END $$;
            CREATE OR REPLACE FUNCTION public.workspace_bind_execution(a uuid,c uuid,w uuid,s uuid,t uuid,amount bigint,until timestamptz) RETURNS void LANGUAGE plpgsql AS $$ BEGIN
              IF NOT EXISTS(SELECT 1 FROM public.gate_workspace_access WHERE actor=a AND workspace=w AND active) THEN RAISE EXCEPTION 'forbidden' USING ERRCODE='42501'; END IF;
              INSERT INTO public.gate_workspace_bindings VALUES(w,s,t,amount); END $$;")
            .execute(&pool).await.unwrap();
        let fake = Arc::new(Fake {
            pool: pool.clone(), chat: AtomicU64::new(0), analysis: AtomicU64::new(0), analysis_input: AtomicU64::new(0), provider: AtomicU64::new(0),
            replayed: AtomicU64::new(0), parked: Mutex::new(Vec::new()),
        });
        let mock = Router::new()
            .route("/chat", post(model))
            .route("/functions/v1/use-selection", post(provider))
            .route("/auth/v1/user", get(user))
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
        let http = reqwest::Client::new();
        let store = Store { pool: pool.clone() };
        let engine = Arc::new(Engine {
            store: store.clone(),
            catalog: CatalogCache::new(pool.clone()),
            parser: Parser::new(http.clone(), format!("{base}/chat"), "fixture".into(), "fixture".into()),
            signer: Signer::new(vec![6; 32]).unwrap(),
            executor: Executor { store, http: http.clone(), supabase_url: base.clone(), enabled: true },
            app_url: "http://localhost:5173".into(),
        });
        let app = router(ServerState {
            engine: engine.clone(),
            auth: Arc::new(Auth { pool: pool.clone(), http: http.clone(), supabase_url: base, publishable_key: "fixture".into() }),
            permits: Arc::new(tokio::sync::Semaphore::new(16)),
            executor_compat: None,
        }, "http://localhost:5173").unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { pool, fake, engine, gateway, http }
    }

    async fn send(&self, person: &Person, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let url = format!("{}{path}", self.gateway);
        let mut request = match &body {
            Some(body) => self.http.post(url).json(body),
            None => self.http.get(url),
        }.bearer_auth(&person.bearer);
        if let Some(workspace) = person.workspace {
            request = request.header("x-apiosk-workspace", workspace.to_string());
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn chat(&self, person: &Person, request_id: Uuid, question: &str) -> Value {
        let (status, body) = self.send(person, "/v2/chat", Some(json!({"request_id":request_id,"question":question,"state":null}))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    /// A priced question: the task, its quote and the chat response.
    async fn priced(&self, person: &Person, label: &str) -> (Uuid, Value, Value) {
        self.priced_question(person, &format!("Jaarrekening van {label}")).await
    }

    /// A priced question whose last word is the fixture label.
    async fn priced_question(&self, person: &Person, question: &str) -> (Uuid, Value, Value) {
        let priced = self.chat(person, Uuid::new_v4(), question).await;
        assert_eq!(priced["status"], "ready", "{priced}");
        assert_eq!(priced["proposal"]["max_total_atomic"], "138", "search 23 and accounts 115, to the micro-USD");
        (task_of(&priced), priced["proposal"].clone(), priced)
    }

    async fn authorize(&self, person: &Person, task: Uuid, proposal: &Value) -> (StatusCode, Value) {
        self.send(person, &format!("/v2/tasks/{task}/authorize"),
            Some(json!({"quote_ref":proposal["quote_ref"],"max_total_atomic":proposal["max_total_atomic"]}))).await
    }

    async fn requote(&self, person: &Person, task: Uuid, state: &Value, quote_ref: &Value) -> (StatusCode, Value) {
        self.send(person, &format!("/v2/tasks/{task}/requote"),
            Some(json!({"request_id":Uuid::new_v4(),"state":state,"quote_ref":quote_ref}))).await
    }

    async fn view(&self, person: &Person, task: Uuid) -> (StatusCode, Value) {
        self.send(person, &format!("/v2/tasks/{task}"), None).await
    }

    /// Waits for the server worker to reach a saved outcome.
    async fn settled(&self, person: &Person, task: Uuid) -> Value {
        let mut view = Value::Null;
        for _ in 0..300 {
            view = self.view(person, task).await.1;
            let terminal = ["succeeded", "partial", "failed", "cancelled", "unsupported", "needs_input", "needs_selection"]
                .contains(&view["status"].as_str().unwrap_or_default());
            if terminal && view["context_view"]["worker_active"] != true && view["context_view"]["analyzing"] != true {
                return view;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("task {task} never settled: {view}");
    }

    async fn executions(&self, task: Uuid) -> Vec<String> {
        sqlx::query_scalar("SELECT status FROM executions WHERE task_id=$1 ORDER BY created_at").bind(task).fetch_all(&self.pool).await.unwrap()
    }

    async fn ledger(&self, task: Uuid) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM public.balance_charges c JOIN executions e ON c.idempotency_key=e.id::text WHERE e.task_id=$1")
            .bind(task).fetch_one(&self.pool).await.unwrap()
    }
}

fn task_of(response: &Value) -> Uuid {
    response["state"]["state_ref"].as_str().unwrap().parse().unwrap()
}

fn execute_body(response: &Value, kind: &str, input: Value) -> Value {
    let action = response["next_actions"].as_array().unwrap().iter().find(|action| action["kind"] == kind)
        .unwrap_or_else(|| panic!("no {kind} action: {response}"));
    json!({"request_id":Uuid::new_v4(),"action_id":action["action_id"],"state":response["state"],
        "quote_ref":response["proposal"]["quote_ref"],"authorization_ref":null,"idempotency_key":action["action_id"],"input":input})
}

/// A reload or a lost response re-sends what the page last sent: the same
/// chat request replays its saved reply, a second approval is the first one,
/// and a re-sent execute returns its receipt. Nothing is interpreted, bought
/// or charged twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-release-gate.mjs); no live database"]
async fn gate_reload_replays_receipts() {
    let gate = Gate::start().await;
    let person = Person::new();
    let request = Uuid::new_v4();
    let priced = gate.chat(&person, request, "Jaarrekening van Reload").await;
    assert_eq!(priced["status"], "ready", "{priced}");
    let task = task_of(&priced);
    let proposal = priced["proposal"].clone();
    let replayed = gate.chat(&person, request, "Jaarrekening van Reload").await;
    assert_eq!((task_of(&replayed), &replayed["proposal"]["quote_ref"]), (task, &proposal["quote_ref"]), "the same request returns its saved reply");
    assert_eq!(gate.fake.counts(), (1, 0, 0), "a replayed question is not interpreted again");

    assert_eq!(gate.authorize(&person, task, &proposal).await.0, StatusCode::OK);
    let answered = gate.settled(&person, task).await;
    assert_eq!(answered["status"], "succeeded", "{answered}");
    assert_eq!(gate.fake.counts(), (1, 1, 2), "one interpretation, one analysis, search then accounts once each");

    // The page reloads: it approves again, re-sends its last execute twice and reads the task.
    assert_eq!(gate.authorize(&person, task, &proposal).await.0, StatusCode::OK, "a repeated approval is the saved approval");
    let resend = execute_body(&priced, "execute_quoted_step", Value::Null);
    let (first_status, first) = gate.send(&person, "/v2/execute", Some(resend.clone())).await;
    let (second_status, second) = gate.send(&person, "/v2/execute", Some(resend)).await;
    assert_eq!((first_status, &first), (second_status, &second), "the same execute request returns its receipt");
    let reread = gate.view(&person, task).await.1;
    assert_eq!(reread["context_view"]["analysis"], answered["context_view"]["analysis"]);
    assert_eq!(gate.fake.counts(), (1, 1, 2), "reload, re-approval, re-sent execute and reads add no model or source call");
    assert_eq!(gate.fake.replayed.load(SeqCst), 0, "nothing even reached the provider again");
    assert_eq!(gate.ledger(task).await, 2);
    assert_eq!(gate::assert_all(&gate.pool, task, &proposal, &answered).await, 1, "the answer cites its source");
    println!("gate_reload_replays_receipts: 1 interpretation, 1 approval, 2 source calls, 2 charges");
}

/// The Edge response is lost. With the charge already in the ledger the
/// worker settles from it at once; with the call still running the execution
/// stays unresolved and is polled, never dispatched again, until the ledger
/// shows the outcome.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-release-gate.mjs); no live database"]
async fn gate_lost_response_recovery() {
    let gate = Gate::start().await;
    let person = Person::new();
    let (lost, lost_quote, _) = gate.priced(&person, "Lost").await;
    assert_eq!(gate.authorize(&person, lost, &lost_quote).await.0, StatusCode::OK);
    let recovered = gate.settled(&person, lost).await;
    assert_eq!(recovered["status"], "succeeded", "{recovered}");
    assert_eq!(gate.fake.provider.load(SeqCst), 2, "the lost call was settled from the ledger, not bought again");
    let reconciled: bool = sqlx::query_scalar("SELECT bool_or((response->>'reconciled')::boolean) FROM executions WHERE task_id=$1")
        .bind(lost).fetch_one(&gate.pool).await.unwrap();
    assert!(reconciled, "the accounts receipt came from the ledger");
    gate::assert_all(&gate.pool, lost, &lost_quote, &recovered).await;

    let (vanished, vanished_quote, _) = gate.priced(&person, "Vanished").await;
    assert_eq!(gate.authorize(&person, vanished, &vanished_quote).await.0, StatusCode::OK);
    for _ in 0..100 {
        if gate.executions(vanished).await.last().map(String::as_str) == Some("unresolved") { break; }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(gate.executions(vanished).await, ["succeeded", "unresolved"], "an unknown outcome stays unresolved");
    let before = gate.fake.provider.load(SeqCst);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(gate.fake.provider.load(SeqCst), before, "no new paid dispatch while the outcome is unknown");
    let pending = gate.view(&person, vanished).await.1;
    assert_ne!(pending["status"], "succeeded", "{pending}");
    assert!(pending["context_view"]["analysis"].is_null(), "no answer before the outcome is known");
    gate.fake.settle_parked().await;
    let settled = gate.settled(&person, vanished).await;
    assert_eq!(settled["status"], "succeeded", "{settled}");
    assert_eq!(gate.fake.provider.load(SeqCst), 4, "each call reached the provider once");
    assert_eq!(gate.fake.replayed.load(SeqCst), 0);
    gate::assert_all(&gate.pool, vanished, &vanished_quote, &settled).await;
    println!("gate_lost_response_recovery: lost response settled from the ledger; unknown outcome polled without redispatch");
}

/// Two tabs approve the same quote at once, and two workers race for one
/// task: one approval, one lease, one call per step.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-release-gate.mjs); no live database"]
async fn gate_parallel_approval_and_worker() {
    let gate = Gate::start().await;
    let person = Person::new();
    let (tabs, tabs_quote, _) = gate.priced(&person, "Tabs").await;
    let ((first, _), (second, _)) = tokio::join!(gate.authorize(&person, tabs, &tabs_quote), gate.authorize(&person, tabs, &tabs_quote));
    assert_eq!((first, second), (StatusCode::OK, StatusCode::OK));
    let answered = gate.settled(&person, tabs).await;
    assert_eq!(answered["status"], "succeeded", "{answered}");
    assert_eq!(gate.fake.provider.load(SeqCst), 2, "provider calls equal the quoted steps");
    gate::assert_all(&gate.pool, tabs, &tabs_quote, &answered).await;

    let (workers, workers_quote, _) = gate.priced(&person, "Workers").await;
    let principal = person.principal();
    let quote: Uuid = workers_quote["quote_ref"].as_str().unwrap().parse().unwrap();
    let cap = workers_quote["max_total_atomic"].as_str().unwrap();
    let (a, b) = tokio::join!(gate.engine.executor.approve(&principal, workers, quote, cap), gate.engine.executor.approve(&principal, workers, quote, cap));
    assert_eq!(a.unwrap(), b.unwrap(), "parallel approvals return the same authorization");
    let (a, b) = tokio::join!(
        gate.engine.start_worker(principal.clone(), person.bearer.clone(), workers),
        gate.engine.start_worker(principal.clone(), person.bearer.clone(), workers));
    a.unwrap();
    b.unwrap();
    let leases: i64 = sqlx::query_scalar("SELECT count(*) FROM execution_leases WHERE task_id=$1").bind(workers).fetch_one(&gate.pool).await.unwrap();
    assert!(leases <= 1, "one lease per task");
    let answered = gate.settled(&person, workers).await;
    assert_eq!(answered["status"], "succeeded", "{answered}");
    assert_eq!(gate.fake.provider.load(SeqCst), 4, "the second worker bought nothing");
    assert_eq!(gate.fake.replayed.load(SeqCst), 0);
    gate::assert_all(&gate.pool, workers, &workers_quote, &answered).await;
    println!("gate_parallel_approval_and_worker: 2 tabs, 2 workers; 1 approval and 2 source calls per task");
}

/// A task belongs to the workspace it was asked in. Another workspace of the
/// same person cannot read, approve, execute or list it; an event link is
/// bound to its task and owner; removed access is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-release-gate.mjs); no live database"]
async fn gate_workspace_isolation() {
    let gate = Gate::start().await;
    let person = Person::new();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    for workspace in [a, b] {
        sqlx::query("INSERT INTO public.gate_workspace_access VALUES($1,$2,true)").bind(person.owner).bind(workspace).execute(&gate.pool).await.unwrap();
    }
    let (in_a, in_b, personal) = (person.in_workspace(Some(a)), person.in_workspace(Some(b)), person.in_workspace(None));
    let (task, proposal, priced) = gate.priced(&in_a, "Workspace").await;

    for (who, name) in [(&in_b, "another workspace"), (&personal, "the personal account")] {
        let (status, body) = gate.view(who, task).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{name} cannot read the task: {body}");
        assert!(!body.to_string().contains("Workspace"), "{name}: nothing of the task leaks");
        assert_eq!(gate.authorize(who, task, &proposal).await.0, StatusCode::FORBIDDEN, "{name} cannot approve");
        let (status, _) = gate.send(who, "/v2/execute", Some(execute_body(&priced, "execute_quoted_step", Value::Null))).await;
        assert!(!status.is_success(), "{name} cannot execute");
        let (_, listed) = gate.send(who, "/v2/tasks", None).await;
        assert!(!listed.to_string().contains(&task.to_string()), "{name} does not list the task");
    }
    gate::assert_no_approval(&gate.pool, task).await;

    // The event link reads only its own task for its own owner.
    let events = priced["context_view"]["events_path"].as_str().unwrap().to_owned();
    let other = gate.priced(&in_b, "Other").await.0;
    let forged_task = events.replacen(&task.to_string(), &other.to_string(), 1);
    let forged_owner = events.replacen(&person.owner.to_string(), &Uuid::new_v4().to_string(), 1);
    for forged in [forged_task, forged_owner] {
        let status = gate.http.get(format!("{}{forged}", gate.gateway)).send().await.unwrap().status();
        assert_eq!(status, StatusCode::FORBIDDEN, "a changed event link is refused");
    }
    let mut stream = gate.http.get(format!("{}{events}&compact=true", gate.gateway)).send().await.unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    let first = String::from_utf8(stream.chunk().await.unwrap().unwrap().to_vec()).unwrap();
    assert!(first.contains(&task.to_string()) && !first.contains(&other.to_string()), "the stream carries its own task only");
    drop(stream);

    assert_eq!(gate.authorize(&in_a, task, &proposal).await.0, StatusCode::OK);
    let answered = gate.settled(&in_a, task).await;
    assert_eq!(answered["status"], "succeeded", "{answered}");
    let bound: Vec<Uuid> = sqlx::query_scalar("SELECT workspace FROM public.gate_workspace_bindings WHERE task=$1").bind(task).fetch_all(&gate.pool).await.unwrap();
    assert_eq!(bound, [a, a], "both calls are paid by the task's workspace");
    assert_eq!(gate.view(&in_b, task).await.0, StatusCode::NOT_FOUND, "the answer stays in its workspace");
    gate::assert_all(&gate.pool, task, &proposal, &answered).await;

    sqlx::query("UPDATE public.gate_workspace_access SET active=false WHERE actor=$1 AND workspace=$2").bind(person.owner).bind(a).execute(&gate.pool).await.unwrap();
    let (status, body) = gate.view(&in_a, task).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "removed access is refused: {body}");
    assert_eq!(body["errors"][0]["code"], "workspace_access_denied", "{body}");
    assert_eq!(gate.fake.provider.load(SeqCst), 2);
    println!("gate_workspace_isolation: read, approve, execute, list and events refused across workspaces");
}

/// An expired quote is never approved, and an approval that expires before
/// its worker starts dispatches nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-release-gate.mjs); no live database"]
async fn gate_expired_quote_never_dispatches() {
    let gate = Gate::start().await;
    let person = Person::new();
    let expire = "UPDATE tasks SET document=jsonb_set(document,'{proposal,expires_at}',to_jsonb(now()-interval '1 minute')) WHERE id=$1";

    let (stale, stale_quote, stale_priced) = gate.priced(&person, "Expired").await;
    sqlx::query(expire).bind(stale).execute(&gate.pool).await.unwrap();
    let (status, body) = gate.authorize(&person, stale, &stale_quote).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "an expired quote cannot be approved: {body}");
    let view = gate.view(&person, stale).await.1;
    assert_eq!((&view["billing"]["authorization_active"], &view["billing"]["total_charged"]), (&json!(false), &json!("0")), "{view}");
    gate::assert_no_approval(&gate.pool, stale).await;

    // #23: refreshing that same expired quote issues a new one from the
    // stored intent, with no model call and still no approval or spend.
    let chats_before = gate.fake.chat.load(SeqCst);
    let (status, refreshed) = gate.requote(&person, stale, &stale_priced["state"], &stale_quote["quote_ref"]).await;
    assert_eq!(status, StatusCode::OK, "{refreshed}");
    assert_eq!(refreshed["status"], "ready", "{refreshed}");
    assert_eq!(refreshed["context_view"]["requote"]["changed"], false, "the same steps still bind");
    assert_eq!(refreshed["context_view"]["requote"]["previous_quote_ref"], stale_quote["quote_ref"]);
    assert_ne!(refreshed["proposal"]["quote_ref"], stale_quote["quote_ref"], "a refresh always issues a new quote_ref");
    assert_eq!(gate.fake.chat.load(SeqCst), chats_before, "no model call to refresh a quote");
    gate::assert_no_approval(&gate.pool, stale).await;
    let restale = "UPDATE tasks SET document=jsonb_set(document,'{proposal,expires_at}',to_jsonb(now()-interval '1 minute')) WHERE id=$1";
    sqlx::query(restale).bind(stale).execute(&gate.pool).await.unwrap();
    let (status, mismatched) = gate.requote(&person, stale, &refreshed["state"], &stale_quote["quote_ref"]).await;
    assert_eq!(status, StatusCode::OK, "{mismatched}");
    assert_eq!(mismatched["status"], "state_conflict", "the stale quote_ref no longer matches: {mismatched}");
    assert_eq!(mismatched["errors"][0]["code"], "quote_mismatch");

    let (lapsed, lapsed_quote, lapsed_priced) = gate.priced(&person, "Lapsed").await;
    let principal = person.principal();
    let quote: Uuid = lapsed_quote["quote_ref"].as_str().unwrap().parse().unwrap();
    gate.engine.executor.approve(&principal, lapsed, quote, lapsed_quote["max_total_atomic"].as_str().unwrap()).await.unwrap();
    sqlx::query(expire).bind(lapsed).execute(&gate.pool).await.unwrap();
    sqlx::query("UPDATE authorizations SET expires_at=now()-interval '1 minute' WHERE task_id=$1").bind(lapsed).execute(&gate.pool).await.unwrap();
    gate.engine.start_worker(principal, person.bearer.clone(), lapsed).await.unwrap();
    let (_, executed) = gate.send(&person, "/v2/execute", Some(execute_body(&lapsed_priced, "execute_quoted_step", Value::Null))).await;
    assert_ne!(executed["status"], "succeeded", "{executed}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(gate.fake.provider.load(SeqCst), 0, "no source call after the quote expired");
    assert_eq!(gate.ledger(lapsed).await, 0);
    let view = gate.view(&person, lapsed).await.1;
    gate::assert_one_approval(&gate.pool, lapsed, &lapsed_quote).await;
    gate::assert_cap(&gate.pool, lapsed).await;
    gate::assert_single_settlement(&gate.pool, lapsed).await;
    assert_eq!(gate::assert_evidence_complete(&view, &gate::saved_documents(&gate.pool, lapsed).await), 0);
    println!("gate_expired_quote_never_dispatches: expired quote refused; lapsed approval dispatched nothing");
}

/// A quoted source changes after approval: the chain is checked before the
/// first reservation, so nothing is bought or charged and the task says why.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-release-gate.mjs); no live database"]
async fn gate_missing_source_zero_charge() {
    let gate = Gate::start().await;
    let person = Person::new();
    let (task, proposal, _) = gate.priced(&person, "Missing").await;
    sqlx::query("UPDATE public.api_endpoints SET price=0.000101 WHERE path='/company.accounts'").execute(&gate.pool).await.unwrap();
    assert_eq!(gate.authorize(&person, task, &proposal).await.0, StatusCode::OK);
    let ended = gate.settled(&person, task).await;
    sqlx::query("UPDATE public.api_endpoints SET price=0.0001 WHERE path='/company.accounts'").execute(&gate.pool).await.unwrap();
    assert_eq!(gate.fake.provider.load(SeqCst), 0, "no source call for a changed quote: {ended}");
    assert_eq!(gate.ledger(task).await, 0);
    assert_ne!(ended["status"], "succeeded", "{ended}");
    assert!(ended["errors"].as_array().unwrap().iter().any(|error| error["code"] == "quote_changed"), "the task says the quote changed: {ended}");
    assert_eq!(ended["billing"]["total_charged"], "0", "{ended}");
    let charged: i64 = sqlx::query_scalar("SELECT coalesce(sum(coalesce(actual_atomic,0)),0)::bigint FROM executions WHERE task_id=$1").bind(task).fetch_one(&gate.pool).await.unwrap();
    assert_eq!(charged, 0);
    assert!(ended["context_view"]["analysis"]["observations"].as_array().is_none_or(Vec::is_empty), "no answer without a source");
    gate::assert_all(&gate.pool, task, &proposal, &ended).await;
    println!("gate_missing_source_zero_charge: changed quote refused before any reservation; charged 0");
}

/// 5,000 rows: the saved result is paged, every page is readable without a
/// purchase, and the analysis answers from question-directed evidence
/// selected across the whole document, with pointers into the original.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-release-gate.mjs); no live database"]
async fn gate_long_data() {
    let gate = Gate::start().await;
    let person = Person::new();
    let (task, proposal, _) = gate.priced_question(&person, "Balance sheet line 4321 in de jaarrekening van Long").await;
    assert_eq!(gate.authorize(&person, task, &proposal).await.0, StatusCode::OK);
    let ended = gate.settled(&person, task).await;
    assert_eq!(ended["status"], "succeeded", "{ended}");
    let documents = gate::saved_documents(&gate.pool, task).await;
    let long = documents.values().find(|document| document["data"]["rows"].is_array()).expect("the long result is saved");
    assert_eq!(long["data"]["rows"].as_array().unwrap().len(), 5000, "every row is saved");
    assert!(long.to_string().len() > 256 * 1024);

    let analysis = &ended["context_view"]["analysis"];
    assert_eq!(analysis["status"], "completed", "the analysis is never missing above 256 KiB: {analysis}");
    assert!(gate.fake.analysis_input.load(SeqCst) <= 256 * 1024, "the model reads a bounded selection");
    let evidence = &analysis["observations"][0]["evidence"];
    assert_eq!(evidence[0]["pointer"], "/data/rows/4321/value_eur", "the named row, far beyond any first rows: {analysis}");
    assert_eq!(evidence[0]["value"], long["data"]["rows"][4321]["value_eur"], "the value comes from the original");
    assert_eq!(evidence[1]["value"], "Balance sheet line 4321");

    // The page shows the first part and reads the rest part by part, free.
    let (read_ref, mut offset) = match &ended["result"] {
        page if page["next_offset"].is_u64() => (page["result_ref"].clone(), page["next_offset"].as_u64().unwrap()),
        other => panic!("a result above 32 KiB is paged: {}", other.to_string().chars().take(300).collect::<String>()),
    };
    assert!(serde_json::to_vec(&ended).unwrap().len() < 256 * 1024, "the task view stays small");
    let mut state = ended.clone();
    let mut parts = 1;
    while parts < 3 {
        let action = state["next_actions"].as_array().unwrap().iter().find(|action| action["kind"] == "read_result" && action["action_id"] == read_ref)
            .unwrap_or_else(|| panic!("a read action for the paged result: {}", state["next_actions"]));
        let body = json!({"request_id":Uuid::new_v4(),"action_id":action["action_id"],"state":state["state"],"quote_ref":proposal["quote_ref"],
            "authorization_ref":null,"idempotency_key":action["action_id"],"input":{"offset":offset}});
        let (status, next) = gate.send(&person, "/v2/execute", Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{next}");
        assert!(next["result"]["text"].as_str().is_some_and(|text| !text.is_empty()), "{}", next["result"]);
        offset = next["result"]["next_offset"].as_u64().expect("more parts remain");
        state = next;
        parts += 1;
    }
    assert_eq!(gate.fake.provider.load(SeqCst), 2, "reading parts buys nothing");
    assert_eq!(gate::assert_all(&gate.pool, task, &proposal, &ended).await, 2, "every cited pointer resolves in the original");
    println!("gate_long_data: 5000 rows saved and paged; analysis {} from a {}-byte selection", analysis["status"], gate.fake.analysis_input.load(SeqCst));
}
