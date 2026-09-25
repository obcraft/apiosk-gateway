//! Offline replay of saved questions (#6). A case runs the production
//! interpretation, planning, answer-source selection, direct answer and
//! validated analysis code against stored source documents and recorded
//! model responses (a tape served over HTTP). There is no executor here, so
//! a replay can never call or pay a source.
//!
//! Outputs are compared as semantic sets, not JSON shapes: a plan is its
//! steps, cap and order; an answer is its cited facts and presentation
//! cells. Equal shapes with different content are a regression.

pub mod score;

use crate::{
    catalog::Catalog,
    parser::{Intent, Parser},
    resolver::{self, Plan, Resolution},
    state::Task,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

/// A replay case (`eval/replay/v<N>/cases/<id>/case.json`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub lang: String,
    pub question: String,
    pub query_class: String,
    /// A catalog snapshot, relative to the gateway repository.
    pub catalog: String,
    /// A documented edit of a fixture document rather than a recorded one.
    pub synthetic: bool,
    /// What the edit changed, when `synthetic`.
    #[serde(default)]
    pub synthetic_edit: Option<String>,
    /// The saved task before this question; a new chat when absent.
    #[serde(default)]
    pub context: Option<Task>,
}

/// One stored outcome of a plan step (`sources.json`), matched to a replayed
/// step by capability and subject label.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredStep {
    pub capability: String,
    pub subject: String,
    /// The saved result document exactly as `results.document` holds it.
    #[serde(default)]
    pub document: Option<Value>,
    /// The public failure a step ended with instead of a result.
    #[serde(default)]
    pub failure: Option<Value>,
}

/// Fixed result ids, so a recorded analysis can cite them literally.
pub fn result_ref(index: usize) -> Uuid {
    Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0100 + index as u128)
}
fn fixed_id(text: &str) -> bool {
    text.starts_with("00000000-0000-4000-8000-")
}

/// What one replay produced, before normalization.
#[derive(Debug)]
pub struct Outcome {
    pub status: &'static str,
    pub message: Option<String>,
    pub intent: Option<Intent>,
    pub plan: Option<Plan>,
    pub task: Task,
    pub documents: BTreeMap<Uuid, Value>,
    pub analysis: Option<Value>,
}

