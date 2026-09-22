//! Deterministic visual sections. No invented numeric series or inferred title rights.
use crate::report_assets;
use anyhow::Context;
use genpdf::{
    elements::{Break, Image, PageBreak, Paragraph},
    style::{Color, Style},
    Element,
};
use serde_json::Value;
use std::{collections::BTreeMap, io::Cursor};

fn text(doc: &mut genpdf::Document, s: impl AsRef<str>) {
    doc.push(Paragraph::new(super::research_report::wrap(s.as_ref())));
}
fn title(doc: &mut genpdf::Document, s: &str) {
    doc.push(Break::new(0.5));
    doc.push(
        Paragraph::new(s).styled(
            Style::new()
                .bold()
                .with_font_size(18)
                .with_color(Color::Rgb(29, 35, 48)),
        ),
    );
    doc.push(Break::new(0.5));
}
fn small(doc: &mut genpdf::Document, s: impl AsRef<str>) {
    doc.push(
        Paragraph::new(super::research_report::wrap(s.as_ref())).styled(
            Style::new()
                .with_font_size(8)
                .with_color(Color::Rgb(75, 85, 95)),
        ),
    );
}
fn strv(v: &Value) -> String {
    v.as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| v.to_string())
}

// Only unambiguous top-level filing facts with an explicit currency and year
// are charted. Nested contexts and duplicate values must not be collapsed.
#[derive(Debug)]
pub struct Chart {
    pub year: String,
    pub currency: String,
    pub values: Vec<(String, f64, String)>,
}
pub fn financial_chart(data: &Value) -> Option<Chart> {
    let fields = data["opendataFields"].as_array()?;
    let mut by_key: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for f in fields {
        if let Some(k) = f["key"].as_str() {
            if !f["value"].is_null() {
                by_key.entry(k).or_default().push(&f["value"]);
            }
        }
    }
    let unique = |key: &str| -> Option<String> {
        let v = by_key.get(key)?;
        (v.len() == 1).then(|| strv(v[0]))
    };
    let year = unique("FinancialYear")?;
    if year.len() != 4 || !year.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut currencies = Vec::new();
    for key in ["Currency", "PresentationCurrency", "CurrencyISOCode"] {
        if by_key.contains_key(key) {
            currencies.push(unique(key)?);
        }
    }
    let currency = currencies.first()?.clone();
    if currencies.iter().any(|c| c != &currency) {
        return None;
    }
    if currency.len() != 3 || !currency.bytes().all(|c| c.is_ascii_uppercase()) {
        return None;
    }
    // Units/scales are not guessed. Accept literal currency-unit amounts only.
    for key in ["Scale", "Unit", "MonetaryUnit", "AmountsIn"] {
        if by_key.contains_key(key) {
            let v = unique(key)?;
            if !["1", currency.as_str(), "units"].contains(&v.as_str()) {
                return None;
            }
        }
    }
    let mut values = Vec::new();
    for (key, label) in [
        ("Assets", "Total assets"),
        ("Equity", "Equity"),
        ("Liabilities", "Liabilities"),
    ] {
        if let Some(raw) = unique(key) {
            // Numeric strings must be machine-formatted, never locale guessed.
            if raw
                .bytes()
                .any(|b| !b.is_ascii_digit() && !b".-".contains(&b))
            {
                continue;
            }
            if let Ok(n) = raw.parse::<f64>() {
                if n.is_finite() {
                    values.push((label.into(), n, raw));
                }
            }
        }
    }
    (values.len() >= 2 && values.iter().any(|(_, v, _)| *v != 0.0)).then_some(Chart {
        year,
        currency,
        values,
    })
}
struct Bars {
    values: Vec<(String, f64, String)>,
    done: bool,
}
impl Element for Bars {
    fn render(
        &mut self,
        ctx: &genpdf::Context,
        area: genpdf::render::Area<'_>,
        style: Style,
    ) -> Result<genpdf::RenderResult, genpdf::error::Error> {
        let mut r = genpdf::RenderResult::default();
        if self.done {
            return Ok(r);
        }
        let height = self.values.len() as f64 * 18.0 + 8.0;
        if f64::from(area.size().height) < height {
            r.has_more = true;
            return Ok(r);
        }
        let max = self
            .values
            .iter()
            .map(|(_, v, _)| v.abs())
            .fold(0.0_f64, f64::max);
        let has_negative = self.values.iter().any(|(_, v, _)| *v < 0.0);
        let width = (f64::from(area.size().width) - 2.0).min(160.0);
        let zero = if has_negative { width / 2.0 } else { 0.0 };
        let scale = if has_negative { width / 2.0 } else { width };
        for (i, (label, v, raw)) in self.values.iter().enumerate() {
            let y = i as f64 * 18.0;
            area.print_str(
                &ctx.font_cache,
                genpdf::Position::new(0, y),
                style.with_font_size(10),
                format!("{label}  {raw}"),
            )?;
            let end = zero + (v / max) * scale;
            let color = if *v < 0.0 {
                Color::Rgb(174, 63, 62)
            } else {
                Color::Rgb(96, 70, 218)
            };
            for line in 0..12 {
                let yy = y + 8.0 + line as f64 * 0.3;
                area.draw_line(
                    vec![
                        genpdf::Position::new(zero, yy),
                        genpdf::Position::new(end, yy),
                    ],
                    Style::new().with_color(color),
                );
            }
            area.draw_line(
                vec![
                    genpdf::Position::new(zero, y + 7.0),
                    genpdf::Position::new(zero, y + 13.0),
                ],
                Style::new().with_color(Color::Rgb(70, 70, 70)),
            );
        }
        self.done = true;
        r.size.height = genpdf::Mm::from(height);
        r.size.width = area.size().width;
        Ok(r)
    }
}

