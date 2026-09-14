//! Analyze a supplied public-source fixture; never authorizes or executes a purchase.
use apiosk_gateway_v2::parser::Parser;
use serde_json::{json, Value};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    dotenvy::dotenv().ok();
    let data: Value =
        serde_json::from_slice(&std::fs::read("output/ask-live-fix/pdok-3011.json")?)?;
    let sources = vec![
        json!({"result_ref":"public-pdok-fixture","document":{"data":data,"subject":{"label":"3011"},"source":{"name":"PDOK","provider":"pdok","url":"https://api.pdok.nl/cbs/postcode4/ogc/v1/collections/postcode4/items?postcode=3011&limit=100&f=json"}}}),
    ];
    let parser = Parser::new(
        reqwest::Client::new(),
        std::env::var("INTENT_API_URL")?,
        std::env::var("INTENT_API_KEY")?,
        std::env::var("INTENT_MODEL")?,
    );
    let answer=parser.analyze(&json!(["How many people live in postcode 3011, according to PDOK? Use the latest available observation and explicitly state its reference period."]),&sources,&Default::default()).await?;
    anyhow::ensure!(
        !answer["observations"].as_array().unwrap().is_empty(),
        "missing population answer"
    );
    let text = answer["observations"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|o| o["text"].as_str())
        .collect::<Vec<_>>()
        .join(" ");
    anyhow::ensure!(
        text.contains("20440") && text.contains("2024"),
        "answer did not use newest available period"
    );
    std::fs::write(
        "output/ask-live-fix/answer.json",
        serde_json::to_vec_pretty(&answer)?,
    )?;
    for observation in answer["observations"].as_array().unwrap() {
        println!("{}", observation["text"]);
    }
    Ok(())
}
