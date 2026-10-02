//! Constraints select contracts or preserve explicit retrieval scope. They
//! cannot be an alternate channel for generated API parameters or business data.
use super::Goal;
use crate::catalog::Catalog;
use anyhow::ensure;
use serde_json::Value;

fn words(text: &str) -> Vec<String> {
    text.to_lowercase().split(|ch: char| !ch.is_alphanumeric())
        .filter(|part| !part.is_empty()).map(str::to_owned).collect()
}
fn literal(question: &str, value: &str) -> bool {
    let wanted = words(value);
    !wanted.is_empty() && words(question).windows(wanted.len()).any(|part| part == wanted)
}
fn any_literal(question: &str, terms: &[&str]) -> bool {
    terms.iter().any(|term| literal(question, term))
}
pub(super) fn source_matches(catalog: &Catalog, service: &crate::catalog::Service, source: &str) -> bool {
    service.api_slug == source || catalog.known_sources.iter().any(|known|
        known.slug == service.api_slug && known.metadata.provider_slug.as_deref() == Some(source))
}

pub(super) fn validate(question: &str, goal: &Goal, catalog: &Catalog) -> anyhow::Result<()> {
    let source = goal.constraints.get("source").and_then(Value::as_str);
    let selected = |service: &crate::catalog::Service| service.contract.capability_slug == goal.capability
        && source.is_none_or(|source| source_matches(catalog, service, source));
    for (key, value) in &goal.constraints {
        if catalog.vocabulary.contains_key(key) {
            // The shared typed-input validator also checks literal provenance,
            // schema and selected endpoint variants before moving these values.
            continue;
        }
        let text = value.as_str().filter(|text| !text.trim().is_empty() && text.len() <= 500);
        let grounded = match key.as_str() {
            "source" => {
                ensure!(text.is_some(), "invalid retrieval constraint");
                ensure!(catalog.services.iter().any(selected), "unsupported retrieval constraint");
                true
            }
            "endpoint_id" => {
                let id = |v: &Value| v.as_str().and_then(|v| v.parse::<uuid::Uuid>().ok())
                    .is_some_and(|id| catalog.services.iter().any(|service| selected(service) && service.endpoint_id == id));
                ensure!(id(value) || value.as_array().is_some_and(|ids| !ids.is_empty() && ids.iter().all(id)), "invalid retrieval constraint");
                true
            }
            "period" => {
                let period = text.ok_or_else(|| anyhow::anyhow!("invalid retrieval constraint"))?;
                // Documented annual-series normalization may change separators
                // (2024 en 2025 -> 2024-2025), never add a year or date.
                literal(question, period) || annual_range(question, period)
            }
            "sector" => {
                let sector = text.ok_or_else(|| anyhow::anyhow!("invalid retrieval constraint"))?;
                let construction = ["bouw", "bouwsector", "nederlandse bouwsector", "bouwnijverheid", "construction", "dutch construction"];
                literal(question, sector) || (construction.contains(&sector.to_lowercase().as_str()) && any_literal(question, &construction))
            }
            "currency" => {
                let currency = text.ok_or_else(|| anyhow::anyhow!("invalid retrieval constraint"))?;
                literal(question, currency) || match currency.to_ascii_uppercase().as_str() {
                    "EUR" => question.contains('€') || any_literal(question, &["euro", "euros"]),
                    "USD" => any_literal(question, &["US dollar", "US dollars", "American dollars", "Amerikaanse dollars"]),
                    "GBP" => any_literal(question, &["pound sterling", "pounds sterling", "pond sterling"]),
                    _ => false,
                }
            }
            "jurisdiction" => {
                let country = text.ok_or_else(|| anyhow::anyhow!("invalid retrieval constraint"))?;
                literal(question, country) || crate::ask_v2::search::named_countries(question).contains(country.to_ascii_uppercase().as_str())
            }
            "max_age_seconds" => {
                let seconds = value.as_u64().ok_or_else(|| anyhow::anyhow!("invalid retrieval constraint"))?;
                duration_supplied(question, seconds)
            }
            "coverage" => {
                ensure!(text == Some("complete"), "invalid retrieval constraint");
                any_literal(question, &["all", "every", "complete", "entire", "exhaustive", "alle", "volledig", "volledige", "compleet"])
            }
            "filing_selection" => {
                let selection = text.ok_or_else(|| anyhow::anyhow!("invalid retrieval constraint"))?;
                ensure!(["latest", "latest_two", "specific"].contains(&selection), "invalid retrieval constraint");
                match selection {
                    "latest" => any_literal(question, &["latest", "most recent", "last", "laatste", "recentste", "meest recente"]),
                    "latest_two" => any_literal(question, &["latest two", "last two", "two latest", "laatste twee", "twee laatste", "latest 2", "last 2", "laatste 2"]),
                    _ => any_literal(question, &["specific", "specified", "specifieke", "specifiek"])
                        || words(question).iter().any(|word| word.len() == 4 && word.parse::<u16>().is_ok_and(|year| (1900..=2100).contains(&year))),
                }
            }
            "source_period_filter" => {
                let filter = value.as_bool().ok_or_else(|| anyhow::anyhow!("invalid retrieval constraint"))?;
                !filter || any_literal(question, &["filter", "filtered", "filtering", "filteren", "filtert", "gefilterd", "gefilterde", "date filter", "datumfilter"])
            }
            _ => anyhow::bail!("unsupported retrieval constraint"),
        };
        ensure!(grounded, "ungrounded retrieval constraint");
    }
    Ok(())
}

