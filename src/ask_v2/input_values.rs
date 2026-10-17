//! Literal bindings shared by endpoint review and the planner fallback. These
//! are known semantic input types, never a fuzzy match on an arbitrary field.
use crate::capability_intake::Subject;
use serde_json::{json, Value};

/// The ECB reference-rate contract fixes the base to EUR. Preserve the
/// direction of an explicit pair when serializing its target-currency input.
pub(crate) fn ecb_quote_for_pair(value: &str) -> Option<Value> {
    let (base, quote) = value.split_once('/')?;
    (base == "EUR" && quote.len() == 3 && quote.bytes().all(|b| b.is_ascii_uppercase()))
        .then(|| json!(quote))
}

/// A supplied public profile URL contains the exact provider identifier. Strip
/// only the documented entity path; never infer a permalink from a name.
pub(crate) fn crunchbase_profile_id(field: &str, value: &str) -> Option<Value> {
    let entity = match field {
        "company.crunchbase_entity_id" => "organization",
        "person.crunchbase_entity_id" => "person",
        "funding.crunchbase_entity_id" => "funding_round",
        _ => return None,
    };
    let url = reqwest::Url::parse(value).ok()?;
    if url.scheme() != "https" || !matches!(url.host_str(), Some("crunchbase.com" | "www.crunchbase.com"))
        || !url.username().is_empty() || url.password().is_some() || url.port().is_some()
        || url.query().is_some() || url.fragment().is_some() { return None; }
    let path: Vec<_> = url.path().trim_matches('/').split('/').collect();
    if path.len() != 2 || path[0] != entity || path[1].is_empty() || path[1].len() > 200
        || !path[1].bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')) { return None; }
    Some(json!(path[1]))
}

pub(crate) fn literal(field: &str, subject: &Subject) -> Option<Value> {
    // Intake canonicalizes identifier names. Match that same complete field
    // identity; a flattened namespace must not become a fuzzy suffix match.
    let canonical_field = field.to_lowercase().split(|ch: char| !ch.is_alphanumeric())
        .filter(|part| !part.is_empty()).collect::<Vec<_>>().join("_");
    let exact: std::collections::BTreeSet<_> = subject.identifiers.iter()
        .filter(|(key, _)| field == *key || canonical_field == **key || field.ends_with(&format!(".{key}")))
        .map(|(_, value)| value).collect();
    if exact.len() == 1 { return Some(json!(exact.first().unwrap())); }
    if exact.len() > 1 { return None; }
    let identifier = |keys: &[&str]| keys.iter().find_map(|key| subject.identifiers.get(*key));
    if field == "fx.ecb.quote" {
        if let Some(pair) = identifier(&["currency_pair", "pair"]) {
            return ecb_quote_for_pair(pair);
        }
        if subject.kind == "currency_pair" { return ecb_quote_for_pair(&subject.label); }
    }
    if let Some(value) = identifier(&["url", "profile_url", "crunchbase_url"])
        .and_then(|url| crunchbase_profile_id(field, url))
        .or_else(|| crunchbase_profile_id(field, &subject.label)) { return Some(value); }
    let value = match field {
        "web.website_query" => identifier(&["query"]),
        "web.website_url" => identifier(&["url"]),
        "company_registry.kvknummer" => identifier(&["kvk_number", "kvknummer"]),
        "company.registration.gb.companies_house" => identifier(&["companies_house_number", "company_number", "crn"]),
        "vehicle.registration.nl.plate" => identifier(&["license_plate", "kenteken"]),
        "domain.name" => identifier(&["domain", "domain_name"]),
        "domain.dns_name" => identifier(&["dns_name", "domain", "domain_name"]),
        "finance_markets.cityfalcon_identifiers" => identifier(&["ticker", "isin"]),
        "company_intel.input.search" | "subsidy.discovery.keyword" | "source.koop.q" => identifier(&["query"]),
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
    if field == "geo.input.q" && address {
        // The model may split an address between its label and locality
        // identifiers. Keep every supplied part in the lookup; never infer a city.
        let unique = |keys: &[&str]| -> Vec<&str> {
            let mut values = Vec::new();
            for value in keys.iter().filter_map(|key| subject.identifiers.get(*key)).map(|value| value.trim()).filter(|value| !value.is_empty()) {
                if !values.iter().any(|prior: &&str| prior.eq_ignore_ascii_case(value)) { values.push(value); }
            }
            values
        };
        // Municipality is an administrative area, not a conflicting city.
        let mut locality = unique(&["city", "woonplaats"]);
        if locality.is_empty() { locality = unique(&["place", "location"]); }
        if locality.is_empty() { locality = unique(&["municipality"]); }
        let postcode = unique(&["postal_code", "postcode", "zip_code"]);
        if locality.len() > 1 || postcode.len() > 1 { return None; }
        let mut query = subject.label.clone();
        let mut missing = Vec::new();
        let words = |value: &str| value.to_lowercase().split(|ch: char| !ch.is_alphanumeric()).filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" ");
        for part in postcode {
            let compact = |value: &str| words(value).replace(' ', "");
            if !compact(&query).contains(&compact(part)) { missing.push(part); }
        }
        for part in locality {
            if !format!(" {} ", words(&query)).contains(&format!(" {} ", words(part))) {
                missing.push(part);
            }
        }
        if !missing.is_empty() { query.push_str(", "); query.push_str(&missing.join(" ")); }
        return Some(json!(query));
    }
    let named = field == format!("{kind}.name") || field == format!("{kind}.registered_name")
        || (matches!(field, "company.name" | "company.crunchbase_query") && company)
        || (matches!(field, "news.reuters_search_query" | "web.query" | "web.source_search_query") && (company || topic))
        || (matches!(field, "company_intel.input.search" | "subsidy.discovery.keyword" | "source.koop.q") && topic)
        || (matches!(field, "domain.name" | "domain.dns_name") && kind == "domain");
    named.then(|| json!(subject.label))
}

