//! Inspect a captured catalog; optionally check a natural-language model intent.
use apiosk_gateway_v2::{
    catalog::{Catalog, Snapshot},
    parser::Parser,
    state::Task,
};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let path = std::env::args()
        .nth(1)
        .unwrap_or("output/sirenic-20260912/local-catalog.json".into());
    let snapshot: Snapshot = serde_json::from_slice(&std::fs::read(path)?)?;
    let c = Catalog::build(snapshot);
    anyhow::ensure!(
        c.services
            .iter()
            .filter(|s| s.api_slug == "sirenic")
            .count()
            == 6,
        "Sirenic runtime rejection: {:?}",
        c.rejected
    );
    if std::env::args().any(|a| a == "--paid") {
        let mut checked = 0;
        for entry in std::fs::read_dir("output/sirenic-20260912")? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("canonical-paid-") || !name.ends_with(".json") {
                continue;
            }
            let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(entry.path())?)?;
            let service = c
                .services
                .iter()
                .find(|s| Some(s.path.as_str()) == doc["path"].as_str())
                .ok_or_else(|| anyhow::anyhow!("missing live service"))?;
            anyhow::ensure!(doc["body"]["did"] == true, "unconfirmed live call");
            let extraction = service
                .contract
                .extract(&doc["body"]["execution"]["body"], &c.vocabulary)?;
            anyhow::ensure!(
                extraction.rejected.is_empty(),
                "Live {} mismatch: {:?}",
                service.path,
                extraction.rejected
            );
            checked += 1;
            println!(
                "Live {}: {} extracted subjects",
                service.path,
                extraction.subjects.len()
            );
        }
        anyhow::ensure!(
            checked == 6,
            "Expected six successful paid response captures, found {checked}"
        );
    }
    if std::env::args().any(|a| a == "--model") {
        let parser = Parser::new(
            reqwest::Client::new(),
            std::env::var("INTENT_API_URL")?,
            std::env::var("INTENT_API_KEY")?,
            std::env::var("INTENT_MODEL")?,
        );
        let intent = parser
            .parse(
                uuid::Uuid::new_v4(),
                "Retrieve the filed financial accounts of DANONE in France.",
                &Task::default(),
                &c,
            )
            .await?;
        anyhow::ensure!(
            intent
                .goals
                .iter()
                .any(|g| g.capability == "company.fr-financials"),
            "French financial capability not selected: {:?}",
            intent
        );
        println!(
            "Real model selected French financial accounts without an explicit source request"
        );
    }
    println!("Six Sirenic contracts accepted from the database catalog snapshot");
    Ok(())
}