/// Runs one case through production code. Model calls go to `parser`, which
/// a replay points at a tape server.
pub async fn run(parser: &Parser, catalog: &Catalog, case: &Case, stored: &[StoredStep]) -> anyhow::Result<Outcome> {
    let mut task = case.context.clone().unwrap_or_default();
    let reply = parser.chat(&case.question, &task, catalog).await?;
    let mut outcome = Outcome { status: "needs_input", message: Some(reply.message.clone()), intent: None, plan: None, task: Task::default(), documents: BTreeMap::new(), analysis: None };
    let Some(mut intent) = reply.intent.filter(|_| reply.discover) else {
        task.record_turn(&case.question, Some(reply.message));
        outcome.task = task;
        return Ok(outcome);
    };
    crate::uk_supplier::expand(&mut intent, catalog);
    crate::engine::bind_subjects(&mut task, catalog, &mut intent)?;
    task.record_turn(&case.question, Some(reply.message));
    let mut resolution = resolver::resolve(catalog, &task, &intent);
    if let Resolution::Unsupported(reason) = &resolution {
        let review = parser.review_plan(&case.question, &task, catalog, &intent, reason).await?;
        outcome.message = Some(review.message);
        let Some(revised) = review.intent else {
            // A planning clarification: the person is asked, nothing is priced.
            return Ok(Outcome { intent: Some(intent), task, ..outcome });
        };
        intent = revised;
        resolution = resolver::resolve(catalog, &task, &intent);
    }
    task.intent = Some(serde_json::to_value(&intent)?);
    let plan = match resolution {
        Resolution::Plan(plan) => plan,
        Resolution::Missing { .. } => return Ok(Outcome { intent: Some(intent), task, ..outcome }),
        Resolution::Unsupported(_) => return Ok(Outcome { status: "unsupported", intent: Some(intent), task, ..outcome }),
    };
    let mut documents = BTreeMap::new();
    for (index, step) in plan.steps.iter().enumerate() {
        let label = task.entities.iter().find(|entity| entity.entity_ref == step.entity_ref).map(|entity| entity.label.clone()).unwrap_or_default();
        let saved = stored.iter().find(|saved| saved.capability == step.service.contract.capability_slug && saved.subject == label);
        match saved {
            Some(StoredStep { document: Some(document), .. }) => {
                let mut document = document.clone();
                document["source"]["endpoint_id"] = json!(step.service.endpoint_id);
                document["subject"]["entity_ref"] = json!(step.entity_ref);
                document["subject"]["label"] = json!(label);
                documents.insert(result_ref(index), document);
                task.active_results.push(result_ref(index));
                task.result_refs.push(result_ref(index));
                task.completed.push(step.key());
            }
            Some(StoredStep { failure: Some(failure), .. }) => {
                task.failed_steps.insert(step.key(), failure.clone());
            }
            _ => {
                task.failed_steps.insert(step.key(), json!({"code":"source_unavailable","message":"No stored result for this step."}));
            }
        }
    }
    task.proposal = Some(serde_json::to_value(&plan)?);
    let sources = crate::engine::answer_sources(&plan, &intent, &task, &documents);
    let direct = task.failed_steps.is_empty().then(|| crate::direct_answer::compose(&case.question, &intent, &sources)).flatten();
    let analysis = match direct {
        Some(answer) => answer,
        None => {
            let operations = crate::engine::analysis_operations(&task);
            let requirements = crate::completeness::requirements(&intent);
            let answer = if crate::uk_supplier::is_check(&intent) {
                parser.analyze_workflow(&operations, &sources, &task.failed_steps, &requirements).await
            } else {
                parser.analyze(&operations, &sources, &task.failed_steps, &requirements).await
            };
            answer.unwrap_or_else(|_| json!({"status":"unavailable","observations":[],"limitations":[]}))
        }
    };
    Ok(Outcome { status: "answered", message: outcome.message, intent: Some(intent), plan: Some(plan), task, documents, analysis: Some(analysis) })
}

/// A cited source value as text; null when the pointer names no scalar.
fn scalar(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(text)) => json!(text),
        Some(value @ (Value::Number(_) | Value::Bool(_))) => json!(value.to_string()),
        _ => Value::Null,
    }
}

/// A sorted, de-duplicated list: the semantic set of one output section.
fn set(items: impl IntoIterator<Item = Value>) -> Value {
    let unique: BTreeSet<String> = items.into_iter().map(|item| item.to_string()).collect();
    Value::Array(unique.into_iter().filter_map(|item| serde_json::from_str(&item).ok()).collect())
}

