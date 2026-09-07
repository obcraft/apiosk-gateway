//! One real model call against a LOCAL catalog; no provider execution/payment.
use apiosk_gateway_v2::{catalog::CatalogCache, parser::Parser, state::Task};
use sqlx::postgres::PgPoolOptions;
use std::env;
use uuid::Uuid;
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let database = env::var("DATABASE_URL")?;
    let u = url::Url::parse(&database)?;
    anyhow::ensure!(
        matches!(u.host_str(), Some("127.0.0.1" | "localhost")) && u.path() == "/apiosk_v2_dev",
        "local development database required"
    );
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database)
        .await?;
    let catalog = CatalogCache::new(pool.clone()).get().await?;
    let parser = Parser::new(
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        env::var("INTENT_API_URL")?,
        env::var("INTENT_API_KEY")?,
        env::var("INTENT_MODEL")?,
    );
    let question = "Zoek de bedrijfsregistratie van ASML in NL.";
    let intent = parser
        .parse(Uuid::new_v4(), question, &Task::default(), &catalog)
        .await?;
    println!("Real intent parser validated: {} subjects, {} goals, {} catalog contracts. No provider execution.",intent.subjects.len(),intent.goals.len(),catalog.services.len());
    pool.close().await;
    Ok(())
}
