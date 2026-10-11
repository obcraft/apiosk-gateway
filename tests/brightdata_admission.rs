//! Real dispatch/SQL/payment-boundary test. No Bright Data network or real key.
//! Run with scripts/test-brightdata-admission.py against disposable PostgreSQL.
use apiosk_gateway_v2::{
    auth::Principal,
    catalog::CatalogCache,
    execution::{Dispatch, Executor},
    resolver::{Plan, Step},
    state::Task,
    store::Store,
};
use axum::{extract::State, routing::post, Json, Router};
use chrono::Utc;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use uuid::Uuid;

#[derive(Clone)]
struct Payment {
    pool: PgPool,
    calls: Arc<AtomicUsize>,
    list: i64,
    total: i64,
}
async fn pay(State(s): State<Payment>, Json(request): Json<Value>) -> Json<Value> {
    // Same shared cache read as the legacy adapter; never pay an unfinished job.
    let result = brightdata_linkedin::answer(
        &s.pool,
        "POST",
        "/linkedin/company",
        None,
        &serde_json::to_vec(&request["inputs"]).unwrap(),
    )
    .await
    .unwrap();
    s.calls.fetch_add(1, Ordering::SeqCst);
    Json(
        json!({"ok":true,"execution_id":Uuid::new_v4(),"result":result,
        "charged_micro_usd":s.total,"provider_micro_usd":s.list,"fee_micro_usd":s.total-s.list}),
    )
}

fn key(url: &str) -> String {
    hex::encode(Sha256::digest(format!("company\n{url}")))
}

async fn seed(pool: &PgPool, url: &str, state: &str, result: Option<Value>) {
    sqlx::query("INSERT INTO gateway_private.brightdata_linkedin_jobs(request_key,kind,source_url,state,result) VALUES($1,'company',$2,$3,$4)")
        .bind(key(url)).bind(url).bind(state).bind(result).execute(pool).await.unwrap();
}

