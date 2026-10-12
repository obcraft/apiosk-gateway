//! Isolated, unpaid end-to-end recipe/worker test. Run against a fresh
//! tests/support-db.mjs instance; never a production database. The real recurring
//! migrations run over explicit local tenancy and balance-reservation fixtures.
use apiosk_gateway_v2::{
    auth::{Auth, Principal},
    catalog::CatalogCache,
    engine::Engine,
    execution::Executor,
    integrations::composio_user_id,
    parser::Parser,
    recurring,
    resolver::{Plan, Step},
    server::{router as gateway_router, ServerState},
    state::{digest, Fact, Signer, Task},
    store::Store,
};
use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, Request, StatusCode},
    routing::{get, post},
    Json, Router,
};
use chrono::{Duration as ChronoDuration, Utc};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use tower::ServiceExt;
use uuid::Uuid;

async fn http(
    app: &Router,
    method: &str,
    path: &str,
    token: &str,
    workspace: Option<Uuid>,
    body: Option<Value>,
) -> (u16, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    if let Some(workspace) = workspace {
        request = request.header("x-apiosk-workspace", workspace.to_string());
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(
                    body.map(|value| Body::from(value.to_string()))
                        .unwrap_or_else(Body::empty),
                )
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

const FIXTURE_SCHEMA: &str = r#"
CREATE ROLE service_role BYPASSRLS;
CREATE SCHEMA auth; CREATE SCHEMA billing; CREATE SCHEMA IF NOT EXISTS actions; CREATE SCHEMA tenancy;
CREATE FUNCTION auth.uid() RETURNS uuid LANGUAGE sql AS $$ SELECT NULL::uuid $$;
CREATE TABLE auth.users(id uuid PRIMARY KEY,email text,email_confirmed_at timestamptz,deleted_at timestamptz);
DROP VIEW IF EXISTS actions.user_selected_apis;
ALTER TABLE public.user_selected_apis SET SCHEMA actions;
ALTER TABLE actions.user_selected_apis ADD COLUMN created_at timestamptz DEFAULT now();
CREATE VIEW public.user_selected_apis AS SELECT * FROM actions.user_selected_apis;
ALTER TABLE public.user_balances SET SCHEMA billing;
ALTER TABLE billing.user_balances ADD COLUMN workspace_id uuid, ADD COLUMN reversal_debt_micro_usd bigint DEFAULT 0;
CREATE VIEW public.user_balances AS SELECT * FROM billing.user_balances;
ALTER TABLE public.balance_charges SET SCHEMA billing;
ALTER TABLE billing.balance_charges ADD COLUMN workspace_id uuid, ADD COLUMN settled_at timestamptz;
ALTER TABLE billing.balance_charges ALTER COLUMN id SET DEFAULT gen_random_uuid();
ALTER TABLE billing.balance_charges ADD UNIQUE(user_id,idempotency_key);
CREATE VIEW public.balance_charges AS SELECT * FROM billing.balance_charges;
ALTER TABLE public.job_budget_reservations SET SCHEMA billing;
CREATE VIEW public.job_budget_reservations AS SELECT * FROM billing.job_budget_reservations;
CREATE TABLE tenancy.organisations(id uuid);
CREATE TABLE tenancy.workspaces(id uuid,organisation_id uuid,name text,icon text,colour text,archived_at timestamptz,created_at timestamptz DEFAULT now());
CREATE TABLE billing.balance_topups(id uuid,user_id uuid,workspace_id uuid,status text,amount_micro_eur bigint,origin text,credited_at timestamptz,created_at timestamptz);
CREATE TABLE billing.workspace_budget_transfers(id uuid,organisation_id uuid,workspace_id uuid,actor_id uuid,direction text,amount_micro_usd bigint,created_at timestamptz);
CREATE TABLE tenancy.feature_flags(name text,enabled boolean);
INSERT INTO tenancy.feature_flags VALUES('management',true);
CREATE TABLE tenancy.organisation_billing_accounts(organisation_id uuid,account_id uuid);
CREATE FUNCTION public.workspace_context(uuid,uuid) RETURNS void LANGUAGE sql AS $$ SELECT $$;
CREATE FUNCTION tenancy.org_admin(uuid,uuid) RETURNS boolean LANGUAGE sql AS $$ SELECT true $$;
CREATE FUNCTION tenancy.verified_actor(uuid) RETURNS void LANGUAGE sql AS $$ SELECT $$;
CREATE FUNCTION tenancy.workspace_permission(uuid,uuid,text) RETURNS boolean LANGUAGE sql AS $$ SELECT true $$;
CREATE FUNCTION tenancy.reservation_account(uuid,uuid,bigint) RETURNS uuid LANGUAGE sql AS $$ SELECT $1 $$;
CREATE FUNCTION public.reserve_balance_for_call(p_user uuid,p_key text,p_provider bigint,p_total bigint,p_selection uuid,p_metadata jsonb,p_gateway_key text) RETURNS jsonb LANGUAGE plpgsql AS $$
DECLARE existing uuid; BEGIN
 SELECT id INTO existing FROM billing.balance_charges WHERE user_id=p_user AND idempotency_key=p_key;
 IF existing IS NOT NULL THEN RETURN jsonb_build_object('charge_id',existing,'replayed',true); END IF;
 UPDATE billing.user_balances SET balance_micro_usd=balance_micro_usd-p_total WHERE account_id=p_user AND balance_micro_usd>=p_total;
 IF NOT FOUND THEN RETURN '{"error":"insufficient_funds"}'::jsonb; END IF;
 INSERT INTO billing.balance_charges(user_id,idempotency_key,status,charged_micro_usd,provider_price_micro_usd,fee_micro_usd,metadata)
 VALUES(p_user,p_key,'reserved',p_total,p_provider,p_total-p_provider,p_metadata) RETURNING id INTO existing;
 RETURN jsonb_build_object('charge_id',existing,'replayed',false);
END $$;
"#;

#[derive(Clone)]
struct Fake {
    pool: PgPool,
    owner: Uuid,
    searches: Arc<AtomicUsize>,
    sources: Arc<Mutex<Vec<(Uuid, i32, Value)>>>,
    deliveries: Arc<Mutex<Vec<Value>>>,
    tool_reads: Arc<AtomicUsize>,
    model_calls: Arc<AtomicUsize>,
    control_after_source: Arc<Mutex<Option<String>>>,
    drift_after_source: Arc<Mutex<Option<String>>>,
}

async fn source(State(fake): State<Fake>, headers: HeaderMap) -> Json<Value> {
    let id: Uuid = headers["x-apiosk-recurring-call"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(headers["authorization"]
        .to_str()
        .unwrap()
        .starts_with("Bearer recurring."));
    let row=sqlx::query("SELECT c.run_id,c.ordinal,c.selection_id,c.inputs,c.cap_micro_usd,s.provider_price_micro_usd FROM billing.recurring_calls c JOIN actions.user_selected_apis s ON s.id=c.selection_id WHERE c.id=$1")
        .bind(id).fetch_one(&fake.pool).await.unwrap();
    let run: Uuid = row.get("run_id");
    let ordinal: i32 = row.get("ordinal");
    let inputs: Value = row.get("inputs");
    fake.sources
        .lock()
        .unwrap()
        .push((run, ordinal, inputs.clone()));
    let data = if ordinal == 0 {
        assert_eq!(inputs, json!({"company.name":"Recurring fixture"}));
        let n = fake.searches.fetch_add(1, Ordering::SeqCst) + 1;
        json!({"companies":[{"id":format!("current-{n}")}]})
    } else {
        let n = fake.searches.load(Ordering::SeqCst);
        assert_eq!(
            inputs,
            json!({"company.id":format!("current-{n}")}),
            "B must consume this occurrence's A result"
        );
        json!({"accounts":format!("accounts-current-{n}")})
    };
    let provider: i64 = row.get("provider_price_micro_usd");
    let cap: i64 = row.get("cap_micro_usd");
    let reservation: Value =
        sqlx::query_scalar("SELECT public.reserve_recurring_call($1,$2,$3,$4,$5,$6,'{}',$3)")
            .bind(id)
            .bind(fake.owner)
            .bind(id.to_string())
            .bind(provider)
            .bind(cap)
            .bind(row.get::<Uuid, _>("selection_id"))
            .fetch_one(&fake.pool)
            .await
            .unwrap();
    let charge: Uuid = reservation["charge_id"].as_str().unwrap().parse().unwrap();
    sqlx::query("UPDATE billing.balance_charges SET status='charged',settled_at=now() WHERE id=$1")
        .bind(charge)
        .execute(&fake.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO public.call_runs(id,charge_id,user_id,status,output,output_truncated) VALUES($1,$2,$3,'succeeded',$4,false)")
        .bind(Uuid::new_v4()).bind(charge).bind(fake.owner).bind(&data).execute(&fake.pool).await.unwrap();
    let control = fake.control_after_source.lock().unwrap().take();
    if let Some(state) = control {
        assert_eq!(
            ordinal, 0,
            "control is exercised after source A has settled"
        );
        sqlx::query("SELECT billing.control_recurring_order($1,(SELECT order_id FROM billing.recurring_runs WHERE id=$2),$3)")
            .bind(fake.owner).bind(run).bind(state).execute(&fake.pool).await.unwrap();
    }
    let drift = fake.drift_after_source.lock().unwrap().take();
    if let Some(kind) = drift {
        assert_eq!(ordinal, 0, "drift occurs only after A was paid and saved");
        let endpoint: Uuid = sqlx::query_scalar("SELECT (o.sources#>>'{1,selection,candidate_id}')::uuid FROM billing.recurring_orders o JOIN billing.recurring_runs r ON r.order_id=o.id WHERE r.id=$1")
            .bind(run).fetch_one(&fake.pool).await.unwrap();
        match kind.as_str() {
            "price" => sqlx::query("UPDATE public.api_endpoints SET price=price+0.000001 WHERE id=$1")
                .bind(endpoint).execute(&fake.pool).await.unwrap(),
            "contract" => sqlx::query("UPDATE public.capability_execution_contracts SET version=version+1 WHERE endpoint_id=$1")
                .bind(endpoint).execute(&fake.pool).await.unwrap(),
            _ => panic!("unknown fixture drift"),
        };
    }
    Json(
        json!({"ok":true,"result":data,"charged_micro_usd":cap,"provider_micro_usd":provider,"fee_micro_usd":cap-provider}),
    )
}

async fn seed_completed(engine: &Engine, owner: Uuid) -> (Uuid, i64) {
    let catalog = engine.catalog.fresh().await.unwrap();
    let services = ["company.search", "company.accounts"]
        .into_iter()
        .map(|capability| {
            catalog
                .services
                .iter()
                .find(|s| s.contract.capability_slug == capability)
                .unwrap()
                .clone()
        })
        .collect::<Vec<_>>();
    let entity = Uuid::new_v4();
    let task_id = Uuid::new_v4();
    let auth = Uuid::new_v4();
    let steps = services
        .into_iter()
        .map(|service| Step {
            service,
            entity_ref: entity,
            goal_index: 0,
            browser_fallback: None,
        })
        .collect::<Vec<_>>();
    let cap = steps.iter().map(|step| step.service.total()).sum();
    let plan = Plan { compute: None,
        quote_ref: Uuid::new_v4(),
        cap_atomic: cap,
        expires_at: Utc::now() + ChronoDuration::minutes(10),
        hash: digest(&steps),
        steps,
        fee_bps: 1500,
        workflow_slug: None,
    };
    let now = Utc::now();
    let task = Task {
        proposal: Some(serde_json::to_value(&plan).unwrap()),
        completed: plan.steps.iter().map(Step::key).collect(),
        facts: vec![
            Fact {
                entity_ref: entity,
                semantic_type: "company.name".into(),
                value: json!("Recurring fixture"),
                provenance: json!({"kind":"user_input"}),
                observed_at: now,
                expires_at: now + ChronoDuration::hours(1),
                verification: "user_asserted".into(),
            },
            Fact {
                entity_ref: entity,
                semantic_type: "company.id".into(),
                value: json!("original-0"),
                provenance: json!({"kind":"provider_response","contract_id":plan.steps[0].service.id}),
                observed_at: now,
                expires_at: now + ChronoDuration::hours(1),
                verification: "source_observed".into(),
            },
        ],
        ..Task::default()
    };
    sqlx::query("INSERT INTO gateway_v2.tasks(id,owner_id,document) VALUES($1,$2,$3)")
        .bind(task_id)
        .bind(owner)
        .bind(serde_json::to_value(&task).unwrap())
        .execute(&engine.store.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO gateway_v2.authorizations(id,task_id,owner_id,quote_ref,plan_hash,cap_atomic,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7)")
        .bind(auth).bind(task_id).bind(owner).bind(plan.quote_ref).bind(&plan.hash).bind(cap).bind(plan.expires_at).execute(&engine.store.pool).await.unwrap();
    let mut results = Vec::new();
    // Deliberately insert receipt B before A. The quote must reconstruct plan
    // order from endpoint+capability+entity, never execution completion time.
    for index in [1usize, 0] {
        let step = &plan.steps[index];
        let selection = Uuid::new_v4();
        let execution = Uuid::new_v4();
        let result = Uuid::new_v4();
        let inputs = if index == 0 {
            json!({"company.name":"Recurring fixture"})
        } else {
            json!({"company.id":"original-0"})
        };
        let data = if index == 0 {
            json!({"companies":[{"id":"original-0"}]})
        } else {
            json!({"accounts":"original accounts"})
        };
        let body = json!({"ok":true,"result":data,"charged_micro_usd":plan.step_total(step)});
        sqlx::query("INSERT INTO actions.user_selected_apis(id,user_id,prompt,kind,source,name,capability,api_slug,candidate_id,method,provider_price_micro_usd,charged_micro_usd,metadata) VALUES($1,$2,'Fixture completed question','reviewed','apiosk',$3,$4,$5,$6,$7,$8,$9,$10)")
            .bind(selection).bind(owner).bind(&step.service.name).bind(&step.service.contract.capability_slug).bind(&step.service.api_slug).bind(step.service.endpoint_id).bind(&step.service.method).bind(step.service.list_atomic).bind(plan.step_total(step))
            .bind(json!({"inputs":inputs,"gateway_v2":true,"task_id":task_id,"action_id":execution})).execute(&engine.store.pool).await.unwrap();
        sqlx::query("INSERT INTO gateway_v2.executions(id,owner_id,task_id,action_id,authorization_id,payload_hash,selection_id,reserved_atomic,actual_atomic,status,response) VALUES($1,$2,$3,$1,$4,'fixture',$5,$6,$6,'succeeded',$7)")
            .bind(execution).bind(owner).bind(task_id).bind(auth).bind(selection).bind(plan.step_total(step)).bind(body).execute(&engine.store.pool).await.unwrap();
        sqlx::query("INSERT INTO gateway_v2.results(id,owner_id,task_id,execution_id,document) VALUES($1,$2,$3,$4,$5)")
            .bind(result).bind(owner).bind(task_id).bind(execution).bind(json!({"data":data,"subject":{"entity_ref":entity},"source":step.service.result_source()})).execute(&engine.store.pool).await.unwrap();
        results.push(result);
    }
    sqlx::query(
        "UPDATE gateway_v2.tasks SET document=jsonb_set(document,'{result_refs}',$2) WHERE id=$1",
    )
    .bind(task_id)
    .bind(json!(results))
    .execute(&engine.store.pool)
    .await
    .unwrap();
    (task_id, cap)
}

async fn quoted(engine: &Engine, owner: Uuid, task: Uuid, count: usize) -> (Uuid, i64) {
    quoted_to(engine, owner, task, count, json!({"toolkit":"slack","recipient":"self","connected_account_id":"ca_selected"})).await
}

async fn quoted_to(engine: &Engine, owner: Uuid, task: Uuid, count: usize, delivery: Value) -> (Uuid, i64) {
    let id = Uuid::new_v4();
    let request:recurring::QuoteRequest=serde_json::from_value(json!({
        "request_id":id,"task_ids":[task],"question":"Repeat the same two approved sources in order",
        "schedule":{"start":(Utc::now()+ChronoDuration::days(1)).date_naive(),"time":"09:00:00","timezone":"UTC","frequency":"daily","count":count},
        "delivery":delivery,
        "billing_mode":"prepaid","change_only":false
    })).unwrap();
    let quote = recurring::quote(
        engine,
        &Principal {
            owner,
            connection: None,
            workspace: None,
        },
        request,
    )
    .await
    .unwrap();
    let total = quote["total"].as_str().unwrap().parse::<i64>().unwrap();
    let cap = quote["total_per_run"]
        .as_str()
        .unwrap()
        .parse::<i64>()
        .unwrap();
    let fingerprint = quote["workflow"]["fingerprint"].as_str().unwrap();
    assert_eq!(quote["workflow"]["execution_mode"], "sequential");
    for _ in 0..2 {
        sqlx::query("SELECT billing.approve_locked_recurring_order($1,$2,$3,$4,$5)")
            .bind(owner)
            .bind(id)
            .bind(total)
            .bind(cap)
            .bind(fingerprint)
            .execute(&engine.store.pool)
            .await
            .unwrap();
    }
    (id, total)
}

#[tokio::test]
#[ignore = "fresh local tests/support-db.mjs only; installs real recurring migrations; no live services"]
async fn five_locked_runs_bind_current_results_resume_without_repurchase_and_deliver_pinned_contract(
) {
    let database = std::env::var("TEST_DATABASE_URL")
        .expect("Start a fresh tests/support-db.mjs instance and set TEST_DATABASE_URL");
    let url = url::Url::parse(&database).unwrap();
    assert!(
        matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1")),
        "Only a local fixture database is permitted"
    );
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SET search_path TO public,gateway_v2,billing,actions,catalog")
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect(&database)
        .await
        .unwrap();
    sqlx::raw_sql(FIXTURE_SCHEMA).execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../supabase/migrations/20260930091005_recurring_orders.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../supabase/migrations/20261009110915_locked_recurring_workflows.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../supabase/migrations/20261009142403_recurring_self_email.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let owner = Uuid::new_v4();
    std::env::set_var("RECURRING_ALLOWED_OWNER_IDS", owner.to_string());
    std::env::set_var("V2_EXECUTION_ENABLED", "1");
    std::env::set_var("RECURRING_ORDERS_ENABLED", "1");
    sqlx::query("INSERT INTO auth.users(id) VALUES($1)")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO billing.user_balances(user_id,balance_micro_usd) VALUES($1,1000000)")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    let fake = Fake {
        pool: pool.clone(),
        owner,
        searches: Arc::new(AtomicUsize::new(0)),
        sources: Default::default(),
        deliveries: Default::default(),
        tool_reads: Arc::new(AtomicUsize::new(0)),
        model_calls: Arc::new(AtomicUsize::new(0)),
        control_after_source: Default::default(),
        drift_after_source: Default::default(),
    };
    let router=Router::new()
        .route("/functions/v1/recurring-execute",post(source))
        .route("/auth/v1/user",get(|State(f):State<Fake>,headers:HeaderMap|async move {
            match headers.get("authorization").and_then(|value|value.to_str().ok()) {
                Some("Bearer fixture-person") => (StatusCode::OK,Json(json!({"id":f.owner}))),
                Some("Bearer fixture-other") => (StatusCode::OK,Json(json!({"id":Uuid::nil()}))),
                _ => (StatusCode::UNAUTHORIZED,Json(json!({}))),
            }
        }))
        .route("/connected_accounts/:id",get(|State(f):State<Fake>,Path(id):Path<String>|async move {
            assert!(["ca_selected","ca_gmail"].contains(&id.as_str()));
            let toolkit = if id == "ca_gmail" { "gmail" } else { "slack" };
            Json(json!({"id":id,"user_id":composio_user_id(f.owner),"toolkit":{"slug":toolkit},"status":"ACTIVE"}))
        }))
        .route("/tools/:tool",get(|State(f):State<Fake>,Path(tool):Path<String>|async move {
            if tool == "SLACK_RETRIEVE_CONVERSATION_INFORMATION" { return Json(json!({"slug":tool,"toolkit":{"slug":"slack"},"version":"20261008_00","input_parameters":{"type":"object","properties":{"channel":{"type":"string"}},"required":["channel"]}})); }
            if tool == "GMAIL_SEND_EMAIL" { return Json(json!({"slug":tool,"toolkit":{"slug":"gmail"},"version":"20261008_00","tags":["createHint"], "input_parameters":{"type":"object","properties":{"recipient_email":{"type":"string"},"subject":{"type":"string"},"body":{"type":"string"}},"required":["recipient_email","subject","body"],"additionalProperties":false}})); }
            if tool == "SLACK_TEST_AUTH" { return Json(json!({"slug":tool,"toolkit":{"slug":"slack"},"version":"20261008_00", "input_parameters":{"type":"object","properties":{}}})); }
            if tool == "SLACK_OPEN_DM" { return Json(json!({"slug":tool,"toolkit":{"slug":"slack"},"version":"20261008_00", "input_parameters":{"type":"object","properties":{"users":{"type":"string"},"return_im":{"type":"boolean"}},"required":["users"],"additionalProperties":false}})); }
            f.tool_reads.fetch_add(1,Ordering::SeqCst);assert_eq!(tool,"SLACK_SEND_MESSAGE");
            Json(json!({"slug":tool,"toolkit":{"slug":"slack"},"version":"20261008_00","tags":["createHint"],
                "input_parameters":{"type":"object","properties":{"channel":{"type":"string"},"text":{"type":"string"}},"required":["channel","text"],"additionalProperties":false}}))
        }))
        .route("/tools/execute/:tool",post(|State(f):State<Fake>,Path(tool):Path<String>,Json(body):Json<Value>|async move {
            assert_eq!(body["version"],"20261008_00");
            assert_eq!(body["user_id"],composio_user_id(f.owner));
            if tool == "SLACK_RETRIEVE_CONVERSATION_INFORMATION" { assert_eq!(body["arguments"]["channel"],"GTEAM"); return Json(json!({"successful":true,"data":{"ok":true,"channel":{"id":"GTEAM","name":"team-chat","is_mpim":true,"is_member":true}}})); }
            if tool == "SLACK_TEST_AUTH" { assert_eq!(body["arguments"],json!({})); return Json(json!({"successful":true,"data":{"ok":true,"user_id":"UFIXTURE"}})); }
            if tool == "SLACK_OPEN_DM" { assert_eq!(body["arguments"],json!({"users":"UFIXTURE","return_im":true})); return Json(json!({"successful":true,"data":{"ok":true,"channel":{"id":"DSELF","user":"UFIXTURE","is_im":true}}})); }
            assert_eq!(tool,"SLACK_SEND_MESSAGE");assert_eq!(body["version"],"20261008_00");assert_eq!(body["connected_account_id"],"ca_selected");assert!(["DSELF","GTEAM"].contains(&body["arguments"]["channel"].as_str().unwrap()));
            f.deliveries.lock().unwrap().push(body);Json(json!({"successful":true,"data":{"sent":true}}))
        }))
        .route("/model",post(|State(f):State<Fake>|async move {f.model_calls.fetch_add(1,Ordering::SeqCst);Json(json!({"error":"No model is permitted in frozen replay"}))}))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    std::env::set_var("COMPOSIO_BASE_URL", &base);
    std::env::set_var("COMPOSIO_API_KEY", "local-fixture-not-a-real-key");
    std::env::set_var(
        "RECURRING_WORKER_SECRET",
        "local-fixture-secret-at-least-32-characters",
    );
    let make_engine = || {
        let store = Store { pool: pool.clone() };
        let http = reqwest::Client::new();
        Arc::new(Engine {
            store: store.clone(),
            catalog: CatalogCache::new(pool.clone()),
            parser: Parser::new(
                http.clone(),
                format!("{base}/model"),
                "fixture".into(),
                "fixture".into(),
            ),
            signer: Signer::new(vec![7; 32]).unwrap(),
            executor: Executor {
                store,
                http,
                supabase_url: base.clone(),
                enabled: true,
            },
            app_url: "http://localhost".into(),
        })
    };
    let engine = make_engine();
    let (task, cap) = seed_completed(&engine, owner).await;
    let email_quote_id = Uuid::new_v4();
    let email_request = json!({
        "request_id":email_quote_id,"task_ids":[task],"question":"My own email only",
        "schedule":{"start":(Utc::now()+ChronoDuration::days(1)).date_naive(),"time":"09:00:00","timezone":"UTC","frequency":"daily","count":1},
        "delivery":{"toolkit":"gmail","recipient":"self","connected_account_id":"ca_gmail"},
        "billing_mode":"prepaid","change_only":false
    });
    let principal = Principal {
        owner,
        connection: None,
        workspace: None,
    };
    assert!(
        recurring::quote(
            &engine,
            &principal,
            serde_json::from_value(email_request.clone()).unwrap()
        )
        .await
        .is_err(),
        "unverified email cannot receive scheduled results"
    );
    sqlx::query("UPDATE auth.users SET email='fixture-owner@example.com',email_confirmed_at=now() WHERE id=$1")
        .bind(owner).execute(&pool).await.unwrap();
    let email_quote = recurring::quote(
        &engine,
        &principal,
        serde_json::from_value(email_request.clone()).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        email_quote["delivery"]["destination"],
        "fixture-owner@example.com"
    );
    assert_eq!(email_quote["delivery"]["recipient"], "self");
    sqlx::query("UPDATE auth.users SET email='fixture-updated@example.com' WHERE id=$1")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    let replay = recurring::quote(
        &engine,
        &principal,
        serde_json::from_value(email_request).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        replay["delivery"]["destination"], "fixture-owner@example.com",
        "retries preserve reviewed identity"
    );
    assert!(
        recurring::verify(&engine, &principal, email_quote_id)
            .await
            .is_err(),
        "changed email needs a new reviewed quote"
    );
    let (order, total) = quoted(&engine, owner, task, 5).await;
    assert_eq!(total, 5 * cap);
    let runs: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM billing.recurring_runs WHERE order_id=$1 ORDER BY scheduled_at",
    )
    .bind(order)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        runs.len(),
        5,
        "duplicate approval must not create duplicate occurrences"
    );
    assert!(
        sqlx::query("UPDATE billing.recurring_orders SET workflow='{}' WHERE id=$1")
            .bind(order)
            .execute(&pool)
            .await
            .is_err(),
        "approved source recipes must be immutable"
    );
    for (index, run) in runs.iter().enumerate() {
        sqlx::query("UPDATE billing.recurring_runs SET scheduled_at=$2 WHERE id=$1")
            .bind(run)
            .bind(Utc::now() - ChronoDuration::seconds(1))
            .execute(&pool)
            .await
            .unwrap();
        if index == 0 {
            for setting in [
                None,
                Some(String::new()),
                Some(format!("{owner},invalid")),
                Some(Uuid::new_v4().to_string()),
            ] {
                match setting {
                    Some(value) => std::env::set_var("RECURRING_ALLOWED_OWNER_IDS", value),
                    None => std::env::remove_var("RECURRING_ALLOWED_OWNER_IDS"),
                }
                recurring::run_one_pass(&engine).await.unwrap();
                recurring::deliver_one_pass(&engine).await.unwrap();
                let state: String =
                    sqlx::query_scalar("SELECT state FROM billing.recurring_runs WHERE id=$1")
                        .bind(run)
                        .fetch_one(&pool)
                        .await
                        .unwrap();
                assert_eq!(state, "queued", "denied owners must not be claimed");
                assert!(fake.sources.lock().unwrap().is_empty());
                assert!(fake.deliveries.lock().unwrap().is_empty());
            }
            std::env::set_var("RECURRING_ALLOWED_OWNER_IDS", owner.to_string());
        }
        recurring::run_one_pass(&engine).await.unwrap();
        let state: String =
            sqlx::query_scalar("SELECT state FROM billing.recurring_runs WHERE id=$1")
                .bind(run)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(state, "succeeded", "occurrence {}", index + 1);
        assert_eq!(fake.sources.lock().unwrap().len(), 2 * (index + 1));
        if index == 0 {
            // Simulate loss of worker progress after both calls settled. A new
            // Engine must use charge/call_runs receipts instead of POST again.
            sqlx::query("UPDATE billing.recurring_runs SET state='running',finished_at=NULL,lease_until=now()-interval '1 second' WHERE id=$1").bind(run).execute(&pool).await.unwrap();
            sqlx::query("UPDATE billing.recurring_calls SET state='dispatching',response=NULL WHERE run_id=$1").bind(run).execute(&pool).await.unwrap();
            recurring::run_one_pass(&make_engine()).await.unwrap();
            assert_eq!(
                fake.sources.lock().unwrap().len(),
                2,
                "restart cannot repurchase settled calls"
            );
            std::env::set_var("RECURRING_ALLOWED_OWNER_IDS", Uuid::new_v4().to_string());
            recurring::deliver_one_pass(&engine).await.unwrap();
            assert!(
                fake.deliveries.lock().unwrap().is_empty(),
                "revoked owners must not send"
            );
            let delivery_state: String = sqlx::query_scalar(
                "SELECT state FROM billing.recurring_deliveries WHERE run_id=$1",
            )
            .bind(run)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(delivery_state, "queued", "denied delivery stays unclaimed");
            std::env::set_var("RECURRING_ALLOWED_OWNER_IDS", owner.to_string());
        }
        recurring::deliver_one_pass(&engine).await.unwrap();
        recurring::deliver_one_pass(&engine).await.unwrap();
        assert_eq!(
            fake.deliveries.lock().unwrap().len(),
            index + 1,
            "delivery is one saved result per occurrence"
        );
    }
    assert_eq!(
        fake.tool_reads.load(Ordering::SeqCst),
        1,
        "runs must use the pinned tool, never rediscover latest"
    );
    assert_eq!(
        fake.model_calls.load(Ordering::SeqCst),
        0,
        "the frozen recipe cannot replan or freely resynthesize"
    );
    let events = fake.sources.lock().unwrap();
    for (index, pair) in events.chunks_exact(2).enumerate() {
        assert_eq!(pair[0].1, 0);
        assert_eq!(pair[1].1, 1);
        assert_eq!(pair[1].2["company.id"], format!("current-{}", index + 1));
    }
    drop(events);
    let balance: i64 =
        sqlx::query_scalar("SELECT balance_micro_usd FROM billing.user_balances WHERE user_id=$1")
            .bind(owner)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(balance, 1_000_000 - total);
    let charges: i64 =
        sqlx::query_scalar("SELECT count(*) FROM billing.balance_charges WHERE user_id=$1")
            .bind(owner)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(charges, 10);

    let (paused, _) = quoted(&engine, owner, task, 2).await;
    sqlx::query("SELECT billing.control_recurring_order($1,$2,'paused')")
        .bind(owner)
        .bind(paused)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE billing.recurring_runs SET scheduled_at=now()-interval '2 hours' WHERE id=(SELECT id FROM billing.recurring_runs WHERE order_id=$1 ORDER BY scheduled_at LIMIT 1)").bind(paused).execute(&pool).await.unwrap();
    recurring::run_one_pass(&engine).await.unwrap();
    sqlx::query("SELECT billing.control_recurring_order($1,$2,'cancelled')")
        .bind(owner)
        .bind(paused)
        .execute(&pool)
        .await
        .unwrap();
    recurring::run_one_pass(&engine).await.unwrap();
    assert_eq!(
        fake.sources.lock().unwrap().len(),
        10,
        "pause/cancel cannot dispatch a source"
    );
    let balance: i64 =
        sqlx::query_scalar("SELECT balance_micro_usd FROM billing.user_balances WHERE user_id=$1")
            .bind(owner)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        balance,
        1_000_000 - total,
        "all unused prepaid funds return"
    );

    // A pause/cancel can arrive during an in-flight paid call. Preserve A's
    // settled result, prevent B, and return every unspent reserved amount.
    for control in ["paused", "cancelled"] {
        let (order, _) = quoted(&engine, owner, task, 2).await;
        let run: Uuid = sqlx::query_scalar(
            "SELECT id FROM billing.recurring_runs WHERE order_id=$1 ORDER BY scheduled_at LIMIT 1",
        )
        .bind(order)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE billing.recurring_runs SET scheduled_at=now()-interval '1 second' WHERE id=$1",
        )
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
        *fake.control_after_source.lock().unwrap() = Some(control.into());
        let before = fake.sources.lock().unwrap().len();
        recurring::run_one_pass(&engine).await.unwrap();
        let row = sqlx::query("SELECT state,result FROM billing.recurring_runs WHERE id=$1")
            .bind(run)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            row.get::<String, _>("state"),
            if control == "paused" {
                "skipped"
            } else {
                "cancelled"
            }
        );
        let partial: Value = row.get("result");
        assert_eq!(partial["partial"], true);
        assert_eq!(partial["sources"].as_array().unwrap().len(), 1);
        assert_eq!(
            partial["sources"][0]["document"]["data"]["companies"][0]["id"],
            format!("current-{}", fake.searches.load(Ordering::SeqCst))
        );
        assert_eq!(
            fake.sources.lock().unwrap().len(),
            before + 1,
            "B cannot run after control takes effect"
        );
        sqlx::query("SELECT billing.control_recurring_order($1,$2,'cancelled')")
            .bind(owner)
            .bind(order)
            .execute(&pool)
            .await
            .unwrap();
        recurring::run_one_pass(&make_engine()).await.unwrap();
        assert_eq!(
            fake.sources.lock().unwrap().len(),
            before + 1,
            "restart cannot resume a cancelled sequence"
        );
    }
    // A price or contract change while A is in flight must be checked again
    // before dispatching B. It cannot silently use stale catalog approval.
    for drift in ["price", "contract"] {
        let (order, _) = quoted(&engine, owner, task, 2).await;
        let endpoint: Uuid = sqlx::query_scalar("SELECT (sources#>>'{1,selection,candidate_id}')::uuid FROM billing.recurring_orders WHERE id=$1")
            .bind(order).fetch_one(&pool).await.unwrap();
        let run: Uuid = sqlx::query_scalar(
            "SELECT id FROM billing.recurring_runs WHERE order_id=$1 ORDER BY scheduled_at LIMIT 1",
        )
        .bind(order)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE billing.recurring_runs SET scheduled_at=now()-interval '1 second' WHERE id=$1",
        )
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
        *fake.drift_after_source.lock().unwrap() = Some(drift.into());
        let before = fake.sources.lock().unwrap().len();
        recurring::run_one_pass(&engine).await.unwrap();
        let state: String =
            sqlx::query_scalar("SELECT state FROM billing.recurring_orders WHERE id=$1")
                .bind(order)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(state, "attention", "{drift} drift needs a new review");
        let partial: Value =
            sqlx::query_scalar("SELECT result FROM billing.recurring_runs WHERE id=$1")
                .bind(run)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            partial["sources"][0]["document"]["data"]["companies"][0]["id"],
            format!("current-{}", fake.searches.load(Ordering::SeqCst))
        );
        assert_eq!(
            fake.sources.lock().unwrap().len(),
            before + 1,
            "changed B cannot be called or substituted"
        );
        let paid_calls: i64 = sqlx::query_scalar("SELECT count(*) FROM billing.recurring_calls WHERE run_id=$1 AND allocated_micro_usd>0")
            .bind(run).fetch_one(&pool).await.unwrap();
        assert_eq!(paid_calls, 1, "only A may incur a charge");
        sqlx::query("SELECT billing.control_recurring_order($1,$2,'cancelled')")
            .bind(owner)
            .bind(order)
            .execute(&pool)
            .await
            .unwrap();
        match drift {
            "price" => sqlx::query("UPDATE public.api_endpoints SET price=price-0.000001 WHERE id=$1")
                .bind(endpoint).execute(&pool).await.unwrap(),
            "contract" => sqlx::query("UPDATE public.capability_execution_contracts SET version=version-1 WHERE endpoint_id=$1")
                .bind(endpoint).execute(&pool).await.unwrap(),
            _ => unreachable!(),
        };
        recurring::run_one_pass(&make_engine()).await.unwrap();
        assert_eq!(fake.sources.lock().unwrap().len(), before + 1);
    }

    // A completed occurrence can have a queued send when its order is stopped.
    // The saved source result remains available, but no stale send is allowed.
    let (cancelled_send, _) = quoted(&engine, owner, task, 2).await;
    let run: Uuid = sqlx::query_scalar(
        "SELECT id FROM billing.recurring_runs WHERE order_id=$1 ORDER BY scheduled_at LIMIT 1",
    )
    .bind(cancelled_send)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE billing.recurring_runs SET scheduled_at=now()-interval '1 second' WHERE id=$1",
    )
    .bind(run)
    .execute(&pool)
    .await
    .unwrap();
    recurring::run_one_pass(&engine).await.unwrap();
    let delivery_state: String =
        sqlx::query_scalar("SELECT state FROM billing.recurring_deliveries WHERE run_id=$1")
            .bind(run)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(delivery_state, "queued");
    sqlx::query("SELECT billing.control_recurring_order($1,$2,'cancelled')")
        .bind(owner)
        .bind(cancelled_send)
        .execute(&pool)
        .await
        .unwrap();
    let sends_before = fake.deliveries.lock().unwrap().len();
    recurring::deliver_one_pass(&engine).await.unwrap();
    recurring::deliver_one_pass(&make_engine()).await.unwrap();
    assert_eq!(
        fake.deliveries.lock().unwrap().len(),
        sends_before,
        "a cancelled order cannot deliver its queued result"
    );
    let active_deliveries: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM billing.recurring_deliveries WHERE state IN ('queued','sending')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        active_deliveries, 0,
        "cancellation also stops unsent failure notifications"
    );
    let saved: Value = sqlx::query_scalar("SELECT result FROM billing.recurring_runs WHERE id=$1")
        .bind(run)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        saved["sources"].as_array().unwrap().len(),
        2,
        "cancellation preserves the completed result"
    );

    let final_balance: i64 =
        sqlx::query_scalar("SELECT balance_micro_usd FROM billing.user_balances WHERE user_id=$1")
            .bind(owner)
            .fetch_one(&pool)
            .await
            .unwrap();
    let paid: i64 = sqlx::query_scalar("SELECT sum(charged_micro_usd)::bigint FROM billing.balance_charges WHERE user_id=$1 AND status='charged'")
        .bind(owner).fetch_one(&pool).await.unwrap();
    assert_eq!(
        final_balance,
        1_000_000 - paid,
        "partial runs keep only actually paid source charges"
    );

    // Exercise the actual HTTP authentication path after workspace access is
    // removed. A stopped member may release their own hold, but cannot read,
    // resume, operate another owner's order, or redirect a workspace refund.
    let workspace = Uuid::new_v4();
    let workspace_order = Uuid::new_v4();
    sqlx::query("INSERT INTO billing.user_balances(user_id,balance_micro_usd) VALUES($1,10000)")
        .bind(workspace)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO public.fixture_workspace_access(actor_id,workspace_id,can_spend) VALUES($1,$2,true)")
        .bind(owner).bind(workspace).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO billing.recurring_orders(id,owner_id,workspace_id,question,timezone,configuration,sources,delivery,dates,provider_per_run,total_per_run,approved_micro_usd,billing_mode,workflow,change_only) SELECT $1,owner_id,$2,question,timezone,configuration,sources,delivery,dates,provider_per_run,total_per_run,approved_micro_usd,billing_mode,workflow,change_only FROM billing.recurring_orders WHERE id=$3")
        .bind(workspace_order).bind(workspace).bind(cancelled_send).execute(&pool).await.unwrap();
    sqlx::query("SELECT billing.approve_locked_recurring_order($1,id,approved_micro_usd,total_per_run,workflow->>'fingerprint') FROM billing.recurring_orders WHERE id=$2")
        .bind(owner).bind(workspace_order).execute(&pool).await.unwrap();
    let held: i64 = sqlx::query_scalar(
        "SELECT balance_micro_usd FROM billing.user_balances WHERE account_id=$1",
    )
    .bind(workspace)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(held, 10000 - 2 * cap);
    std::env::set_var("WORKSPACE_CONTEXT_ENABLED", "true");
    std::env::set_var("V2_EXECUTION_ENABLED", "1");
    std::env::set_var("RECURRING_ORDERS_ENABLED", "1");
    let app = gateway_router(
        ServerState {
            engine: engine.clone(),
            auth: Arc::new(Auth {
                pool: pool.clone(),
                http: reqwest::Client::new(),
                supabase_url: base.clone(),
                publishable_key: "fixture".into(),
            }),
            permits: Arc::new(tokio::sync::Semaphore::new(4)),
            executor_compat: None,
        },
        "http://localhost",
    )
    .unwrap();
    let order_path = format!("/v2/recurring-orders/{workspace_order}");
    let control_path = format!("{order_path}/control");
    assert_eq!(
        http(
            &app,
            "GET",
            "/v2/recurring-orders/access",
            "fixture-person",
            None,
            None
        )
        .await,
        (200, json!({"enabled":true}))
    );
    assert_eq!(
        http(
            &app,
            "GET",
            "/v2/recurring-orders/access",
            "fixture-other",
            None,
            None
        )
        .await,
        (200, json!({"enabled":false}))
    );
    assert_eq!(
        http(
            &app,
            "GET",
            "/v2/recurring-orders/access",
            "missing",
            None,
            None
        )
        .await
        .0,
        401
    );
    for token in ["fixture-other", "fixture-person"] {
        if token == "fixture-person" {
            // One malformed entry invalidates the entire setting.
            std::env::set_var("RECURRING_ALLOWED_OWNER_IDS", format!("{owner},invalid"));
            assert_eq!(
                http(
                    &app,
                    "GET",
                    "/v2/recurring-orders/access",
                    token,
                    None,
                    None
                )
                .await,
                (200, json!({"enabled":false}))
            );
        }
        for (method, path, body) in [
            ("GET", "/v2/recurring-orders".to_owned(), None),
            ("GET", order_path.clone(), None),
            ("GET", format!("{order_path}/runs/{}", Uuid::new_v4()), None),
            (
                "POST",
                control_path.clone(),
                Some(json!({"state":"cancelled"})),
            ),
            (
                "POST",
                format!("{order_path}/approve"),
                Some(json!({"total":"0","per_run_cap":"0","workflow_hash":"test"})),
            ),
        ] {
            let (status, body) = http(&app, method, &path, token, None, body).await;
            assert_eq!(
                status, 403,
                "private rollout blocks {method} {path}: {body}"
            );
            assert_eq!(body["errors"][0]["code"], "recurring_forbidden");
        }
    }
    std::env::set_var("RECURRING_ALLOWED_OWNER_IDS", owner.to_string());
    assert_eq!(
        http(
            &app,
            "GET",
            &order_path,
            "fixture-person",
            Some(workspace),
            None
        )
        .await
        .0,
        200
    );
    sqlx::query(
        "DELETE FROM public.fixture_workspace_access WHERE actor_id=$1 AND workspace_id=$2",
    )
    .bind(owner)
    .bind(workspace)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        http(
            &app,
            "GET",
            &order_path,
            "fixture-person",
            Some(workspace),
            None
        )
        .await
        .0,
        403
    );
    assert_eq!(
        http(
            &app,
            "POST",
            &control_path,
            "fixture-person",
            Some(workspace),
            Some(json!({"state":"active"}))
        )
        .await
        .0,
        403
    );
    for (token, scope) in [
        ("fixture-other", Some(workspace)),
        ("fixture-person", Some(Uuid::new_v4())),
        ("fixture-person", None),
    ] {
        let (status, body) = http(
            &app,
            "POST",
            &control_path,
            token,
            scope,
            Some(json!({"state":"cancelled"})),
        )
        .await;
        assert_ne!(status, 200, "wrong owner/workspace cannot cancel: {body}");
    }
    let (status, ack) = http(
        &app,
        "POST",
        &control_path,
        "fixture-person",
        Some(workspace),
        Some(json!({"state":"cancelled"})),
    )
    .await;
    assert_eq!(status, 200, "owner must still release future holds: {ack}");
    assert_eq!(
        ack,
        json!({"id":workspace_order,"state":"cancelled","access_revoked":true}),
        "revoked membership gets no sources or balances"
    );
    assert_eq!(
        http(
            &app,
            "GET",
            &order_path,
            "fixture-person",
            Some(workspace),
            None
        )
        .await
        .0,
        403
    );
    let refunded: i64 = sqlx::query_scalar(
        "SELECT balance_micro_usd FROM billing.user_balances WHERE account_id=$1",
    )
    .bind(workspace)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(refunded, 10000, "refund belongs to the workspace");
    let personal: i64 = sqlx::query_scalar(
        "SELECT balance_micro_usd FROM billing.user_balances WHERE account_id=$1",
    )
    .bind(owner)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        personal, final_balance,
        "owner cancellation cannot redirect workspace funds"
    );
    // A selected group goes through quote, approval, funding, execution and
    // pinned delivery using only these fake local providers.
    let (group_order, _) = quoted_to(&engine, owner, task, 1, json!({"toolkit":"slack","recipient":"conversation","destination":"GTEAM","connected_account_id":"ca_selected"})).await;
    recurring::verify(&engine, &principal, group_order).await.unwrap();
    sqlx::query("UPDATE billing.recurring_runs SET scheduled_at=now()-interval '1 second' WHERE order_id=$1").bind(group_order).execute(&pool).await.unwrap();
    recurring::run_one_pass(&engine).await.unwrap();
    let group_view = recurring::read(&engine, &principal, group_order).await.unwrap();
    assert_eq!(group_view["state"],"completed");
    assert_eq!(group_view["delivery"]["destination_label"],"team-chat");
    assert_eq!(group_view["spent"],group_view["total"]);
    sqlx::query("UPDATE billing.recurring_deliveries SET next_attempt_at=now()+interval '1 day' WHERE run_id IN (SELECT id FROM billing.recurring_runs WHERE order_id<>$1)").bind(group_order).execute(&pool).await.unwrap();
    recurring::deliver_one_pass(&engine).await.unwrap();
    assert_eq!(fake.deliveries.lock().unwrap().last().unwrap()["arguments"]["channel"],"GTEAM");

    // Lifetime accounting must not be derived from the latest 100 run cards.
    // Use journal fixtures for a long history, a refund, a pending call and a
    // lower actual charge. The real reserve/settle/cancel flow is exercised above.
    let before = recurring::read(&engine, &principal, order).await.unwrap();
    let before_spent: i64 = before["spent"].as_str().unwrap().parse().unwrap();
    let history_selection: Uuid = sqlx::query_scalar("SELECT c.selection_id FROM billing.recurring_calls c JOIN billing.recurring_runs r ON r.id=c.run_id WHERE r.order_id=$1 LIMIT 1").bind(order).fetch_one(&pool).await.unwrap();
    for index in 0..104i64 {
        let run: Uuid = sqlx::query_scalar("INSERT INTO billing.recurring_runs(order_id,scheduled_at,state,finished_at) VALUES($1,now()+interval '1000 days'+$2*interval '1 day','succeeded',now()) RETURNING id")
            .bind(order).bind(index).fetch_one(&pool).await.unwrap();
        let (status, allocated, actual) = match index { 101 => ("refunded",11i64,11i64), 102 => ("reserved",13,13), 103 => ("charged",17,5), _ => ("charged",7,7) };
        let call = Uuid::new_v4();
        let selection = Uuid::new_v4();
        sqlx::query("INSERT INTO actions.user_selected_apis(id,user_id,prompt,kind,source,name,capability,api_slug,candidate_id,method,provider_price_micro_usd,charged_micro_usd,metadata) SELECT $1,user_id,prompt,kind,source,name,capability,api_slug,candidate_id,method,provider_price_micro_usd,charged_micro_usd,metadata FROM actions.user_selected_apis WHERE id=$2")
            .bind(selection).bind(history_selection).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO billing.recurring_calls(id,run_id,ordinal,selection_id,inputs,cap_micro_usd,allocated_micro_usd,state) VALUES($1,$2,0,$3,'{}',$4,$4,'succeeded')")
            .bind(call).bind(run).bind(selection).bind(allocated).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO billing.balance_charges(user_id,idempotency_key,status,charged_micro_usd,provider_price_micro_usd,fee_micro_usd,metadata) VALUES($1,$2,$3,$4,0,$4,'{}')")
            .bind(owner).bind(call.to_string()).bind(status).bind(actual).execute(&pool).await.unwrap();
    }
    let summary = recurring::read(&engine, &principal, order).await.unwrap();
    assert_eq!(summary["runs"].as_array().unwrap().len(),100);
    assert_eq!(summary["spent"],(before_spent + 101*7 + 5).to_string());
    assert_eq!(summary["pending"],"13");
    assert_eq!(summary["refunded"],"23");
    server.abort();
    pool.close().await;
}
