//! Refine only the reviewed PDOK address lookup from complete literal input.
//! The original fact stays unchanged for source-row identity checks. Unsupported
//! address forms retain the normal lookup and its ambiguity/completeness guard.
use crate::catalog::Service;
use serde_json::{Map, Value};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Address {
    pub(crate) street: String,
    pub(crate) number: u32,
    pub(crate) city: Option<String>,
    pub(crate) postcode: Option<String>,
}

/// A deliberately small grammar: alphabetic street, one unsuffixed house
/// integer, then an explicit locality and/or Dutch postcode. A locality is one
/// lexical name (including apostrophes/hyphens), so prose following an address
/// cannot silently become a locality. Multiword cities and suffixes need the
/// ordinary search/selection flow until their syntax is supported explicitly.
pub(crate) fn parse_literal(query: &str) -> Option<Address> {
    if query.len() > 300 || query.chars().any(char::is_control) {
        return None;
    }
    let parts: Vec<_> = query.trim().split(',').collect();
    if parts.len() > 2 {
        return None;
    }
    let left: Vec<_> = parts[0].split_whitespace().collect();
    if parts.len() == 2 {
        let (number, street) = left.split_last()?;
        return address(
            street,
            number,
            &parts[1].split_whitespace().collect::<Vec<_>>(),
        );
    }
    // Require exactly one valid boundary. Street digits, suffixes and extra
    // numeric components cannot be reinterpreted as a different address.
    let mut candidates = (1..left.len())
        .filter_map(|index| address(&left[..index], left[index], &left[index + 1..]));
    let parsed = candidates.next()?;
    candidates.next().is_none().then_some(parsed)
}

fn address(street: &[&str], number: &str, tail: &[&str]) -> Option<Address> {
    let street = street.join(" ");
    if !name(&street, true)
        || number.starts_with('0')
        || number.len() > 5
        || !number.bytes().all(|ch| ch.is_ascii_digit())
    {
        return None;
    }
    let number = number.parse::<u32>().ok().filter(|n| *n > 0)?;
    let (postcode, remaining) = match tail {
        [code, rest @ ..] if valid_postcode(code) => (Some(code.to_ascii_uppercase()), rest),
        [digits, letters, rest @ ..]
            if digits.len() == 4
                && letters.len() == 2
                && valid_postcode(&format!("{digits}{letters}")) =>
        {
            (
                Some(format!("{digits}{letters}").to_ascii_uppercase()),
                rest,
            )
        }
        _ => (None, tail),
    };
    let city = match remaining {
        [city] if name(city, false) => Some((*city).to_owned()),
        [] if postcode.is_some() => None,
        _ => return None,
    };
    Some(Address {
        street,
        number,
        city,
        postcode,
    })
}

fn name(value: &str, street: bool) -> bool {
    value.chars().filter(|ch| ch.is_alphabetic()).count() >= 3
        && value.chars().all(|ch| {
            ch.is_alphabetic()
                || matches!(ch, '\'' | '’' | '-')
                || (street && matches!(ch, ' ' | '.'))
        })
}

fn valid_postcode(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 6
        && matches!(bytes[0], b'1'..=b'9')
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4..].iter().all(u8::is_ascii_alphabetic)
}

fn quoted(value: &str) -> String {
    let mut result = String::from("\"");
    for ch in value.chars() {
        if matches!(
            ch,
            '+' | '-'
                | '&'
                | '|'
                | '!'
                | '('
                | ')'
                | '{'
                | '}'
                | '['
                | ']'
                | '^'
                | '"'
                | '~'
                | '*'
                | '?'
                | ':'
                | '\\'
                | '/'
        ) {
            result.push('\\');
        }
        result.push(ch);
    }
    result.push('"');
    result
}

fn filter(address: &Address) -> String {
    let mut filter = format!(
        "type:adres AND straatnaam:{} AND huisnummer:{}",
        quoted(&address.street),
        address.number
    );
    if let Some(city) = &address.city {
        filter.push_str(&format!(" AND woonplaatsnaam:{}", quoted(city)));
    }
    if let Some(postcode) = &address.postcode {
        filter.push_str(&format!(" AND postcode:{}", quoted(postcode)));
    }
    // The number alone also matches 1A and 1-1. Absence is an explicit part of
    // an unsuffixed address; never guess which suffixed unit the person meant.
    filter.push_str(" AND -huisletter:[* TO *] AND -huisnummertoevoeging:[* TO *]");
    filter
}

