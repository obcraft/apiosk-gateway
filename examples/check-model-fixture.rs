//! One model request using a wholly synthetic fixture catalog. No database,
//! production catalog, account data, provider execution or wallet access.
use apiosk_gateway_v2::{
    catalog::{Catalog, Snapshot},
    parser::Parser,
    state::Task,
};
use std::env;
use uuid::Uuid;
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let snapshot: Snapshot =
        serde_json::from_str(include_str!("../tests/fixtures/integration-catalog.json"))?;
    let catalog = Catalog::build(snapshot);
    let model = env::var("INTENT_MODEL")?;
    let parser = Parser::new(
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        "https://openrouter.ai/api/v1/chat/completions".into(),
        env::var("INTENT_API_KEY")?,
        model.clone(),
    );
    let before = std::time::Instant::now();
    let intent = parser
        .parse(
            Uuid::new_v4(),
            "Get company accounts for Fictional Sample Company.",
            &Task::default(),
            &catalog,
        )
        .await?;
    anyhow::ensure!(
        intent
            .goals
            .iter()
            .any(|g| g.capability == "company.accounts"),
        "Expected accounts goal"
    );
    println!("Validated model={model}, subjects={}, goals={}, elapsed_ms={}. Synthetic inputs only; no database or provider calls.", intent.subjects.len(), intent.goals.len(), before.elapsed().as_millis());
    Ok(())
}
