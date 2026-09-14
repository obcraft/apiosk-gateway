//! Production catalog + real intent model for the Reuters and Bloomberg news
//! sources. No provider purchases or customer state mutations.
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
    for (slug, expected) in [("reuters", 21), ("bloomberg", 1)] {
        let n = c.services.iter().filter(|s| s.api_slug == slug).count();
        anyhow::ensure!(
            n == expected,
            "{slug}: {n} services; rejected={:?}",
            c.rejected
        );
    }
    let view = c.parser_view();
    for slug in ["reuters", "bloomberg"] {
        anyhow::ensure!(
            view["sources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["slug"] == slug && s["available"] != false),
            "{slug} not available to the parser"
        );
    }
    let parser = Parser::new(
        reqwest::Client::new(),
        std::env::var("INTENT_API_URL")?,
        std::env::var("INTENT_API_KEY")?,
        std::env::var("INTENT_MODEL")?,
    );
    for (question, cap) in [
        ("What is the latest Reuters news?", "news.reuters-latest"),
        (
            "Geef me de 5 nieuwste Reuters headlines.",
            "news.reuters-latest",
        ),
        (
            "Search Reuters for news about bitcoin.",
            "news.reuters-search",
        ),
        ("Zoek Reuters nieuws over Nvidia.", "news.reuters-search"),
        (
            "Show me the Reuters headlines for Africa.",
            "news.reuters-section-africa",
        ),
        (
            "Wat zijn de technologie headlines van Reuters?",
            "news.reuters-section-technology",
        ),
        (
            "Welk marktnieuws heeft Reuters vandaag?",
            "news.reuters-section-markets",
        ),
        (
            "Which news sections does Reuters have?",
            "news.reuters-sections",
        ),
        (
            "What is the latest news on Bloomberg?",
            "news.bloomberg-latest",
        ),
        (
            "Wat is het laatste nieuws van Bloomberg?",
            "news.bloomberg-latest",
        ),
    ] {
        let mut task = Task::default();
        let mut intent = parser
            .parse(uuid::Uuid::new_v4(), question, &task, &c)
            .await?;
        anyhow::ensure!(
            intent.goals.iter().any(|g| g.capability == cap),
            "Wrong goal for {question:?}: {intent:?}"
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
            anyhow::bail!("No executable plan for {question:?}: {intent:?}")
        };
        let step = plan
            .steps
            .iter()
            .find(|s| s.service.contract.capability_slug == cap)
            .ok_or_else(|| anyhow::anyhow!("{cap} not planned for {question:?}"))?;
        let inputs = step.service.inputs(&task, step.entity_ref, &c.vocabulary)?;
        let text = |k: &str| {
            inputs
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_lowercase()
        };
        match cap {
            "news.reuters-search" => anyhow::ensure!(
                text("q").contains("bitcoin") || text("q").contains("nvidia"),
                "search keywords not bound: {inputs}"
            ),
            "news.reuters-section-africa" => anyhow::ensure!(
                text("url") == "https://www.reuters.com/world/africa/",
                "section not bound: {inputs}"
            ),
            "news.reuters-section-technology" => anyhow::ensure!(
                text("url") == "https://www.reuters.com/technology/",
                "section not bound: {inputs}"
            ),
            _ => {}
        }
        println!(
            "Model + planner + binding passed: {question:?} -> {cap} {inputs}; quote={}",
            plan.cap_atomic
        );
    }
    Ok(())
}
