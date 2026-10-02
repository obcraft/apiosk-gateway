//! Literal bindings shared by endpoint review and the planner fallback. These
//! are known semantic input types, never a fuzzy match on an arbitrary field.
use crate::capability_intake::Subject;
use serde_json::{json, Value};

pub(crate) fn literal(field: &str, subject: &Subject) -> Option<Value> {
    let exact: Vec<_> = subject.identifiers.iter()
        .filter(|(key, _)| field == *key || field.ends_with(&format!(".{key}"))).collect();
    if exact.len() == 1 { return Some(json!(exact[0].1)); }
    let identifier = |keys: &[&str]| keys.iter().find_map(|key| subject.identifiers.get(*key));
    let value = match field {
        "company_registry.kvknummer" => identifier(&["kvk_number", "kvknummer"]),
        "company.registration.gb.companies_house" => identifier(&["companies_house_number", "company_number", "crn"]),
        "vehicle.registration.nl.plate" => identifier(&["license_plate", "kenteken"]),
        "domain.name" => identifier(&["domain", "domain_name"]),
        "finance_markets.cityfalcon_identifiers" => identifier(&["ticker", "isin"]),
        "vat.eu.country_code" | "vat.eu.number" => {
            let number = identifier(&["vat_number", "btw_number"])?;
            // VIES takes the two literal parts separately. Do not infer a
            // country from the company name, language or a different subject.
            if number.len() <= 2 || !number.is_ascii() || !number[..2].bytes().all(|c| c.is_ascii_alphabetic()) { return None; }
            return Some(json!(if field == "vat.eu.country_code" { &number[..2] } else { &number[2..] }));
        }
        _ => None,
    };
    if let Some(value) = value { return Some(json!(value)); }
    let kind = subject.kind.as_str();
    let company = matches!(kind, "company" | "organization" | "organisation" | "business");
    let topic = matches!(kind, "topic" | "query" | "keyword");
    let address = matches!(kind, "address" | "location" | "place" | "property" | "building");
    let named = field == format!("{kind}.name") || field == format!("{kind}.registered_name")
        || (field == "company.name" && company)
        || (matches!(field, "news.reuters_search_query" | "web.query" | "web.source_search_query") && (company || topic))
        || (matches!(field, "company_intel.input.search" | "subsidy.discovery.keyword" | "source.koop.q") && topic)
        || (field == "geo.input.q" && address)
        || (field == "domain.name" && kind == "domain");
    named.then(|| json!(subject.label))
}
