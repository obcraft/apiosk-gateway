//! What the answer model is allowed to read.
//!
//! An answer is written from the saved source documents themselves, so a large
//! response would otherwise exceed the model request limit and leave a question
//! unanswered. Shrinking is deterministic and lossy in one direction only: long
//! arrays keep their first rows and long strings their first characters, so
//! every value the model can cite still exists at the same JSON pointer in the
//! document handed to validation. A document that was shortened says so, and
//! the prompt asks for that caveat in the answer's limitations.
use serde_json::{json, Map, Value};

/// Progressively tighter reading limits: (array rows kept, string characters kept).
const LIMITS: [(usize, usize); 4] = [(200, 4000), (50, 1200), (12, 400), (3, 120)];

/// Bound `sources` (`[{"result_ref":…,"document":…}]`) to `budget` serialized bytes.
pub fn bounded(sources: Vec<Value>, budget: usize) -> Vec<Value> {
    if fits(&sources, budget) {
        return sources;
    }
    for (rows, characters) in LIMITS {
        let shortened: Vec<Value> = sources.iter().map(|source| shorten(source, rows, characters)).collect();
        if fits(&shortened, budget) {
            return shortened;
        }
    }
    // Nothing readable fits. An empty set produces a stated gap, never a guess.
    Vec::new()
}

/// One source with its document data shortened, marked when anything was cut.
fn shorten(source: &Value, rows: usize, characters: usize) -> Value {
    let mut short = source.clone();
    let cut = match short.pointer_mut("/document/data") {
        Some(data) => {
            let smaller = shrink(data, rows, characters);
            let cut = smaller != *data;
            *data = smaller;
            cut
        }
        None => false,
    };
    if cut {
        if let Some(fields) = short.as_object_mut() {
            fields.insert("shortened_for_reading".into(), json!(true));
        }
    }
    short
}

fn fits(sources: &[Value], budget: usize) -> bool {
    serde_json::to_vec(sources).is_ok_and(|bytes| bytes.len() <= budget)
}

fn shrink(value: &Value, rows: usize, characters: usize) -> Value {
    match value {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .take(rows)
                .map(|item| shrink(item, rows, characters))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, field)| (key.clone(), shrink(field, rows, characters)))
                .collect::<Map<_, _>>(),
        ),
        Value::String(text) if text.chars().count() > characters => {
            Value::String(text.chars().take(characters).collect())
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_large_document_is_shortened_and_its_kept_values_stay_at_their_pointers() {
        let rows: Vec<Value> = (0..5_000).map(|n| json!({"naam": format!("Company {n}")})).collect();
        let sources = vec![json!({"result_ref":"saved","document":{"data":{"resultaten":rows}}})];
        let bounded = bounded(sources, 8 * 1024);
        let document = &bounded[0]["document"];
        assert_eq!(bounded[0]["shortened_for_reading"], json!(true));
        assert_eq!(
            document.pointer("/data/resultaten/0/naam").and_then(Value::as_str),
            Some("Company 0")
        );
        assert!(document["data"]["resultaten"].as_array().unwrap().len() < 5_000);
        assert!(serde_json::to_vec(&bounded).unwrap().len() <= 8 * 1024);
    }
    #[test]
    fn a_document_within_the_budget_is_handed_over_untouched() {
        let sources = vec![json!({"result_ref":"saved","document":{"data":{"year":2025,"name":"HEMA"}}})];
        assert_eq!(bounded(sources.clone(), 8 * 1024), sources);
    }
}
