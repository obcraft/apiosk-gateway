//! Named extractors, for response shapes a path cannot address.
//!
//! The path dialect deliberately has no filters (see [`super::path`]), and
//! almost every response is fine with that. Some are not. A recurring pattern
//! across providers is the KEY/VALUE TREE: instead of naming its fields, a
//! response carries a nested list of `{key, value}` pairs.
//!
//! ```json
//! { "opendataFields": [
//!     { "key": "FinancialYear", "value": "2025" },
//!     { "key": "BalanceSheet", "opendataFields": [
//!         { "key": "AssetsTitle", "opendataFields": [
//!             { "key": "Assets", "value": "682987" } ] } ] } ] }
//! ```
//!
//! `Assets` sits at a depth that varies with the document, under grouping keys
//! that vary with it too. No fixed path reaches it, and the construct that
//! would — a filter on `key` — is the one thing the dialect refuses to grow,
//! because a filter language pointed at an upstream body is a liability that
//! grows with every contract someone writes in it.
//!
//! So the escape hatch is not a more powerful string. It is a NAMED FUNCTION
//! IN THIS FILE: reviewed, tested against stored real payloads, and listed in
//! [`KNOWN`]. A contract may name one; it cannot invent one.
//!
//! ---------------------------------------------------------------------------
//! Named for the shape, never for the provider
//! ---------------------------------------------------------------------------
//!
//! This adapter was first called `kvk.opendata-fields` and hard-coded the child
//! array's name. That was wrong twice over: it put a vendor's name in the one
//! place the engine is supposed to be vendor-neutral, and it meant the next
//! provider shipping the identical shape under `fields` or `entries` would need
//! a second copy of the same function.
//!
//! It is `tree.key-value` now, and it walks whatever the container is called.
//! The only names it knows are the CONVENTIONAL pair names — `key`/`value` and
//! their common synonyms — which describe the shape, not who serves it.

use serde_json::Value;
#[path = "uk_supplier.rs"]
mod uk_supplier;

/// Every adapter a contract may name. A contract naming anything else fails
/// its check and never reaches `validated`.
pub const KNOWN: &[&str] = &[
    "tree.key-value",
    "record.country-identifier",
    "point.rd-extent",
    "roster.active-director",
    "search.uk-vat-candidates",
];

/// Field names that carry the NAME half of a pair. Shape vocabulary, not a
/// vendor list: a provider is free to say `name` where another says `key`.
const KEY_FIELDS: &[&str] = &["key", "name", "label", "field"];

/// Field names that carry the VALUE half.
const VALUE_FIELDS: &[&str] = &["value", "val", "amount", "content"];

/// Run a named adapter. `None` means the key is not in this body — a normal
/// outcome, not a failure: a small filing has no income statement, and that is
/// data, not an error.
pub fn run(adapter: &str, key: &str, body: &Value) -> Option<Value> {
    match adapter {
        "tree.key-value" => key_value_tree(body, key),
        "record.country-identifier" => country_identifier(body, key),
        "point.rd-extent" => rd_extent(body, key),
        "roster.active-director" => uk_supplier::directors(body, key),
        "search.uk-vat-candidates" => uk_supplier::vat_candidates(body, key),
        _ => None,
    }
}

/// A 100-metre map extent around a source's RD point. A malformed or out-of-
/// bounds point never turns into a location for a dependent paid call.
fn rd_extent(body: &Value, key: &str) -> Option<Value> {
    // JSON Pointer supports a preserved source record without guessing coordinates.
    let value = if key.starts_with('/') {
        body.pointer(key)?
    } else {
        body.get(key)?
    };
    let text = value.as_str()?.strip_prefix("POINT(")?.strip_suffix(')')?;
    let numbers: Vec<f64> = text
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    if numbers.len() != 2
        || !numbers.iter().all(|n| n.is_finite())
        || !(0.0..=300000.0).contains(&numbers[0])
        || !(280000.0..=630000.0).contains(&numbers[1])
    {
        return None;
    }
    Some(Value::String(format!(
        "{},{},{},{}",
        numbers[0] - 50.0,
        numbers[1] - 50.0,
        numbers[0] + 50.0,
        numbers[1] + 50.0
    )))
}

