//! A website-search query bound to the name and identifier in one source record.
//! This does not infer a domain or assert ownership of any returned website.
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fields {
    name: String,
    identifier: String,
    registry: String,
    pattern: String,
}

pub(super) fn query(body: &Value, key: &str) -> Option<Value> {
    if key.len() > 1000 {
        return None;
    }
    let fields: Fields = serde_json::from_str(key).ok()?;
    if !fields.name.starts_with('/')
        || !fields.identifier.starts_with('/')
        || fields.registry.is_empty()
        || fields.registry.len() > 50
        || !fields
            .registry
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == ' ')
        || fields.pattern.len() > 150
        || !fields.pattern.starts_with('^')
        || !fields.pattern.ends_with('$')
    {
        return None;
    }
    let name = body.pointer(&fields.name)?.as_str()?;
    let identifier = body.pointer(&fields.identifier)?.as_str()?;
    if name.trim().is_empty()
        || name.chars().count() > 250
        || name.chars().any(char::is_control)
        || identifier.is_empty()
        || identifier.len() > 64
        || identifier.chars().any(char::is_control)
        || !Regex::new(&fields.pattern).ok()?.is_match(identifier)
    {
        return None;
    }
    let literal = serde_json::to_string(name).ok()?;
    Some(Value::String(format!(
        "{literal} {} {identifier} official website products services",
        fields.registry
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn key() -> String {
        json!({"name":"/naam","identifier":"/kvkNummer","registry":"KVK","pattern":"^[0-9]{8}$"})
            .to_string()
    }

    #[test]
    fn uses_exact_name_and_identifier_from_the_same_source_record() {
        for (name, id) in [("Mollie B.V.", "30204462"), ("Another Company", "00123456")] {
            let body = json!({"naam":name,"kvkNummer":id,"handelsnamen":[{"naam":"Unrelated"}]});
            let original = body.clone();
            assert_eq!(
                query(&body, &key()),
                Some(json!(format!(
                    "\"{name}\" KVK {id} official website products services"
                )))
            );
            assert_eq!(body, original);
        }
    }

    #[test]
    fn missing_malformed_or_multiple_values_never_create_a_query() {
        for body in [
            json!({}),
            json!({"naam":"Mollie B.V."}),
            json!({"kvkNummer":"30204462"}),
            json!({"naam":" ","kvkNummer":"30204462"}),
            json!({"naam":"A","kvkNummer":30204462}),
            json!({"naam":"A","kvkNummer":"123"}),
            json!({"naam":"A","kvkNummer":["30204462","00123456"]}),
            json!({"naam":["A","B"],"kvkNummer":"30204462"}),
            json!({"naam":"A\nB","kvkNummer":"30204462"}),
            json!({"naam":"A","kvkNummer":"30204462\n"}),
        ] {
            assert_eq!(query(&body, &key()), None, "{body}");
        }
    }

    #[test]
    fn reviewed_field_configuration_is_generic_and_quotes_source_names() {
        let config = json!({"name":"/selected/name","identifier":"/selected/id","registry":"Registry","pattern":"^GB[0-9]{4}$"});
        assert_eq!(
            query(
                &json!({"selected":{"name":"The \"Company\"","id":"GB0012"}}),
                &config.to_string()
            ),
            Some(json!(
                "\"The \\\"Company\\\"\" Registry GB0012 official website products services"
            ))
        );
        for bad in [
            json!({}),
            json!({"name":"naam","identifier":"/kvkNummer","registry":"KVK","pattern":"^[0-9]{8}$"}),
            json!({"name":"/naam","identifier":"/kvkNummer","registry":"KVK","pattern":"[0-9]{8}"}),
            json!({"name":"/naam","identifier":"/kvkNummer","registry":"KVK\nother","pattern":"^[0-9]{8}$"}),
        ] {
            assert_eq!(
                query(
                    &json!({"naam":"A","kvkNummer":"30204462"}),
                    &bad.to_string()
                ),
                None
            );
        }
    }
    #[test]
    fn migration_adapter_extracts_a_bound_query_from_the_real_saved_profile() {
        use crate::facts::contract::{ExecutionContract, FactType, Vocabulary};
        let migration = include_str!(
            "../../../../supabase/migrations/20261004182009_kvk_registered_website_bridge.sql"
        );
        let extract: Value =
            serde_json::from_str(migration.split("$query_output$").nth(1).unwrap()).unwrap();
        let contract: ExecutionContract = serde_json::from_value(json!({
            "capability_slug":"company.registry-profile", "version":9,"cardinality":"one",
            "identifies":"company_registry.kvknummer", "output_map":{"base":"$","produces":{
                "company_registry.kvknummer":{"path":"$.kvkNummer"}, "web.company_website_query":extract
            }}
        })).unwrap();
        let types: Vocabulary = [
            (
                "company_registry.kvknummer",
                json!({"type":"string","pattern":"^[0-9]{8}$"}),
            ),
            (
                "web.company_website_query",
                json!({"type":"string","minLength":1,"maxLength":1000}),
            ),
        ]
        .into_iter()
        .map(|(slug, value_schema)| {
            (
                slug.to_owned(),
                FactType {
                    slug: slug.to_owned(),
                    value_schema,
                },
            )
        })
        .collect();
        contract.check(&types).unwrap();
        let body: Value = serde_json::from_str(include_str!(
            "../../fixtures/contracts/payloads/kvk-basisprofiel-mollie.json"
        ))
        .unwrap();
        let extracted = contract.extract(&body, &types).unwrap();
        let subject = extracted.single().unwrap();
        assert_eq!(subject.identity, Some(json!("30204462")));
        assert_eq!(
            subject.fact("web.company_website_query").unwrap().value,
            json!("\"Mollie B.V.\" KVK 30204462 official website products services")
        );
        // The bridge works without any website field and preserves exact registry
        // identity. A malformed identity never yields a downstream query.
        let mut malformed = body;
        malformed["kvkNummer"] = json!("3020446");
        assert!(contract
            .extract(&malformed, &types)
            .unwrap()
            .subjects
            .is_empty());
    }
}
