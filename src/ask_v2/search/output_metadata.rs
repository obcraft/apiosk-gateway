//! Endpoint-local discovery evidence from admitted output schemas. A source's
//! marketing, request inputs and raw sample values are never output metadata.
use crate::{
    ask_v2::intent_fields::documented,
    catalog::{Catalog, Service},
    parser::plan_coverage::output_aliases,
};
use serde_json::Value;
use std::collections::BTreeSet;

/// Normalize inflection while retaining whether the request names a record
/// category. A scalar `employees` field may contain a count or size band, so it
/// must not be the same evidence as `employee_details`. Only a documented
/// structured field can supply the record-category variant below.
pub(super) fn concept(name: &str) -> String {
    let words = super::words(name);
    let tokens = super::tokens(name);
    // A bare plural is a collection category, while a named scalar such as
    // `employee_count` keeps its measurement identity. Do not use arbitrary
    // prose or a provider-specific noun list to infer a record shape.
    let collection = tokens.len() == 1
        && tokens[0].ends_with('s')
        && super::singular(&tokens[0]).is_some_and(|singular| singular != tokens[0]);
    let record = collection || words.iter().any(|word| record_word(word));
    let concept = words
        .into_iter()
        .filter(|word| {
            !super::vague(word)
                && !record_word(word)
                && !matches!(
                    word.as_str(),
                    "id" | "url" | "value" | "label" | "title" | "description"
                )
        })
        .collect::<Vec<_>>()
        .join("_");
    if record && !concept.is_empty() {
        format!("record:{concept}")
    } else {
        concept
    }
}

fn record_word(word: &str) -> bool {
    matches!(word, "detail" | "record" | "information" | "info" | "overview" | "data")
}

fn insert_concept(name: &str, structured: bool, concepts: &mut BTreeSet<String>) {
    let concept = concept(name);
    if concept.is_empty() || (!structured && concept.starts_with("record:")) {
        return;
    }
    if structured && !concept.starts_with("record:") {
        concepts.insert(format!("record:{concept}"));
    }
    concepts.insert(concept);
}

fn schema_kind(value: &Value) -> Option<&str> {
    match &value["type"] {
        Value::String(kind) => Some(kind),
        Value::Array(kinds) => {
            let mut concrete = kinds
                .iter()
                .filter_map(Value::as_str)
                .filter(|kind| *kind != "null");
            let kind = concrete.next()?;
            concrete.next().is_none().then_some(kind)
        }
        _ => None,
    }
}

fn collect(
    schema: &Value,
    name: &str,
    depth: usize,
    terms: &mut BTreeSet<String>,
    concepts: &mut BTreeSet<String>,
) {
    if depth > 10
        || terms.len() >= 256
        || ["$ref", "anyOf", "oneOf", "allOf", "not", "if"]
            .iter()
            .any(|key| schema.get(*key).is_some())
    {
        return;
    }
    let Some(kind) = schema_kind(schema) else {
        return;
    };
    // Use the field registry's documented-schema rule, permitting its same
    // concrete types when a provider declares them nullable. Unknown shapes
    // still contribute no names, aliases or descriptive claims.
    let mut concrete = schema.clone();
    concrete["type"] = Value::String(kind.into());
    if documented(&concrete) {
        let structured = matches!(kind, "object" | "array");
        if !name.is_empty() {
            terms.insert(name.chars().take(160).collect());
            insert_concept(name, structured, concepts);
        }
        let aliases = output_aliases(schema);
        terms.extend(aliases.iter().cloned());
        for alias in aliases {
            insert_concept(&alias, structured, concepts);
        }
        if let Some(description) = schema["description"].as_str() {
            terms.insert(description.chars().take(240).collect());
        }
    }
    if kind == "object" {
        for (name, child) in schema["properties"].as_object().into_iter().flatten() {
            collect(child, name, depth + 1, terms, concepts);
        }
    } else if kind == "array" {
        if let Some(items) = schema.get("items") {
            collect(items, "", depth + 1, terms, concepts);
        }
    }
}

#[derive(Default)]
pub(super) struct Metadata {
    pub text: String,
    pub concepts: BTreeSet<String>,
}

pub(super) fn metadata(service: &Service, catalog: &Catalog) -> Metadata {
    if service.unverified {
        return Metadata::default();
    }
    let mut terms = BTreeSet::new();
    let mut concepts = BTreeSet::new();
    for fact in service.contract.output_map.produces.keys() {
        if let Some(definition) = catalog.vocabulary.get(fact) {
            collect(&definition.value_schema, "", 0, &mut terms, &mut concepts);
        }
    }
    Metadata {
        text: terms.into_iter().collect::<Vec<_>>().join(" "),
        concepts,
    }
}

#[cfg(test)]
mod tests;
