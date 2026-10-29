use apiosk_gateway_v2::{
    auth::Auth,
    catalog::CatalogCache,
    compat::CommerceService,
    engine::Engine,
    execution::Executor,
    parser::Parser,
    server::{router, ServerState},
    state::Signer,
    store::Store,
};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use std::{env, future::IntoFuture, str::FromStr, sync::Arc, time::Duration};
use tokio::sync::Semaphore;
use tracing_subscriber::prelude::*;

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
fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    // Retain commerce error reporting for the lifetime of the single process.
    // Its SDK must initialize before Tokio, not inside an async main function.
    let _error_reporting = commerce_runtime::init_error_reporting();
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(commerce_runtime::error_reporting_layer())
        .with(tracing_subscriber::fmt::layer().json())
        .init();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run())
}

async fn run() -> anyhow::Result<()> {
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
    // Ask capability interpretation has its own pinned model. Planning,
    // source analysis and conversation replies retain their separate models.
    let intent_model = apiosk_gateway_v2::parser::pinned_model("INTENT_MODEL");
    let analysis_model = apiosk_gateway_v2::parser::pinned_model("ANALYSIS_MODEL");
    let ask_model = apiosk_gateway_v2::ask_v2::DEFAULT_MODEL.to_owned();
    tracing::info!(model=%intent_model, "intent parser configured");
    let openrouter_key = env::var("OPENROUTER_API_KEY").ok().filter(|key| !key.trim().is_empty())
        .or_else(|| env::var("INTENT_API_KEY").ok()).unwrap_or_default();
    let engine = Arc::new(Engine {
        store: store.clone(),
        // Ask search also reads the catalog in English through the separate
        // background translator (ASK_V2_TRANSLATION=0 turns it off).
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
    apiosk_gateway_v2::company_checks::start(engine.clone());
    // Commerce initializes once in this process with its existing database
    // role and payment configuration. The public listener serves both routers.
    let commerce_runtime = commerce_runtime::CommerceRuntime::from_env().await?;
    let app = router(
        ServerState {
            engine,
            auth,
            permits: Arc::new(Semaphore::new(32)),
            commerce: Some(CommerceService::with_in_flight(commerce_runtime.router(), commerce_runtime.in_flight())),
        },
        &app_url,
    )?;
    let bind = env::var("BIND_ADDRESS").unwrap_or_else(|_| "127.0.0.1:8082".into());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    apiosk_gateway_v2::start_supporting_news();
    tracing::info!(address=%bind,"gateway v2 listening");
    let (stop_admission, stopped) = tokio::sync::oneshot::channel();
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async move { let _ = stopped.await; })
        .into_future();
    tokio::pin!(server);
    let deadline = tokio::select! {
        result = &mut server => {
            result?;
            tokio::time::Instant::now() + Duration::from_secs(120)
        }
        signal = shutdown_signal() => {
            signal?;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
            let _ = stop_admission.send(());
            for handle in &kvk_handles { handle.abort(); }
            // Stop new background jobs while existing HTTP and payment work
            // drain together, within the existing Fly shutdown budget.
            let (http, _) = tokio::join!(
                tokio::time::timeout_at(deadline, &mut server),
                tokio::time::timeout_at(deadline, commerce_runtime.shutdown(Duration::from_secs(120))),
            );
            match http {
                Ok(result) => result?,
                Err(_) => tracing::warn!("HTTP drain reached the shutdown deadline"),
            }
            deadline
        }
    };
    // A request accepted before shutdown can begin native dispatch after the
    // first idle observation. Recheck after HTTP admission has fully drained.
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    let drained = tokio::time::timeout_at(deadline, commerce_runtime.shutdown(remaining))
        .await.unwrap_or(false);
    if !drained {
        tracing::warn!("Commerce drain reached the shutdown deadline; durable request status remains authoritative");
    }
    for handle in kvk_handles {
        handle.abort();
    }
    Ok(())
}

async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}
