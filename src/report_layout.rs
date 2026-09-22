//! Human-readable presentation of saved evidence. No enrichment or paid calls.
use anyhow::{ensure, Context};
use genpdf::{
    elements::{Break, Image, LinearLayout, PageBreak, Paragraph, TableLayout},
    style::{Color, Style},
    Element,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const INK: Color = Color::Rgb(29, 35, 48);
const PURPLE: Color = Color::Rgb(96, 70, 218);
const MUTED: Color = Color::Rgb(97, 105, 119);

pub fn clean(s: &str) -> String {
    s.replace(['\u{2011}', '\u{2013}', '\u{2014}'], "-")
        .replace('\u{00a0}', " ")
        .lines()
        .map(crate::research_report::wrap)
        .collect::<Vec<_>>()
        .join("\n")
}
pub fn text(doc: &mut genpdf::Document, s: &str) {
    doc.push(Paragraph::new(clean(s)));
}
pub fn small(doc: &mut genpdf::Document, s: &str) {
    doc.push(Paragraph::new(clean(s)).styled(Style::new().with_font_size(8).with_color(MUTED)));
}
pub fn heading(doc: &mut genpdf::Document, s: &str, size: u8) {
    doc.push(
        Paragraph::new(clean(s)).styled(Style::new().bold().with_font_size(size).with_color(INK)),
    );
    doc.push(Break::new(0.6));
}
fn section(doc: &mut genpdf::Document, label: &str, title: &str) {
    doc.push(PageBreak::new());
    small(doc, label);
    doc.push(Break::new(0.5));
    heading(doc, title, 23);
}
pub fn date(s: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|date| {
            date.with_timezone(&chrono::Utc)
                .format("%d %b %Y, %H:%M UTC")
                .to_string()
        })
        .unwrap_or_else(|_| s.into())
}
pub fn source_name(d: &Value) -> String {
    match d["source"]["provider"].as_str().unwrap_or("") {
        "kvk-dutch-business-register" => "KVK Business Register".into(),
        "pdok" => "PDOK / BAG".into(),
        "pdok-ruimtelijke-plannen" => "PDOK Spatial Planning".into(),
        "opensanctions" => "OpenSanctions".into(),
        "exa" => "Exa".into(),
        "sirenic" => "Sirenic".into(),
        _ => d["source"]["name"]
            .as_str()
            .unwrap_or("Source")
            .replace('-', " "),
    }
}
fn brand(provider: &str) -> Option<&'static [u8]> {
    Some(match provider {
        "apiosk" => include_bytes!("../assets/report-brands/apiosk.png"),
        "kvk-dutch-business-register" => include_bytes!("../assets/report-brands/kvk.png"),
        "pdok" | "pdok-ruimtelijke-plannen" => include_bytes!("../assets/report-brands/pdok.png"),
        "opensanctions" => include_bytes!("../assets/report-brands/opensanctions.png"),
        "exa" => include_bytes!("../assets/report-brands/exa.png"),
        "sirenic" => include_bytes!("../assets/report-brands/sirenic.png"),
        _ => return None,
    })
}
fn logo(provider: &str, width: f64, height: f64) -> anyhow::Result<Option<Image>> {
    let Some(bytes) = brand(provider) else {
        return Ok(None);
    };
    let limit = if provider == "apiosk" { 320 } else { 96 };
    let rgba = image::load_from_memory(bytes)?
        .thumbnail(limit, limit)
        .to_rgba8();
    let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
    for (x, y, p) in rgba.enumerate_pixels() {
        let a = u32::from(p[3]);
        rgb.put_pixel(
            x,
            y,
            image::Rgb([0, 1, 2].map(|c| ((u32::from(p[c]) * a + 255 * (255 - a)) / 255) as u8)),
        );
    }
    let dpi = (rgba.width() as f64 * 25.4 / width).max(rgba.height() as f64 * 25.4 / height);
    Ok(Some(
        Image::from_dynamic_image(image::DynamicImage::ImageRgb8(rgb))?.with_dpi(dpi),
    ))
}
pub fn source_badge(doc: &mut genpdf::Document, d: &Value, reference: &str) -> anyhow::Result<()> {
    let mut row = TableLayout::new(vec![1, 22]);
    let mut cells = row.row();
    if let Some(image) = logo(d["source"]["provider"].as_str().unwrap_or(""), 6., 6.)? {
        cells = cells.element(image);
    } else {
        cells = cells.element(Paragraph::new(""));
    }
    cells
        .element(
            Paragraph::new(format!("{}  |  {}", source_name(d), reference))
                .styled(Style::new().with_font_size(8).with_color(MUTED))
                .padded(genpdf::Margins::trbl(1, 0, 2, 2)),
        )
        .push()?;
    doc.push(row);
    Ok(())
}

