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

/// Generated operation fields have opaque ids. A documented company/entity
/// name input may still accept the exact typed subject; a bare field named
/// `name` alone is not enough to establish that meaning.
pub(crate) fn documented_literal(field: &str, schema: &Value, subject: &Subject) -> Option<Value> {
    if let Some(value) = literal(field, subject) { return Some(value); }
    if !field.starts_with("source_input.") { return None; }
    let description = schema["description"].as_str().unwrap_or_default().to_lowercase();
    let company = matches!(subject.kind.as_str(), "company" | "organization" | "organisation" | "business");
    let person = matches!(subject.kind.as_str(), "person" | "individual");
    let company_name = ["company name", "business name", "organization name", "organisation name", "entity name", "name of the company", "name of the entity"]
        .iter().any(|term| description.contains(term));
    let person_name = description.contains("person name") || description.contains("person or entity name");
    ((company && company_name) || (person && person_name)).then(|| json!(subject.label))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn documented_names_respect_company_and_person_types() {
        for (kind, description, expected) in [
            ("company","Person or entity name to screen",true),
            ("person","Person or entity name to screen",true),
            ("company","Company name",true),
            ("person","Person name",true),
            ("company","Person name",false),
            ("person","Company name",false),
            ("company","Name",false),
        ] {
            let subject=Subject {id:"s1".into(),label:"Literal subject".into(),kind:kind.into(),identifiers:Default::default()};
            assert_eq!(documented_literal("source_input.example.name",&json!({"type":"string","description":description}),&subject).is_some(),expected,"{kind}: {description}");
        }
    }
}
