//! A source-backed projection of CBS bankruptcy table 82244NED. This module
//! performs no retrieval. A complete latest monthly slice and its saved
//! codebooks must be present before it can describe a sector comparison.
use crate::capability_intake::{Capability, Deliverable, Meta, ParsedRequest, Subject};
use anyhow::{ensure, Context};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const CAPABILITY: &str = "statistics.cbs-bankruptcy-sector-comparison";
const POPULATION: &str = "A047596";
const MEASURE: &str = "M001327";
const TOTAL: &str = "T001081";
const UNKNOWN: &str = "999999";

fn language(question: &str) -> Option<&'static str> {
    let text = question.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    match text.trim_end_matches(['.', '?', '!']) {
        "compare bankruptcies per sector in the netherlands"
        | "compare bankruptcies by sector in the netherlands" => Some("en"),
        "vergelijk faillissementen per sector in nederland"
        | "vergelijk faillissementen per bedrijfstak in nederland" => Some("nl"),
        _ => None,
    }
}

/// Consume the entire plain request. Explicit years, rates, extra deliverables,
/// countries or additional questions remain on the ordinary interpretation path.
pub(crate) fn canonical_request(question: &str) -> Option<ParsedRequest> {
    let language = language(question)?;
    Some(ParsedRequest {
        language: language.into(),
        subjects: vec![Subject { id: "s1".into(), label: if language == "nl" { "Nederland" } else { "Netherlands" }.into(), kind: "country".into(), identifiers: BTreeMap::new() }],
        capabilities: [
            ("counts", "Dutch bankruptcy counts by sector", "Retrieve absolute monthly bankruptcy counts for disjoint Dutch economic sectors, companies, institutions and sole proprietors. Use the latest available common month and keep the national total separate.", vec!["bankruptcy_count"]),
            ("sectors", "CBS bankruptcy sector definitions", "Resolve the official sector code labels and section hierarchy for the bankruptcy sector comparison.", vec!["sector"]),
            ("periods", "CBS bankruptcy reporting periods", "Resolve the official reporting period labels and provisional or definitive status for the bankruptcy comparison.", vec!["period", "period_status"]),
            ("measures", "CBS bankruptcy measures and units", "Resolve the official bankruptcy measure codebook and count unit for the comparison.", vec!["unit"]),
            ("population", "CBS bankruptcy entity type definitions", "Resolve the official bankruptcy entity-types codebook to preserve the compared business population.", vec!["population"]),
        ].into_iter().map(|(slug, name, description, outputs)| Capability {
            slug: format!("research.bankruptcies.{slug}"), name: name.into(), description: description.into(),
            domain: "research".into(), inputs: vec![], outputs: outputs.into_iter().map(str::to_owned).collect(),
            subject_id: Some("s1".into()), source_hint: Some("cbs-statline".into()), confidence: 1.0, missing: vec![],
        }).collect(),
        deliverable: Deliverable { format: "chat".into(), operations: vec!["compare".into()] },
        meta: Meta { model: "reviewed-cbs-comparison-v1".into(), parsed_at: chrono::Utc::now().to_rfc3339(), text_sha256: hex::encode(Sha256::digest(question.as_bytes())) },
    })
}