/// Address components repeated in a complete address label still reach the
/// documented geocoding input. This does not treat arbitrary search text as an
/// enforced filter, and whole token boundaries keep Dam 1 distinct from Dam 10.
pub(crate) fn address_component_preserved(field:&str,subject:&Subject,key:&str,value:&str,bound:&Value)->bool {
    if field!="geo.input.q" || !matches!(subject.kind.as_str(),"address"|"location"|"place"|"property"|"building")
        || !matches!(key,"address"|"street_address"|"street"|"street_name"|"house_number"|"city"|"woonplaats"|"place"|"location"|"municipality"|"postal_code"|"postcode"|"zip_code")
        || literal(field,subject).as_ref()!=Some(bound) {return false;}
    let normalized=|text:&str|text.to_lowercase().split(|ch:char|!ch.is_alphanumeric()).filter(|part|!part.is_empty()).collect::<Vec<_>>().join(" ");
    let value=normalized(value);
    !value.is_empty() && bound.as_str().is_some_and(|query|format!(" {} ",normalized(query)).contains(&format!(" {value} ")))
}

/// Read only bounded, valid aliases explicitly reviewed in the input schema.
fn reviewed_input_aliases(schema:&Value)->Option<std::collections::BTreeSet<String>> {
    let comment=schema.get("$comment").and_then(Value::as_str).filter(|value|value.len()<=2048)?;
    let metadata=serde_json::from_str::<Value>(comment).ok()?;
    let metadata=metadata.as_object().filter(|metadata|metadata.len()==1)?;
    let aliases=metadata.get("apiosk_input_aliases_v1")?.as_array().filter(|values|!values.is_empty() && values.len()<=32)?;
    let mut names=std::collections::BTreeSet::new();
    for alias in aliases {
        let name=alias.as_str().filter(|name|!name.is_empty() && name.len()<=100
            && name.bytes().all(|c|c.is_ascii_lowercase() || c.is_ascii_digit() || c==b'_'))?;
        if !names.insert(name.to_owned()) {return None;}
    }
    Some(names)
}

