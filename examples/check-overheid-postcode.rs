//! Read-only upstream and answer check. Never authorizes an Apiosk purchase.
#[path = "../legacy-executor/src/proxy/overheid.rs"]
mod overheid;
use apiosk_gateway_v2::{catalog::Catalog, parser::Parser};
use serde_json::{json, Value};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let key = std::env::var("OVERHEID_IO_API_KEY").or_else(|_| std::env::var("overheid.io"))?;
    let url = overheid::prepare(
        "overheid-io",
        "GET",
        "https://api.overheid.io/bag?filters%5Bpostcode%5D=3011",
    )
    .map_err(anyhow::Error::msg)?
    .expect("postcode area must be normalized");
    let http = reqwest::Client::new();
    let data: Value = http
        .get(&url)
        .header("ovio-api-key", key)
        .header("accept", "application/json")
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let rows = data["_embedded"]["adres"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("missing addresses"))?;
    anyhow::ensure!(
        !rows.is_empty() && rows.len() <= 25,
        "missing or unbounded address page"
    );
    anyhow::ensure!(
        rows.iter().all(|row| row["postcode"]
            .as_str()
            .is_some_and(|postcode| postcode.starts_with("3011"))),
        "incorrect postcode area"
    );
    let catalog = Catalog::build(serde_json::from_str(include_str!(
        "../tests/fixtures/overheid-bag/catalog.json"
    ))?);
    let service = &catalog.services[0];
    let extraction = service.contract.extract(&data, &catalog.vocabulary)?;
    anyhow::ensure!(
        extraction.subjects.len() == rows.len() && extraction.rejected.is_empty(),
        "live address data does not match the execution contract"
    );
    anyhow::ensure!(
        extraction.subjects.iter().all(|subject| service
            .contract
            .output_map
            .produces
            .keys()
            .all(|kind| subject.facts.iter().any(|fact| &fact.fact_type == kind))),
        "missing required address fields"
    );
    let sources = vec![
        json!({"result_ref":"overheid-bag-live-check","document":{"data":data,"source":{"provider":"overheid-io","name":"Overheid.io","url":url},"coverage":"partial","coverage_note":"One page of matching addresses; more pages are available."}}),
    ];
    let parser = Parser::new(
        http,
        std::env::var("INTENT_API_URL")?,
        std::env::var("INTENT_API_KEY")?,
        std::env::var("INTENT_MODEL")?,
    );
    let answer = parser.analyze(&json!(["Find addresses in postcode area 3011 using Overheid.io. Give the reported total and a few examples from this page; state that these are addresses, not people."]), &sources, &Default::default()).await?;
    anyhow::ensure!(
        !answer["observations"].as_array().unwrap().is_empty(),
        "missing natural answer"
    );
    std::fs::create_dir_all("output/overheid-postcode-fix")?;
    std::fs::write(
        "output/overheid-postcode-fix/verified.json",
        serde_json::to_vec_pretty(&json!({"source":sources,"analysis":answer}))?,
    )?;
    println!(
        "{}",
        json!({"url":url,"rows":rows.len(),"total":data["totalItemCount"],"analysis":answer})
    );
    Ok(())
}