pub fn sections(doc: &mut genpdf::Document, document: &Value) -> anyhow::Result<()> {
    let sources = document["sources"].as_array().context("missing sources")?;
    for (i, s) in sources.iter().enumerate() {
        let d = &s["document"];
        if let Some(chart) = financial_chart(&d["data"]) {
            doc.push(PageBreak::new());
            title(doc, "Financial position");
            crate::report_layout::source_badge(doc, d, &format!("S{}", i + 1))?;
            text(
                doc,
                format!(
                    "{} | {} | {} | S{}",
                    d["subject"]["label"].as_str().unwrap_or("Entity"),
                    chart.year,
                    chart.currency,
                    i + 1
                ),
            );
            small(doc,"Reported balance sheet amounts. Common zero baseline; bars are not a risk score. Values are not added together.");
            doc.push(Break::new(0.5));
            doc.push(Bars {
                values: chart.values,
                done: false,
            });
            small(
                doc,
                format!(
                    "Source S{}. Currency, year and values are taken from the same filing.",
                    i + 1
                ),
            );
        } else if crate::annual_report::eligible(d) {
            text(doc,format!("S{}: financial chart unavailable: the saved filing does not provide unambiguous comparable amounts, currency and period. The original fields are retained in the evidence bundle.",i+1));
        }
        if let Some(assets) = d[report_assets::KEY]["assets"].as_array() {
            for a in assets {
                if a["kind"] != "map" {
                    continue;
                }
                let bytes = report_assets::bytes(a)?;
                let reader = image::io::Reader::new(Cursor::new(&bytes)).with_guessed_format()?;
                let dims = reader.into_dimensions()?;
                anyhow::ensure!(
                    matches!(dims, (1000, 625) | (1000, 1000)),
                    "invalid saved map dimensions"
                );
                let rgb = image::load_from_memory(&bytes)?.to_rgb8();
                doc.push(PageBreak::new());
                title(doc, a["name"].as_str().unwrap_or("Map"));
                crate::report_layout::source_badge(doc, d, &format!("S{}", i + 1))?;
                text(doc, a["address"].as_str().unwrap_or(""));
                small(
                    doc,
                    format!(
                        "S{} | {} | {}",
                        i + 1,
                        a["attribution"].as_str().unwrap_or(""),
                        crate::report_layout::date(a["retrieved_at"].as_str().unwrap_or(""))
                    ),
                );
                doc.push(Break::new(0.8));
                doc.push(MarkedMap(
                    Image::from_dynamic_image(image::DynamicImage::ImageRgb8(rgb))?
                        .with_dpi(if dims.1 == 1000 { 175.0 } else { 150.0 })
                        .with_alignment(genpdf::Alignment::Center),
                    a["center"].is_array(),
                ));
                doc.push(Break::new(0.5));
                text(doc, a["scope"].as_str().unwrap_or(""));
                small(doc,"The original map image, retrieval details and source link are included in the evidence bundle.");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn charts_never_guess_currency_scale_or_duplicate_contexts() {
        let mut d = json!({"opendataFields":[{"key":"FinancialYear","value":"2025"},{"key":"Currency","value":"EUR"},{"key":"Assets","value":"1200"},{"key":"Equity","value":"-50"}]});
        let c = financial_chart(&d).unwrap();
        assert_eq!(c.values[1].1, -50.0);
        d["opendataFields"]
            .as_array_mut()
            .unwrap()
            .push(json!({"key":"Currency","value":"USD"}));
        assert!(financial_chart(&d).is_none());
        d["opendataFields"].as_array_mut().unwrap().pop();
        d["opendataFields"][1]["value"] = Value::Null;
        assert!(financial_chart(&d).is_none());
        d["opendataFields"][1]["value"] = json!("EUR");
        d["opendataFields"]
            .as_array_mut()
            .unwrap()
            .push(json!({"key":"Scale","value":"1000"}));
        assert!(financial_chart(&d).is_none());
    }
}

// Annotation is a report overlay; the original evidence PNG remains unchanged.
struct MarkedMap(Image, bool);
impl Element for MarkedMap {
    fn render(
        &mut self,
        ctx: &genpdf::Context,
        area: genpdf::render::Area<'_>,
        style: Style,
    ) -> Result<genpdf::RenderResult, genpdf::error::Error> {
        let r = self.0.render(ctx, area.clone(), style)?;
        if self.1 && !r.has_more && f64::from(r.size.height) > 0.0 {
            let x = f64::from(area.size().width) / 2.0;
            let y = f64::from(r.size.height) / 2.0;
            for offset in [-0.2, 0.0, 0.2] {
                let red = Style::new().with_color(Color::Rgb(195, 35, 35));
                area.draw_line(
                    vec![
                        genpdf::Position::new(x - 3.0, y + offset),
                        genpdf::Position::new(x + 3.0, y + offset),
                    ],
                    red,
                );
                area.draw_line(
                    vec![
                        genpdf::Position::new(x + offset, y - 3.0),
                        genpdf::Position::new(x + offset, y + 3.0),
                    ],
                    red,
                );
            }
        }
        Ok(r)
    }
}