pub(crate) fn reviewed_input_alias(schema:&Value,key:&str)->bool {
    reviewed_input_aliases(schema).is_some_and(|aliases| aliases.iter().any(|alias| input_concept(alias) == input_concept(key)))
}

// Exact semantic synonyms, not suffix/fuzzy matches. The provider must still
// explicitly declare this concept in its reviewed input schema.
fn input_concept(key: &str) -> &str {
    match key {
        "service_category" | "service_type" => "service",
        "search_term" | "search_phrase" => "keyword",
        _ => key,
    }
}

fn name_family(value: &str) -> Option<&'static str> {
    match value {
        "company_name" | "business_name" | "organization_name" | "organisation_name" => Some("company"),
        "person_name" | "individual_name" => Some("person"),
        _ => None,
    }
}

fn subject_family(subject: &Subject) -> Option<&'static str> {
    match subject.kind.as_str() {
        "company" | "organization" | "organisation" | "business" => Some("company"),
        "person" | "individual" => Some("person"),
        _ => None,
    }
}

/// Generated operation fields have opaque ids. A documented company/entity
/// name input may still accept the exact typed subject; a bare field named
/// `name` alone is not enough to establish that meaning.
pub(crate) fn documented_literal(field: &str, schema: &Value, subject: &Subject) -> Option<Value> {
    // Reviewed schema aliases bind literal semantic criteria to a documented
    // provider input. They never select a source or rewrite the request.
    if let Some(comment) = schema.get("$comment").and_then(Value::as_str).filter(|value| value.len() <= 2048) {
        if let Ok(Value::Object(metadata)) = serde_json::from_str(comment) {
            if metadata.contains_key("apiosk_input_aliases_v1") {
                let aliases=reviewed_input_aliases(schema)?;
                let mut values = std::collections::BTreeSet::new();
                for name in &aliases {
                    // A shared name input may serve several typed operations.
                    // A company's contact name is not its company-search key,
                    // and a person's employer is not their person-search key.
                    if name_family(name).zip(subject_family(subject)).is_some_and(|(alias, subject)| alias != subject) { continue; }
                    for value in subject.identifiers.iter().filter(|(key, value)|
                        input_concept(key) == input_concept(name) && !value.trim().is_empty()).map(|(_, value)| value) {
                        values.insert(value);
                    }
                }
                // Competing filters need explicit disambiguation; never pick
                // one arbitrarily or silently combine provider parameters.
                if values.len() > 1 { return None; }
                if let Some(value) = values.first() { return Some(json!(value)); }
                if !subject.label.trim().is_empty() && aliases.iter().any(|name|
                    name_family(name).zip(subject_family(subject)).is_some_and(|(alias, subject)| alias == subject)) {
                    return Some(json!(subject.label));
                }
            }
        }
    }
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
    fn shared_name_aliases_keep_the_subject_type_and_literal_label() {
        let fixture: Value = serde_json::from_str(include_str!("../../tests/fixtures/crunchbase/typed-name-input.json")).unwrap();
        let schema = &fixture["after"];
        let subject = |kind: &str, label: &str, identifiers: Value| Subject {
            id: "s1".into(), kind: kind.into(), label: label.into(), identifiers: serde_json::from_value(identifiers).unwrap(),
        };
        for (kind, label, identifiers) in [
            ("company", "Example Robotics", json!({})),
            ("company", "Example Robotics", json!({"company_name":"Example Robotics","person_name":"Ada Example"})),
            ("person", "Ada Example", json!({})),
            ("person", "Ada Example", json!({"company_name":"Example Robotics","person_name":"Ada Example"})),
        ] {
            assert_eq!(documented_literal("shared.provider_name_query", schema, &subject(kind, label, identifiers)), Some(json!(label)));
        }
        for (kind, identifiers) in [("company", json!({"person_name":"Ada Example"})),
            ("person", json!({"company_name":"Example Robotics"}))] {
            assert_eq!(documented_literal("shared.provider_name_query", schema, &subject(kind, "", identifiers)), None);
        }
        assert_eq!(documented_literal("shared.provider_name_query", schema, &subject("topic", "robotics startups", json!({}))), None);
        assert_eq!(documented_literal("shared.provider_name_query", schema, &subject("company", "Example Robotics",
            json!({"company_name":"Example Robotics","business_name":"Different Robotics"}))), None);
        assert_eq!(documented_literal("shared.provider_name_query", &fixture["before"], &subject("company", "Example Robotics", json!({}))), None);
    }
    #[test]
    fn reviewed_input_aliases_bind_only_declared_literal_criteria() {
        let schema = json!({"type":"string","$comment":"{\"apiosk_input_aliases_v1\":[\"service\",\"keyword\"]}"});
        let subject = |identifiers: Value| Subject {id:"s1".into(),label:"public records".into(),kind:"record".into(),
            identifiers:serde_json::from_value(identifiers).unwrap()};
        assert_eq!(documented_literal("provider.input.search",&schema,&subject(json!({"service":"cleaning services","query":"Find all requested records."}))),Some(json!("cleaning services")));
        assert_eq!(documented_literal("provider.input.search",&schema,&subject(json!({"service":"cleaning services","keyword":"cleaning services"}))),Some(json!("cleaning services")));
        for alias in ["service_category", "service_type", "search_term", "search_phrase"] {
            assert!(reviewed_input_alias(&schema, alias));
            assert_eq!(documented_literal("provider.input.search", &schema, &subject(json!({alias:"cleaning services"}))), Some(json!("cleaning services")));
        }
        assert!(!reviewed_input_alias(&schema, "cpv_code"));
        assert_eq!(documented_literal("provider.input.search", &schema, &subject(json!({"service":"cleaning services","service_category":"security"}))), None);
        for identifiers in [json!({"query":"Find all requested records."}),json!({"service":"cleaning services","keyword":"other work"}),json!({"service":" "})] {
            assert_eq!(documented_literal("provider.input.search",&schema,&subject(identifiers)),None);
        }
        for comment in ["{\"apiosk_input_aliases_v1\":[\"service\",\"service\"]}","{\"apiosk_input_aliases_v1\":[\"Service Name\"]}","{\"apiosk_input_aliases_v1\":[\"service\"],\"extra\":true}"] {
            assert_eq!(documented_literal("provider.input.search",&json!({"type":"string","$comment":comment}),&subject(json!({"service":"cleaning services"}))),None);
        }
    }
    #[test]
    fn fixed_base_currency_input_keeps_the_supplied_pair_direction() {
        assert_eq!(ecb_quote_for_pair("EUR/USD"), Some(json!("USD")));
        for pair in ["USD/EUR", "GBP/USD", "EUR/USD/JPY", "EUR/US", "EUR/usd", "USD"] {
            assert_eq!(ecb_quote_for_pair(pair), None, "{pair}");
        }
        let subject = Subject { id:"s1".into(), label:"EUR/USD".into(), kind:"currency_pair".into(),
            identifiers:serde_json::from_value(json!({"currency_pair":"EUR/USD"})).unwrap() };
        assert_eq!(literal("fx.ecb.quote", &subject), Some(json!("USD")));
        assert_eq!(literal("another_provider.quote", &subject), None);
    }
    #[test]
    fn normalized_identifiers_keep_complete_field_identity_and_reject_ambiguity() {
        let subject = |identifiers: Value| Subject {
            id: "s1".into(), label: "A literal subject".into(), kind: "topic".into(),
            identifiers: serde_json::from_value(identifiers).unwrap(),
        };
        assert_eq!(literal("fx.ecb.quote", &subject(json!({"fx_ecb_quote":"USD"}))), Some(json!("USD")));
        assert_eq!(literal("fx.ecb.quote", &subject(json!({"fx.ecb.quote":"USD"}))), Some(json!("USD")));
        assert_eq!(literal("fx.ecb.quote", &subject(json!({"fx.ecb.quote":"USD","fx_ecb_quote":"USD"}))), Some(json!("USD")));
        assert_eq!(literal("company.name", &subject(json!({"company.name":"Example B.V.","company_name":"Example B.V."}))), Some(json!("Example B.V.")));
        assert_eq!(literal("company.name", &subject(json!({"company.name":"Other B.V.","company_name":"Example B.V."}))), None);
        assert_eq!(literal("fx.ecb.quote", &subject(json!({"other_ecb_quote":"USD"}))), None);
        assert_eq!(literal("fx.ecb.quote", &subject(json!({"ecb_quote":"USD"}))), None);
        assert_eq!(literal("fx.ecb.quote", &subject(json!({"fx_ecb_quote":"USD","quote":"GBP"}))), None);
    }
    #[test]
    fn crunchbase_profile_links_keep_the_exact_entity_and_public_host() {
        for (field, path) in [("company.crunchbase_entity_id", "organization"),
            ("person.crunchbase_entity_id", "person"), ("funding.crunchbase_entity_id", "funding_round")] {
            assert_eq!(crunchbase_profile_id(field, &format!("https://www.crunchbase.com/{path}/exact-profile_123")), Some(json!("exact-profile_123")));
        }
        for url in ["Starbucks", "https://crunchbase.com.evil.test/organization/starbucks",
            "https://crunchbase.com/person/starbucks", "https://crunchbase.com/organization/starbucks/people",
            "https://user@crunchbase.com/organization/starbucks", "https://crunchbase.com/organization/starbucks?next=evil",
            "https://crunchbase.com/organization/star%62ucks", "http://crunchbase.com/organization/starbucks"] {
            assert_eq!(crunchbase_profile_id("company.crunchbase_entity_id", url), None, "{url}");
        }
    }
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
    #[test]
    fn address_lookup_keeps_supplied_city_and_postcode_without_inventing_or_duplicating_them() {
        let address = |label: &str, identifiers: Value| Subject {
            id: "s1".into(), label: label.into(), kind: "address".into(),
            identifiers: serde_json::from_value(identifiers).unwrap(),
        };
        for (label, identifiers, expected) in [
            ("Damrak 1", json!({"location":"Amsterdam"}), "Damrak 1, Amsterdam"),
            ("Damrak 1", json!({"city":"Amsterdam","location":"Amsterdam","postcode":"1012 LG"}), "Damrak 1, 1012 LG Amsterdam"),
            ("Damrak 1, 1012LG Amsterdam", json!({"city":"Amsterdam","postal_code":"1012 LG"}), "Damrak 1, 1012LG Amsterdam"),
            ("Damrak 1-A, Amsterdam", json!({}), "Damrak 1-A, Amsterdam"),
            ("Damrak 1", json!({}), "Damrak 1"),
            ("Amsterdamstraat 1", json!({"city":"Amsterdam"}), "Amsterdamstraat 1, Amsterdam"),
            ("Brinklaan 1", json!({"city":"Bussum","municipality":"Gooise Meren"}), "Brinklaan 1, Bussum"),
        ] {
            assert_eq!(literal("geo.input.q", &address(label, identifiers)), Some(json!(expected)));
        }
        assert_eq!(literal("geo.input.q", &address("Damrak 1", json!({"city":"Amsterdam","woonplaats":"Utrecht"}))), None);
    }

    #[test]
    fn repeated_address_components_remain_proven_without_accepting_other_filters() {
        let subject=Subject {id:"s1".into(),label:"Dam 1, Amsterdam".into(),kind:"building".into(),
            identifiers:serde_json::from_value(json!({"street_address":"Dam 1","city":"Amsterdam"})).unwrap()};
        let query=json!("Dam 1, Amsterdam");
        for (key,value) in [("street_address","Dam 1"),("city","Amsterdam")] {
            assert!(address_component_preserved("geo.input.q",&subject,key,value,&query));
        }
        for (field,key,value,bound) in [("web.query","city","Amsterdam",query.clone()),
            ("geo.input.q","country","Amsterdam",query.clone()),("geo.input.q","street_address","Dam 10",query.clone()),
            ("geo.input.q","city","Amsterdam",json!("Dam 1")),("geo.input.q","revenue","Dam 1",query)] {
            assert!(!address_component_preserved(field,&subject,key,value,&bound));
        }
    }

}
