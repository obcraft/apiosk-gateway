use apiosk_gateway_v2::{
    auth::Auth,
    catalog::CatalogCache,
    engine::Engine,
    execution::Executor,
    parser::Parser,
    server::{router, ServerState},
    state::Signer,
    store::Store,
};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use std::{env, str::FromStr, sync::Arc, time::Duration};
use tokio::sync::Semaphore;

fn required(key: &str) -> anyhow::Result<String> {
    env::var(key).map_err(|_| anyhow::anyhow!("missing {key}"))
}
fn checked_url(key: &str, value: String) -> anyhow::Result<String> {
    let url = url::Url::parse(&value)?;
    anyhow::ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "invalid {key}"
    );
    anyhow::ensure!(
        url.scheme() == "https"
            || (url.scheme() == "http"
                && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))),
        "{key} requires HTTPS outside loopback"
    );
    Ok(value.trim_end_matches('/').to_owned())
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .json()
        .init();
    let database = required("DATABASE_URL")?;
    let database_url = url::Url::parse(&database)?;
    let mut database_options = PgConnectOptions::from_str(&database)?;
    if !matches!(
        database_url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]")
    ) {
        database_options = database_options.ssl_mode(PgSslMode::VerifyFull);
    }
    let pool = PgPoolOptions::new()
        .max_connections(16)
        .acquire_timeout(Duration::from_secs(5))
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SET statement_timeout='10s'")
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect_with(database_options)
        .await?;
    // Startup does not run migrations or change production schemas.
    sqlx::query("SELECT id FROM gateway_v2.tasks LIMIT 0")
        .execute(&pool)
        .await?;
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30))
        .build()?;
    let supabase_url = checked_url("SUPABASE_URL", required("SUPABASE_URL")?)?;
    let app_url = checked_url("APP_ORIGIN", required("APP_ORIGIN")?)?;
    let auth = Arc::new(Auth {
        pool: pool.clone(),
        http: http.clone(),
        supabase_url: supabase_url.clone(),
        publishable_key: required("SUPABASE_PUBLISHABLE_KEY")?,
    });
    let store = Store { pool: pool.clone() };
    let housekeeping = store.clone();
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(300));
        loop {
            timer.tick().await;
            if housekeeping.prune().await.is_err() {
                tracing::warn!("state retention maintenance failed");
            }
        }
    });
    let engine = Arc::new(Engine {
        store: store.clone(),
        catalog: CatalogCache::new(pool.clone()),
        parser: Parser::new(
            http.clone(),
            checked_url(
                "INTENT_API_URL",
                env::var("INTENT_API_URL")
                    .unwrap_or_else(|_| "https://openrouter.ai/api/v1/chat/completions".into()),
            )?,
            env::var("INTENT_API_KEY").unwrap_or_default(),
            env::var("INTENT_MODEL").unwrap_or_else(|_| "google/gemini-2.5-flash".into()),
        ),
        signer: Signer::new(required("STATE_SIGNING_KEY")?.into_bytes())?,
        executor: Executor {
            store,
            http,
            supabase_url,
            enabled: env::var("V2_EXECUTION_ENABLED").as_deref() == Ok("1"),
        },
        app_url: app_url.clone(),
    });
    let catalog = engine.catalog.get().await?;
    tracing::info!(
        services = catalog.services.len(),
        quarantined = catalog.rejected.len(),
        "catalog loaded"
    );
    let app = router(
        ServerState {
            engine,
            auth,
            permits: Arc::new(Semaphore::new(32)),
        },
        &app_url,
    )?;
    let bind = env::var("BIND_ADDRESS").unwrap_or_else(|_| "127.0.0.1:8082".into());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(address=%bind,"gateway v2 listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
        })
        .await?;
    Ok(())
}
