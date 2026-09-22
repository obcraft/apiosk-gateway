//! Exercise the production renderer with labelled test financials and real PDOK maps.
//! No paid APIs, purchases or assertions about ASML.
use apiosk_gateway_v2::{report_assets, research_report};
use serde_json::{json, Value};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let fixture: Value = serde_json::from_str(include_str!(
        "../tests/fixtures/research-report-catalog.json"
    ))?;
    let mut sources = vec![];
    for (i, s) in fixture["services"].as_array().unwrap().iter().enumerate() {
        let mut data = s["sample_payload"].clone();
        if data["opendataFields"].is_array() {
            data["opendataFields"]
                .as_array_mut()
                .unwrap()
                .push(json!({"key":"Currency","value":"EUR"}));
        }
        let mut d = json!({"data":data,"source":{"provider":s["api_slug"],"name":s["name"],"path":s["path"],"retrieved_at":chrono::Utc::now(),"url":s["source_url"]},"subject":{"label":"TEST DOSSIER - fixture company, not an ASML assessment"},"coverage":"source_response"});
        d[report_assets::KEY] = report_assets::capture(&d).await;
        sources.push(json!({"result_ref":format!("fixture-{}",i+1),"document":d}));
    }
    let document = json!({"question":"TEST DOSSIER: production renderer, fixture financials and live official maps. Not an assessment of ASML.","quote_ref":"fixture","analysis":{"status":"partial","observations":[],"limitations":["Financial values and company details are test fixtures. Only the map images were retrieved live from PDOK.","Legal ownership, mortgage status, soil condition and creditworthiness have not been established."]},"sources":sources});
    std::fs::create_dir_all("output/pdf")?;
    std::fs::write(
        "output/pdf/research-dossier-test.pdf",
        research_report::render(&document)?,
    )?;
    std::fs::write(
        "output/pdf/research-dossier-test.zip",
        research_report::evidence_bundle(&document)?,
    )?;
    std::fs::write(
        "output/pdf/research-dossier-test.json",
        serde_json::to_vec_pretty(&document)?,
    )?;
    println!("Wrote PDF and evidence ZIP; fixture company/financials; maps captured live. No paid source calls.");
    Ok(())
}
