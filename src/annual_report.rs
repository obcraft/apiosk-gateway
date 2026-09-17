//! Deterministic PDF projection of saved KVK data. No provider calls or inferred figures.
use genpdf::{elements::{Break, Paragraph, TableLayout, LinearLayout}, fonts::{FontData, FontFamily}, style::{Color, Style}, Element};
use serde_json::Value;

pub const ENDPOINT: &str = "/api/v1/hvds/jaarrekeningen/kvknummer";
const SECTIONS: [&str; 4] = ["Algemene gegevens", "Balans", "Kasstroomoverzicht", "Winst- en verliesrekening"];

pub fn eligible(document: &Value) -> bool {
    document["source"]["provider"] == "kvk-dutch-business-register"
        && (document["source"]["path"] == ENDPOINT
            || document["source"]["endpoint_id"] == "a4d6eb45-a960-4f50-9298-a91b8063e374")
}

#[derive(Debug)]
struct Row { section: usize, context: String, key: String, value: String }
fn section(key: &str, inherited: usize) -> usize {
    let key = key.to_ascii_lowercase();
    if key.contains("balancesheet") { 1 }
    else if key.contains("cashflow") { 2 }
    else if key.contains("incomestatement") || key.contains("profitandloss") || key.contains("profitloss") { 3 }
    else { inherited }
}
fn collect(value: &Value, context: &str, group: usize, rows: &mut Vec<Row>) {
    match value {
        Value::Object(map) => {
            let key = map.get("key").and_then(Value::as_str);
            let group = key.map_or(group, |k| section(k, group));
            if let Some(k) = key {
                if let Some(v) = map.get("value") {
                    rows.push(Row { section: group, context: context.into(), key: k.into(), value: scalar(v) });
                }
            }
            for (k, v) in map {
                if key.is_some() && (k == "key" || k == "value") { continue; }
                let name = if k == "opendataFields" { key.unwrap_or("opendataFields") } else { k };
                let path = if context.is_empty() { name.to_owned() } else { format!("{context} / {name}") };
                collect(v, &path, section(k, group), rows);
            }
        }
        Value::Array(items) => for (i, v) in items.iter().enumerate() {
            collect(v, &format!("{context} [{}]", i + 1), group, rows);
        },
        _ => rows.push(Row { section: group, context: context.into(), key: String::new(), value: scalar(value) }),
    }
}
fn scalar(v: &Value) -> String {
    match v { Value::String(s) => s.clone(), Value::Null => "Niet aangeleverd (null)".into(), _ => v.to_string() }
}
/// Format financial-section values without a float conversion or rounding.
/// General metadata, years, codes and already formatted strings stay literal.
fn display_value(row: &Row) -> String {
    let key = row.key.to_ascii_lowercase();
    if row.section == 0 || matches!(key.as_str(), "financialyear" | "year" | "fiscalyear" | "bookyear"
        | "sbibusinesscode" | "documentadoptiondate" | "kvknummer" | "unit" | "currency" | "decimals" | "scale")
        || key.ends_with("code") || key.ends_with("identifier") || key.ends_with("date") {
        return row.value.clone();
    }
    let (sign, digits) = if row.value.starts_with(['-', '+']) {
        row.value.split_at(1)
    } else { ("", row.value.as_str()) };
    let (integer, fraction) = digits.split_once('.').map_or((digits, None), |(i, f)| (i, Some(f)));
    if integer.is_empty() || !integer.bytes().all(|b| b.is_ascii_digit())
        || (integer.len() > 1 && integer.starts_with('0'))
        || fraction.is_some_and(|f| f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit())) {
        return row.value.clone();
    }
    let mut formatted = sign.to_owned();
    for (i, digit) in integer.chars().enumerate() {
        if i > 0 && (integer.len() - i).is_multiple_of(3) { formatted.push('.'); }
        formatted.push(digit);
    }
    if let Some(fraction) = fraction { formatted.push(','); formatted.push_str(fraction); }
    formatted
}
// Long source identifiers must wrap too. This only adds layout whitespace.
fn wrap(text: &str) -> String { wrap_at(text, 50) }
fn wrap_at(text: &str, width: usize) -> String {
    let mut out = String::new(); let mut run = 0;
    for ch in text.chars() {
        if ch.is_whitespace() { run = 0; out.push(' '); }
        else {
            if run == width { out.push(' '); run = 0; }
            out.push(if ch.is_control() { ' ' } else { ch }); run += 1;
        }
    }
    out
}
fn field_label(key: &str) -> &str {
    match key {
        "FinancialYear" => "Boekjaar", "SbiBusinessCode" => "SBI-code",
        "DocumentAdoptionDate" => "Datum vaststelling",
        "BalanceSheetBeforeAfterAppropriationResults" => "Voor / na resultaatbestemming",
        "AssetsNoncurrentOther" => "Overige vaste activa", "AssetsNoncurrent" => "Vaste activa",
        "AssetsCurrent" => "Vlottende activa", "AssetsCurrentOther" => "Overige vlottende activa",
        "Assets" => "Totaal activa", "Equity" => "Eigen vermogen",
        "EquityAndLiabilities" => "Totaal eigen vermogen en schulden",
        "Liabilities" => "Schulden", "LiabilitiesMaturityExceedingOneYear" => "Schulden langer dan een jaar",
        "LiabilitiesMaturityLessThanOneYear" => "Schulden korter dan een jaar",
        "Revenue" => "Omzet", "ResultAfterTax" => "Resultaat na belasting",
        "IncomeTaxExpense" => "Winstbelasting", _ => key,
    }
}
fn heading(doc: &mut genpdf::Document, text: &str, size: u8) {
    doc.push(Paragraph::new(text).styled(Style::new().bold().with_font_size(size).with_color(Color::Rgb(0, 80, 103))));
}
pub fn render(document: &Value) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(eligible(document), "not a KVK annual account");
    let mut rows = Vec::new();
    collect(&document["data"], "", 0, &mut rows);
    anyhow::ensure!(rows.len() <= 20_000, "report capacity exceeded; source data remains available");
    let font = |bytes: &[u8]| FontData::new(bytes.to_vec(), None);
    let family = FontFamily {
        regular: font(include_bytes!("../assets/fonts/LiberationSans-Regular.ttf"))?,
        bold: font(include_bytes!("../assets/fonts/LiberationSans-Bold.ttf"))?,
        italic: font(include_bytes!("../assets/fonts/LiberationSans-Italic.ttf"))?,
        bold_italic: font(include_bytes!("../assets/fonts/LiberationSans-BoldItalic.ttf"))?,
    };
    let mut doc = genpdf::Document::new(family);
    doc.set_title("Apiosk - Jaarrekeningrapport");
    doc.set_font_size(10);
    let mut decorator = genpdf::SimplePageDecorator::new();
    decorator.set_margins(18);
    decorator.set_header(|page| Paragraph::new(format!("APIOSK  /  JAARREKENING                                      Pagina {page}")).styled(Style::new().with_font_size(8)).padded(genpdf::Margins::trbl(0, 0, 6, 0)));
    doc.set_page_decorator(decorator);
    heading(&mut doc, "Jaarrekeningrapport", 24);
    if let Some(name) = document["report_subject"]["name"].as_str() {
        doc.push(Paragraph::new(wrap(name)).styled(Style::new().with_font_size(14)));
    }
    if let Some(ids) = document["report_subject"]["identifiers"].as_array() {
        for id in ids { doc.push(Paragraph::new(format!("KVK: {}", scalar(id)))); }
    }
    doc.push(Break::new(1));
    for (label, value) in [("Bron", &document["source"]["provider"]), ("Opgehaald", &document["source"]["retrieved_at"]), ("Bronadres", &document["source"]["url"]), ("Uitvoering", &document["execution_id"])] {
        if let Some(value) = value.as_str() { doc.push(Paragraph::new(wrap(&format!("{label}: {value}"))).styled(Style::new().with_font_size(8))); }
    }
    doc.push(Break::new(1));
    doc.push(Paragraph::new("Bronwaarden zijn ongewijzigd; bedragen zijn leesbaar opgemaakt met punten voor duizendtallen en een komma voor decimalen, zonder afronding. Eenheid, valuta, periode en consolidatiestatus gelden alleen wanneer de bron die vermeldt. Een ontbrekend onderdeel betekent niet dat de waarde nul is. Dit rapport is een weergave van de ontvangen gegevens, geen accountantsverklaring."));
    if document["coverage"] == "partial" {
        doc.push(Paragraph::new("LET OP: de opgeslagen bronrespons is onvolledig. Raadpleeg ook de ontbrekende velden hieronder.").styled(Style::new().bold()));
        doc.push(Paragraph::new(wrap(&scalar(&document["missing_fields"]))));
    }
    for (index, title) in SECTIONS.iter().enumerate() {
        doc.push(Break::new(1)); heading(&mut doc, title, 16);
        let section_rows: Vec<_> = rows.iter().filter(|r| r.section == index).collect();
        if section_rows.is_empty() { doc.push(Paragraph::new("Niet aangeleverd in deze bronrespons.")); }
        let mut table = TableLayout::new(vec![3, 1]);
        for row in section_rows {
            let mut label = LinearLayout::vertical();
            label.push(Paragraph::new(wrap(field_label(&row.key))).styled(Style::new().bold()));
            label.push(Paragraph::new(wrap(&format!("{} / {}", row.context, row.key))).styled(Style::new().with_font_size(7).with_color(Color::Rgb(90, 100, 110))));
            table.row()
                .element(label.padded(genpdf::Margins::trbl(1, 4, 2, 0)))
                .element(Paragraph::new(wrap_at(&display_value(row), 20)).aligned(genpdf::Alignment::Right).padded(genpdf::Margins::trbl(1, 0, 2, 0)))
                .push()?;
        }
        doc.push(table);
    }
    let mut bytes = Vec::new(); doc.render(&mut bytes)?; Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn amounts_use_dutch_separators_without_changing_precision_or_metadata() {
        let mut row = Row { section: 1, context: String::new(), key: "Assets".into(), value: String::new() };
        for (raw, expected) in [("-159000", "-159.000"), ("18539000", "18.539.000"),
            ("1234567.50", "1.234.567,50"), ("0", "0"), ("-0.0050", "-0,0050"),
            ("9007199254740993.01", "9.007.199.254.740.993,01"),
            ("01234567", "01234567"), ("not available", "not available"), ("1,234.50", "1,234.50")] {
            row.value = raw.into();
            assert_eq!(display_value(&row), expected);
            assert_eq!(row.value, raw);
        }
        for key in ["FinancialYear", "SbiBusinessCode", "DocumentAdoptionDate", "kvkNummer"] {
            row.key = key.into(); row.value = "2024".into();
            assert_eq!(display_value(&row), "2024");
        }
        row.key = "LiabilitiesMaturityExceedingOneYear".into(); row.value = "955105".into();
        assert_eq!(display_value(&row), "955.105");
        row.section = 0; row.key = "OtherMetadata".into(); row.value = "12345678".into();
        assert_eq!(display_value(&row), "12345678");
    }
    #[test]
    fn all_fields_preserve_zero_negative_values_and_sections() {
        let data = json!({"opendataFields":[{"key":"FinancialYear","value":"2025"},{"key":"ConsolidatedBalanceSheet","opendataFields":[{"key":"Equity","value":"-373267"}]},{"key":"CashFlowStatement","opendataFields":[{"key":"Total","value":0}]},{"key":"IncomeStatement","opendataFields":[{"key":"Revenue","value":"123.45"}]}]});
        let mut rows = vec![]; collect(&data, "", 0, &mut rows);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows.iter().map(|r| r.section).collect::<Vec<_>>(), vec![0,1,2,3]);
        assert_eq!(rows[1].value, "-373267"); assert_eq!(rows[2].value, "0");
        assert!(rows[1].context.contains("ConsolidatedBalanceSheet"));
    }
    #[test]
    fn many_fields_and_long_source_values_are_not_truncated() {
        let fields: Vec<_> = (0..150).map(|i| json!({"key":format!("SourceField{i}"),"value":format!("https://example.com/{}", "a".repeat(80))})).collect();
        let document=json!({"source":{"provider":"kvk-dutch-business-register","path":ENDPOINT},"data":{"opendataFields":fields}});
        let mut rows=vec![]; collect(&document["data"], "", 0, &mut rows);
        assert_eq!(rows.len(),150);
        assert!(render(&document).unwrap().starts_with(b"%PDF-"));
    }
    #[test]
    fn report_tokens_are_scoped_and_expire() {
        use crate::state::Signer; use uuid::Uuid;
        let signer = Signer::new(vec![42;32]).unwrap();
        let owner=Uuid::new_v4(); let task=Uuid::new_v4(); let result=Uuid::new_v4();
        let expires=chrono::Utc::now().timestamp()+60;
        let sig=signer.report_signature(owner,task,result,expires);
        assert!(signer.verify_report(owner,task,result,expires,&sig).is_ok());
        assert!(signer.verify_report(Uuid::new_v4(),task,result,expires,&sig).is_err());
        assert!(signer.verify_report(owner,task,Uuid::new_v4(),expires,&sig).is_err());
        assert!(signer.verify_report(owner,task,result,0,&sig).is_err());
    }
    #[test]
    fn source_fixture_renders_without_invented_statements() {
        let data: Value=serde_json::from_str(include_str!("../crates/catalog-core/fixtures/contracts/payloads/kvk-jaarrekening-86717162.json")).unwrap();
        let document=json!({"source":{"provider":"kvk-dutch-business-register","path":ENDPOINT},"data":data});
        let mut rows=vec![]; collect(&document["data"],"",0,&mut rows);
        assert!(!rows.iter().any(|r| r.section==2 || r.section==3));
        assert!(render(&document).unwrap().starts_with(b"%PDF-"));
        assert!(!eligible(&json!({"source":{"provider":"other"},"data":data})));
    }
}