/// Plan, facts, presentation and answer status as semantic sets.
pub fn normalize(outcome: &Outcome) -> Value {
    let label = |entity: Uuid| outcome.task.entities.iter().find(|item| item.entity_ref == entity).map(|item| item.label.clone()).unwrap_or_default();
    let plan = match &outcome.plan {
        Some(plan) => json!({
            "status": "plan",
            "cap_atomic": plan.cap_atomic,
            "steps": set(plan.steps.iter().map(|step| json!([step.service.contract.capability_slug, step.service.api_slug, label(step.entity_ref)]))),
            "order": plan.steps.iter().map(|step| json!(step.service.contract.capability_slug)).collect::<Vec<_>>(),
            "inputs": set(outcome.intent.iter().flat_map(|intent| intent.subjects.iter()).flat_map(|subject| subject.values.iter().map(|(kind, value)| json!([subject.label, kind, value])))),
        }),
        None => json!({"status": outcome.status}),
    };
    // A citation: the source, subject, pointer and the value it points at.
    let cite = |evidence: &Value| {
        let pointer = evidence["pointer"].as_str().unwrap_or_default();
        let document = evidence["result_ref"].as_str().and_then(|id| id.parse::<Uuid>().ok()).and_then(|id| outcome.documents.get(&id));
        let value = scalar(document.and_then(|document| document.pointer(pointer)));
        (document.map_or(Value::Null, |document| document["source"]["provider"].clone()),
            document.map_or(Value::Null, |document| document["subject"]["label"].clone()), json!(pointer), value)
    };
    let analysis = outcome.analysis.as_ref();
    let facts = set(analysis.and_then(|analysis| analysis["observations"].as_array()).into_iter().flatten()
        .flat_map(|observation| observation["evidence"].as_array().cloned().unwrap_or_default())
        .map(|evidence| {
            let (source, subject, pointer, value) = cite(&evidence);
            json!([source, subject, pointer, value])
        }));
    let presentation = analysis.map(|analysis| &analysis["presentation"]).filter(|presentation| presentation.is_object());
    // Validated presentations hold a `value` per heading and cell; a sourced
    // cell also names its result and pointer.
    let cells = presentation.map(|presentation| {
        let columns: Vec<Value> = presentation["columns"].as_array().into_iter().flatten().map(|column| scalar(Some(&column["value"]))).collect();
        let mut cells = Vec::new();
        for row in presentation["rows"].as_array().into_iter().flatten().filter_map(Value::as_array) {
            let row_label = scalar(row.first().map(|cell| &cell["value"]));
            for (index, cell) in row.iter().enumerate().filter(|(_, cell)| cell.get("pointer").is_some()) {
                let (_, _, pointer, value) = cite(cell);
                cells.push(json!([row_label, columns.get(index).cloned().unwrap_or(Value::Null), value, pointer]));
            }
        }
        set(cells)
    });
    json!({
        "plan": plan,
        "facts": facts,
        "presentation": presentation.map_or(Value::Null, |presentation| json!({"kind": presentation["kind"], "cells": cells})),
        "answer": analysis.map_or(json!({"status": outcome.status}), |analysis| json!({
            "status": analysis["status"],
            "follow_up": analysis["follow_up_question"].is_string(),
            "limitations": analysis["limitations"].as_array().map_or(0, Vec::len),
            "observations": analysis["observations"].as_array().map_or(0, Vec::len),
        })),
    })
}

/// How a replayed section compares with its golden one. Only `Equal` passes;
/// `Superset` needs review; the others are regressions. `ShapeOnlyEqual`
/// marks output that kept its JSON shape while its content changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Equal,
    Superset,
    Subset,
    ChangedValue,
    ShapeOnlyEqual,
    Different,
}
impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Equal => "equal",
            Verdict::Superset => "superset",
            Verdict::Subset => "subset",
            Verdict::ChangedValue => "changed_value",
            Verdict::ShapeOnlyEqual => "shape_only_equal",
            Verdict::Different => "different",
        }
    }
}

/// The JSON shape of a value: its structure and value kinds, not content.
fn shape(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(map.iter().map(|(key, value)| (key.clone(), shape(value))).collect()),
        Value::Array(items) => Value::Array(items.iter().map(shape).collect()),
        Value::String(_) => json!("string"),
        Value::Number(_) => json!("number"),
        Value::Bool(_) => json!("bool"),
        Value::Null => Value::Null,
    }
}

/// Compares two sets of tuples whose first `key_len` members identify an item.
fn compare_set(golden: &Value, actual: &Value, key_len: usize) -> Verdict {
    if golden == actual {
        return Verdict::Equal;
    }
    let items = |value: &Value| value.as_array().cloned().unwrap_or_default();
    let (golden_items, actual_items) = (items(golden), items(actual));
    let key = |item: &Value| Value::Array(item.as_array().map(|parts| parts.iter().take(key_len).cloned().collect()).unwrap_or_default());
    let golden_set: BTreeSet<String> = golden_items.iter().map(Value::to_string).collect();
    let actual_set: BTreeSet<String> = actual_items.iter().map(Value::to_string).collect();
    if golden_set.is_subset(&actual_set) {
        return Verdict::Superset;
    }
    if actual_set.is_subset(&golden_set) {
        return Verdict::Subset;
    }
    let golden_keys: BTreeSet<String> = golden_items.iter().map(|item| key(item).to_string()).collect();
    let actual_keys: BTreeSet<String> = actual_items.iter().map(|item| key(item).to_string()).collect();
    if golden_keys == actual_keys {
        Verdict::ChangedValue
    } else if shape(golden) == shape(actual) {
        Verdict::ShapeOnlyEqual
    } else {
        Verdict::Different
    }
}

