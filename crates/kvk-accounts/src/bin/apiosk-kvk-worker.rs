//! One standalone Fly worker. No HTTP ingress, buyer tokens, or payment keys.
use anyhow::{ensure, Context};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use std::{env, str::FromStr, time::Duration};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .json()
        .init();
    ensure!(
        env::var("KVK_EGRESS_ROUTES").is_ok(),
        "standalone worker requires a fixed egress route"
    );
    let database = env::var("DATABASE_URL").context("missing DATABASE_URL")?;
    let options = PgConnectOptions::from_str(&database)?.ssl_mode(PgSslMode::VerifyFull);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(10))
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SET statement_timeout='10s'")
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect_with(options)
        .await?;
    let mut routes = kvk_accounts::Workers::configured(pool.clone()).await?;
    ensure!(
        routes.len() == 1,
        "each Fly worker must use exactly one fixed egress route"
    );
    let worker = routes.remove(0);
    worker.check_schema().await?;
    tracing::info!(
        egress_ip = worker.egress_identity(),
        machine = env::var("FLY_MACHINE_ID").unwrap_or_default(),
        "KVK standalone worker ready"
    );
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = async {
            loop {
                if let Err(error) = worker.run_once().await { tracing::warn!(error=%error,"KVK queue worker failed"); }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        } => {}
    }
    pool.close().await;
    Ok(())
}
