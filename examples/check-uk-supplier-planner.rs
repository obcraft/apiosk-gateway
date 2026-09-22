//! One internal model-routing check. No source execution, customer approval,
//! balance access or database writes. Catalog and entities are test inputs.
use apiosk_gateway_v2::{
    catalog::{Catalog, Snapshot, CATALOG_SQL},
    parser::Parser,
    resolver::{resolve, Resolution},
    state::{Fact, Task},
    uk_supplier,
};
use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if let Some(path) = std::env::args().nth(1) {
        dotenvy::from_path(path)?;
    }
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!("../tests/fixtures/uk-supplier/catalog.json"))?;
    if std::env::var("CHECK_FULL_CATALOG").as_deref() == Ok("1") {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .after_connect(|connection, _| Box::pin(async move {
                sqlx::query("SET search_path TO public, gateway_v2, kyb, providers, catalog, actions, billing, agents, wallets, research, integrations, observability, support, content, operations, identity, extensions").execute(connection).await?;
                Ok(())
            }))
            .connect(&std::env::var("DATABASE_URL")?)
            .await?;
        let value: Value = sqlx::query_scalar(CATALOG_SQL).fetch_one(&pool).await?;
        pool.close().await;
        let mut live: Snapshot = serde_json::from_value(value)?;
        // Retain current production contracts; merge only the not-yet-active additions.
        if std::env::var("CHECK_PUBLISHED_ONLY").as_deref() != Ok("1") {
            for service in snapshot.services {
                if !live.services.iter().any(|s| s.id == service.id) {
                    live.services.push(service);
                }
            }
            for fact in snapshot.vocabulary {
                if !live.vocabulary.iter().any(|f| f.slug == fact.slug) {
                    live.vocabulary.push(fact);
                }
            }
        }
        snapshot = live;
    }
    let catalog = Catalog::build(snapshot);
    let model = std::env::var("INTENT_MODEL")?;
    let parser = Parser::new(
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        "https://openrouter.ai/api/v1/chat/completions".into(),
        std::env::var("INTENT_API_KEY")?,
        model.clone(),
    );
    let question="We’re onboarding Anthropic as a supplier. Use Apiosk to verify its UK entity: ownership, filed accounts, VAT validity, and director screening.";
    let mut intent = parser
        .parse(Uuid::new_v4(), question, &Task::default(), &catalog)
        .await?;
    println!("model={model}; intent={}", serde_json::to_string(&intent)?);
    anyhow::ensure!(
        intent.goals.len() == 1 && intent.goals[0].capability == uk_supplier::PRESET,
        "Exact question did not route to the complete preset"
    );
    uk_supplier::expand(&mut intent, &catalog);
    let mut task = Task::default();
    for subject in &mut intent.subjects {
        let entity = Uuid::new_v4();
        subject.entity_ref = Some(entity);
        for (kind, value) in &subject.values {
            task.facts.push(Fact {
                entity_ref: entity,
                semantic_type: kind.clone(),
                value: value.clone(),
                provenance: json!({}),
                observed_at: Utc::now(),
                expires_at: Utc::now() + chrono::Duration::hours(1),
                verification: "user_asserted".into(),
            });
        }
    }
    let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else {
        anyhow::bail!("Preset did not resolve");
    };
    anyhow::ensure!(plan.steps.len() == 12, "Expected 12 bounded planned calls");
    println!(
        "services={}; plan={}",
        catalog.services.len(),
        json!({"cap_atomic":plan.cap_atomic,"steps":plan.steps.iter().map(|s|json!({"capability":s.service.contract.capability_slug,"source":s.service.api_slug})).collect::<Vec<_>>()})
    );
    Ok(())
}
