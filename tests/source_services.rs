//! Grouped service browsing against an isolated source catalog. The integration
//! runner gives this target its own database so other suites may mutate theirs.
use apiosk_gateway_v2::{
    auth::Auth, catalog::CatalogCache, engine::Engine, execution::Executor,
    parser::Parser, server::{router, ServerState}, state::Signer, store::Store,
};
use reqwest::Client;
use serde_json::Value;
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;
use uuid::Uuid;

const READ_ONLY: &str = "apk_org_live_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

struct Api { http: Client, base: String }
impl Api {
    async fn get(&self, path: &str, key: &str) -> (u16, Value) {
        let response = self.http.get(format!("{}{path}", self.base)).bearer_auth(key).send().await.unwrap();
        (response.status().as_u16(), response.json().await.unwrap())
    }
}
async fn start_api(pool: &PgPool) -> Api {
    let http = Client::new();
    let store = Store { pool: pool.clone() };
    let origin = "http://127.0.0.1:1";
    let engine = Arc::new(Engine {
        store: store.clone(), catalog: CatalogCache::new(pool.clone()),
        parser: Parser::new(http.clone(), format!("{origin}/chat"), "fixture".into(), "fixture".into()),
        signer: Signer::new(vec![5; 32]).unwrap(),
        executor: Executor { store, http: http.clone(), supabase_url: origin.into(), enabled: false },
        app_url: "http://localhost:5173".into(),
    });
    let app = router(ServerState {
        engine,
        auth: Arc::new(Auth { pool: pool.clone(), http: http.clone(), supabase_url: origin.into(), publishable_key: "fixture".into() }),
        permits: Arc::new(Semaphore::new(4)), executor_compat: None,
    }, "http://localhost:5173").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Api { http, base }
}

#[tokio::test]
#[ignore = "requires local tests/support-db.mjs; no live database"]
async fn grouped_service_browsing_preserves_visibility_and_authentication() {
    let pool = PgPoolOptions::new()
        .after_connect(|conn, _| Box::pin(async move {
            sqlx::query("SET search_path TO public, gateway_v2, providers, catalog, actions, billing, agents, wallets, research, integrations, observability, support, content, operations, identity, extensions").execute(conn).await?;
            Ok(())
        }))
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(10))
        .connect(&std::env::var("TEST_DATABASE_URL").unwrap_or_else(|_| "postgresql://postgres@127.0.0.1:54329/postgres?sslmode=disable".into()))
        .await.unwrap();
    let bazaar_owner = Uuid::new_v4();
    sqlx::query("INSERT INTO public.provider_profiles(id,username) VALUES($1,'coinbase-bazaar')")
        .bind(bazaar_owner).execute(&pool).await.unwrap();
    for (slug, status, active, verified, discoverable, suspended) in [
        ("cbz-alpha", "active", true, true, true, false),
        ("cbz-zeta", "active", true, true, true, false),
        ("cbz-hidden-pending", "pending", true, true, true, false),
        ("cbz-hidden-inactive", "active", false, true, true, false),
        ("cbz-hidden-unverified", "active", true, false, true, false),
        ("cbz-hidden-undiscoverable", "active", true, true, false, false),
        ("cbz-hidden-suspended", "active", true, true, true, true),
    ] {
        sqlx::query("INSERT INTO public.apis(id,owner_id,slug,name,status,active,is_verified,discoverable,suspended_at,description) VALUES($1,$2,$3,$3,$4,$5,$6,$7,CASE WHEN $8 THEN now() ELSE NULL END,'Unique oracle')")
            .bind(Uuid::new_v4()).bind(bazaar_owner).bind(slug).bind(status).bind(active).bind(verified).bind(discoverable).bind(suspended)
            .execute(&pool).await.unwrap();
    }
    // No model or provider listener exists: this route reads catalog metadata only.
    let api = start_api(&pool).await;
    let (status, _) = api.get("/v2/sources/coinbase-bazaar/services", "apk_org_live_unknown").await;
    assert_eq!(status, 401, "group service browsing requires source authentication");
    let (status, page) = api.get("/v2/sources/coinbase-bazaar/services?limit=1&sort=name-asc", READ_ONLY).await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["total"], 2, "only visible listings are paged, including both discovery-only services");
    assert_eq!(page["items"][0]["slug"], "cbz-alpha");
    assert_eq!(page["next_offset"], 1);
    let (status, page) = api.get("/v2/sources/coinbase-bazaar/services?limit=1&offset=1&sort=name-asc&search=oracle", READ_ONLY).await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["items"][0]["slug"], "cbz-zeta");
    assert!(page["next_offset"].is_null());
    assert_eq!(api.get("/v2/sources/coinbase-bazaar/services?limit=51", READ_ONLY).await.0, 400);
    assert_eq!(api.get("/v2/sources/fixture/services", READ_ONLY).await.0, 404);
    println!("PASS grouped services: scoped authentication, discovery-only pagination, search and all five visibility gates");
}
