//! PDF projection of one saved research turn. No model or provider calls on download.
use crate::{
    engine::Engine,
    state::{digest, Task},
};
use anyhow::{ensure, Context};
use chrono::Utc;
use genpdf::{
    elements::{Break, Paragraph},
    fonts::{FontData, FontFamily},
    style::{Color, Style},
    Element,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

fn ready(analysis: &Value) -> bool {
    matches!(analysis["status"].as_str(), Some("completed" | "partial"))
}

fn research_turn(task: &Task, quote: Uuid) -> anyhow::Result<(&str, &[Uuid], &Value)> {
    let (question, refs, analysis) = if task
        .proposal
        .as_ref()
        .is_some_and(|p| p["quote_ref"] == json!(quote))
    {
        ensure!(
            !task.cancelled && task.actions.is_empty(),
            "research is still running"
        );
        (
            task.conversation
                .last()
                .map(|t| t.question.as_str())
                .unwrap_or(""),
            &task.active_results,
            task.analysis.as_ref(),
        )
    } else {
        let turn = task
            .conversation
            .iter()
            .find(|t| {
                t.output.as_ref().is_some_and(|o| {
                    o.proposal
                        .as_ref()
                        .is_some_and(|p| p["quote_ref"] == json!(quote))
                })
            })
            .context("report turn unavailable")?;
        let output = turn.output.as_ref().unwrap();
        (
            turn.question.as_str(),
            &output.result_refs,
            output.analysis.as_ref(),
        )
    };
    let analysis = analysis
        .filter(|a| ready(a))
        .context("analysis is not complete")?;
    ensure!(
        !refs.is_empty() && refs.len() <= 32,
        "report source capacity exceeded"
    );
    Ok((question, refs, analysis))
}

fn research_document_from_results(
    task: &Task,
    quote: Uuid,
    documents: &BTreeMap<Uuid, Value>,
) -> anyhow::Result<Value> {
    let (question, refs, analysis) = research_turn(task, quote)?;
    let sources = refs
        .iter()
        .map(|result| {
            let document = documents.get(result).context("report result unavailable")?;
            Ok(json!({"result_ref":result,"document":document}))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(json!({"question":question,"quote_ref":quote,"analysis":analysis,"sources":sources}))
}

impl Engine {
    pub async fn research_document(
        &self,
        owner: Uuid,
        id: Uuid,
        task: &Task,
        quote: Uuid,
    ) -> anyhow::Result<Value> {
        let (_, refs, _) = research_turn(task, quote)?;
        let documents = self.store.results(owner, id, refs).await?;
        research_document_from_results(task, quote, &documents)
    }

    pub(crate) fn research_report_link_from_results(
        &self,
        owner: Uuid,
        id: Uuid,
        task: &Task,
        quote: Uuid,
        documents: &BTreeMap<Uuid, Value>,
    ) -> anyhow::Result<Value> {
        let document = research_document_from_results(task, quote, documents)?;
        self.sign_research_report(owner, id, quote, &document)
    }

    pub async fn research_report_link(
        &self,
        owner: Uuid,
        id: Uuid,
        task: &Task,
        quote: Uuid,
    ) -> anyhow::Result<Value> {
        let document = self.research_document(owner, id, task, quote).await?;
        self.sign_research_report(owner, id, quote, &document)
    }

    fn sign_research_report(
        &self,
        owner: Uuid,
        id: Uuid,
        quote: Uuid,
        document: &Value,
    ) -> anyhow::Result<Value> {
        let snapshot = digest(&document);
        let expires = (Utc::now() + chrono::Duration::hours(24)).timestamp();
        let signature = self
            .signer
            .research_signature(owner, id, quote, &snapshot, expires);
        Ok(
            json!({"format":"pdf","filename":format!("analyse-{quote}.pdf"),"expires_at":expires,
            "download_path":format!("/v2/tasks/{id}/reports/{quote}/report.pdf?owner={owner}&snapshot={snapshot}&expires={expires}&signature={signature}"),
            "evidence_download_path":format!("/v2/tasks/{id}/reports/{quote}/evidence.zip?owner={owner}&snapshot={snapshot}&expires={expires}&signature={signature}")}),
        )
    }
}

pub(crate) fn wrap(s: &str) -> String {
    let mut run = 0;
    let mut text = String::new();
    for c in s.chars() {
        if c.is_whitespace() {
            run = 0;
            text.push(' ');
        } else {
            if run == 65 {
                text.push(' ');
                run = 0;
            }
            text.push(c);
            run += 1;
        }
    }
    text
}
fn scalar(v: &Value) -> String {
    v.as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| v.to_string())
}
fn paragraph(doc: &mut genpdf::Document, s: &str) {
    doc.push(Paragraph::new(wrap(s)));
}
fn heading(doc: &mut genpdf::Document, s: &str, size: u8) {
    doc.push(Break::new(0.6));
    doc.push(
        Paragraph::new(wrap(s)).styled(
            Style::new()
                .bold()
                .with_font_size(size)
                .with_color(Color::Rgb(0, 80, 103)),
        ),
    );
}
fn fields(v: &Value, path: &str, rows: &mut Vec<(String, String)>) {
    if rows.len() >= 500 {
        return;
    }
    match v {
        Value::Object(o)
            if o.get("key").and_then(Value::as_str).is_some() && o.contains_key("value") =>
        {
            rows.push((o["key"].as_str().unwrap().to_owned(), scalar(&o["value"])));
        }
        Value::Object(o) => {
            for (key, value) in o {
                fields(value, &format!("{path}/{key}"), rows)
            }
        }
        Value::Array(a) => {
            for (i, value) in a.iter().enumerate() {
                fields(value, &format!("{path}/{i}"), rows)
            }
        }
        _ => rows.push((path.to_owned(), scalar(v))),
    }
}
fn source_name(d: &Value) -> &str {
    match d["source"]["provider"].as_str() {
        Some("pdok") => "PDOK",
        Some("kvk-dutch-business-register") => "KVK Handelsregister",
        _ => d["source"]["name"].as_str().unwrap_or("Bron"),
    }
}
fn excerpt(d: &Value) -> Value {
    let data = &d["data"];
    if data.get("_embedded").is_some() && data.get("kvkNummer").is_some() {
        let main = &data["_embedded"]["hoofdvestiging"];
        return json!({"KVK-nummer":data["kvkNummer"],"Statutaire naam":data["statutaireNaam"],"Handelsnamen":data["handelsnamen"],"Werkzame personen":data["totaalWerkzamePersonen"],"RSIN":data["_embedded"]["eigenaar"]["rsin"],"SBI-activiteiten":data["sbiActiviteiten"],"Vestigingsnummer":main["vestigingsnummer"],"Adressen":main["adressen"]});
    }
    if data["route"] == "company-address" {
        return json!({"Locaties":data["items"].as_array().map(|items|items.iter().map(|i|json!({"Adres":i["address"],"Gemeente":i["municipality"],"Wijk":i["district"],"Buurt":i["neighbourhood"],"BAG-adres":i["bag_address_id"],"BAG-object":i["bag_object_id"],"Coordinaten WGS84":i["centroid_wgs84"],"Gekoppelde percelen":i["parcel_designations"]})).collect::<Vec<_>>()),"Reikwijdte":data["coverage"]["scope"]});
    }
    data.clone()
}
fn label(path: &str) -> String {
    match path {
        "FinancialYear" => "Boekjaar".into(),
        "Assets" => "Totaal activa".into(),
        "Equity" => "Eigen vermogen".into(),
        "DocumentAdoptionDate" => "Vaststellingsdatum".into(),
        _ => path
            .trim_start_matches('/')
            .replace('/', " / ")
            .replace('_', " "),
    }
}
pub fn render(document: &Value) -> anyhow::Result<Vec<u8>> {
    ensure!(ready(&document["analysis"]), "analysis is not available");
    let font = |b: &[u8]| FontData::new(b.to_vec(), None);
    let family = FontFamily {
        regular: font(include_bytes!("../assets/fonts/LiberationSans-Regular.ttf"))?,
        bold: font(include_bytes!("../assets/fonts/LiberationSans-Bold.ttf"))?,
        italic: font(include_bytes!("../assets/fonts/LiberationSans-Italic.ttf"))?,
        bold_italic: font(include_bytes!(
            "../assets/fonts/LiberationSans-BoldItalic.ttf"
        ))?,
    };
    let mut doc = genpdf::Document::new(family);
    doc.set_title("Apiosk - Research report");
    doc.set_font_size(10);
    doc.set_line_spacing(1.15);
    let mut decorator = genpdf::SimplePageDecorator::new();
    decorator.set_margins(18);
    decorator.set_header(|p| {
        Paragraph::new(format!(
            "APIOSK  /  RESEARCH                                            Page {p}"
        ))
        .styled(Style::new().with_font_size(8))
        .padded(genpdf::Margins::trbl(0, 0, 6, 0))
    });
    doc.set_page_decorator(decorator);
    heading(&mut doc, "Research report", 24);
    paragraph(&mut doc, document["question"].as_str().unwrap_or(""));
    paragraph(&mut doc,"Prepared from saved source evidence. Findings and underlying records are presented separately. Missing figures are not zero; this report is not an audit or a title certificate.");
    if document["analysis"]["status"] == "partial" {
        heading(&mut doc, "Evidence gaps remain", 14);
    }
    heading(&mut doc, "Findings and conclusion", 18);
    for observation in document["analysis"]["observations"]
        .as_array()
        .context("missing observations")?
    {
        paragraph(&mut doc, observation["text"].as_str().unwrap_or(""));
        for evidence in observation["evidence"]
            .as_array()
            .context("missing evidence")?
        {
            let source = document["sources"]
                .as_array()
                .context("missing sources")?
                .iter()
                .find(|s| s["result_ref"] == evidence["result_ref"])
                .context("unknown evidence source")?;
            let pointer = evidence["pointer"]
                .as_str()
                .filter(|p| crate::report_assets::evidence_pointer(p))
                .context("invalid evidence pointer")?;
            let value = source["document"]
                .pointer(pointer)
                .context("missing evidence field")?;
            if let Some(quote)=evidence.get("quote").filter(|q|!q.is_null()) {
                let quote=quote.as_str().filter(|q|!q.trim().is_empty() && q.len()<=1024).context("invalid saved quote")?;
                ensure!(value.as_str().is_some_and(|text|text.contains(quote)) && evidence["value"]==quote,"evidence quote changed");
            } else { ensure!(value == &evidence["value"], "evidence changed"); }
            let value=&evidence["value"];
            let d = &source["document"];
            paragraph(
                &mut doc,
                &format!(
                    "Evidence: {} | {} | {}: {}",
                    source_name(d),
                    d["subject"]["label"].as_str().unwrap_or(""),
                    label(evidence["field"].as_str().unwrap_or(pointer)),
                    scalar(value)
                ),
            );
            doc.push(
                Paragraph::new(wrap(&format!(
                    "Retrieved: {}",
                    d["source"]["retrieved_at"]
                        .as_str()
                        .unwrap_or("Not recorded")
                )))
                .styled(Style::new().with_font_size(8)),
            );
        }
        doc.push(Break::new(0.5));
    }
    heading(&mut doc, "Information gaps and follow-up", 16);
    for item in document["analysis"]["limitations"]
        .as_array()
        .context("missing limitations")?
    {
        paragraph(&mut doc, item.as_str().unwrap_or(""));
    }
    paragraph(&mut doc,"Units, currency, reporting year and scope are retained as supplied. Location records identify an address or area; they do not establish ownership or business activity at that site.");
    crate::report_visuals::source_index(&mut doc, document)?;
    crate::report_visuals::sections(&mut doc, document)?;
    doc.push(genpdf::elements::PageBreak::new());
    heading(&mut doc, "Source records", 20);
    paragraph(&mut doc,"Selected source fields are listed below. Complete saved responses and captured files are included in the evidence bundle.");
    for (index, source) in document["sources"].as_array().context("missing sources")?.iter().enumerate() {
        if index > 0 { doc.push(genpdf::elements::PageBreak::new()); }
        let d = &source["document"];
        heading(
            &mut doc,
            &format!(
                "Source: {} - {}",
                source_name(d),
                d["subject"]["label"].as_str().unwrap_or("")
            ),
            16,
        );
        for key in ["url", "retrieved_at"] {
            if let Some(s) = d["source"][key].as_str() {
                paragraph(&mut doc, &format!("{key}: {s}"));
            }
        }
        paragraph(
            &mut doc,
            &format!("Result reference: {}", scalar(&source["result_ref"])),
        );
        if d["coverage"] == "partial" {
            paragraph(
                &mut doc,
                &format!(
                    "Partial source response: {} {}",
                    d["missing_fields"], d["coverage_note"]
                ),
            );
        }
        let mut rows = Vec::new();
        fields(&excerpt(d), "", &mut rows);
        for (key, value) in &rows {
            paragraph(&mut doc, &format!("{}: {value}", label(key)));
        }
        if rows.len() == 500 {
            paragraph(&mut doc,"This appendix shows at most five hundred fields per source. Complete saved responses are available in the evidence bundle.");
        }
    }
    let mut bytes = Vec::new();
    doc.render(&mut bytes)?;
    Ok(bytes)
}

/// The same signed immutable snapshot as the PDF. No network calls or purchases.
pub fn evidence_bundle(document: &Value) -> anyhow::Result<Vec<u8>> {
    use std::io::{Cursor, Write};
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("report.pdf", options)?;
    zip.write_all(&render(document)?)?;
    let mut manifest = document.clone();
    let sources = document["sources"].as_array().context("missing sources")?;
    let mut total = 0;
    for (i, source) in sources.iter().enumerate() {
        let d = &source["document"];
        let public = crate::report_assets::public_document(d);
        manifest["sources"][i]["document"] = public.clone();
        zip.start_file(format!("sources/S{:02}.json", i + 1), options)?;
        zip.write_all(&serde_json::to_vec_pretty(&public)?)?;
        if let Some(assets) = d[crate::report_assets::KEY]["assets"].as_array() {
            for (n, a) in assets.iter().enumerate() {
                let bytes = crate::report_assets::bytes(a)?;
                total += bytes.len();
                ensure!(
                    total <= 64 * 1024 * 1024,
                    "evidence bundle exceeds capacity"
                );
                let ext = match a["mime_type"].as_str() {
                    Some("image/png") => "png",
                    Some("application/pdf") => "pdf",
                    _ => anyhow::bail!("unsupported saved artifact"),
                };
                let name = format!("evidence/S{:02}-{:02}.{ext}", i + 1, n + 1);
                manifest["sources"][i]["document"][crate::report_assets::KEY]["assets"][n]
                    ["bundle_path"] = json!(name);
                zip.start_file(name, options)?;
                zip.write_all(&bytes)?;
            }
        }
    }
    zip.start_file("manifest.json", options)?;
    zip.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
    zip.start_file("README.txt", options)?;
    zip.write_all(b"Apiosk evidence bundle\n\nreport.pdf: source-backed research report.\nsources/: saved source results, not newly purchased data.\nevidence/: captured official maps and available original PDFs.\nmanifest.json: source URLs, retrieval dates, evidence pointers and SHA-256 file digests.\n\nMap outlines do not establish ownership or mortgage status. Missing soil records do not establish clean soil. Report limitations remain applicable. No new source calls occur on download.\n")?;
    Ok(zip.finish()?.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoted_source_and_pdf_excerpt_render_only_when_verbatim() {
        for pointer in ["/data/text","/report_assets/assets/0/extracted_text"] {
            let mut source=json!({"result_ref":"saved","document":{"data":{"text":"The company reported EUR 1200 in total assets."},"source":{"provider":"test"},"report_assets":{"assets":[{"extracted_text":"The company reported EUR 1200 in total assets."}]}}});
            if pointer.starts_with("/data/") { source["document"]["report_assets"]=json!({"assets":[]}); }
            let input=json!({"observations":[{"text":"Reported assets: {{a}}.","evidence":[{"result_ref":"saved","pointer":pointer,"quote":"EUR 1200"}]}],"limitations":[]});
            let sources=vec![source];
            let analysis=crate::parser::validate_analysis(&input.to_string(),&sources,false).unwrap();
            let mut document=json!({"question":"Test excerpt report","analysis":analysis,"sources":sources});
            assert!(render(&document).unwrap().starts_with(b"%PDF-"));
            document["analysis"]["observations"][0]["evidence"][0]["value"]=json!("EUR 9999");
            assert!(render(&document).is_err());
        }
    }
}
