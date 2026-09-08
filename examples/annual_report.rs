use serde_json::{json, Value};
fn main() -> anyhow::Result<()> {
    let data: Value = serde_json::from_str(include_str!("../crates/catalog-core/fixtures/contracts/payloads/kvk-jaarrekening-86717162.json"))?;
    let document = json!({"source":{"provider":"kvk-dutch-business-register","path":apiosk_gateway_v2::annual_report::ENDPOINT,"url":"https://opendata.kvk.nl","retrieved_at":"2026-09-08 (voorbeeld uit opgeslagen bronfixture)"},"report_subject":{"name":"KVK 86717162 - voorbeeldrapport"},"data":data});
    std::fs::create_dir_all("output/pdf")?;
    std::fs::write("output/pdf/jaarrekening-voorbeeld.pdf",apiosk_gateway_v2::annual_report::render(&document)?)?;
    Ok(())
}
