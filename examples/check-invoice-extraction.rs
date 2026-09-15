//! Real intent model: read one invoice through all three reading paths (text
//! layer, page image, original PDF) into reviewable supplier-check questions.
//! No storage, no source purchase.
//! Usage: INVOICE_TEXT=invoice.txt INVOICE_IMAGE=invoice.png INVOICE_PDF=invoice.pdf cargo run --example check-invoice-extraction
use apiosk_gateway_v2::{documents::{self, Content}, invoice_document, parser::Parser, supplier_check};
use base64::Engine as _;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let parser = Parser::new(reqwest::Client::new(), std::env::var("INTENT_API_URL")?, std::env::var("INTENT_API_KEY")?, std::env::var("INTENT_MODEL")?);
    let text = std::fs::read_to_string(std::env::var("INVOICE_TEXT")?)?;
    let image = format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(std::fs::read(std::env::var("INVOICE_IMAGE")?)?));
    let pdf = std::fs::read(std::env::var("INVOICE_PDF")?)?;
    for label in ["text_layer", "page_image", "server_file"] {
        let started = std::time::Instant::now();
        let raw = match label {
            "text_layer" => parser.extract_invoice(Content::Text(&text)).await?,
            "page_image" => parser.extract_invoice(Content::Image(&image)).await?,
            _ => parser.extract_invoice(Content::File { file_name: "invoice.pdf", data_url: documents::data_url("application/pdf", &pdf) }).await?,
        };
        let verified_text = (label == "text_layer").then_some(text.as_str());
        let (fields, removed) = invoice_document::verify(&raw, verified_text);
        let question = invoice_document::question(&fields, true);
        let parsed = supplier_check::parse(&question);
        println!("{label} ({} ms): removed={removed:?}\n  question={question}\n  parsed kvk={:?} vat={:?} iban={:?} domain={:?} postcode={:?} number={:?}",
            started.elapsed().as_millis(), parsed.kvk, parsed.vat, parsed.iban, parsed.domain, parsed.postcode, parsed.house_number);
        anyhow::ensure!(fields["supplier_name"] == "Voorbeeld Levering B.V.", "wrong supplier: {fields}");
        anyhow::ensure!(parsed.iban.as_deref() == Some("NL91ABNA0417164300"), "document instruction changed the IBAN: {fields}");
        anyhow::ensure!(parsed.kvk.as_deref() == Some("30204462") && parsed.vat.as_deref() == Some("NL815839091B01"), "identifiers: {fields}");
    }
    Ok(())
}
