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
        serde_json::from_str(include_str!("../tests/fixtures/research-report-catalog.json"))?;
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
            "Haal voor MoneyBird B.V. (KVK-nummer 58209344) de beschikbare jaarrekening en het KVK-bedrijfsprofiel op. Verrijk de adressen uit het profiel met PDOK-locatiegegevens. Vat samen en analyseer in een downloadbaar PDF-document. Toon eerst één totaalprijs ter goedkeuring.",
            &Task::default(),
            &catalog,
        )
        .await?;
    anyhow::ensure!(
        intent
            .goals
            .iter()
            .any(|g| g.capability == "company.financial-statements"),
        "Expected annual accounts goal"
    );
    for capability in ["company.registry-profile","company.registered-address-context"] {
        anyhow::ensure!(intent.goals.iter().any(|g|g.capability==capability),"missing {capability}");
    }
    anyhow::ensure!(intent.goals.len()==3 && intent.unresolved_references.is_empty(),"PDF must not become an unsupported provider goal");
    anyhow::ensure!(intent.answer_operations.iter().any(|a|a.to_lowercase().contains("pdf")),"PDF output requirement lost");
    println!("Validated model={model}, subjects={}, goals={}, elapsed_ms={}. Synthetic inputs only; no database or provider calls.", intent.subjects.len(), intent.goals.len(), before.elapsed().as_millis());
    Ok(())
}