/// Only the reviewed fixed native calls can implement this default-period
/// request. Raw OData inputs and generic unfiltered observations remain separate.
pub(crate) fn reviewed_services(catalog: &crate::catalog::Catalog) -> anyhow::Result<Vec<&crate::catalog::Service>> {
    let definitions = [
        (CAPABILITY, "/odata/v1/CBS/82244NED/Observations", json!({"$filter":"TypeGefailleerde eq 'A047596' and contains(Perioden,'MM')", "$orderby":"Perioden desc", "$top":1000, "$count":true})),
        ("statistics.cbs-bankruptcy-sectors", "/odata/v1/CBS/{tableId}/BedrijfstakkenBranchesSBI2008Codes", json!({"tableId":"82244NED", "$select":"Identifier,Title,DimensionGroupId"})),
        ("statistics.cbs-bankruptcy-periods", "/odata/v1/CBS/82244NED/PeriodenCodes", json!({"$select":"Identifier,Title,Status"})),
        ("statistics.cbs-bankruptcy-measures", "/odata/v1/CBS/{tableId}/MeasureCodes", json!({"tableId":"82244NED", "$select":"Identifier,Title,Unit"})),
        ("statistics.cbs-bankruptcy-entity-types", "/odata/v1/CBS/{tableId}/TypeGefailleerdeCodes", json!({"tableId":"82244NED", "$select":"Identifier,Title"})),
    ];
    definitions.into_iter().map(|(capability, path, fixed)| {
        let matching: Vec<_> = catalog.services.iter().filter(|service| service.api_slug == "cbs-statline"
            && service.contract.capability_slug == capability && service.method == "GET"
            && service.path == path && service.fixed_params == fixed && service.contract.requires.is_empty()
            && service.contract.input_map.is_empty() && !service.unverified).collect();
        ensure!(matching.len() == 1, "reviewed CBS comparison source is unavailable or changed: {capability}");
        Ok(matching[0])
    }).collect()
}

struct Source<'a> {
    saved: &'a Value,
    rows: &'a [Value],
}

impl Source<'_> {
    fn reference(&self, row: usize, field: &str) -> Value {
        json!({"result_ref": self.saved["result_ref"], "pointer": format!("/data/value/{row}/{field}")})
    }
}

fn source<'a>(sources: &'a [Value], resource: &str, complete: bool) -> anyhow::Result<Source<'a>> {
    let context = format!("https://datasets.cbs.nl/odata/v1/CBS/82244NED/$metadata#{resource}");
    let matches: Vec<_> = sources.iter().filter(|saved| {
        let document = &saved["document"];
        document["source"]["provider"] == "cbs-statline"
            && document["coverage"] == "source_response"
            && document["data"]["@odata.context"].as_str().is_some_and(|value| {
                value == context || value.strip_prefix(&context).is_some_and(|tail| tail.starts_with('(') && tail.ends_with(')'))
            })
    }).collect();
    ensure!(matches.len() == 1, "CBS comparison needs exactly one saved {resource} source for table 82244NED");
    let saved = matches[0];
    ensure!(saved["result_ref"].as_str().is_some_and(|id| uuid::Uuid::parse_str(id).is_ok()), "invalid saved CBS source reference");
    let data = &saved["document"]["data"];
    let rows = data["value"].as_array().context("CBS source has no value rows")?;
    ensure!(!rows.is_empty(), "CBS comparison source is empty");
    if complete {
        ensure!(data.get("@odata.nextLink").is_none_or(Value::is_null), "CBS codebook is paginated");
        ensure!(data.get("@odata.count").is_none_or(|count| count.as_u64() == Some(rows.len() as u64)), "CBS codebook is incomplete");
    }
    Ok(Source { saved, rows })
}

fn code_index<'a>(source: &Source<'a>) -> anyhow::Result<BTreeMap<&'a str, usize>> {
    let mut index = BTreeMap::new();
    for (row, value) in source.rows.iter().enumerate() {
        let code = value["Identifier"].as_str().filter(|s| !s.is_empty()).context("CBS code has no identifier")?;
        ensure!(value["Title"].as_str().is_some_and(|title| !title.trim().is_empty()), "CBS code has no label");
        ensure!(index.insert(code, row).is_none(), "duplicate CBS code");
    }
    Ok(index)
}

fn monthly(code: &str) -> bool {
    code.len() == 8 && code.is_ascii() && code[0..4].bytes().all(|b| b.is_ascii_digit())
        && &code[4..6] == "MM" && code[6..8].parse::<u8>().is_ok_and(|m| (1..=12).contains(&m))
}

