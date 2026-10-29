//! The JSON Schema subset a fact's value is validated against.
//!
//! A fact only exists once its value has been checked against the schema its
//! type declares. That check is the difference between "the KVK number is
//! 30204054" and "whatever was at `$.resultaten[0].kvkNummer`, which today is
//! the string `null`". The release gate is blunt about it: a schema error
//! never yields a valid fact.
//!
//! WHY A SUBSET AND NOT A LIBRARY. Draft 2020-12 has `$ref`, `$dynamicRef`,
//! `unevaluatedProperties`, `if`/`then`/`else` and remote schema resolution.
//! None of that is wanted in a file a human reviews before money is spent
//! against it, and a resolver that fetches a remote `$ref` while validating an
//! upstream response is a request nobody planned. The eleven keywords below
//! cover every schema in `fact_types`, and adding a twelfth is a deliberate
//! change to this file rather than a property someone drops into a migration.
//!
//! WHY AN UNKNOWN KEYWORD IS AN ERROR. The usual JSON Schema rule is that an
//! unrecognised keyword is an annotation and is ignored. Here that rule is
//! actively dangerous: a schema written with `maxItems` would silently accept
//! any array, and the person who wrote it would have no way to notice. A
//! validator that ignores what it does not understand tells you a value is
//! valid when it has not checked. So an unknown keyword fails the SCHEMA, at
//! contract-validation time, on the desk of whoever wrote it.

use std::collections::BTreeSet;

use serde_json::Value;

/// The keywords this validator implements. Anything else in a schema is a
/// schema error.
const SUPPORTED: &[&str] = &[
    "type",
    "required",
    "properties",
    "items",
    "enum",
    "pattern",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "additionalProperties",
];

/// Keywords that carry no constraint and are skipped: documentation for
/// whoever reads the schema, and for the plan text shown to a buyer.
const ANNOTATIONS: &[&str] = &["title", "description", "examples", "default", "$comment"];

/// Why a value is not an instance of its type's schema.
//
// `PartialEq` but not `Eq`: two of these carry an `f64` bound read straight
// from the schema, and f64 has no total equality. Nothing compares these for
// ordering, so the weaker bound costs nothing.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SchemaError {
    /// The schema itself is wrong. Not the provider's fault and not the
    /// buyer's: a contract carrying one cannot reach `validated`.
    #[error("schema at `{at}` uses unsupported keyword `{keyword}`")]
    UnsupportedKeyword { at: String, keyword: String },
    #[error("schema at `{at}` is malformed: {detail}")]
    MalformedSchema { at: String, detail: String },
    #[error("`{at}` is {found}, expected {expected}")]
    WrongType {
        at: String,
        expected: String,
        found: String,
    },
    #[error("`{at}` is missing required property `{property}`")]
    MissingProperty { at: String, property: String },
    #[error("`{at}` has unexpected property `{property}`")]
    UnexpectedProperty { at: String, property: String },
    #[error("`{at}` does not match pattern `{pattern}`")]
    PatternMismatch { at: String, pattern: String },
    #[error("`{at}` is not one of the permitted values")]
    NotInEnum { at: String },
    #[error("`{at}` is {found}, below the minimum {limit}")]
    BelowMinimum { at: String, limit: f64, found: f64 },
    #[error("`{at}` is {found}, above the maximum {limit}")]
    AboveMaximum { at: String, limit: f64, found: f64 },
    #[error("`{at}` is {found} characters, shorter than {limit}")]
    TooShort {
        at: String,
        limit: u64,
        found: usize,
    },
    #[error("`{at}` is {found} characters, longer than {limit}")]
    TooLong {
        at: String,
        limit: u64,
        found: usize,
    },
}

/// Check `value` against `schema`. `Ok(())` is the only thing that makes a
/// fact real.
pub fn validate(schema: &Value, value: &Value) -> Result<(), SchemaError> {
    check(schema, value, "$")
}

/// Check that a schema is one this validator can actually enforce, without
/// needing a value. Run when a contract moves out of `draft`, so an
/// unenforceable schema is caught by its author and not by a job.
pub fn check_schema(schema: &Value) -> Result<(), SchemaError> {
    walk_schema(schema, "$")
}