async fn count(pool: &PgPool, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires scripts/test-brightdata-admission.py (disposable PostgreSQL)"]
async fn approved_collection_waits_without_reservation_then_delivers_once() {
    let database = std::env::var("BRIGHTDATA_ADMISSION_TEST_DATABASE_URL")
        .expect("isolated database required");
    assert!(database.starts_with("postgresql://postgres@127.0.0.1:"));
    std::env::set_var("BRIGHTDATA_API_KEY", "local-fixture-never-sent");
    // A single connection proves admission releases its transaction before the
    // shared adapter acquires one, including the ready-result revalidation pass.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(2))
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SET search_path TO public, actions")
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect(&database)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE ROLE anon; CREATE ROLE authenticated; CREATE ROLE apiosk_gateway; CREATE ROLE service_role;")
        .execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../supabase/migrations/20260907094100_gateway_v2_state.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let source_schema =
        include_str!("../supabase/migrations/20261008092408_onboard_brightdata_linkedin.sql")
            .split("DO $onboard$")
            .next()
            .unwrap();
    sqlx::raw_sql(&format!("{source_schema}\nCOMMIT;"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE SCHEMA actions; CREATE SCHEMA providers; CREATE SCHEMA catalog;
        ALTER TABLE gateway_v2.tasks SET SCHEMA actions; ALTER TABLE gateway_v2.requests SET SCHEMA actions;
        ALTER TABLE gateway_v2.results SET SCHEMA actions; ALTER TABLE gateway_v2.executions SET SCHEMA actions;
        ALTER TABLE gateway_v2.authorizations SET SCHEMA actions;
        CREATE TABLE actions.user_selected_apis(id uuid primary key,user_id uuid,connection_id uuid,prompt text,kind text,source text,name text,capability text,api_slug text,candidate_id uuid,method text,provider_price_micro_usd bigint,charged_micro_usd bigint,metadata jsonb);
        CREATE TABLE providers.apis(id uuid primary key,slug text,name text,listing_type text,status text,active boolean,is_verified boolean,discoverable boolean,suspended_at timestamptz,docs_url text,website_url text,listing_metadata jsonb DEFAULT '{}');
        CREATE TABLE providers.api_endpoints(id uuid primary key,api_id uuid,method text,path text,price numeric,active boolean,description text,query_params jsonb,request_body jsonb);
        CREATE TABLE catalog.capabilities(slug text primary key,status text,description text);
        CREATE TABLE catalog.fact_types(slug text primary key,status text,value_schema jsonb,jurisdiction text);
        CREATE TABLE catalog.capability_execution_contracts(id uuid primary key,endpoint_id uuid,capability_slug text,version int,status text,requires jsonb,input_map jsonb,output_map jsonb,identifies text,cardinality text,max_fan_out int,fixed_params jsonb,preconditions jsonb,pagination jsonb,freshness_seconds int,sample_payload jsonb,validated_at timestamptz,effect text,price_model text,produces jsonb DEFAULT '[]',adapter_version text);")
        .execute(&pool).await.unwrap();
    let source = Uuid::new_v4();
    let endpoint = Uuid::new_v4();
    sqlx::query("INSERT INTO providers.apis(id,slug,name,listing_type,status,active,is_verified,discoverable,docs_url) VALUES($1,'brightdata','Bright Data LinkedIn','api','active',true,true,true,'https://docs.brightdata.com/')")
        .bind(source).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO providers.api_endpoints VALUES($1,$2,'POST','/linkedin/company',0.0015,true,'Read one public LinkedIn company page','{\"type\":\"object\",\"properties\":{}}',$3)")
        .bind(endpoint).bind(source).bind(json!({"type":"object","required":["url"],"additionalProperties":false,"properties":{"url":{"type":"string"}}}))
        .execute(&pool).await.unwrap();
    let catalog = CatalogCache::new(pool.clone())
        .fresh_execution(&[endpoint])
        .await
        .unwrap();
    let service = catalog
        .services
        .iter()
        .find(|s| s.endpoint_id == endpoint)
        .expect("registered source executable")
        .clone();
    let step = Step {
        service,
        entity_ref: Uuid::new_v4(),
        goal_index: 0,
        browser_fallback: None,
    };
    let plan = Plan { compute: None,
        quote_ref: Uuid::new_v4(),
        steps: vec![step.clone()],
        cap_atomic: step.service.total(),
        expires_at: Utc::now() + chrono::Duration::minutes(10),
        hash: "fixture-hash".into(),
        fee_bps: apiosk_gateway_v2::resolver::standard_markup(),
        workflow_slug: None,
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/functions/v1/use-selection", post(pay))
        .with_state(Payment {
            pool: pool.clone(),
            calls: calls.clone(),
            list: step.service.list_atomic,
            total: step.service.total(),
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let executor = Arc::new(Executor {
        store: Store { pool: pool.clone() },
        http: reqwest::Client::new(),
        supabase_url: origin,
        enabled: true,
    });
    let mut buyers = Vec::new();
    for _ in 0..4 {
        let p = Principal {
            owner: Uuid::new_v4(),
            connection: None,
            workspace: None,
        };
        let task = Uuid::new_v4();
        let auth = Uuid::new_v4();
        sqlx::query("INSERT INTO actions.tasks(id,owner_id,document) VALUES($1,$2,$3)")
            .bind(task)
            .bind(p.owner)
            .bind(serde_json::to_value(Task::default()).unwrap())
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO actions.authorizations(id,task_id,owner_id,quote_ref,plan_hash,cap_atomic,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(auth).bind(task).bind(p.owner).bind(plan.quote_ref).bind(&plan.hash).bind(plan.cap_atomic).bind(plan.expires_at).execute(&pool).await.unwrap();
        buyers.push((p, task, auth, Uuid::new_v4(), Uuid::new_v4()));
    }
    let (p, task, auth, action, identity) = &buyers[0];
    let unapproved = json!({"url":"https://www.linkedin.com/company/unapproved"});
    assert!(executor
        .dispatch(
            p,
            "fixture",
            *task,
            0,
            *action,
            *identity,
            Uuid::new_v4(),
            &plan,
            &step,
            &unapproved
        )
        .await
        .is_err());
    sqlx::query("UPDATE actions.authorizations SET cap_atomic=0 WHERE id=$1")
        .bind(auth)
        .execute(&pool)
        .await
        .unwrap();
    assert!(executor
        .dispatch(
            p,
            "fixture",
            *task,
            0,
            *action,
            *identity,
            *auth,
            &plan,
            &step,
            &unapproved
        )
        .await
        .is_err());
    sqlx::query("UPDATE actions.authorizations SET cap_atomic=$2 WHERE id=$1")
        .bind(auth)
        .bind(plan.cap_atomic)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE providers.apis SET active=false WHERE id=$1")
        .bind(source)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        executor
            .dispatch(
                p,
                "fixture",
                *task,
                0,
                *action,
                *identity,
                *auth,
                &plan,
                &step,
                &unapproved
            )
            .await
            .unwrap(),
        Dispatch::Failed { .. }
    ));
    sqlx::query("UPDATE providers.apis SET active=true WHERE id=$1")
        .bind(source)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        count(&pool, "gateway_private.brightdata_linkedin_jobs").await,
        0,
        "no provider claim before approval, budget and catalog checks"
    );
    let url = "https://www.linkedin.com/company/fixture";
    seed(&pool, url, "submitting", None).await;
    let mut waiters = Vec::new();
    for (p, task, auth, action, identity) in buyers.clone() {
        let executor = executor.clone();
        let plan = plan.clone();
        let step = step.clone();
        waiters.push(tokio::spawn(async move {
            assert!(matches!(
                tokio::time::timeout(
                    Duration::from_secs(5),
                    executor.dispatch(
                        &p,
                        "fixture",
                        task,
                        0,
                        action,
                        identity,
                        auth,
                        &plan,
                        &step,
                        &json!({"url":url})
                    )
                )
                .await
                .unwrap()
                .unwrap(),
                Dispatch::Waiting { .. }
            ));
        }));
    }
    for waiter in waiters {
        waiter.await.unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(count(&pool, "actions.executions").await, 0);
    assert_eq!(count(&pool, "actions.user_selected_apis").await, 0);
    // Failed and unknown submissions release this approved action without ever
    // allocating a payment identity or issuing another collection.
    for state in ["unknown", "failed"] {
        let url = format!("https://www.linkedin.com/company/{state}");
        seed(&pool, &url, state, None).await;
        let Dispatch::Failed { body } = executor
            .dispatch(
                p,
                "fixture",
                *task,
                0,
                Uuid::new_v4(),
                Uuid::new_v4(),
                *auth,
                &plan,
                &step,
                &json!({"url":url}),
            )
            .await
            .unwrap()
        else {
            panic!("terminal provider state must fail before reservation")
        };
        assert_eq!(body["charged_micro_usd"], 0);
    }
    assert_eq!(count(&pool, "actions.executions").await, 0);
    let data = json!([{"name":"Fixture company","url":url}]);
    sqlx::query("UPDATE gateway_private.brightdata_linkedin_jobs SET state='ready',result=$2 WHERE request_key=$1")
        .bind(key(url)).bind(&data).execute(&pool).await.unwrap();
    // Revocation after collection still blocks payment; admission must rerun on
    // each waiting action, rather than treating ready cache data as approval.
    sqlx::query("UPDATE actions.authorizations SET revoked_at=now() WHERE id=$1")
        .bind(auth)
        .execute(&pool)
        .await
        .unwrap();
    assert!(executor
        .dispatch(
            p,
            "fixture",
            *task,
            0,
            *action,
            *identity,
            *auth,
            &plan,
            &step,
            &json!({"url":url})
        )
        .await
        .is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    sqlx::query("UPDATE actions.authorizations SET revoked_at=NULL WHERE id=$1")
        .bind(auth)
        .execute(&pool)
        .await
        .unwrap();
    for (p, task, auth, action, identity) in &buyers {
        for _ in 0..2 {
            let Dispatch::Done { body, .. } = tokio::time::timeout(
                Duration::from_secs(5),
                executor.dispatch(
                    p,
                    "fixture",
                    *task,
                    0,
                    *action,
                    *identity,
                    *auth,
                    &plan,
                    &step,
                    &json!({"url":url}),
                ),
            )
            .await
            .unwrap()
            .unwrap() else {
                panic!("ready answer not dispatched")
            };
            assert_eq!(body["result"]["data"], data);
        }
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        buyers.len(),
        "one payment call per buyer, including exact retries"
    );
    assert_eq!(
        count(&pool, "actions.executions").await,
        buyers.len() as i64
    );
    server.abort();
    std::env::remove_var("BRIGHTDATA_API_KEY");
}