struct PageBrand {
    page: usize,
    mark: Image,
}
impl genpdf::PageDecorator for PageBrand {
    fn decorate_page<'a>(
        &mut self,
        ctx: &genpdf::Context,
        mut area: genpdf::render::Area<'a>,
        style: Style,
    ) -> Result<genpdf::render::Area<'a>, genpdf::error::Error> {
        self.page += 1;
        let caption = Style::new().with_font_size(8).with_color(MUTED);
        area.print_str(
            &ctx.font_cache,
            genpdf::Position::new(18, 13),
            caption,
            "RESEARCH  /  SOURCE REVIEW",
        )?;
        let mut corner = area.clone();
        corner.add_offset(genpdf::Position::new(166, 10));
        self.mark.clone().render(ctx, corner, style)?;
        area.draw_line(
            vec![
                genpdf::Position::new(18, 24),
                genpdf::Position::new(192, 24),
            ],
            Style::new().with_color(Color::Rgb(226, 228, 236)),
        );
        area.print_str(
            &ctx.font_cache,
            genpdf::Position::new(18, 283),
            caption,
            "Apiosk evidence report  |  Saved sources and supporting records",
        )?;
        area.print_str(
            &ctx.font_cache,
            genpdf::Position::new(184, 283),
            caption,
            self.page.to_string(),
        )?;
        area.add_margins(genpdf::Margins::trbl(32, 18, 23, 18));
        Ok(area)
    }
}
pub fn decorate(doc: &mut genpdf::Document) -> anyhow::Result<()> {
    doc.set_page_decorator(PageBrand {
        page: 0,
        mark: logo("apiosk", 26., 8.)?.context("missing Apiosk wordmark")?,
    });
    Ok(())
}
struct RowRules;
impl genpdf::elements::CellDecorator for RowRules {
    fn decorate_cell(
        &mut self,
        _column: usize,
        _row: usize,
        _has_more: bool,
        area: genpdf::render::Area<'_>,
        _style: Style,
    ) {
        let size = area.size();
        area.draw_line(
            vec![
                genpdf::Position::new(0, size.height),
                genpdf::Position::new(size.width, size.height),
            ],
            Style::new().with_color(Color::Rgb(226, 228, 236)),
        );
    }
}
pub fn table(doc: &mut genpdf::Document, rows: &[(String, String)]) -> anyhow::Result<()> {
    let mut table = TableLayout::new(vec![3, 7]);
    table.set_cell_decorator(RowRules);
    for (key, value) in rows {
        let mut lines = LinearLayout::vertical();
        for line in value.lines() {
            lines.push(Paragraph::new(clean(line)));
        }
        table
            .row()
            .element(
                Paragraph::new(clean(key))
                    .styled(Style::new().bold().with_font_size(9).with_color(MUTED))
                    .padded(3),
            )
            .element(lines.padded(3))
            .push()?;
    }
    doc.push(table);
    doc.push(Break::new(0.5));
    Ok(())
}
fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(if *b { "Yes" } else { "No" }.into()),
        Value::Array(items) => {
            let parts: Vec<_> = items.iter().filter_map(scalar).collect();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        _ => None,
    }
}
fn row(rows: &mut Vec<(String, String)>, label: &str, value: &Value) {
    if let Some(value) = scalar(value) {
        rows.push((label.into(), value));
    }
}
fn sources(document: &Value) -> anyhow::Result<&Vec<Value>> {
    document["sources"].as_array().context("missing sources")
}
fn cited(document: &Value) -> BTreeSet<String> {
    document["analysis"]["observations"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|o| o["evidence"].as_array().into_iter().flatten())
        .filter_map(|e| e["result_ref"].as_str().map(str::to_owned))
        .collect()
}
pub fn company_name(document: &Value) -> String {
    let names = document["sources"].as_array().into_iter().flatten().rev();
    let mut registered = BTreeSet::new();
    for source in names {
        let d = &source["document"];
        if d["source"]["provider"] == "kvk-dutch-business-register"
            && d["data"]["kvkNummer"].is_string()
        {
            if let Some(name) = d["data"]["statutaireNaam"]
                .as_str()
                .or(d["data"]["naam"].as_str())
            {
                registered.insert(name);
            }
        }
    }
    if registered.len() == 1 {
        return registered.into_iter().next().unwrap().into();
    }
    if registered.len() > 1 {
        return "Company comparison".into();
    }
    let names: BTreeSet<_> = document["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s["document"]["subject"]["label"].as_str())
        .filter(|name| !name.starts_with("Kandidaat ") && !name.starts_with("Result "))
        .collect();
    if names.len() == 1 {
        names.into_iter().next().unwrap().into()
    } else {
        "Research report".into()
    }
}
pub fn overview(doc: &mut genpdf::Document, document: &Value) -> anyhow::Result<()> {
    let financing = document["question"].as_str().is_some_and(|q| {
        let q = q.to_lowercase();
        q.contains("financing") || q.contains("financiering")
    });
    small(
        doc,
        if financing {
            "FINANCING DESK REVIEW"
        } else {
            "EVIDENCE-BASED RESEARCH"
        },
    );
    doc.push(Break::new(0.6));
    heading(doc, &company_name(document), 28);
    let collected = sources(document)?
        .iter()
        .filter_map(|s| s["document"]["source"]["retrieved_at"].as_str())
        .max()
        .map(date)
        .unwrap_or_else(|| "Date not recorded".into());
    small(doc, &format!("Evidence retrieved through {collected}"));
    doc.push(Break::new(0.8));
    doc.push(
        Paragraph::new(if document["analysis"]["status"] == "partial" {
            "FURTHER EVIDENCE REQUIRED"
        } else {
            "SOURCE REVIEW COMPLETED"
        })
        .styled(Style::new().bold().with_font_size(10).with_color(PURPLE)),
    );
    doc.push(Break::new(0.7));
    text(
        doc,
        if financing {
            "This desk review brings together the available company and location evidence. It supports further assessment; it is not a financing approval or a property title certificate."
        } else {
            "This report brings together the available source evidence, with references and supporting records. Findings describe the scope of the retrieved data; open questions are listed separately."
        },
    );
    doc.push(Break::new(0.8));
    heading(doc, "Key findings", 17);
    for (number, observation) in document["analysis"]["observations"]
        .as_array()
        .context("missing observations")?
        .iter()
        .enumerate()
    {
        text(
            doc,
            &format!(
                "{}. {}",
                number + 1,
                observation["text"].as_str().unwrap_or("")
            ),
        );
        let mut refs = BTreeSet::new();
        for evidence in observation["evidence"]
            .as_array()
            .context("missing evidence")?
        {
            let (index, source) = sources(document)?
                .iter()
                .enumerate()
                .find(|(_, s)| s["result_ref"] == evidence["result_ref"])
                .context("unknown evidence source")?;
            let pointer = evidence["pointer"]
                .as_str()
                .filter(|p| crate::report_assets::evidence_pointer(p))
                .context("invalid evidence pointer")?;
            let value = source["document"]
                .pointer(pointer)
                .context("missing evidence field")?;
            if let Some(quote) = evidence.get("quote").filter(|q| !q.is_null()) {
                let quote = quote
                    .as_str()
                    .filter(|q| !q.trim().is_empty() && q.len() <= 1024)
                    .context("invalid saved quote")?;
                ensure!(
                    value.as_str().is_some_and(|text| text.contains(quote))
                        && evidence["value"] == quote,
                    "evidence quote changed"
                );
            } else {
                ensure!(value == &evidence["value"], "evidence changed");
            }
            if refs.insert(index) {
                source_badge(doc, &source["document"], &format!("S{}", index + 1))?;
            }
        }
        doc.push(Break::new(0.5));
    }
    if document["analysis"]["observations"]
        .as_array()
        .is_some_and(Vec::is_empty)
    {
        text(
            doc,
            "No source-supported findings are available in this snapshot.",
        );
    }
    Ok(())
}
pub fn gaps(doc: &mut genpdf::Document, document: &Value) -> anyhow::Result<()> {
    let gaps = document["analysis"]["limitations"]
        .as_array()
        .context("missing limitations")?;
    if gaps.is_empty() {
        return Ok(());
    }
    section(doc, "FOLLOW-UP", "Information still needed");
    text(doc,"These gaps remain open. An unavailable record is not a failed check, and a location match does not establish ownership.");
    doc.push(Break::new(0.8));
    table(
        doc,
        &gaps
            .iter()
            .enumerate()
            .map(|(i, gap)| {
                (
                    format!("Open item {:02}", i + 1),
                    gap.as_str().unwrap_or("").into(),
                )
            })
            .collect::<Vec<_>>(),
    )?;
    small(doc,"Financial charts appear only when the saved figures have an explicit reporting period, consistent currency and comparable units. Missing figures are never replaced with estimates.");
    Ok(())
}
pub fn details(doc: &mut genpdf::Document, document: &Value) -> anyhow::Result<()> {
    let mut seen = BTreeSet::new();
    let cited = cited(document);
    let mut ordered: Vec<_> = sources(document)?.iter().enumerate().rev().collect();
    ordered.sort_by_key(|(_, s)| s["document"]["data"]["route"] == "company-address");
    for (index, source) in ordered {
        let d = &source["document"];
        let data = &d["data"];
        if d["source"]["provider"] == "kvk-dutch-business-register"
            && data["_embedded"]["hoofdvestiging"].is_object()
        {
            let id = data["kvkNummer"].as_str().unwrap_or("");
            // Do not turn an unrelated result into the subject of this report.
            if !cited.contains(source["result_ref"].as_str().unwrap_or("")) {
                continue;
            }
            if !seen.insert(format!("kvk:{id}")) {
                continue;
            }
            section(doc, "LEGAL ENTITY", "Company registration");
            source_badge(
                doc,
                d,
                &format!(
                    "S{} | {}",
                    index + 1,
                    date(d["source"]["retrieved_at"].as_str().unwrap_or(""))
                ),
            )?;
            let mut rows = Vec::new();
            row(&mut rows, "Registered name", &data["statutaireNaam"]);
            row(&mut rows, "KVK registration number", &data["kvkNummer"]);
            row(
                &mut rows,
                "Legal form",
                &data["_embedded"]["eigenaar"]["rechtsvorm"],
            );
            row(
                &mut rows,
                "RSIN identifier",
                &data["_embedded"]["eigenaar"]["rsin"],
            );
            row(
                &mut rows,
                "Establishment number",
                &data["_embedded"]["hoofdvestiging"]["vestigingsnummer"],
            );
            if let Some(addresses) = data["_embedded"]["hoofdvestiging"]["adressen"].as_array() {
                for address in addresses {
                    row(
                        &mut rows,
                        "Registered visiting address",
                        &address["volledigAdres"],
                    );
                }
            }
            if let Some(activities) = data["sbiActiviteiten"].as_array() {
                for activity in activities {
                    if let Some(name) = activity["sbiOmschrijving"].as_str() {
                        rows.push((
                            "Registered activity".into(),
                            format!("{} - {}", activity["sbiCode"].as_str().unwrap_or(""), name),
                        ));
                    }
                }
            }
            row(
                &mut rows,
                "Reported persons employed",
                &data["totaalWerkzamePersonen"],
            );
            table(doc, &rows)?;
            small(doc,"Registry scope: this legal entity. Registry employment figures do not describe the entire corporate group. The legal form is not a shareholder or beneficial-owner register.");
        }
        if data["route"] == "company-address" && data["coverage"]["complete"] == true {
            for item in data["items"].as_array().into_iter().flatten() {
                let identity = item["bag_address_id"].as_str().unwrap_or("");
                if !seen.insert(format!("address:{identity}")) {
                    continue;
                }
                section(doc, "PROPERTY CONTEXT", "Registered address & parcels");
                source_badge(
                    doc,
                    d,
                    &format!(
                        "S{} | {}",
                        index + 1,
                        date(d["source"]["retrieved_at"].as_str().unwrap_or(""))
                    ),
                )?;
                let mut rows = Vec::new();
                for (label, key) in [
                    ("Address", "address"),
                    ("Municipality", "municipality"),
                    ("District", "district"),
                    ("Neighbourhood", "neighbourhood"),
                    ("BAG address identifier", "bag_address_id"),
                    ("BAG object identifier", "bag_object_id"),
                    ("Linked cadastral parcels", "parcel_designations"),
                ] {
                    row(&mut rows, label, &item[key]);
                }
                if let Some(point) = item["centroid_wgs84"]
                    .as_str()
                    .and_then(|s| s.strip_prefix("POINT("))
                    .and_then(|s| s.strip_suffix(')'))
                {
                    let xy: Vec<_> = point.split_whitespace().collect();
                    if xy.len() == 2 {
                        rows.push((
                            "Coordinates (WGS84)".into(),
                            format!("Latitude {}\nLongitude {}", xy[1], xy[0]),
                        ));
                    }
                }
                table(doc, &rows)?;
                heading(doc, "What this establishes", 14);
                text(doc,"The registry address has been located in BAG and linked to the parcel references above. This is the company's registered location, not confirmation of a proposed collateral property.");
                doc.push(Break::new(0.6));
                small(doc,"A linked parcel does not establish legal ownership, mortgages, current planning permission or soil condition. The maps on the following pages show geographic context; they are not interior floor plans.");
            }
        }
    }
    Ok(())
}
pub fn register(doc: &mut genpdf::Document, document: &Value) -> anyhow::Result<()> {
    section(doc, "AUDIT TRAIL", "Sources & supporting evidence");
    text(doc,"One entry per provider. References connect the findings to the original saved responses in the evidence bundle.");
    doc.push(Break::new(0.7));
    let cited = cited(document);
    let mut providers: BTreeMap<String, Vec<(usize, &Value)>> = BTreeMap::new();
    for (index, source) in sources(document)?.iter().enumerate() {
        providers
            .entry(source_name(&source["document"]))
            .or_default()
            .push((index, source));
    }
    for records in providers.values() {
        let refs = records
            .iter()
            .map(|(i, _)| format!("S{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let d = &records[0].1["document"];
        source_badge(doc, d, &refs)?;
        let used = records
            .iter()
            .any(|(_, s)| cited.contains(s["result_ref"].as_str().unwrap_or("")));
        let assets = records
            .iter()
            .flat_map(|(_, s)| {
                s["document"][crate::report_assets::KEY]["assets"]
                    .as_array()
                    .into_iter()
                    .flatten()
            })
            .collect::<Vec<_>>();
        let status = if used {
            "Cited in the findings"
        } else if !assets.is_empty() {
            "Supporting documents or maps"
        } else {
            "Retrieved, not cited in the findings"
        };
        small(doc, status);
        if let Some(url) = d["source"]["url"].as_str() {
            small(doc, url);
        }
        for asset in assets {
            small(
                doc,
                &format!(
                    "Saved attachment: {}",
                    asset["name"]
                        .as_str()
                        .or(asset["source_url"].as_str())
                        .unwrap_or("Source document")
                ),
            );
        }
        doc.push(Break::new(0.8));
    }
    heading(doc, "Evidence bundle", 14);
    text(doc,"The accompanying ZIP contains the original source responses, map images, available source PDFs and a manifest with retrieval times, links and file hashes. Unused search hits remain in that bundle for auditability; they are not treated as findings about the company.");
    small(doc,"Provider logos identify the origin of information. They do not imply endorsement or certification of this report.");
    Ok(())
}
