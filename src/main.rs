use apiosk_gateway_v2::{
    auth::Auth,
    catalog::CatalogCache,
    compat::ExecutorCompat,
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
    // The Supabase session pooler admits at most 15 clients for this role and
    // refuses the 16th outright instead of queueing it. A pool that may grow
    // past that turns a burst of parallel API calls into 503s at
    // authentication; below it, a burst waits for a free connection instead.
    // The headroom is for migrations and operator sessions on the same role.
    let pool_size = env::var("DATABASE_POOL_SIZE").ok().and_then(|v| v.parse::<u32>().ok()).filter(|n| (1..=14).contains(n)).unwrap_or(12);
    let pool = PgPoolOptions::new()
        .max_connections(pool_size)
        .acquire_timeout(Duration::from_secs(5))
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SET search_path TO public, gateway_v2, kyb, providers, catalog, actions, billing, agents, wallets, research, integrations, observability, support, content, operations, identity, extensions")
                    .execute(&mut *conn)
                    .await?;
                sqlx::query("SET statement_timeout='10s'")
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect_with(database_options)
        .await?;
    // Startup does not run migrations or change production schemas.
    sqlx::query("SELECT id FROM tasks LIMIT 0")
        .execute(&pool)
        .await?;
    let mut kvk_handles = Vec::new();
    if env::var("V2_EXECUTION_ENABLED").as_deref() == Ok("1")
        && env::var("KVK_WORKERS_ENABLED").as_deref() != Ok("0")
    {
        let routes = kvk_accounts::Workers::configured(pool.clone()).await?;
        for route in &routes {
            route.check_schema().await?;
        }
        // Five configured routes: one worker each. A single direct route gets
        // five consumers but still only one shared per-IP upstream allowance.
        let consumers = if routes.len() == 1 { 5 } else { 1 };
        for route in routes {
            kvk_handles.extend(route.start(consumers));
        }
    }
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
            match housekeeping.reconcile().await {
                Ok((closed, held)) => {
                    if closed > 0 {
                        tracing::info!(closed, "released abandoned dispatches");
                    }
                    // Charged, with no outcome recorded. Nothing here may decide
                    // that on a timer, so say it every sweep until someone does.
                    if held > 0 {
                        tracing::warn!(
                            held,
                            "executions hold a ledger charge with no recorded outcome"
                        );
                    }
                }
                Err(_) => tracing::warn!("execution reconciliation failed"),
            }
        }
    });
    // One model for every stage (planning, analysis, Ask parse), via OpenRouter;
    // the conversation replies alone use their own constant model
    // (parser::conversation_reply), never a setting.
    let intent_model = apiosk_gateway_v2::parser::pinned_model("INTENT_MODEL");
    let analysis_model = apiosk_gateway_v2::parser::pinned_model("ANALYSIS_MODEL");
    let ask_model = apiosk_gateway_v2::parser::pinned_model("ASK_V2_MODEL");
    tracing::info!(model=%intent_model, "intent parser configured");
    let openrouter_key = env::var("OPENROUTER_API_KEY").ok().filter(|key| !key.trim().is_empty())
        .or_else(|| env::var("INTENT_API_KEY").ok()).unwrap_or_default();
    let engine = Arc::new(Engine {
        store: store.clone(),
        // Ask search also reads the catalog in English, translated in the
        // background by the same model (ASK_V2_TRANSLATION=0 turns it off).
        catalog: CatalogCache::new(pool.clone()).with_english(
            apiosk_gateway_v2::english::English::from_env(http.clone(), &openrouter_key),
        ),
        parser: Parser::new(
            http.clone(),
            checked_url(
                "INTENT_API_URL",
                env::var("INTENT_API_URL")
                    .unwrap_or_else(|_| "https://openrouter.ai/api/v1/chat/completions".into()),
            )?,
            env::var("INTENT_API_KEY").unwrap_or_default(),
            intent_model,
        ).with_routing(
            Some(analysis_model),
            Some(env::var("INTENT_PROVIDER_SORT").unwrap_or_else(|_| "latency".into())),
            env::var("ANALYSIS_PROVIDER_SORT").ok(),
        )?.with_ask_v2(apiosk_gateway_v2::ask_v2::Client::new(
            http.clone(),
            openrouter_key.clone(),
            ask_model,
        )).with_conversation_replies(apiosk_gateway_v2::parser::conversation_reply::Client::new(
            http.clone(),
            openrouter_key,
        )),
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
    // Warm optional branding without holding up the listener or health checks.
    let branding = engine.clone();
    tokio::spawn(async move {
        if branding.catalog.sources().await.is_err() {
            tracing::warn!("source branding will refresh on demand");
        }
    });
    apiosk_gateway_v2::recurring::start(engine.clone());
    let app = router(
        ServerState {
            engine,
            auth,
            permits: Arc::new(Semaphore::new(32)),
            executor_compat: env::var("EMBEDDED_EXECUTOR_ORIGIN")
                .ok()
                .map(|origin| ExecutorCompat::new(reqwest::Client::new(), origin))
                .transpose()?,
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
    for handle in kvk_handles {
        handle.abort();
    }
    Ok(())
}
