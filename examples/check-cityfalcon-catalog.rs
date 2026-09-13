//! Production catalog + real model + complete captured provider responses.
//! No provider purchases or customer state mutations.
use apiosk_gateway_v2::{
    catalog::CatalogCache,
    parser::Parser,
    resolver::{resolve, Resolution},
    state::{Fact, Task},
};
use serde_json::{json, Value};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let pool=sqlx::postgres::PgPoolOptions::new().max_connections(1).after_connect(|conn,_|Box::pin(async move {
  sqlx::query("SET search_path TO public, gateway_v2, kyb, providers, catalog, actions, billing, agents, wallets, research, integrations, observability, support, content, operations, identity, extensions").execute(conn).await?;Ok(())
 })).connect(&std::env::var("DATABASE_URL")?).await?;
    let c = CatalogCache::new(pool).fresh().await?;
    anyhow::ensure!(
        c.services
            .iter()
            .filter(|s| s.api_slug == "cityfalcon-financial-api")
            .count()
            == 4,
        "CityFALCON missing; rejected={:?}",
        c.rejected
    );
    for s in c
        .services
        .iter()
        .filter(|s| s.api_slug == "cityfalcon-financial-api")
    {
        let path = format!(
            "../evidence/2026-09-13/catalog-repair/cityfalcon/{}.json",
            s.path.replace('/', "_")
        );
        let raw: Value = serde_json::from_slice(&std::fs::read(path)?)?;
        let result = s.contract.extract(&raw["data"], &c.vocabulary)?;
        anyhow::ensure!(
            result.rejected.is_empty() && result.subjects.len() == 1,
            "Full response invalid {}",
            s.path
        );
        println!("Full live response accepted: {}", s.path);
    }
    let parser = Parser::new(
        reqwest::Client::new(),
        std::env::var("INTENT_API_URL")?,
        std::env::var("INTENT_API_KEY")?,
        std::env::var("INTENT_MODEL")?,
    );
    for (question, cap) in [
        (
            "Haal via CityFALCON het nieuws op voor AAPL_US.",
            "finance_markets.cityfalcon-news",
        ),
        (
            "Haal via CityFALCON het sentiment op voor AAPL_US.",
            "finance_markets.cityfalcon-sentiment",
        ),
        (
            "Haal via CityFALCON de insidertransacties op voor AAPL_US.",
            "finance_markets.cityfalcon-insider-transactions",
        ),
        (
            "Haal via CityFALCON de investor relations documenten op voor AAPL_US.",
            "finance_markets.cityfalcon-investor-relations",
        ),
        (
            "Gebruik StableEnrich voor een webzoekopdracht naar Apiosk data APIs.",
            "web.stableenrich-search",
        ),
        (
            "Gebruik Linkup om een antwoord met bronnen te zoeken op de vraag: wat doet Apiosk?",
            "web.linkup-answer",
        ),
        ("Lees https://example.com via Linkup.", "web.linkup-fetch"),
        (
            "Zoek via Browserbase naar Apiosk.",
            "web.browserbase-search",
        ),
        (
            "Lees https://example.com via Browserbase.",
            "web.browserbase-fetch",
        ),
    ] {
        let mut task = Task::default();
        let mut intent = parser
            .parse(uuid::Uuid::new_v4(), question, &task, &c)
            .await?;
        anyhow::ensure!(
            intent.goals.iter().any(|g| g.capability == cap),
            "Wrong goal: {intent:?}"
        );
        for subject in &mut intent.subjects {
            let id = uuid::Uuid::new_v4();
            subject.entity_ref = Some(id);
            for (k, v) in &subject.values {
                task.facts.push(Fact {
                    entity_ref: id,
                    semantic_type: k.clone(),
                    value: v.clone(),
                    provenance: json!({"kind":"user_input"}),
                    observed_at: chrono::Utc::now(),
                    expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
                    verification: "user_asserted".into(),
                });
            }
        }
        let Resolution::Plan(plan) = resolve(&c, &task, &intent) else {
            anyhow::bail!("No executable plan for {question}: {intent:?}")
        };
        anyhow::ensure!(plan
            .steps
            .iter()
            .any(|s| s.service.contract.capability_slug == cap));
        for step in &plan.steps {
            let inputs = step.service.inputs(&task, step.entity_ref, &c.vocabulary)?;
            if cap.starts_with("finance_markets.cityfalcon") {
                anyhow::ensure!(inputs["identifiers"] == "AAPL_US");
            } else if cap.ends_with("fetch") {
                anyhow::ensure!(inputs["url"] == "https://example.com");
            } else {
                anyhow::ensure!(inputs
                    .get("q")
                    .or_else(|| inputs.get("query"))
                    .and_then(Value::as_str)
                    .is_some_and(|q| q.to_lowercase().contains("apiosk")));
            }
        }
        println!(
            "Model + planner + binding passed: {cap}; quote={}",
            plan.cap_atomic
        );
    }
    Ok(())
}