fn walk_schema(schema: &Value, at: &str) -> Result<(), SchemaError> {
    let object = schema
        .as_object()
        .ok_or_else(|| SchemaError::MalformedSchema {
            at: at.to_string(),
            detail: "a schema must be an object".to_string(),
        })?;

    for key in object.keys() {
        if ANNOTATIONS.contains(&key.as_str()) {
            continue;
        }
        if !SUPPORTED.contains(&key.as_str()) {
            return Err(SchemaError::UnsupportedKeyword {
                at: at.to_string(),
                keyword: key.clone(),
            });
        }
    }

    if let Some(properties) = object.get("properties") {
        let map = properties
            .as_object()
            .ok_or_else(|| SchemaError::MalformedSchema {
                at: at.to_string(),
                detail: "`properties` must be an object".to_string(),
            })?;
        for (name, sub) in map {
            walk_schema(sub, &format!("{at}.{name}"))?;
        }
    }
    if let Some(items) = object.get("items") {
        walk_schema(items, &format!("{at}[*]"))?;
    }
    if let Some(required) = object.get("required") {
        required
            .as_array()
            .ok_or_else(|| SchemaError::MalformedSchema {
                at: at.to_string(),
                detail: "`required` must be an array".to_string(),
            })?;
    }
    if let Some(pattern) = object.get("pattern") {
        let text = pattern
            .as_str()
            .ok_or_else(|| SchemaError::MalformedSchema {
                at: at.to_string(),
                detail: "`pattern` must be a string".to_string(),
            })?;
        regex::Regex::new(text).map_err(|error| SchemaError::MalformedSchema {
            at: at.to_string(),
            detail: format!("`pattern` is not a regular expression: {error}"),
        })?;
    }
    Ok(())
}

fn check(schema: &Value, value: &Value, at: &str) -> Result<(), SchemaError> {
    let object = schema
        .as_object()
        .ok_or_else(|| SchemaError::MalformedSchema {
            at: at.to_string(),
            detail: "a schema must be an object".to_string(),
        })?;

    for key in object.keys() {
        if ANNOTATIONS.contains(&key.as_str()) {
            continue;
        }
        if !SUPPORTED.contains(&key.as_str()) {
            return Err(SchemaError::UnsupportedKeyword {
                at: at.to_string(),
                keyword: key.clone(),
            });
        }
    }

    if let Some(expected) = object.get("type").and_then(Value::as_str) {
        if !type_matches(expected, value) {
            return Err(SchemaError::WrongType {
                at: at.to_string(),
                expected: expected.to_string(),
                found: type_name(value).to_string(),
            });
        }
    }

    if let Some(permitted) = object.get("enum").and_then(Value::as_array) {
        if !permitted.contains(value) {
            return Err(SchemaError::NotInEnum { at: at.to_string() });
        }
    }

    if let Some(text) = value.as_str() {
        if let Some(pattern) = object.get("pattern").and_then(Value::as_str) {
            let regex =
                regex::Regex::new(pattern).map_err(|error| SchemaError::MalformedSchema {
                    at: at.to_string(),
                    detail: format!("`pattern` is not a regular expression: {error}"),
                })?;
            if !regex.is_match(text) {
                return Err(SchemaError::PatternMismatch {
                    at: at.to_string(),
                    pattern: pattern.to_string(),
                });
            }
        }
        // Characters, not bytes: a name with an é in it is not longer for it.
        let length = text.chars().count();
        if let Some(limit) = object.get("minLength").and_then(Value::as_u64) {
            if (length as u64) < limit {
                return Err(SchemaError::TooShort {
                    at: at.to_string(),
                    limit,
                    found: length,
                });
            }
        }
        if let Some(limit) = object.get("maxLength").and_then(Value::as_u64) {
            if (length as u64) > limit {
                return Err(SchemaError::TooLong {
                    at: at.to_string(),
                    limit,
                    found: length,
                });
            }
        }
    }

    if let Some(number) = value.as_f64() {
        if let Some(limit) = object.get("minimum").and_then(Value::as_f64) {
            if number < limit {
                return Err(SchemaError::BelowMinimum {
                    at: at.to_string(),
                    limit,
                    found: number,
                });
            }
        }
        if let Some(limit) = object.get("maximum").and_then(Value::as_f64) {
            if number > limit {
                return Err(SchemaError::AboveMaximum {
                    at: at.to_string(),
                    limit,
                    found: number,
                });
            }
        }
    }

    if let Some(map) = value.as_object() {
        if let Some(required) = object.get("required").and_then(Value::as_array) {
            for entry in required {
                let Some(name) = entry.as_str() else {
                    return Err(SchemaError::MalformedSchema {
                        at: at.to_string(),
                        detail: "`required` must hold strings".to_string(),
                    });
                };
                // A property present but null is not a property. A provider
                // that returns `{"kvkNummer": null}` has told us it does not
                // know, and reading that back as a known fact is the exact
                // failure this whole module exists to prevent.
                match map.get(name) {
                    None | Some(Value::Null) => {
                        return Err(SchemaError::MissingProperty {
                            at: at.to_string(),
                            property: name.to_string(),
                        })
                    }
                    Some(_) => {}
                }
            }
        }
        if let Some(properties) = object.get("properties").and_then(Value::as_object) {
            for (name, sub) in properties {
                if let Some(found) = map.get(name) {
                    // An absent optional property is fine; a present one is
                    // checked. A null in an OPTIONAL slot is read as absent,
                    // for the same reason as above.
                    if found.is_null() {
                        continue;
                    }
                    check(sub, found, &format!("{at}.{name}"))?;
                }
            }
            if object
                .get("additionalProperties")
                .and_then(Value::as_bool)
                .is_some_and(|allowed| !allowed)
            {
                let declared: BTreeSet<&str> = properties.keys().map(String::as_str).collect();
                for name in map.keys() {
                    if !declared.contains(name.as_str()) {
                        return Err(SchemaError::UnexpectedProperty {
                            at: at.to_string(),
                            property: name.clone(),
                        });
                    }
                }
            }
        }
    }

    if let (Some(items), Some(schema_for_item)) = (value.as_array(), object.get("items")) {
        for (index, item) in items.iter().enumerate() {
            check(schema_for_item, item, &format!("{at}[{index}]"))?;
        }
    }

    Ok(())
}