pub(crate) fn refine(service: &Service, inputs: &mut Map<String, Value>) {
    if service.api_slug != "pdok"
        || service.method != "GET"
        || service.path != "/bzk/locatieserver/search/v3_1/free"
        || service.contract.capability_slug != "location.property-search"
        || service.fixed_params["fq"] != "type:adres"
        || inputs.get("fq").and_then(Value::as_str) != Some("type:adres")
        || !service.contract.requires.iter().any(|required| {
            required.fact_type == "geo.input.q"
                && required.subject_role == "subject"
                && !required.optional
        })
        || !service
            .contract
            .input_map
            .get("geo.input.q")
            .is_some_and(|binding| {
                binding.location.as_deref() == Some("query")
                    && binding.name == "q"
                    && binding.wrap.is_none()
            })
        || service
            .contract
            .input_map
            .values()
            .any(|binding| binding.name == "fq")
    {
        return;
    }
    let Some(address) = inputs
        .get("q")
        .and_then(Value::as_str)
        .and_then(parse_literal)
    else {
        return;
    };
    inputs.insert("q".into(), Value::String("*:*".into()));
    inputs.insert("fq".into(), Value::String(filter(&address)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        catalog::{Catalog, Snapshot},
        state::{Fact, Task},
    };
    use chrono::{Duration, Utc};
    use serde_json::json;
    use uuid::Uuid;

    fn catalog() -> Catalog {
        let snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/fixtures/workflows/catalog.json")).unwrap();
        Catalog::build(snapshot)
    }

    #[test]
    fn exact_supplied_address_refines_upstream_inputs_but_preserves_literal_fact() {
        let catalog = catalog();
        let service = catalog
            .services
            .iter()
            .find(|s| s.contract.capability_slug == "location.property-search")
            .unwrap();
        for literal in ["Damrak 1, Amsterdam", "Damrak 1 Amsterdam"] {
            let entity = Uuid::new_v4();
            let task = Task {
                facts: vec![Fact {
                    entity_ref: entity,
                    semantic_type: "geo.input.q".into(),
                    value: json!(literal),
                    provenance: json!({"kind":"user"}),
                    observed_at: Utc::now(),
                    expires_at: Utc::now() + Duration::minutes(5),
                    verification: "explicit".into(),
                }],
                ..Task::default()
            };
            assert_eq!(
                service.inputs(&task, entity, &catalog.vocabulary).unwrap(),
                json!({
                    "q":"*:*", "rows":5,
                    "fq":"type:adres AND straatnaam:\"Damrak\" AND huisnummer:1 AND woonplaatsnaam:\"Amsterdam\" AND -huisletter:[* TO *] AND -huisnummertoevoeging:[* TO *]"
                })
            );
            assert_eq!(task.facts[0].value, literal);
        }
    }

    #[test]
    fn explicit_postcodes_and_other_literal_values_are_preserved() {
        for input in [
            "Damrak 1, 1012 LG Amsterdam",
            "Damrak 1 1012LG Amsterdam",
            "Damrak 1, 1012lg",
        ] {
            let parsed = parse_literal(input).unwrap();
            assert_eq!(parsed.postcode.as_deref(), Some("1012LG"));
            assert!(filter(&parsed).contains("AND postcode:\"1012LG\""));
        }
        let parsed = parse_literal("Lange Voorhout 12, Utrecht").unwrap();
        assert_eq!(parsed.street, "Lange Voorhout");
        assert_eq!(parsed.number, 12);
        assert_eq!(parsed.city.as_deref(), Some("Utrecht"));
        assert!(!filter(&parsed).contains("Amsterdam"));
    }

    #[test]
    fn incomplete_ambiguous_suffixed_and_prose_inputs_keep_original_lookup() {
        let catalog = catalog();
        let service = catalog
            .services
            .iter()
            .find(|s| s.contract.capability_slug == "location.property-search")
            .unwrap();
        for query in [
            "Damrak",
            "Damrak 1",
            "Damrak Amsterdam",
            "Damrak 1A, Amsterdam",
            "Damrak 1 A Amsterdam",
            "Damrak 1-2, Amsterdam",
            "Damrak 1 bis Amsterdam",
            "Damrak 1 Amsterdam in 2020",
            "Damrak 1 Amsterdam as JSON",
            "Damrak 1 Amsterdam owner details",
            "Damrak 1, Amsterdam & ownership",
            "Damrak 1, Amsterdam + energy label",
            "Damrak 1, Den Haag",
            "Damrak 1, Amsterdam, Nederland",
            "Damrak 1 2 Amsterdam",
            "Damrak 1, 1012 ZZ Amsterdam extra",
            "Damrak 01, Amsterdam",
            "Damrak 1 A",
        ] {
            assert!(parse_literal(query).is_none(), "{query}");
            let original = json!({"q":query,"fq":"type:adres","rows":5});
            let mut inputs = original.as_object().unwrap().clone();
            refine(service, &mut inputs);
            assert_eq!(Value::Object(inputs), original, "{query}");
        }
    }

    #[test]
    fn query_syntax_is_never_promoted_from_user_text() {
        for query in [
            r#"Damrak 1, Amsterdam\" OR *:*"#,
            "Damrak 1, Amsterdam OR Utrecht",
            "Damrak 1, Amsterdam:Utrecht",
            "Damrak 1, Amsterdam*",
            "Damrak 1, (Amsterdam)",
            "Damrak 1, Amsterdam\nOR Utrecht",
        ] {
            assert!(parse_literal(query).is_none(), "{query}");
        }
        assert_eq!(quoted(r#"A\" OR *:* \\ B"#), r#""A\\\" OR \*\:\* \\\\ B""#);
        let parsed = parse_literal("Sint-Jansstraat 12, 's-Hertogenbosch").unwrap();
        assert!(filter(&parsed).contains(r#"straatnaam:"Sint\-Jansstraat""#));
        assert!(filter(&parsed).contains(r#"woonplaatsnaam:"'s\-Hertogenbosch""#));
    }

    #[test]
    fn only_the_reviewed_pdok_address_binding_is_refined() {
        let catalog = catalog();
        let base = catalog
            .services
            .iter()
            .find(|s| s.contract.capability_slug == "location.property-search")
            .unwrap();
        for mode in 0..9 {
            let mut service = base.clone();
            match mode {
                0 => service.api_slug = "other".into(),
                1 => service.path = "/other".into(),
                2 => service.method = "POST".into(),
                3 => service.contract.capability_slug = "geo.search".into(),
                4 => {
                    service
                        .contract
                        .input_map
                        .get_mut("geo.input.q")
                        .unwrap()
                        .name = "search".into()
                }
                5 => {
                    service
                        .contract
                        .input_map
                        .get_mut("geo.input.q")
                        .unwrap()
                        .location = Some("body".into())
                }
                6 => service.fixed_params["fq"] = json!("type:perceel"),
                7 => service.contract.requires[0].optional = true,
                8 => {
                    service.contract.input_map.insert(
                        "geo.input.fq".into(),
                        catalog_core::facts::contract::InputBinding {
                            location: Some("query".into()),
                            name: "fq".into(),
                            wrap: None,
                            default: None,
                        },
                    );
                }
                _ => unreachable!(),
            }
            let original = json!({"q":"Damrak 1, Amsterdam","fq":"type:adres","rows":5});
            let mut inputs = original.as_object().unwrap().clone();
            refine(&service, &mut inputs);
            assert_eq!(Value::Object(inputs), original, "mode {mode}");
        }
        let original =
            json!({"q":"Damrak 1, Amsterdam","fq":"type:adres AND provinciecode:PV27","rows":5});
        let mut inputs = original.as_object().unwrap().clone();
        refine(base, &mut inputs);
        assert_eq!(Value::Object(inputs), original);
    }
}
