//! PDF projection of one saved research turn. No model or provider calls on download.
use crate::{
    engine::Engine,
    state::{digest, Task},
};
use anyhow::{ensure, Context};
use chrono::Utc;
use genpdf::fonts::{FontData, FontFamily};
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
    doc.set_title(format!(
        "Apiosk - {} - Research report",
        crate::report_layout::company_name(document)
    ));
    doc.set_font_size(10);
    doc.set_line_spacing(1.15);
    crate::report_layout::decorate(&mut doc)?;
    crate::report_layout::overview(&mut doc, document)?;
    crate::report_layout::findings(&mut doc, document)?;
    crate::report_layout::details(&mut doc, document)?;
    crate::report_visuals::sections(&mut doc, document)?;
    crate::report_layout::gaps(&mut doc, document)?;
    crate::report_layout::register(&mut doc, document)?;
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
        for pointer in ["/data/text", "/report_assets/assets/0/extracted_text"] {
            let mut source = json!({"result_ref":"saved","document":{"data":{"text":"The company reported EUR 1200 in total assets."},"source":{"provider":"test"},"report_assets":{"assets":[{"extracted_text":"The company reported EUR 1200 in total assets."}]}}});
            if pointer.starts_with("/data/") {
                source["document"]["report_assets"] = json!({"assets":[]});
            }
            let input = json!({"observations":[{"text":"Reported assets: {{a}}.","evidence":[{"result_ref":"saved","pointer":pointer,"quote":"EUR 1200"}]}],"limitations":[]});
            let sources = vec![source];
            let analysis =
                crate::parser::validate_analysis(&input.to_string(), &sources, false).unwrap();
            let mut document =
                json!({"question":"Test excerpt report","analysis":analysis,"sources":sources});
            assert!(render(&document).unwrap().starts_with(b"%PDF-"));
            document["analysis"]["observations"][0]["evidence"][0]["value"] = json!("EUR 9999");
            assert!(render(&document).is_err());
        }
    }
}