/// Per section: plan, facts, presentation and answer status.
pub fn compare(golden: &Value, actual: &Value) -> BTreeMap<&'static str, Verdict> {
    let plan = if golden["plan"] == actual["plan"] {
        Verdict::Equal
    } else if golden["plan"]["status"] != actual["plan"]["status"] || golden["plan"]["order"] != actual["plan"]["order"] {
        if shape(&golden["plan"]) == shape(&actual["plan"]) { Verdict::ShapeOnlyEqual } else { Verdict::Different }
    } else if golden["plan"]["steps"] != actual["plan"]["steps"] {
        compare_set(&golden["plan"]["steps"], &actual["plan"]["steps"], 1)
    } else {
        Verdict::ChangedValue
    };
    let presentation = if golden["presentation"]["kind"] != actual["presentation"]["kind"] {
        if golden["presentation"] == actual["presentation"] { Verdict::Equal } else { Verdict::Different }
    } else {
        compare_set(&golden["presentation"]["cells"], &actual["presentation"]["cells"], 2)
    };
    BTreeMap::from([
        ("plan", plan),
        ("facts", compare_set(&golden["facts"], &actual["facts"], 3)),
        ("presentation", presentation),
        ("answer", if golden["answer"] == actual["answer"] { Verdict::Equal } else if shape(&golden["answer"]) == shape(&actual["answer"]) { Verdict::ShapeOnlyEqual } else { Verdict::Different }),
    ])
}

fn uuid_like(text: &str) -> bool {
    text.len() == 36 && Uuid::parse_str(text).is_ok()
}
fn timestamp_like(text: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(text).is_ok()
}

/// A model request's user content with run-specific ids and timestamps
/// replaced by first-seen ordinals, and the values they stood for.
pub fn canonical(user: &str) -> (String, Vec<String>) {
    fn walk(value: &mut Value, seen: &mut Vec<String>) {
        match value {
            Value::String(text) if (uuid_like(text) && !fixed_id(text)) || timestamp_like(text) => {
                let index = seen.iter().position(|known| known == text).unwrap_or_else(|| {
                    seen.push(text.clone());
                    seen.len() - 1
                });
                *text = format!("<{}:{index}>", if uuid_like(text) { "id" } else { "time" });
            }
            Value::Array(items) => items.iter_mut().for_each(|item| walk(item, seen)),
            Value::Object(map) => map.values_mut().for_each(|item| walk(item, seen)),
            _ => {}
        }
    }
    let mut seen = Vec::new();
    match serde_json::from_str::<Value>(user) {
        Ok(mut value) => {
            walk(&mut value, &mut seen);
            (value.to_string(), seen)
        }
        Err(_) => (user.to_owned(), seen),
    }
}

pub fn sha256(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// The tape key of a model request: model, system prompt hash and canonical
/// user content. The returned values restore a recorded response.
pub fn tape_key(request: &Value) -> (String, String, Vec<String>) {
    let model = request["model"].as_str().unwrap_or_default();
    let system = request["messages"][0]["content"].as_str().unwrap_or_default();
    let user = request["messages"][1]["content"].as_str().unwrap_or_default();
    let (user, values) = canonical(user);
    let prompt_hash = sha256(system);
    (sha256(&format!("{model}\n{prompt_hash}\n{user}")), prompt_hash, values)
}

/// Which model surface a request is, from its user content.
pub fn surface_of(request: &Value) -> &'static str {
    let user: Value = request["messages"][1]["content"].as_str().and_then(|text| serde_json::from_str(text).ok()).unwrap_or(Value::Null);
    match user["surface"].as_str() {
        Some("ask-chat") => "chat",
        Some("planning-review") => "planning",
        _ if user.get("answer_operations").is_some() => "analysis",
        _ if user.get("workflow_presets").is_some() => "catalog-selection",
        _ if user.get("gap").is_some() || user.get("missing").is_some() => "clarification",
        _ => "intent",
    }
}