fn count(value: &Value) -> anyhow::Result<Option<f64>> {
    if value["Value"].is_null() {
        ensure!(value.get("Value").is_some(), "CBS observation has no value field");
        return Ok(None);
    }
    let number = value["Value"].as_f64().context("CBS bankruptcy count is not numeric")?;
    ensure!(number.is_finite() && number >= 0.0 && number.fract() == 0.0
        && value["ValueAttribute"] == "None" && value["StringValue"].is_null(), "CBS bankruptcy count is not an ordinary nonnegative count");
    Ok(Some(number))
}

/// `None` means this question is outside the reviewed whole-request scope.
/// An error for a recognized request must stop synthesis: a bounded or mismatched
/// source response cannot be passed off as a complete sector comparison.
pub(crate) fn compose(question: &str, sources: &[Value]) -> anyhow::Result<Option<Value>> {
    let Some(language) = language(question) else { return Ok(None) };
    let dutch = language == "nl";
    let observations = source(sources, "Observations", false)?;
    let sectors = source(sources, "BedrijfstakkenBranchesSBI2008Codes", true)?;
    let periods = source(sources, "PeriodenCodes", true)?;
    let measures = source(sources, "MeasureCodes", true)?;
    let populations = source(sources, "TypeGefailleerdeCodes", true)?;
    let sector_codes = code_index(&sectors)?;
    let period_codes = code_index(&periods)?;
    let measure_codes = code_index(&measures)?;
    let population_codes = code_index(&populations)?;
    let (&period, &period_row) = period_codes.iter().filter(|(code, _)| monthly(code)).next_back().context("CBS has no monthly reporting period")?;
    let &measure_row = measure_codes.get(MEASURE).context("CBS bankruptcy measure is missing")?;
    let &population_row = population_codes.get(POPULATION).context("CBS business population is missing")?;
    ensure!(measures.rows[measure_row]["Unit"] == "Aantal", "CBS measure is not a count");
    ensure!(populations.rows[population_row]["Title"] == "Bedrijven, instellingen en eenmanszaken", "CBS business population definition changed");
    ensure!(matches!(periods.rows[period_row]["Status"].as_str(), Some("Definitief" | "Voorlopig" | "NaderVoorlopig")), "CBS reporting status is missing or unknown");

    let mut latest = BTreeMap::new();
    for (row, value) in observations.rows.iter().enumerate() {
        let code = value["BedrijfstakkenBranchesSBI2008"].as_str().context("CBS observation has no sector")?;
        let observed_period = value["Perioden"].as_str().context("CBS observation has no period")?;
        ensure!(sector_codes.contains_key(code) && period_codes.contains_key(observed_period)
            && monthly(observed_period) && value["Measure"] == MEASURE && value["TypeGefailleerde"] == POPULATION,
            "CBS comparison observation falls outside the reviewed monthly business population");
        if observed_period == period {
            count(value)?;
            ensure!(latest.insert(code, row).is_none(), "duplicate CBS sector in latest month");
        }
    }
    ensure!(latest.len() == sector_codes.len() && sector_codes.keys().all(|code| latest.contains_key(code)), "CBS latest month is incomplete; do not compare a truncated sector page");

    // CBS table 82244NED's section group is disjoint; detailed industries are
    // nested below it and must never be summed or ranked alongside sections.
    let mut section_rows = Vec::new();
    let mut letters = BTreeSet::new();
    for (&code, &row) in &sector_codes {
        if sectors.rows[row]["DimensionGroupId"] == "3" {
            let title = sectors.rows[row]["Title"].as_str().unwrap_or("");
            let bytes = title.as_bytes();
            ensure!(bytes.len() > 2 && bytes[1] == b' ' && (b'A'..=b'T').contains(&bytes[0]) && letters.insert(bytes[0]), "CBS section hierarchy changed");
            section_rows.push((code, row, latest[code], count(&observations.rows[latest[code]])?));
        }
    }
    ensure!(letters == (b'A'..=b'T').collect::<BTreeSet<_>>(), "CBS section codebook is incomplete");
    let &total_sector = sector_codes.get(TOTAL).context("CBS national total is missing")?;
    let &unknown_sector = sector_codes.get(UNKNOWN).context("CBS unclassified sector is missing")?;
    ensure!(sectors.rows[total_sector]["DimensionGroupId"] == "1" && sectors.rows[unknown_sector]["DimensionGroupId"] == "4", "CBS total or unclassified hierarchy changed");
    let total_row = latest[TOTAL];
    let unknown_row = latest[UNKNOWN];
    let total = count(&observations.rows[total_row])?.context("CBS national total is unavailable")?;
    let unknown = count(&observations.rows[unknown_row])?;
    let missing = section_rows.iter().any(|row| row.3.is_none()) || unknown.is_none();
    if !missing {
        ensure!(section_rows.iter().map(|row| row.3.unwrap()).sum::<f64>() + unknown.unwrap() == total, "CBS sections do not reconcile to the national total");
    }
    section_rows.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(b.0)));
    section_rows.push((UNKNOWN, unknown_sector, unknown_row, unknown));
    let rows: Vec<_> = section_rows.iter().map(|(_, sector, observation, _)| json!([
        sectors.reference(*sector, "Title"), observations.reference(*observation, "Value"),
        periods.reference(period_row, "Title"), periods.reference(period_row, "Status")
    ])).collect();
    let mut limitations = vec![if dutch {
        "Dit zijn absolute aantallen, geen faillissementspercentages of maatstaf voor het risico per bedrijf. Gedetailleerde onderliggende branches worden niet naast hun bovenliggende sector opgeteld. De onbekende sector staat afzonderlijk onderaan."
    } else {
        "These are absolute counts, not bankruptcy rates or a measure of risk per business. Detailed industries are not added alongside their parent sectors. The unclassified sector is shown separately at the bottom."
    }];
    if missing { limitations.push(if dutch { "Ontbrekende sectorwaarden blijven onbekend; de som van de sectoren kan daardoor niet worden gecontroleerd." } else { "Missing sector values remain unknown, so the sector sum cannot be verified." }); }
    let raw = json!({
        "observations": [
            {"text": if dutch { "Vergelijking voor de meest recente beschikbare maand: {{a}}. De publicatiestatus van CBS is {{b}}." } else { "Comparison for the latest available month: {{a}}. The CBS publication status is {{b}}." },
             "evidence": [periods.reference(period_row, "Title"), periods.reference(period_row, "Status")]},
            {"text": if dutch { "De bronpopulatie is {{a}}. De maatstaf is {{b}}, met eenheid {{c}}." } else { "The source population is {{a}} (companies, institutions and sole proprietors). The measure is {{b}}, with unit {{c}}." },
             "evidence": [populations.reference(population_row, "Title"), measures.reference(measure_row, "Title"), measures.reference(measure_row, "Unit")]},
            {"text": if dutch { "Het afzonderlijke landelijke totaal voor {{a}} is {{b}}. De tabel vergelijkt uitsluitend niet-overlappende sectoren voor dezelfde maand en populatie." } else { "The separate national total for {{a}} is {{b}}. The table compares disjoint sectors for the same month and business population." },
             "evidence": [sectors.reference(total_sector, "Title"), observations.reference(total_row, "Value")]}
        ],
        "limitations": limitations,
        "presentation": {"kind":"table", "title": if dutch {"Faillissementen per sector in Nederland"} else {"Bankruptcies by sector in the Netherlands"},
            "columns":[{"label":if dutch {"Sector"}else{"Sector"}}, {"label":if dutch {"Faillissementen"}else{"Bankruptcies"}}, {"label":if dutch {"Maand"}else{"Month"}}, {"label":if dutch {"CBS-status"}else{"CBS status"}}], "rows":rows}
    });
    let mut analysis = crate::parser::validate_analysis(&raw.to_string(), sources, missing)?;
    analysis["method"] = json!("saved_cbs_bankruptcy_sector_comparison");
    Ok(Some(analysis))
}

#[cfg(test)]
#[path = "cbs_comparison_tests.rs"]
mod tests;