fn type_matches(expected: &str, value: &Value) -> bool {
    match expected {
        "string" => value.is_string(),
        // JSON has one number type. A schema that means "whole number" says
        // `integer`, and that is checked as such rather than accepting 3.5.
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "null" => value.is_null(),
        _ => false,
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_kvk_number_is_eight_digits_and_nothing_else() {
        let schema = json!({ "type": "string", "pattern": "^[0-9]{8}$" });
        assert!(validate(&schema, &json!("30204054")).is_ok());
        assert!(matches!(
            validate(&schema, &json!("3020405")),
            Err(SchemaError::PatternMismatch { .. })
        ));
        assert!(matches!(
            validate(&schema, &json!("Mollie B.V.")),
            Err(SchemaError::PatternMismatch { .. })
        ));
        assert!(matches!(
            validate(&schema, &json!(30204054)),
            Err(SchemaError::WrongType { .. })
        ));
    }

    #[test]
    fn a_null_never_satisfies_a_required_property() {
        let schema = json!({
            "type": "object",
            "required": ["legal_name"],
            "properties": { "legal_name": { "type": "string" } }
        });
        assert!(validate(&schema, &json!({ "legal_name": "Mollie B.V." })).is_ok());
        assert!(matches!(
            validate(&schema, &json!({ "legal_name": null })),
            Err(SchemaError::MissingProperty { .. })
        ));
        assert!(matches!(
            validate(&schema, &json!({})),
            Err(SchemaError::MissingProperty { .. })
        ));
    }

    #[test]
    fn a_null_in_an_optional_slot_is_read_as_absent() {
        let schema = json!({
            "type": "object",
            "required": ["legal_name"],
            "properties": {
                "legal_name": { "type": "string" },
                "employee_count": { "type": "number", "minimum": 0 }
            }
        });
        assert!(validate(
            &schema,
            &json!({ "legal_name": "Mollie B.V.", "employee_count": null })
        )
        .is_ok());
    }

    #[test]
    fn nested_properties_are_checked() {
        let schema = json!({
            "type": "object",
            "properties": {
                "address": {
                    "type": "object",
                    "required": ["postcode"],
                    "properties": { "postcode": { "type": "string", "pattern": "^[0-9]{4}\\s?[A-Za-z]{2}$" } }
                }
            }
        });
        assert!(validate(&schema, &json!({ "address": { "postcode": "1017 CE" } })).is_ok());
        let error = validate(&schema, &json!({ "address": { "postcode": "nope" } })).unwrap_err();
        assert!(
            matches!(error, SchemaError::PatternMismatch { ref at, .. } if at == "$.address.postcode")
        );
    }

    #[test]
    fn every_array_element_is_checked() {
        let schema = json!({ "type": "array", "items": { "type": "string" } });
        assert!(validate(&schema, &json!(["6201", "6202"])).is_ok());
        let error = validate(&schema, &json!(["6201", 6202])).unwrap_err();
        assert!(matches!(error, SchemaError::WrongType { ref at, .. } if at == "$[1]"));
    }

    #[test]
    fn integer_rejects_a_fraction_where_number_accepts_it() {
        assert!(validate(&json!({ "type": "number" }), &json!(3.5)).is_ok());
        assert!(matches!(
            validate(&json!({ "type": "integer" }), &json!(3.5)),
            Err(SchemaError::WrongType { .. })
        ));
    }

    #[test]
    fn bounds_are_inclusive() {
        let schema = json!({ "type": "number", "minimum": 0, "maximum": 100 });
        assert!(validate(&schema, &json!(0)).is_ok());
        assert!(validate(&schema, &json!(100)).is_ok());
        assert!(matches!(
            validate(&schema, &json!(-1)),
            Err(SchemaError::BelowMinimum { .. })
        ));
        assert!(matches!(
            validate(&schema, &json!(101)),
            Err(SchemaError::AboveMaximum { .. })
        ));
    }

    #[test]
    fn length_counts_characters_not_bytes() {
        let schema = json!({ "type": "string", "minLength": 3, "maxLength": 4 });
        assert!(validate(&schema, &json!("café")).is_ok());
        assert!(matches!(
            validate(&schema, &json!("ab")),
            Err(SchemaError::TooShort { .. })
        ));
        assert!(matches!(
            validate(&schema, &json!("abcde")),
            Err(SchemaError::TooLong { .. })
        ));
    }

    #[test]
    fn an_unknown_keyword_fails_the_schema_instead_of_being_ignored() {
        // The whole point: `maxItems` is real JSON Schema and is NOT
        // implemented here, so a schema using it must not quietly pass.
        let schema = json!({ "type": "array", "maxItems": 2 });
        assert!(matches!(
            validate(&schema, &json!([1, 2, 3])),
            Err(SchemaError::UnsupportedKeyword { ref keyword, .. }) if keyword == "maxItems"
        ));
        assert!(matches!(
            check_schema(&schema),
            Err(SchemaError::UnsupportedKeyword { .. })
        ));
    }

    #[test]
    fn an_unknown_keyword_is_found_nested_too() {
        let schema = json!({
            "type": "object",
            "properties": { "rows": { "type": "array", "items": { "$ref": "#/x" } } }
        });
        assert!(matches!(
            check_schema(&schema),
            Err(SchemaError::UnsupportedKeyword { ref at, ref keyword })
                if at == "$.rows[*]" && keyword == "$ref"
        ));
    }

    #[test]
    fn documentation_keywords_are_allowed_and_constrain_nothing() {
        let schema = json!({
            "title": "KVK number",
            "description": "Eight digits.",
            "type": "string",
            "pattern": "^[0-9]{8}$"
        });
        assert!(check_schema(&schema).is_ok());
        assert!(validate(&schema, &json!("30204054")).is_ok());
    }

    #[test]
    fn a_broken_pattern_is_caught_before_a_value_is_ever_seen() {
        let schema = json!({ "type": "string", "pattern": "([0-9]" });
        assert!(matches!(
            check_schema(&schema),
            Err(SchemaError::MalformedSchema { .. })
        ));
    }

    #[test]
    fn additional_properties_false_is_enforced() {
        let schema = json!({
            "type": "object",
            "properties": { "a": { "type": "string" } },
            "additionalProperties": false
        });
        assert!(validate(&schema, &json!({ "a": "x" })).is_ok());
        assert!(matches!(
            validate(&schema, &json!({ "a": "x", "b": 1 })),
            Err(SchemaError::UnexpectedProperty { .. })
        ));
    }

    #[test]
    fn every_seeded_fact_type_schema_is_enforceable() {
        // These are the seven schemas migration 112 seeds. If one of them uses
        // a keyword this validator does not implement, the migration is wrong
        // and this test says so before it ever reaches a database.
        for schema in [
            json!({"type":"string","minLength":1,"maxLength":500}),
            json!({"type":"string","pattern":"^[0-9]{8}$"}),
            json!({"type":"object","required":["legal_name"],"properties":{"legal_name":{"type":"string","minLength":1},"trade_names":{"type":"array","items":{"type":"string"}},"legal_form":{"type":"string"},"status":{"type":"string"},"founded_on":{"type":"string"},"sbi_codes":{"type":"array","items":{"type":"string"}},"employee_count":{"type":"number","minimum":0}}}),
            json!({"type":"object","required":["financial_year"],"properties":{"financial_year":{"type":"number"},"filed_on":{"type":"string"},"currency":{"type":"string","pattern":"^[A-Z]{3}$"},"equity":{"type":"number"},"total_assets":{"type":"number"},"revenue":{"type":"number"},"result_after_tax":{"type":"number"},"document_url":{"type":"string"}}}),
            json!({"type":"object","required":["postcode"],"properties":{"street":{"type":"string"},"house_number":{"type":"string"},"postcode":{"type":"string","pattern":"^[0-9]{4}\\s?[A-Za-z]{2}$"},"city":{"type":"string"},"country":{"type":"string","pattern":"^[A-Z]{2}$"}}}),
            json!({"type":"string","pattern":"^[0-9]{16}$"}),
            json!({"type":"object","properties":{"purpose":{"type":"array","items":{"type":"string"}},"built_year":{"type":"number"},"surface_m2":{"type":"number","minimum":0},"status":{"type":"string"}}}),
        ] {
            check_schema(&schema).expect("a seeded fact type schema must be enforceable");
        }
    }
}