/// A recorded response with this request's own ids and timestamps restored.
pub fn restore(response: &Value, values: &[String]) -> Value {
    let mut text = response.to_string();
    for (index, value) in values.iter().enumerate().rev() {
        text = text.replace(&format!("<id:{index}>"), value).replace(&format!("<time:{index}>"), value);
    }
    serde_json::from_str(&text).unwrap_or_else(|_| response.clone())
}

/// A recorded response with this request's ids and timestamps replaced by
/// the placeholders its key uses, so the tape replays in any later run.
pub fn record(response: &Value, values: &[String]) -> Value {
    let mut text = response.to_string();
    for (index, value) in values.iter().enumerate() {
        let placeholder = format!("<{}:{index}>", if uuid_like(value) { "id" } else { "time" });
        text = text.replace(value.as_str(), &placeholder);
    }
    serde_json::from_str(&text).unwrap_or_else(|_| response.clone())
}

/// One recorded model exchange (`tape.jsonl`).
pub fn tape_entry(surface: &str, key: &str, prompt_hash: &str, response: &Value) -> Value {
    let mut entry = Map::new();
    entry.insert("surface".into(), json!(surface));
    entry.insert("key".into(), json!(key));
    entry.insert("prompt_hash".into(), json!(prompt_hash));
    entry.insert("response".into(), response.clone());
    Value::Object(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_ignore_generated_ids_and_times_but_not_fixed_ones_or_content() {
        let request = |id: &str, question: &str| json!({"model":"m","messages":[{"role":"system","content":"s"},{"role":"user","content":json!({
            "question":question,"entity":id,"again":id,"fixed":result_ref(0),"at":"2026-09-25T01:00:00Z"}).to_string()}]});
        let (first, _, values) = tape_key(&request(&Uuid::new_v4().to_string(), "Vergelijk"));
        let (second, _, _) = tape_key(&request(&Uuid::new_v4().to_string(), "Vergelijk"));
        let (other, _, _) = tape_key(&request(&Uuid::new_v4().to_string(), "Compare"));
        assert_eq!(first, second);
        assert_ne!(first, other);
        assert_eq!(values.len(), 2, "one id seen twice and one timestamp; the fixed id stays literal");
        let recorded = record(&json!({"entity": values[0], "cites": result_ref(0)}), &values);
        assert_eq!(recorded["entity"], "<id:0>");
        let fresh = vec!["7d7a1e8e-0000-4000-8000-000000000999".to_owned(), values[1].clone()];
        assert_eq!(restore(&recorded, &fresh)["entity"], fresh[0].as_str());
        assert_eq!(restore(&recorded, &fresh)["cites"], result_ref(0).to_string());
    }

    #[test]
    fn same_shape_with_other_content_is_not_equal_quality() {
        let facts = |items: Value| json!({"plan":{"status":"plan"},"facts":items,"presentation":null,"answer":{"status":"completed"}});
        let golden = facts(json!([["kvk","Coolblue B.V.","/data/adres","Weena 664"]]));
        let verdict = |actual: Value| compare(&golden, &actual)["facts"];
        assert_eq!(verdict(golden.clone()), Verdict::Equal);
        assert_eq!(verdict(facts(json!([["kvk","Coolblue B.V.","/data/adres","Weena 665"]]))), Verdict::ChangedValue);
        assert_eq!(verdict(facts(json!([["kvk","Mollie B.V.","/data/naam","Mollie"]]))), Verdict::ShapeOnlyEqual);
        assert_eq!(verdict(facts(json!([]))), Verdict::Subset);
        assert_eq!(verdict(facts(json!([["kvk","Coolblue B.V.","/data/adres","Weena 664"],["kvk","Coolblue B.V.","/data/naam","Coolblue"]]))), Verdict::Superset);
    }

    #[test]
    fn surfaces_are_recognised_from_the_request() {
        let request = |user: Value| json!({"messages":[{"content":"s"},{"content":user.to_string()}]});
        assert_eq!(surface_of(&request(json!({"surface":"ask-chat"}))), "chat");
        assert_eq!(surface_of(&request(json!({"answer_operations":[],"sources":[]}))), "analysis");
        assert_eq!(surface_of(&request(json!({"surface":"planning-review"}))), "planning");
        assert_eq!(surface_of(&request(json!({"operations":{},"workflow_presets":[]}))), "catalog-selection");
    }
}