/// Extract a national identifier only for its declared register country.
/// Country and identifier remain in the same record; numeric IDs are never
/// cast because that would discard leading zeroes.
fn country_identifier(body: &Value, country: &str) -> Option<Value> {
    if country.len() != 2 || !country.bytes().all(|b| b.is_ascii_uppercase()) {
        return None;
    }
    let actual = body.get("pays").or_else(|| body.get("country"))?.as_str()?;
    if actual != country {
        return None;
    }
    let id = body
        .get("id_national")
        .or_else(|| body.get("registration_number"))?
        .as_str()?;
    if id.is_empty() {
        return None;
    }
    Some(Value::String(id.to_owned()))
}

/// Depth-first search of a key/value tree for one key's value.
///
/// Returns the FIRST match in document order. These trees are nested
/// taxonomies in which a leaf key appears once — `Assets` lives under
/// `AssetsTitle` and nowhere else — so first-match is not a choice between
/// candidates.
///
/// Recurses through EVERY nested array and object rather than one named child
/// property, which is what makes it independent of what the container is
/// called. Grouping entries carry no value field and are skipped, so asking
/// for one yields `None` rather than an empty string.
fn key_value_tree(node: &Value, wanted: &str) -> Option<Value> {
    match node {
        Value::Array(items) => items.iter().find_map(|item| key_value_tree(item, wanted)),
        Value::Object(map) => {
            let names = KEY_FIELDS
                .iter()
                .filter_map(|field| map.get(*field))
                .filter_map(Value::as_str);
            if names.into_iter().any(|name| name == wanted) {
                if let Some(value) = VALUE_FIELDS.iter().find_map(|field| map.get(*field)) {
                    // A grouping entry can carry a `value` that is itself the
                    // nested list. Only a scalar answers the question.
                    if !value.is_array() && !value.is_object() {
                        return Some(value.clone());
                    }
                }
            }
            // Not this node, or this node groups rather than carries. Either
            // way the answer may be below it, under any property name.
            map.values().find_map(|child| key_value_tree(child, wanted))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rd_extent_keeps_the_exact_selected_record_and_rejects_invalid_points() {
        assert_eq!(
            run(
                "point.rd-extent",
                "centroide_rd",
                &json!({"centroide_rd":"POINT(121394 487383)"})
            ),
            Some(json!("121344,487333,121444,487433"))
        );
        assert_eq!(
            run(
                "point.rd-extent",
                "/source_record/centroide_rd",
                &json!({"source_record":{"centroide_rd":"POINT(121394 487383)"}})
            ),
            Some(json!("121344,487333,121444,487433"))
        );
        for point in [
            "POINT(NaN 487383)",
            "POINT(121394)",
            "POINT(4.89 52.37)",
            "POINT(121394 487383 2)",
            "LINESTRING(121394 487383)",
        ] {
            assert_eq!(
                run(
                    "point.rd-extent",
                    "centroide_rd",
                    &json!({"centroide_rd":point})
                ),
                None
            );
        }
    }

    #[test]
    fn national_identifiers_cannot_cross_country_boundaries() {
        let body = json!({"pays":"GB", "id_national":"00445790"});
        assert_eq!(
            run("record.country-identifier", "GB", &body),
            Some(json!("00445790"))
        );
        assert_eq!(run("record.country-identifier", "NO", &body), None);
        assert_eq!(run("record.country-identifier", "gb", &body), None);
        assert_eq!(
            run(
                "record.country-identifier",
                "GB",
                &json!({"pays":"GB","id_national":445790})
            ),
            None
        );
    }

    /// The real body stored against call_run 3f9b64bf (HTTP 200), trimmed to
    /// the branches under test. Not invented.
    fn filing() -> Value {
        json!({ "opendataFields": [
            { "key": "SbiBusinessCode", "value": "64210" },
            { "key": "DocumentAdoptionDate", "value": "2026-04-26" },
            { "key": "FinancialYear", "value": "2025" },
            { "key": "BalanceSheet", "opendataFields": [
                { "key": "BalanceSheetTitle", "opendataFields": [
                    { "key": "AssetsTitle", "opendataFields": [
                        { "key": "AssetsNoncurrent", "value": "581377" },
                        { "key": "Assets", "value": "682987" }
                    ]},
                    { "key": "EquityAndLiabilitiesTitle", "opendataFields": [
                        { "key": "LiabilitiesTitle", "opendataFields": [
                            { "key": "Liabilities", "value": "1056254" }
                        ]},
                        { "key": "Equity", "value": "-373267" }
                    ]}
                ]}
            ]}
        ]})
    }

    #[test]
    fn reads_a_key_from_the_top_of_the_tree() {
        assert_eq!(
            run("tree.key-value", "FinancialYear", &filing()),
            Some(json!("2025"))
        );
    }

    #[test]
    fn reads_a_key_from_four_levels_down() {
        assert_eq!(
            run("tree.key-value", "Assets", &filing()),
            Some(json!("682987"))
        );
        assert_eq!(
            run("tree.key-value", "Equity", &filing()),
            Some(json!("-373267"))
        );
        assert_eq!(
            run("tree.key-value", "Liabilities", &filing()),
            Some(json!("1056254"))
        );
    }

    #[test]
    fn the_container_may_be_called_anything() {
        // The whole point of the generalisation. Same shape, three different
        // names for the nested list, one adapter.
        for container in ["opendataFields", "fields", "entries", "children"] {
            let body = json!({
                container: [
                    { "key": "Group", container: [ { "key": "Assets", "value": "1" } ] }
                ]
            });
            assert_eq!(
                run("tree.key-value", "Assets", &body),
                Some(json!("1")),
                "failed for a container called {container}"
            );
        }
    }

    #[test]
    fn the_pair_may_use_the_common_synonyms() {
        assert_eq!(
            run(
                "tree.key-value",
                "Assets",
                &json!({ "rows": [{ "name": "Assets", "val": "7" }] })
            ),
            Some(json!("7"))
        );
        assert_eq!(
            run(
                "tree.key-value",
                "Assets",
                &json!({ "rows": [{ "label": "Assets", "amount": 7 }] })
            ),
            Some(json!(7))
        );
    }

    #[test]
    fn a_key_the_filing_does_not_carry_is_absent_not_zero() {
        // This filing is a balance sheet only. Reporting Revenue as 0 would be
        // a fabricated figure about a real company.
        assert_eq!(run("tree.key-value", "Revenue", &filing()), None);
        assert_eq!(run("tree.key-value", "ResultAfterTax", &filing()), None);
    }

    #[test]
    fn a_grouping_key_carries_nothing() {
        assert_eq!(run("tree.key-value", "BalanceSheetTitle", &filing()), None);
        assert_eq!(run("tree.key-value", "AssetsTitle", &filing()), None);
    }

    #[test]
    fn an_unknown_adapter_yields_nothing() {
        assert_eq!(run("kvk.opendata-fields", "Assets", &filing()), None);
        assert!(!KNOWN.contains(&"kvk.opendata-fields"));
        assert!(KNOWN.iter().all(|a| !a.contains("kvk")));
    }

    #[test]
    fn no_adapter_is_named_after_a_provider() {
        // The rule this file exists under. An adapter names a SHAPE.
        for adapter in KNOWN {
            assert!(
                adapter.starts_with("tree.")
                    || adapter.starts_with("list.")
                    || adapter.starts_with("point.")
                    || adapter.starts_with("record."),
                "{adapter} looks like a vendor name, not a shape"
            );
        }
    }

    #[test]
    fn an_empty_body_yields_nothing_rather_than_panicking() {
        assert_eq!(run("tree.key-value", "Assets", &json!({})), None);
        assert_eq!(run("tree.key-value", "Assets", &json!(null)), None);
        assert_eq!(run("tree.key-value", "Assets", &json!([])), None);
    }
}