fn annual_range(question: &str, period: &str) -> bool {
    let parts = words(period);
    let years: Vec<_> = parts.iter().filter(|part| part.len() == 4 && part.parse::<u16>().is_ok_and(|year| (1900..=2100).contains(&year))).collect();
    !years.is_empty() && parts.iter().all(|part| years.contains(&part) || ["and", "en", "to", "tot", "through", "t", "m"].contains(&part.as_str()))
        && years.iter().all(|year| literal(question, year))
}

fn duration_supplied(question: &str, seconds: u64) -> bool {
    if seconds == 0 { return false; }
    let parts: Vec<_> = question.split_whitespace().collect();
    parts.windows(2).any(|pair| {
        // Do not turn 2.5 hours into 5 hours, or -2 hours into 2 hours.
        let operand = pair[0].trim_matches(['(', ')', '[', ']']);
        if !operand.bytes().all(|byte| byte.is_ascii_digit()) { return false; }
        let Ok(number) = operand.parse::<u64>() else { return false; };
        let unit = pair[1].trim_matches(|ch: char| !ch.is_alphanumeric()).to_lowercase();
        let multiplier = match unit.as_str() {
            "second" | "seconds" | "seconde" | "seconden" => 1,
            "minute" | "minutes" | "minuut" | "minuten" => 60,
            "hour" | "hours" | "uur" | "uren" => 3600,
            "day" | "days" | "dag" | "dagen" => 86400,
            _ => return false,
        };
        number.checked_mul(multiplier) == Some(seconds)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn check(question: &str, key: &str, value: Value) -> anyhow::Result<()> {
        let catalog = Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/integration-catalog.json")).unwrap());
        validate(question,&Goal {subject:0,capability:"company.accounts".into(),constraints:BTreeMap::from([(key.into(),value)])},&catalog)
    }
    #[test]
    fn unknown_renamed_and_nested_filters_cannot_invent_business_scope() {
        let question="Validate NL002033354B01 and give sector benchmarks for Dutch retail";
        for (key,value) in [
            ("$filter",json!("SectorCode eq '5210' and Period eq '2023'")),
            ("query",json!({"$filter":"Period eq '2023'"})),
            ("parameters",json!({"sector":5210,"year":2023})),
            ("period",json!("2023")), ("sector",json!("5210")), ("currency",json!("USD")),
            ("period",json!({"year":2023})), ("sector",json!(["5210"])),
            ("coverage",json!("complete for 5210 in 2023")),
            ("source_period_filter",json!("SectorCode eq '5210' and Period eq '2023'")),
            ("source_period_filter",json!(true)), ("coverage",json!("complete")),
            ("filing_selection",json!("latest")), ("max_age_seconds",json!(2023)),
        ] { assert!(check(question,key,value).is_err(),"{key}"); }
    }
    #[test]
    fn documented_policy_normalization_preserves_supplied_scope() {
        for (question,key,value) in [
            ("Compare 2024 en 2025","period",json!("2024-2025")),
            ("Compare 2024-2025","period",json!("2024 and 2025")),
            ("Data for bouwsector","sector",json!("construction")),
            ("Use SectorCode 5210 in 2023","sector",json!("5210")),
            ("Use SectorCode 5210 in 2023","period",json!("2023")),
            ("Totals in euros","currency",json!("EUR")),
            ("Totals in US dollars","currency",json!("USD")),
            ("Dutch company profile","jurisdiction",json!("NL")),
            ("Retrieve all records","coverage",json!("complete")),
            ("Retrieve the latest two filings","filing_selection",json!("latest_two")),
            ("Apply a source date filter for 2024","source_period_filter",json!(true)),
            ("Data at most 2 hours old","max_age_seconds",json!(7200)),
            ("Data at most 60 seconds old","max_age_seconds",json!(60)),
            ("An unnamed source is selected from the catalog","source",json!("fixture")),
            ("An endpoint is selected from the catalog","endpoint_id",json!("484a43fa-3de1-4c09-8c5f-21305829b805")),
        ] {check(question,key,value).unwrap_or_else(|error|panic!("{key}: {error}"));}
        assert!(check("Compare 2024 en 2025","period",json!("2023-2025")).is_err());
        assert!(check("Data for retail","sector",json!("construction")).is_err());
        assert!(check("Data at most 2 hours old","max_age_seconds",json!(86400)).is_err());
        assert!(check("Data at most 2.5 hours old","max_age_seconds",json!(18000)).is_err());
        assert!(check("Data at most -2 hours old","max_age_seconds",json!(7200)).is_err());
        assert!(check("Data at most 2,5 hours old","max_age_seconds",json!(18000)).is_err());
        assert!(check("Choose a catalog source","source",json!("invented-provider")).is_err());
        assert!(check("Choose a catalog endpoint","endpoint_id",json!(uuid::Uuid::new_v4())).is_err());
        check("Choose a catalog endpoint","endpoint_id",json!(["484a43fa-3de1-4c09-8c5f-21305829b805"])).unwrap();
        let mut catalog=Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/integration-catalog.json")).unwrap());
        catalog.known_sources.push(serde_json::from_value(json!({"slug":"fixture","name":"Fixture","block_reason":null,"metadata":{"provider_slug":"fixture-group"}})).unwrap());
        let mut goal=Goal {subject:0,capability:"company.accounts".into(),constraints:BTreeMap::from([("source".into(),json!("fixture-group")),("endpoint_id".into(),json!("484a43fa-3de1-4c09-8c5f-21305829b805"))])};
        validate("Retrieve accounts",&goal,&catalog).unwrap();
        goal.constraints.insert("endpoint_id".into(),json!("7cd248cf-ac7a-4ec3-85dc-536dc3d39f21"));
        assert!(validate("Retrieve accounts",&goal,&catalog).is_err(), "a valid source endpoint for another capability cannot pass");
    }
}
