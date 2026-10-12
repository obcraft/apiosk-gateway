pub mod compute_analysis;
pub(crate) mod conversation_route;
use crate::{
    answer_blocks::{Requirement, RequirementClaim},
    catalog::Catalog,
    state::{digest, Task},
    telemetry::{Cache, ErrorClass, Outcome},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use uuid::Uuid;

mod capability_binding;
use capability_binding::Bindings;
mod analysis_source_coverage;
mod analysis_evidence;
mod analysis_period;
mod analysis_schema;
mod explicit_request;
pub(crate) mod plan_coverage;
pub(crate) mod source_grounding;
mod constraint_policy;
pub mod conversation_reply;

/// Discovery and planning preserve up to sixteen retrievals. The resolver
/// independently enforces the smaller execution-step and dependency limits.
pub(crate) const MAX_GOALS: usize = 16;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subject {
    pub entity_ref: Option<Uuid>,
    pub label: String,
    pub values: BTreeMap<String, Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Goal {
    pub capability: String,
    pub subject: usize,
    pub constraints: BTreeMap<String, Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    pub subjects: Vec<Subject>,
    pub goals: Vec<Goal>,
    pub answer_operations: Vec<String>,
    pub unresolved_references: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanningReview {
    pub message: String,
    pub intent: Option<Intent>,
}

impl PlanningReview {
    fn validate(
        &self,
        original: &Intent,
        question: &str,
        task: &Task,
        catalog: &Catalog,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.message.trim().is_empty() && self.message.len() <= 3000,
            "invalid planning message"
        );
        let Some(revised) = &self.intent else {
            return Ok(());
        };
        revised.validate(question, task, catalog)?;
        anyhow::ensure!(
            serde_json::to_value(&revised.subjects)? == serde_json::to_value(&original.subjects)?
                && revised.goals.len() == original.goals.len()
                && revised.answer_operations == original.answer_operations
                && revised.unresolved_references == original.unresolved_references,
            "planner changed the request"
        );
        for (before, after) in original.goals.iter().zip(&revised.goals) {
            anyhow::ensure!(
                before.capability == after.capability
                    && before.subject == after.subject
                    && before.constraints.len() == after.constraints.len(),
                "planner changed a goal"
            );
            for (key, value) in &before.constraints {
                if after.constraints.get(key) == Some(value) {
                    continue;
                }
                // Semantic source alias matching is an AI decision; an already
                // canonical source is an exact choice and cannot be replaced.
                anyhow::ensure!(
                    key == "source"
                        && !catalog
                            .services
                            .iter()
                            .any(|s| value.as_str() == Some(s.api_slug.as_str()))
                        && catalog.services.iter().any(|s| after
                            .constraints
                            .get(key)
                            .and_then(Value::as_str)
                            == Some(s.api_slug.as_str())
                            && s.contract.capability_slug == after.capability),
                    "planner relaxed a requirement"
                );
            }
        }
        anyhow::ensure!(
            !matches!(
                crate::resolver::resolve(catalog, task, revised),
                crate::resolver::Resolution::Unsupported(_)
            ),
            "corrected plan is still unsupported; ask a clarification instead"
        );
        Ok(())
    }
}
impl Intent {
    /// Source inputs have one validated home. A planner may put a declared
    /// input in constraints; move it only when its literal, schema, subject
    /// and every selected endpoint variant agree. Defaults are never user facts.
    fn bind_constraint_inputs(&mut self, question: &str, catalog: &Catalog) -> anyhow::Result<()> {
        for goal in &mut self.goals {
            constraint_policy::validate(question, goal, catalog)?;
            let subject = self.subjects.get_mut(goal.subject)
                .ok_or_else(|| anyhow::anyhow!("invalid subject index"))?;
            let fields: Vec<_> = goal.constraints.keys()
                .filter(|field| catalog.vocabulary.contains_key(*field)).cloned().collect();
            for field in fields {
                let value = &goal.constraints[&field];
                validate_input_literal(question, &field, value, catalog)?;
                let variants: Vec<_> = catalog.services.iter().filter(|service| {
                    service.contract.capability_slug == goal.capability
                        && goal.constraints.get("source").and_then(Value::as_str)
                            .is_none_or(|source| constraint_policy::source_matches(catalog, service, source))
                        && plan_coverage::endpoint_matches(goal.constraints.get("endpoint_id"), Some(&service.endpoint_id.to_string()))
                }).collect();
                anyhow::ensure!(!variants.is_empty() && variants.iter().all(|service| service.contract.input_map.contains_key(&field)),
                    "source input is not declared for the selected operation");
                anyhow::ensure!(subject.values.get(&field).is_none_or(|existing| existing == value),
                    "input belongs to a different parsed subject");
                subject.values.insert(field.clone(), value.clone());
                goal.constraints.remove(&field);
            }
        }
        Ok(())
    }

    /// Restore literal text and extract documented Crunchbase identifiers from
    /// a supplied profile URL. No names become guessed permalinks; resulting
    /// values must remain exact question spans and pass their input schema.
    fn restore_question_literals(&mut self, question: &str) {
        for subject in &mut self.subjects {
            if subject.entity_ref.is_none() {
                if let Some(original) = question_literal(question, &subject.label) {
                    subject.label = original.to_owned();
                }
            }
            for (kind, value) in &mut subject.values {
                if let Some(id) = value.as_str().filter(|text| question.contains(*text))
                    .and_then(|text| crate::ask_v2::input_values::crunchbase_profile_id(kind, text)) {
                    *value = id;
                }
                if let Some(original) = value.as_str().and_then(|text| question_literal(question, text)) {
                    *value = json!(original);
                }
            }
        }
    }

    pub fn validate(&self, question: &str, task: &Task, catalog: &Catalog) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.subjects.len() <= 8 && !self.goals.is_empty() && self.goals.len() <= MAX_GOALS,
            "invalid intent size"
        );
        for s in &self.subjects {
            if let Some(id) = s.entity_ref {
                anyhow::ensure!(
                    task.entities.iter().any(|e| e.entity_ref == id),
                    "unknown entity reference"
                );
            } else {
                anyhow::ensure!(
                    !s.label.is_empty()
                        && question.to_lowercase().contains(&s.label.to_lowercase()),
                    "subject label must be explicit in question"
                );
            }
            for (kind, value) in &s.values {
                validate_input_literal(question, kind, value, catalog)?;
            }
        }
        for g in &self.goals {
            anyhow::ensure!(g.subject < self.subjects.len(), "invalid subject index");
            constraint_policy::validate(question, g, catalog)?;
            for (kind, value) in &g.constraints {
                if catalog.vocabulary.contains_key(kind) {
                    validate_input_literal(question, kind, value, catalog)?;
                    anyhow::bail!("source inputs must be subject values");
                }
            }
            // A US-style issuer name alone is not evidence of a UK registered
            // company. Prevent a generic filing request from buying a UK-only
            // lookup when the person never asked for Companies House.
            if g.capability == "company.filing-history"
                && g.constraints.get("source").and_then(Value::as_str).is_none_or(|source| source == "companies-house")
                && (self.subjects[g.subject].label.trim_end_matches('.').to_ascii_lowercase().ends_with(" inc")
                    || question.to_ascii_lowercase().contains(" inc."))
                && !["companies house", "uk filing", "british filing", "united kingdom filing"]
                    .iter().any(|term| question.to_ascii_lowercase().contains(term))
            {
                anyhow::bail!("Companies House covers UK entities; this request names an Inc issuer without asking for UK filings. Select an applicable US filing operation or ask for the jurisdiction.");
            }
            if g.capability == crate::uk_supplier::PRESET && crate::uk_supplier::available(catalog) {
                continue;
            }
            if g.capability == crate::supplier_check::PRESET && crate::supplier_check::available(catalog) {
                continue;
            }
            if !catalog.by_capability.contains_key(&g.capability) {
                return Err(ModelFailure::Unsupported.into());
            }
        }
        Ok(())
    }
}

fn validate_input_literal(question: &str, kind: &str, value: &Value, catalog: &Catalog) -> anyhow::Result<()> {
    let literal = match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => anyhow::bail!("explicit values must be scalar literals"),
    };
    let literal_match = if value.is_number() {
        question.match_indices(&literal).any(|(start, _)| {
            let mut before = question[..start].chars().rev();
            let mut after = question[start + literal.len()..].chars();
            let left = before.next().is_some_and(|ch| ch.is_alphanumeric() || matches!(ch, '+' | '-')
                || (matches!(ch, '.' | ',') && before.next().is_some_and(|ch| ch.is_ascii_digit())));
            let right = after.next().is_some_and(|ch| ch.is_alphanumeric()
                || (matches!(ch, '.' | ',') && after.next().is_some_and(|ch| ch.is_ascii_digit())));
            !left && !right
        })
    } else {
        question.to_lowercase().contains(&literal.to_lowercase())
    };
    anyhow::ensure!(!literal.is_empty() && literal_match, "parser invented a value");
    let fact = catalog.vocabulary.get(kind).ok_or_else(|| anyhow::anyhow!("unknown input type"))?;
    catalog_core::facts::schema::validate(&fact.value_schema, value)?;
    Ok(())
}

/// A company name alone does not tell us whether the buyer wants a filing
/// index, a form, or a disclosure topic. None of the connected SEC operations
/// is a complete filing index, so this shape must not reach a paid quote.
pub(crate) fn broad_filing_subject(question: &str) -> Option<&str> {
    let trimmed = question.trim().trim_end_matches(['.', '?', '!']).trim();
    let lower = trimmed.to_ascii_lowercase();
    let prefixes = ["i need filings from ", "show me filings from ", "get filings from ",
        "find filings from ", "give me filings from ", "i want filings from ",
        "filings from ", "filings for ", "filings of "];
    let prefix = prefixes.iter().find(|prefix| lower.starts_with(**prefix))?;
    let subject = trimmed[prefix.len()..].trim();
    if subject.eq_ignore_ascii_case("companies house") { return None; }
    let words: Vec<_> = subject.split_whitespace().collect();
    if words.is_empty() || words.len() > 6 || words.iter().any(|word| {
        word.chars().any(|ch| ch.is_ascii_digit())
            || ["about", "on", "since", "last", "latest", "recent", "between", "in", "from", "at", "uk", "sec"].contains(&word.to_ascii_lowercase().as_str())
    }) { return None; }
    Some(subject)
}

fn question_literal<'a>(question: &'a str, literal: &str) -> Option<&'a str> {
    if question.to_lowercase().contains(&literal.to_lowercase()) {
        return None;
    }
    let words = |text: &str| -> Vec<(usize, usize, String)> {
        let mut result = Vec::new();
        let mut start = None;
        for (index, ch) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
            if ch.is_alphanumeric() {
                start.get_or_insert(index);
            } else if let Some(begin) = start.take() {
                result.push((begin, index, text[begin..index].to_lowercase()));
            }
        }
        result
    };
    let needle = words(literal);
    // Single tokens and identifiers need their existing literal match. This
    // repair only joins words separated differently, e.g. a postcode and city.
    if needle.len() < 2 {
        return None;
    }
    let haystack = words(question);
    let span = haystack.windows(needle.len()).find(|span| {
        span.iter().zip(&needle).all(|(left, right)| left.2 == right.2)
    })?;
    Some(&question[span[0].0..span[span.len() - 1].1])
}

fn decode_intent(raw: &str) -> anyhow::Result<Intent> {
    let mut value: Value = serde_json::from_str(raw)?;
    let one_subject = value
        .get("subjects")
        .and_then(Value::as_array)
        .is_some_and(|subjects| subjects.len() == 1);
    if one_subject {
        if let Some(goals) = value.get_mut("goals").and_then(Value::as_array_mut) {
            for goal in goals {
                if goal.get("subject").is_some_and(Value::is_null) {
                    goal["subject"] = json!(0);
                }
            }
        }
    }
    Ok(serde_json::from_value(value)?)
}

/// Fixed classifications only: model output and user values never enter logs.
fn intent_validation_reason(error: &anyhow::Error) -> &'static str {
    match error.to_string().as_str() {
        "parser invented a value" => "non_literal_input",
        "subject label must be explicit in question" => "non_literal_subject",
        "unknown entity reference" => "unknown_entity",
        "unknown parsed subject reference" => "unknown_subject_binding",
        "unknown parsed value reference" => "unknown_value_binding",
        "unknown input type" => "unknown_input_type",
        "explicit values must be scalar literals" => "non_scalar_input",
        "invalid subject index" => "invalid_subject_index",
        "invalid intent size" => "invalid_intent_size",
        "discovery requires a matched intent" => "missing_matched_intent",
        "unmatched capabilities need clarification before a quote" => "unmatched_capabilities",
        "intent must use discovered operations" => "undiscovered_operation",
        "every matched capability needs a retrieval goal" => "missing_capability_goal",
        "every parsed subject needs its requested retrieval" => "missing_subject_goal",
        "each requested retrieval needs a covered goal" => "requirement_goal_count",
        "every goal must add requested output coverage" => "redundant_goal",
        "duplicate retrieval goal" => "duplicate_goal",
        "input belongs to a different parsed subject" => "wrong_subject_input",
        "source inputs must be subject values" => "misplaced_source_input",
        "source input is not declared for the selected operation" => "unmatched_source_input",
        "unsupported retrieval constraint" => "unsupported_constraint",
        "ungrounded retrieval constraint" => "ungrounded_constraint",
        "invalid retrieval constraint" => "invalid_constraint",
        "unexpected intent for conversational reply" => "conversation_requires_null_intent",
        explicit_request::DECLINED => "declined_explicit_request",
        _ => "invalid_intent_schema",
    }
}

#[derive(Debug, thiserror::Error)]
#[error("the proposed plan does not preserve the requested requirements ({0})")]
struct RequirementRejection(&'static str);

fn requirement_rejection(reason: &str) -> bool {
    matches!(reason, "duplicate_goal" | "redundant_goal" | "missing_subject_goal" | "requirement_goal_count"
        | "missing_capability_goal" | "unmatched_capabilities" | "undiscovered_operation"
        | "wrong_subject_input" | "non_literal_input" | "misplaced_source_input" | "unmatched_source_input"
        | "unsupported_constraint" | "ungrounded_constraint" | "invalid_constraint" | "non_scalar_input" | "missing_source_coverage")
}

fn intent_repair_message(surface: &str, reason: &str) -> String {
    let shape = if matches!(surface, "chat" | "capability-plan") {
        "Return the chat envelope with message, discover and intent. If discover=false, intent MUST be null. If discover=true, place the corrected intent inside intent. Use exact apiosk_operations from source_matches when supplied; parsed_request slugs describe meaning and are not executable operations. When bindings are supplied, subjects contain only subject_ref and values: choose subject_ref from bindings.subjects, and each value is {\"ref\": an exact bindings.values key} or {\"literal\": exact user text or number}. Never emit label or entity_ref in that format."
    } else {
        "Return only the Intent object with subjects, goals, answer_operations and unresolved_references."
    };
    if reason == "declined_explicit_request" {
        return format!("The previous response failed validation ({reason}). {} {shape}", explicit_request::DECLINED_REPAIR);
    }
    format!("The previous response failed validation ({reason}). {shape} New subject labels and every source input value must be exact contiguous text copied from the original question. Source input types belong in subjects.values, never constraints; do not copy schema examples or defaults. This includes search queries: copy the relevant phrase without dropping or reordering words, rather than composing new keywords. Do not invent company names, identifiers, country codes or entity references. Use only catalog input types and capabilities. Preserve every requested retrieval and constraint; do not hide a missing retrieval in answer_operations. If a downstream company or person cannot be identified by a supported dependency, explain that requirement in unresolved_references instead of inventing the entity. Correct the JSON shape, keeping goal subject indices within subjects and between one and sixteen goals.")
}
/// The endpoints a person selected in the App next to the question: shown to
/// the model, whose subject values may then also copy these entered values.
pub struct ModelSelection {
    pub view: Value,
    pub literals: Vec<String>,
}
/// App-only conversational routing; never an executable action or approval.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatReply {
    #[serde(default)]
    pub output_bindings: Vec<source_grounding::Binding>,
    pub message: String,
    pub discover: bool,
    #[serde(default)]
    pub intent: Option<Intent>,
    /// Why the planner returned no plan (`explicit_request::DECLINE_REASONS`);
    /// a genuine one gets its written reply from [`CHAT_MODEL`].
    #[serde(default)]
    pub decline_reason: Option<String>,
}
impl ChatReply {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.message.trim().is_empty() && self.message.len() <= 3000,
            "invalid chat reply"
        );
        Ok(())
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowChatReply {
    pub message: String,
    pub input: BTreeMap<String, Value>,
    pub ready: bool,
}

/// Apiosk's default OpenRouter model for legacy intent/planning and
/// single-provider analysis. Capability planning and synthesis have their
/// own explicit stage constants below; Ask intake is routed independently.
pub const MODEL: &str = "qwen/qwen3-30b-a3b-instruct-2507";
/// Qwen3 235B A22B Instruct 2507, via OpenRouter: the written reply to a
/// message that is not yet a clear data question (small talk, an unclear
/// request, a requirement no source covers), in `conversation_reply`.
/// This constant only controls conversational text, never planning or
/// synthesis. Multi-provider synthesis has its own stage constant below.
pub const CHAT_MODEL: &str = "qwen/qwen3-235b-a22b-2507";
/// Map extracted requirements onto reviewed source contracts when their
/// literal inputs cannot be compiled directly. This does not change intake,
/// ordinary chat, or the optional paid analysis route.
pub const CAPABILITY_PLAN_MODEL: &str = "openai/gpt-6.1-sol";
/// Combining distinct saved providers uses the stronger synthesis stage.
/// This is separate from conversation replies and never changes planning.
pub const MULTISOURCE_ANALYSIS_MODEL: &str = "openai/gpt-6.1-sol";

fn completion_budget(surface: &str) -> (u64, u64) {
    match surface {
        "capability-intake" | "capability-search" => (10, 20),
        "chat" => (20, 30),
        "capability-plan" => (40, 65),
        "clarification" => (6, 12),
        // A complete joined answer can exceed thirty seconds. Let one attempt
        // use the existing stage budget instead of restarting it halfway.
        // The shared deadline still keeps the request below the App's limit.
        "multi-source-analysis" => (60, 65),
        "analysis" => (30, 65),
        _ => (12, 25),
    }
}

fn model_envelope_failure(error: &anyhow::Error) -> &'static str {
    match error.downcast_ref::<reqwest::Error>() {
        Some(error) if error.is_timeout() => "timeout",
        Some(_) => "transport_failure",
        None => "invalid_envelope",
    }
}
// No fixed provider order for [`MODEL`]: OpenRouter's `sort` (latency by
// default, INTENT_PROVIDER_SORT) picks among the healthy providers. A pinned
// Nebius first measured 0.9 s in the morning of 2026-09-28 and 13.9 s or 429
// that night, when it was degraded. The Ask parse routes itself (ask_v2).

/// The deployment boundary: a variable naming any other model (including
/// [`CHAT_MODEL`]) is ignored, loudly, so a stale setting can never
/// reintroduce it. Conversation and multi-provider synthesis use constants,
/// not model environment overrides.
pub fn pinned_model(variable: &str) -> String {
    if let Some(configured) = std::env::var(variable).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty() && v != MODEL) {
        tracing::warn!(variable, %configured, model = MODEL, "ignoring a model other than Apiosk's OpenRouter model");
    }
    MODEL.to_owned()
}

/// Only these public classifications leave the model boundary. Provider
/// envelopes, JSON errors and model internals stay out of client responses.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ModelFailure {
    #[error("The question could not be processed just now. Please try again.")]
    Unavailable,
    #[error("Apiosk does not currently have a supported data source for this question. Try a different question or explore the available sources.")]
    Unsupported,
}

pub struct Parser {
    ask_v2: Option<crate::ask_v2::Client>,
    conversation: Option<conversation_reply::Client>,
    http: reqwest::Client,
    discovery_http: reqwest::Client,
    url: String,
    key: String,
    model: String,
    bazaar_search_url: String,
    analysis_model: String,
    intent_provider_sort: Option<String>,
    analysis_provider_sort: Option<String>,
    cache: moka::future::Cache<String, Arc<Intent>>,
    pub calls: AtomicU64,
}

/// The JSON shape of `Intent`, and of the replies that carry one, for models
/// that need it enforced. Values and constraints stay open maps; their keys
/// are checked by `Intent::validate`, not here.
/// Keeps only the schema's own properties, and reports every required one
/// that is still empty — whatever the model claimed in `missing`.
pub(crate) fn action_arguments(raw: &str, schema: &Value) -> anyhow::Result<Value> {
    let reply: Value = serde_json::from_str(raw.trim()).map_err(|_| anyhow::anyhow!("action_arguments_invalid"))?;
    let proposed = reply["arguments"].as_object().ok_or_else(|| anyhow::anyhow!("action_arguments_invalid"))?;
    let properties = schema["properties"].as_object();
    let arguments: serde_json::Map<String, Value> = proposed
        .iter()
        .filter(|(key, value)| properties.is_some_and(|p| p.contains_key(*key)) && !value.is_null() && value.as_str() != Some(""))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let missing: Vec<&str> = schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str).filter(|key| !arguments.contains_key(*key)).collect();
    Ok(json!({"arguments": arguments, "missing": missing}))
}

fn intent_reply_schema(surface: &str) -> Option<Value> {
    let text = json!({"type":"string"});
    let intent = json!({"type":"object","additionalProperties":false,
        "required":["subjects","goals","answer_operations","unresolved_references"],
        "properties":{
            "subjects":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["entity_ref","label","values"],
                "properties":{"entity_ref":{"type":["string","null"]},"label":text,"values":{"type":"object","additionalProperties":{"type":["string","number"]}}}}},
            "goals":{"type":"array","minItems":1,"maxItems":MAX_GOALS,"items":{"type":"object","additionalProperties":false,"required":["capability","subject","constraints"],
                "properties":{"capability":text,"subject":{"type":"integer","minimum":0},"constraints":{"type":"object"}}}},
            "answer_operations":{"type":"array","items":text},
            "unresolved_references":{"type":"array","items":text}}});
    let optional = json!({"anyOf":[intent,{"type":"null"}]});
    match surface {
        "intent" => Some(intent),
        "chat" | "capability-plan" => Some(json!({"type":"object","additionalProperties":false,"required":["message","discover","intent"],
            "properties":{"message":text,"discover":{"type":"boolean"},"intent":optional}})),
        "planning" => Some(json!({"type":"object","additionalProperties":false,"required":["message","intent"],
            "properties":{"message":text,"intent":optional}})),
        _ => None,
    }
}

/// Constrain the planning model to the vocabulary actually selected for this
/// request. A syntactically valid open map still allowed invented fact names.
fn capability_reply_schema(view: &Value, bindings: &Bindings, selected: &std::collections::BTreeSet<String>, matches_complete: bool, goal_limit: usize) -> Value {
    let mut schema = intent_reply_schema("chat").unwrap();
    schema["properties"]["output_bindings"] = source_grounding::schema();
    schema["required"].as_array_mut().unwrap().push(json!("output_bindings"));
    // Why there is no plan: only a genuine reason keeps a clarification.
    schema["properties"]["decline_reason"] = json!({"type":"string","enum":explicit_request::DECLINE_REASONS});
    schema["required"].as_array_mut().unwrap().push(json!("decline_reason"));
    if selected.is_empty() || !matches_complete {
        schema["properties"]["intent"] = json!({"type":"null"});
        schema["properties"]["discover"] = json!({"type":"boolean","enum":[false]});
        return schema;
    }
    let intent = &mut schema["properties"]["intent"]["anyOf"][0];
    let input_types = view["input_definitions"].as_array().into_iter().flatten()
        .filter_map(|definition| definition["type"].as_str().map(str::to_owned));
    let subjects = &mut intent["properties"]["subjects"];
    subjects["minItems"] = json!(1);
    subjects["maxItems"] = json!(8);
    subjects["items"] = bindings.subject_schema(input_types);
    let goals = &mut intent["properties"]["goals"];
    goals["minItems"] = json!(1);
    goals["maxItems"] = json!(goal_limit.clamp(1,MAX_GOALS));
    goals["items"]["properties"]["capability"]["enum"] = json!(selected);
    goals["items"]["properties"]["subject"]["maximum"] = json!(7);
    schema
}

/// Discovery has already scanned the directory. Keep the selected contracts,
/// dependencies and unavailable-source reasons without resending every source.
fn capability_planning_view(mut view: Value) -> Value {
    if let Some(sources) = view["sources"].as_array_mut() {
        sources.retain(|source| source["available"] == false
            || source["capabilities"].as_array().is_some_and(|caps| !caps.is_empty()));
        for source in sources {
            let repeated_reason = source["reason"].as_str().map(|reason|
                format!("{}: {reason}", source["name"].as_str().unwrap_or_default()));
            if let Some(fields) = source.as_object_mut() {
                fields.retain(|key, _| matches!(key.as_str(), "slug" | "name" | "capabilities" | "available" | "reason" | "readiness" | "capability_descriptions"));
            }
            if let Some(readiness) = source["readiness"].as_object_mut() {
                readiness.retain(|key, _| matches!(key.as_str(), "status" | "restrictions" | "coverage_notices"));
                if let Some(repeated) = repeated_reason {
                    if let Some(restrictions) = readiness.get_mut("restrictions").and_then(Value::as_array_mut) {
                        // This identical source restriction is already present
                        // in `reason`; retain it once, without truncating it.
                        restrictions.retain(|restriction| restriction.as_str() != Some(repeated.as_str()));
                    }
                }
            }
        }
    }
    view
}

fn chat_input(question: &str, task: &Task, catalog: &Catalog) -> Value {
    json!({"surface":"ask-chat","question":question,"context":task.parser_context(),"catalog":catalog.parser_view(),
        "source_groups":crate::eu_capabilities::catalog_view(catalog)})
}

pub(crate) fn validate_analysis(raw: &str, sources: &[Value], partial: bool) -> anyhow::Result<Value> {
    validate_analysis_limits(raw, sources, partial, 8, 6)
}

fn analysis_validation_reason(error: &anyhow::Error) -> &'static str {
    match error.to_string().as_str() {
        "numbers and links must come from source evidence" => "narrative_contains_numbers_or_links",
        "missing source field" => "non_scalar_or_missing_pointer",
        "invalid source pointer" => "invalid_source_pointer",
        "invented source" => "unknown_source_reference",
        "invalid evidence count" | "missing evidence" => "evidence_count",
        "long source text needs a short exact evidence quote" => "exact_quote_required",
        "evidence quote is not in the saved source" => "quote_not_verbatim",
        "invalid evidence quote" => "quote_empty_or_too_long",
        "missing answer value" | "invalid answer value slot" => "invalid_value_placeholder",
        "analysis too large" => "analysis_size_limit",
        "missing multi-source coverage" => "source_coverage_missing",
        "invalid source coverage gap" => "source_coverage_invalid",
        "missing observations" | "missing limitations" | "invalid narrative" | "empty analysis" => "invalid_analysis_shape",
        "unknown presentation source" => "unknown_presentation_source",
        "invalid presentation pointer" | "missing presentation field" | "presentation field must be scalar" | "missing column pointer" => "invalid_presentation_field",
        "table requires explicit relevant row_indices" | "invalid row index" | "invalid selected rows" | "duplicate presentation row" => "invalid_presentation_selection",
        "missing presentation kind" | "unknown presentation kind" | "missing presentation columns" | "invalid presentation column count" | "missing presentation label" | "invalid presentation label" | "missing presentation heading" | "missing presentation records" | "missing presentation rows" | "invalid presentation row" | "presentation row width mismatch" => "invalid_presentation_shape",
        _ => "invalid_analysis_schema",
    }
}

fn validate_analysis_limits(raw: &str, sources: &[Value], partial: bool, max_observations: usize, max_limitations: usize) -> anyhow::Result<Value> {
    validate_analysis_bounds(raw, sources, partial, max_observations, max_limitations, 4)
}

fn validate_analysis_bounds(raw: &str, sources: &[Value], partial: bool, max_observations: usize, max_limitations: usize, max_evidence: usize) -> anyhow::Result<Value> {
    let value: Value = serde_json::from_str(raw)?;
    let observations = value["observations"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("missing observations"))?;
    let limitations = value["limitations"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("missing limitations"))?;
    anyhow::ensure!(
        observations.len() <= max_observations && limitations.len() <= max_limitations,
        "analysis too large"
    );
    let prose = |v: &Value| -> anyhow::Result<String> {
        let text = v
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("invalid narrative"))?;
        anyhow::ensure!(
            !text.trim().is_empty()
                && text.len() <= 2000
                && !text.chars().any(|c| c.is_numeric())
                && !text.contains("://"),
            "numbers and links must come from source evidence"
        );
        Ok(text.to_owned())
    };
    let mut checked = Vec::new();
    for observation in observations {
        let mut text = prose(&observation["text"])?;
        let refs = observation["evidence"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("missing evidence"))?;
        anyhow::ensure!(
            !refs.is_empty() && refs.len() <= max_evidence,
            "invalid evidence count"
        );
        let mut evidence = Vec::new();
        for reference in refs {
            let source = sources
                .iter()
                .find(|s| s["result_ref"] == reference["result_ref"])
                .ok_or_else(|| anyhow::anyhow!("invented source"))?;
            let pointer = reference["pointer"]
                .as_str()
                .filter(|s| crate::report_assets::evidence_pointer(s))
                .ok_or_else(|| anyhow::anyhow!("invalid source pointer"))?;
            let field = source["document"]
                .pointer(pointer)
                .filter(|v| v.is_string() || v.is_number() || v.is_boolean())
                .ok_or_else(|| anyhow::anyhow!("missing source field"))?;
            let value = if let Some(quote) = reference.get("quote").filter(|value| !value.is_null()) {
                let quote = quote.as_str().filter(|quote| !quote.trim().is_empty() && quote.len() <= 1024)
                    .ok_or_else(|| anyhow::anyhow!("invalid evidence quote"))?;
                anyhow::ensure!(field.as_str().is_some_and(|text| text.contains(quote)), "evidence quote is not in the saved source");
                json!(quote)
            } else {
                anyhow::ensure!(field.as_str().is_none_or(|text| text.len() <= 1024), "long source text needs a short exact evidence quote");
                field.clone()
            };
            evidence.push(json!({"result_ref":reference["result_ref"],"pointer":pointer,"value":value,"quote":reference.get("quote"),
                "field":pointer.rsplit_once('/').and_then(|(parent, _)| source["document"].pointer(parent)).and_then(|parent|parent["key"].as_str()).unwrap_or(pointer),
                "subject":source["document"]["subject"],"source":source["document"]["source"]}));
        }
        // Numeric facts are copied from validated pointers, never model prose.
        for (index, letter) in ["a", "b", "c", "d", "e", "f", "g", "h"].iter().enumerate() {
            let slot = format!("{{{{{letter}}}}}");
            if text.contains(&slot) {
                let field = evidence
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("missing answer value"))?;
                let rendered = field["value"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| field["value"].to_string());
                text = text.replace(&slot, &rendered);
            }
        }
        anyhow::ensure!(
            !text.contains("{{") && !text.contains("}}"),
            "invalid answer value slot"
        );
        checked.push(json!({"text":text,"evidence":evidence}));
    }
    let mut caveats = limitations
        .iter()
        .map(prose)
        .collect::<anyhow::Result<Vec<_>>>()?;
    if partial && caveats.is_empty() {
        caveats.push(
            "Some source information is unavailable. The answer uses the available results.".into(),
        );
    }
    anyhow::ensure!(!checked.is_empty() || !caveats.is_empty(), "empty analysis");
    let follow_up_question = (partial || checked.is_empty() || !caveats.is_empty()).then(|| value["follow_up_question"].as_str()
        .and_then(|text| valid_follow_up(text).ok())).flatten();
    let presentation = crate::answer_presentation::validate(&value["presentation"], sources)?;
    Ok(json!({"status":if partial || checked.is_empty() || follow_up_question.is_some() {"partial"} else {"completed"},
        "observations":checked,"limitations":caveats,"follow_up_question":follow_up_question,"presentation":presentation}))
}

fn valid_follow_up(message: &str) -> anyhow::Result<String> {
    let message = message.trim();
    anyhow::ensure!(message.len() <= 350 && !message.is_empty() && !message.contains("://")
        && message.matches('?').count() + message.matches('？').count() == 1,
        "invalid clarification");
    Ok(message.to_owned())
}

fn missing_context_message(raw: &str) -> anyhow::Result<String> {
    let value: Value = serde_json::from_str(raw)?;
    let message = value["message"].as_str().ok_or_else(|| anyhow::anyhow!("missing clarification"))?.trim();
    valid_follow_up(message)
}

#[cfg(test)]
mod analysis_tests {
    use super::*;
    #[test]
    fn analysis_diagnostics_are_specific_fixed_labels_without_payloads() {
        for (message,expected) in [
            ("invalid source pointer","invalid_source_pointer"),
            ("missing presentation field","invalid_presentation_field"),
            ("table requires explicit relevant row_indices","invalid_presentation_selection"),
            ("missing observations","invalid_analysis_shape"),
            ("analysis too large","analysis_size_limit"),
            ("private user or model text","invalid_analysis_schema"),
        ] {assert_eq!(analysis_validation_reason(&anyhow::anyhow!(message)),expected);}
    }
    #[tokio::test]
    async fn missing_context_question_comes_from_model_without_a_source_call() {
        use axum::{routing::post, Json, Router};
        let app = Router::new().route("/", post(|Json(request): Json<Value>| async move {
            let input: Value = serde_json::from_str(request["messages"][1]["content"].as_str().unwrap()).unwrap();
            assert_eq!(input["question"], "Filings from Apple Inc");
            assert_eq!(input["gap"]["stage"], "before_quote");
            Json(json!({"choices":[{"message":{"content":"{\"message\":\"Are you after a complete filing list or a particular filing topic for Apple Inc?\"}"}}]}))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let parser = Parser::new(reqwest::Client::new(), url, "fixture".into(), "fixture".into());
        let reply = parser.ask_for_missing_context("Filings from Apple Inc", &Task::default(), &json!({"stage":"before_quote"})).await.unwrap();
        assert_eq!(reply, "Are you after a complete filing list or a particular filing topic for Apple Inc?");
        server.abort();
    }
    #[tokio::test]
    async fn analysis_repair_receives_the_rejected_answer_and_keeps_evidence_validation() {
        use axum::{routing::post, Json, Router};
        let invalid = json!({"observations":[{"text":"The fee is {{a}}.","evidence":[{"result_ref":"saved","pointer":"/data/text","quote":"€13 monthly"}]}],"limitations":[]}).to_string();
        let rejected = invalid.clone();
        let app = Router::new().route("/", post(move |Json(request): Json<Value>| {
            let rejected = rejected.clone();
            async move {
                let messages = request["messages"].as_array().unwrap();
                let answer = if messages.len() == 2 { rejected.clone() } else {
                    assert_eq!(messages[2]["role"], "assistant");
                    assert_eq!(messages[2]["content"], rejected);
                    assert!(messages[3]["content"].as_str().unwrap().contains("quote_not_verbatim"));
                    json!({"observations":[{"text":"The fee is {{a}}.","evidence":[{"result_ref":"saved","pointer":"/data/text","quote":"€12 monthly"}]}],"limitations":[]}).to_string()
                };
                Json(json!({"choices":[{"message":{"content":answer}}]}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let parser = Parser::new(reqwest::Client::new(), url, "fixture".into(), "fixture".into());
        let sources = vec![json!({"result_ref":"saved","document":{"data":{"text":"The plan costs €12 monthly."}}})];
        assert!(validate_analysis(&invalid, &sources, false).is_err());
        let answer = parser.analyze_with_scope(&json!(["Find the price"]), &sources, &BTreeMap::new(), true, &[]).await.unwrap();
        assert_eq!(answer["observations"][0]["text"], "The fee is €12 monthly.");
        server.abort();
    }

    #[tokio::test]
    async fn analysis_uses_portable_json_mode_and_resolves_saved_evidence() {
        use axum::{http::StatusCode, routing::post, Json, Router};
        let app = Router::new().route("/", post(|Json(request): Json<Value>| async move {
            // Reproduce the provider's rejection of the old schema request.
            if request["response_format"]["type"] != "json_object" {
                return (StatusCode::BAD_REQUEST, Json(json!({"error":"unsupported schema"})));
            }
            let answer = json!({"observations":[{"text":"The company is {{a}}.",
                "evidence":[{"result_ref":"saved-source","pointer":"/data/name"}]}],"limitations":[]});
            (StatusCode::OK, Json(json!({"choices":[{"message":{"content":answer.to_string()}}]})))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let parser = Parser::new(reqwest::Client::new(), url, "fixture".into(), "fixture".into());
        let sources = vec![json!({"result_ref":"saved-source","document":{"data":{"name":"Example B.V."}}})];
        for workflow in [false, true] {
            let answer = parser.analyze_with_scope(&json!(["Identify the company"]), &sources, &BTreeMap::new(), workflow, &[]).await.unwrap();
            assert_eq!(answer["status"], "completed");
            assert_eq!(answer["observations"][0]["text"], "The company is Example B.V..");
            assert_eq!(answer["observations"][0]["evidence"][0]["value"], "Example B.V.");
        }
        server.abort();
    }

    #[tokio::test]
    async fn a_model_that_never_addresses_a_requirement_forces_partial_status() {
        use axum::{routing::post, Json, Router};
        let sources = vec![json!({"result_ref":"saved-source","document":{"subject":{"entity_ref":Uuid::nil().to_string()},"data":{"name":"Example B.V."}}})];
        let requirements = vec![
            crate::answer_blocks::Requirement {
                id: "r1".into(), subject: Some(0), subject_label: Some("Example".into()), entity_ref: Some(Uuid::nil().to_string()),
                criterion: "company.registry-profile".into(), criterion_kind: crate::answer_blocks::CriterionKind::Capability, period: None,
                status: crate::answer_blocks::RequirementStatus::Pending, reason: None, answered_by: vec![], missing_ids: vec![], evidence: vec![],
            },
            crate::answer_blocks::Requirement {
                id: "r2".into(), subject: None, subject_label: None, entity_ref: None, criterion: "Confirm the address".into(),
                criterion_kind: crate::answer_blocks::CriterionKind::Operation, period: None,
                status: crate::answer_blocks::RequirementStatus::Pending, reason: None, answered_by: vec![], missing_ids: vec![], evidence: vec![],
            },
        ];
        for (claims, expected_status) in [
            // No requirement_status at all: an older/fixture model keeps its existing behavior.
            (json!(null), "completed"),
            // r2 is never addressed: forced to partial, with an explicit not_addressed reason.
            (json!([{"requirement_id":"r1","status":"answered","answered_by":["o1"],
                "evidence":[{"result_ref":"saved-source","pointer":"/data/name"}]}]), "partial"),
        ] {
            let claims = claims.clone();
            let app = Router::new().route("/", post(move |Json(_): Json<Value>| {
                let claims = claims.clone();
                async move {
                    let mut answer = json!({"observations":[{"text":"The company is {{a}}.",
                        "evidence":[{"result_ref":"saved-source","pointer":"/data/name"}]}],"limitations":[]});
                    if !claims.is_null() {
                        answer["requirement_status"] = claims;
                    }
                    Json(json!({"choices":[{"message":{"content":answer.to_string()}}]}))
                }
            }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let parser = Parser::new(reqwest::Client::new(), url, "fixture".into(), "fixture".into());
            let answer = parser.analyze_with_scope(&json!(["Identify the company"]), &sources, &BTreeMap::new(), false, &requirements).await.unwrap();
            assert_eq!(answer["status"], expected_status, "{answer}");
            server.abort();
        }
    }

    #[test]
    fn workflow_reports_can_cover_ten_checks_without_relaxing_evidence_validation() {
        let sources=vec![json!({"result_ref":"saved","document":{"data":{"name":"Example","year":2026}}})];
        let observation=json!({"text":"{{a}} was reported in {{b}}.","evidence":[{"result_ref":"saved","pointer":"/data/name"},{"result_ref":"saved","pointer":"/data/year"}]});
        let mut answer=json!({"observations":vec![observation;10],"limitations":[]});
        assert!(validate_analysis(&answer.to_string(),&sources,false).is_err());
        let checked=validate_analysis_limits(&answer.to_string(),&sources,false,24,12).unwrap();
        assert_eq!(checked["observations"].as_array().unwrap().len(),10);
        answer["observations"][9]["evidence"][0]["pointer"]=json!("/data/invented");
        assert!(validate_analysis_limits(&answer.to_string(),&sources,false,24,12).is_err());
    }
    #[test]
    fn answer_slots_copy_only_verified_values() {
        let sources = vec![
            json!({"result_ref":"saved","document":{"data":{"postcode":3011,"population":20440,"year":2024}}}),
        ];
        let mut answer = json!({"observations":[{"text":"Postcode {{a}} had {{b}} residents in {{c}}.","evidence":[{"result_ref":"saved","pointer":"/data/postcode"},{"result_ref":"saved","pointer":"/data/population"},{"result_ref":"saved","pointer":"/data/year"}]}],"limitations":[]});
        let checked = validate_analysis(&answer.to_string(), &sources, false).unwrap();
        assert_eq!(
            checked["observations"][0]["text"],
            "Postcode 3011 had 20440 residents in 2024."
        );
        answer["observations"][0]["text"] = json!("Population {{d}}.");
        assert!(validate_analysis(&answer.to_string(), &sources, false).is_err());
        answer["observations"][0]["text"] = json!("Population {{invented}}.");
        assert!(validate_analysis(&answer.to_string(), &sources, false).is_err());
    }
    #[test]
    fn multisource_slots_are_exact_and_bounded_without_widening_single_source() {
        let sources = vec![json!({"result_ref":"saved","document":{"data":{"values":[0,false,"00007",-12,25,2024,"EUR",99,100]}}})];
        let refs = (0..8).map(|i|json!({"result_ref":"saved","pointer":format!("/data/values/{i}")})).collect::<Vec<_>>();
        let mut answer=json!({"observations":[{"text":"Values {{a}}, {{b}}, {{c}}, {{d}}, {{e}}, {{f}}, {{g}}, {{h}}.","evidence":refs}],"limitations":[]});
        let check=|v:&Value| validate_analysis_bounds(&v.to_string(),&sources,false,8,6,8);
        assert_eq!(check(&answer).unwrap()["observations"][0]["text"],"Values 0, false, 00007, -12, 25, 2024, EUR, 99.");
        assert!(validate_analysis(&answer.to_string(),&sources,false).is_err(),"single-source still allows four");
        answer["observations"][0]["evidence"].as_array_mut().unwrap().push(json!({"result_ref":"saved","pointer":"/data/values/8"}));
        assert!(check(&answer).is_err(),"ninth citation rejected");
        answer["observations"][0]["evidence"].as_array_mut().unwrap().pop();
        answer["observations"][0]["text"]=json!("Unsupported {{i}}.");
        assert!(check(&answer).is_err());
        answer["observations"][0]["text"]=json!("Missing {{h}}.");
        answer["observations"][0]["evidence"].as_array_mut().unwrap().pop();
        assert!(check(&answer).is_err());
        answer["observations"][0]["text"]=json!("Invented literal 777.");
        assert!(check(&answer).is_err(),"numeric facts must still be copied from source evidence");
    }
    #[test]
    fn web_evidence_uses_only_short_exact_quotes_from_the_saved_page() {
        let sources = vec![json!({"result_ref":"page","document":{"data":{"text":format!("{}Plan costs $12 per seat monthly.", "long page ".repeat(1000))}}})];
        let mut answer = json!({"observations":[{"text":"Pricing: {{a}}", "evidence":[{"result_ref":"page","pointer":"/data/text"}]}],"limitations":[]});
        assert!(validate_analysis(&answer.to_string(), &sources, false).is_err());
        answer["observations"][0]["evidence"][0]["quote"] = Value::Null;
        assert!(validate_analysis(&answer.to_string(), &sources, false).is_err(), "null cannot bypass the long-text bound");
        answer["observations"][0]["evidence"][0]["quote"] = json!("$12 per seat monthly");
        let checked = validate_analysis(&answer.to_string(), &sources, false).unwrap();
        assert_eq!(checked["observations"][0]["text"], "Pricing: $12 per seat monthly");
        assert!(checked.to_string().len() < 1024);
        answer["observations"][0]["evidence"][0]["quote"] = json!("$10 per seat monthly");
        assert!(validate_analysis(&answer.to_string(), &sources, false).is_err());
    }
    #[test]
    fn evidence_is_resolved_from_saved_data_and_invented_values_are_rejected() {
        let sources = vec![
            json!({"result_ref":"saved","document":{"subject":{"label":"HEMA"},"data":{"year":2025,"assets":123456}}}),
        ];
        let mut answer = json!({"observations":[{"text":"The source reports assets.","evidence":[{"result_ref":"saved","pointer":"/data/assets","value":999999}]}],"limitations":[]});
        let checked = validate_analysis(&answer.to_string(), &sources, true).unwrap();
        assert_eq!(checked["observations"][0]["evidence"][0]["value"], 123456);
        assert_eq!(checked["status"], "partial");
        answer["observations"][0]["evidence"][0]["quote"] = Value::Null;
        assert_eq!(validate_analysis(&answer.to_string(), &sources, true).unwrap()["observations"][0]["evidence"][0]["value"], 123456);
        answer["observations"][0]["text"] = json!("Assets are 999999.");
        assert!(validate_analysis(&answer.to_string(), &sources, false).is_err());
        answer["observations"][0]["text"] = json!("Reported assets.");
        answer["observations"][0]["evidence"][0]["pointer"] = json!("/data/profit");
        assert!(validate_analysis(&answer.to_string(), &sources, false).is_err());
        answer["observations"][0]["evidence"][0]["pointer"] = json!("/data/year");
        answer["observations"][0]["evidence"][0]["result_ref"] = json!("invented");
        assert!(validate_analysis(&answer.to_string(), &sources, false).is_err());
    }
}
impl Parser {
    /// Conversational clarification only. It cannot create a plan or source call.
    pub async fn ask_for_missing_context(&self, question: &str, task: &Task, gap: &Value) -> anyhow::Result<String> {
        let payload = json!({"model":self.model,"temperature":0,"max_tokens":240,
            "reasoning":{"effort":"minimal","exclude":true},"provider":{"require_parameters":true},
            "response_format":{"type":"json_object"},"messages":[
                {"role":"system","content":include_str!("../contracts/missing-context-prompt.txt")},
                {"role":"user","content":json!({"question":question,"context":task.parser_context(),"gap":gap}).to_string()}
            ]});
        self.completion("clarification", &payload, missing_context_message).await
    }

    /// Proposes arguments for one connected-app action. It only proposes: the
    /// App shows them to the person, who confirms or edits before anything runs.
    pub async fn propose_action_arguments(&self, question: &str, action: &crate::integrations::AppAction, results: &Value) -> anyhow::Result<Value> {
        let payload = json!({"model":self.model,"temperature":0,"max_tokens":1200,
            "reasoning":{"effort":"minimal","exclude":true},"provider":{"require_parameters":true},
            "response_format":{"type":"json_object"},"messages":[
                {"role":"system","content":include_str!("../contracts/action-arguments-prompt.txt")},
                {"role":"user","content":json!({"question":question,"results":results,
                    "action":{"slug":action.slug,"name":action.name,"description":action.description,"input_schema":action.input_schema}}).to_string()}
            ]});
        let schema = action.input_schema.clone();
        self.completion("action-arguments", &payload, move |raw| action_arguments(raw, &schema)).await
    }

    pub async fn analyze(
        &self,
        operations: &Value,
        sources: &[Value],
        failures: &BTreeMap<String, Value>,
        requirements: &[Requirement],
    ) -> anyhow::Result<Value> {
        if failures.is_empty() {
            if let Some(answer) = empty_source_analysis(sources) { return Ok(answer); }
        }
        self.analyze_with_scope(operations, sources, failures, false, requirements).await
    }

    pub async fn analyze_workflow(&self, operations: &Value, sources: &[Value], failures: &BTreeMap<String, Value>, requirements: &[Requirement]) -> anyhow::Result<Value> {
        if failures.is_empty() {
            if let Some(answer) = empty_source_analysis(sources) { return Ok(answer); }
        }
        self.analyze_with_scope(operations, sources, failures, true, requirements).await
    }

    async fn analyze_with_scope(&self, operations: &Value, sources: &[Value], failures: &BTreeMap<String, Value>, workflow: bool, requirements: &[Requirement]) -> anyhow::Result<Value> {
        let (max_observations, max_limitations) = if workflow { (24, 12) } else { (8, 6) };
        // Large saved documents are narrowed to question-relevant evidence;
        // citations are still validated against the originals.
        let mut input = json!({"answer_operations":operations,"sources":[],"unavailable_steps":failures,"requested_parts":requirements});
        let budget = crate::evidence_selection::ANALYSIS_INPUT_BYTES.saturating_sub(serde_json::to_vec(&input)?.len());
        input["sources"] = json!(crate::evidence_selection::project(sources, operations, budget));
        let evidence_index = if analysis_evidence::multiple_providers(sources) {
            let reviewed_projection = crate::cbs_comparison::evidence_projection(sources, requirements);
            let selected = reviewed_projection.as_deref().unwrap_or(sources);
            let mut projection_budget = budget / 2;
            loop {
                let projected = analysis_evidence::project(selected, operations, projection_budget);
                let index = analysis_evidence::Index::new(&projected, sources);
                input["sources"] = json!(index.input_sources());
                if serde_json::to_vec(&input)?.len() <= crate::evidence_selection::ANALYSIS_INPUT_BYTES {
                    break Some(index);
                }
                anyhow::ensure!(projection_budget > 4096, "analysis input too large");
                projection_budget /= 2;
            }
        } else { None };
        anyhow::ensure!(
            serde_json::to_vec(&input)?.len() <= crate::evidence_selection::ANALYSIS_INPUT_BYTES,
            "analysis input too large"
        );
        if sources.is_empty() {
            // These messages have already been projected through billing::public_error.
            // Keep operational failures visible instead of making them look like
            // an empty search result that the buyer should refine.
            let mut limitations: Vec<Value> = Vec::new();
            for failure in failures.values() {
                if let Some(message) = failure["message"].as_str().filter(|message| !message.is_empty()) {
                    let note = json!(message);
                    if !limitations.contains(&note) { limitations.push(note); }
                    if limitations.len() == max_limitations { break; }
                }
            }
            if limitations.is_empty() {
                limitations.push(json!("No source result is available to support an analysis."));
            }
            return Ok(
                json!({"status":"partial","observations":[],"limitations":limitations}),
            );
        }
        let mut prompt = if evidence_index.is_some() {
            include_str!("../contracts/multisource-analysis-prompt.txt")
        } else { include_str!("../contracts/analysis-prompt.txt") }.to_owned();
        if workflow {
            prompt = prompt.replace("At most eight observations, four evidence fields each, and six limitations.", "At most twenty-four observations, four evidence fields each, and twelve limitations. Cover every requested check with a supported finding or an explicit unknown in limitations. For shortlists, name each candidate and explain its fit; for comparisons, group findings by company. A completed retrieval is not a passed check.");
            prompt = prompt.replace("At most eight observations, each at most eight evidence items.", "At most twenty-four observations, each at most eight evidence items, and twelve limitations. Cover each requested check with a supported finding or an explicit unknown.");
        }
        // Multi-provider synthesis uses a model that supports the bounded schema.
        // Single-provider calls keep their existing portable JSON mode. Both
        // paths still validate every source pointer, quote and numeric claim.
        let payload = json!({"model":self.model,"temperature":0,"max_tokens":if workflow || evidence_index.is_some() {8192} else {4096},
            "reasoning":{"effort":"low","exclude":true},"provider":{"require_parameters":true},
            "response_format":if evidence_index.is_some() {analysis_schema::format(max_observations,max_limitations)} else {json!({"type":"json_object"})},"messages":[
                {"role":"system","content":prompt},
                {"role":"user","content":input.to_string()}]});
        self.completion(if evidence_index.is_some() { "multi-source-analysis" } else { "analysis" }, &payload, |raw| {
            let expanded;
            let raw = if let Some(index) = &evidence_index {
                let mut value: Value = serde_json::from_str(&index.expand(raw)?)?;
                analysis_period::hydrate(&mut value, requirements, sources);
                expanded = serde_json::to_string(&value)?;
                expanded.as_str()
            } else { raw };
            let mut checked = validate_analysis_bounds(raw, sources, !failures.is_empty(), max_observations, max_limitations, if evidence_index.is_some() { 8 } else { 4 })?;
            analysis_source_coverage::validate(raw, &mut checked, sources, failures)?;
            // Only a model that engages with requested_parts is held to it:
            // a fixture or an older model that never mentions requirement_status
            // keeps its existing behavior unchanged.
            if !requirements.is_empty() {
                if let Some(status) = serde_json::from_str::<Value>(raw).ok().and_then(|value| value.get("requirement_status").cloned()) {
                    let claims: Vec<RequirementClaim> = serde_json::from_value(status).unwrap_or_default();
                    let completeness = crate::completeness::reconcile(requirements.to_vec(), &claims, sources);
                    if !completeness.complete {
                        checked["status"] = json!("partial");
                    }
                    checked["completeness"] = serde_json::to_value(&completeness)?;
                }
            }
            Ok(checked)
        })
        .await
    }
    pub fn new(http: reqwest::Client, url: String, key: String, model: String) -> Self {
        Self {
            ask_v2: None,
            conversation: None,
            http,
            discovery_http: reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().expect("public discovery HTTP client"),
            url,
            key,
            analysis_model: model.clone(),
            model,
            bazaar_search_url: crate::capability_intake::BAZAAR_SEARCH.into(),
            intent_provider_sort: None,
            analysis_provider_sort: None,
            cache: moka::future::Cache::builder()
                .max_capacity(1024)
                .time_to_live(Duration::from_secs(600))
                .build(),
            calls: AtomicU64::new(0),
        }
    }
    pub fn with_ask_v2(mut self, client: crate::ask_v2::Client) -> Self {
        self.ask_v2 = Some(client);
        self
    }
    pub fn ask_v2(&self) -> Option<&crate::ask_v2::Client> {
        self.ask_v2.as_ref()
    }
    /// Written replies from [`CHAT_MODEL`] when a message is not yet a clear
    /// data question. Without it, the planner's own reply is shown.
    pub fn with_conversation_replies(mut self, client: conversation_reply::Client) -> Self {
        self.conversation = Some(client);
        self
    }
    pub fn conversation_replies(&self) -> Option<&conversation_reply::Client> {
        self.conversation.as_ref()
    }
    /// The reply to a question the planner genuinely declined (conversation,
    /// an unclear request, a requirement no source covers), written by
    /// [`CHAT_MODEL`] from the conversation so far and the live catalog, with
    /// the planner's text as its draft. None keeps the planner's text: no
    /// genuine reason, no configured client, a failure or the deadline.
    pub async fn decline_reply(&self, question: &str, task: &Task, catalog: &Catalog, reply: &ChatReply) -> Option<String> {
        if reply.discover { return None; }
        let reason = conversation_reply::Reason::from_decline(reply.decline_reason.as_deref())?;
        let client = self.conversation.as_ref()?;
        let intake = task.capability_intake.as_ref();
        let request = conversation_reply::Request {
            question, earlier: conversation_reply::earlier_turns(task), reason,
            language: intake.map(|intake| intake.parsed_request.language.as_str()),
            draft: Some(&reply.message),
        };
        client.reply(&request, conversation_reply::catalog_summary(catalog, intake)).await.ok().map(|written| written.text)
    }
    pub fn bazaar_search_url(&self) -> &str { &self.bazaar_search_url }

    pub fn discovery_http(&self) -> &reqwest::Client {
        &self.discovery_http
    }
    /// Isolated HTTP fixtures may replace the public discovery service. An
    /// arbitrary external discovery host is never accepted by this override.
    #[doc(hidden)]
    pub fn with_local_bazaar(mut self, url: String) -> anyhow::Result<Self> {
        let parsed = url::Url::parse(&url)?;
        anyhow::ensure!(parsed.scheme() == "http" && parsed.host_str() == Some("127.0.0.1"), "fixture discovery must be loopback");
        self.bazaar_search_url = url;
        Ok(self)
    }
    /// Explicit stage routing; defaults preserve the configured model. Provider
    /// sorting is an OpenRouter extension and is never sent to other endpoints.
    pub fn with_routing(mut self, analysis_model: Option<String>, intent_sort: Option<String>, analysis_sort: Option<String>) -> anyhow::Result<Self> {
        if let Some(model) = analysis_model {
            anyhow::ensure!(!model.trim().is_empty(), "empty analysis model");
            self.analysis_model = model.trim().to_owned();
        }
        for sort in [&intent_sort, &analysis_sort].into_iter().flatten() {
            anyhow::ensure!(matches!(sort.as_str(), "latency" | "throughput" | "price"), "invalid provider sort");
        }
        self.intent_provider_sort = intent_sort;
        self.analysis_provider_sort = analysis_sort;
        Ok(self)
    }

    fn routed_payload(&self, surface: &str, payload: &Value) -> Value {
        let mut payload = payload.clone();
        let analysis = matches!(surface, "analysis" | "multi-source-analysis");
        payload["model"] = json!(if surface == "capability-plan" { CAPABILITY_PLAN_MODEL } else if surface == "multi-source-analysis" { MULTISOURCE_ANALYSIS_MODEL } else if analysis { &self.analysis_model } else { &self.model });
        if payload["model"].as_str().is_some_and(|model| model.starts_with("google/gemma") || model == MODEL || model == CHAT_MODEL) {
            // Neither Qwen3 Instruct 2507 nor Gemma 3 providers support a
            // reasoning parameter; with require_parameters it would match no
            // provider at all. Gemma 4 supports it, but needs thinking disabled.
            if payload["model"].as_str().is_some_and(|model| model.starts_with("google/gemma-3-") || model == MODEL || model == CHAT_MODEL) {
                payload.as_object_mut().unwrap().remove("reasoning");
            } else if payload.get("reasoning").is_some() {
                payload["reasoning"] = json!({"enabled": false});
            }
            // These models follow the prompt's shape less closely than Gemini
            // (Gemma wrote answer_operations as objects), so they get the schema.
            if payload["response_format"]["type"] == "json_object" {
                if let Some(schema) = intent_reply_schema(surface) {
                    payload["response_format"] = json!({"type":"json_schema","json_schema":{"name":surface.replace('-', "_"),"strict":false,"schema":schema}});
                }
            }
        }
        if matches!(surface, "multi-source-analysis" | "capability-plan") {
            payload.as_object_mut().unwrap().remove("temperature");
        }
        if surface == "capability-plan" && payload["response_format"]["type"] == "json_schema" {
            // Intent has schema-validated dynamic input/policy maps. OpenAI's
            // strict subset requires all map keys to be fixed and required;
            // retain the schema guidance and enforce the complete contract in
            // Bindings, Intent::validate and prequote input/output coverage.
            payload["response_format"]["json_schema"]["strict"] = json!(false);
        }
        if url::Url::parse(&self.url).is_ok_and(|url| url.host_str() == Some("openrouter.ai")) {
            let sort = if analysis { &self.analysis_provider_sort } else { &self.intent_provider_sort };
            if let Some(sort) = sort {
                payload["provider"]["sort"] = json!(sort);
            }
        }
        payload
    }
    pub async fn chat(
        &self,
        question: &str,
        task: &Task,
        catalog: &Catalog,
    ) -> anyhow::Result<ChatReply> {
        self.chat_selected(question, None, task, catalog).await
    }
    pub async fn chat_selected(
        &self,
        question: &str,
        selection: Option<&ModelSelection>,
        task: &Task,
        catalog: &Catalog,
    ) -> anyhow::Result<ChatReply> {
        if selection.is_none() {
            if let Some(reply) = crate::eu_capabilities::browse_reply(question, catalog) {
                return Ok(reply);
            }
        }
        self.chat_baseline(question, selection, task, catalog).await
    }

    async fn chat_baseline(&self, question: &str, selection: Option<&ModelSelection>, task: &Task, catalog: &Catalog) -> anyhow::Result<ChatReply> {
        let view = self.planning_view(question, task, catalog).await?;
        self.chat_with_view(question, selection, task, catalog, view, None).await
    }

    /// Connected agents and API callers read a question the way the Ask page
    /// does: the Ask v2 capability parse, then the deterministic source search.
    /// No model pass ever sees the whole catalog.
    pub async fn ask_intake(&self, question: &str, task: &Task, catalog: &Catalog, earlier: Option<&crate::capability_intake::Intake>) -> anyhow::Result<crate::capability_intake::Intake> {
        use crate::capability_intake as intake;
        let parsed = {
            let client = self.ask_v2.as_ref().ok_or(ModelFailure::Unavailable)?;
            // Interpret the user's requirements before discovering services.
            // An arbitrary first hundred catalog slugs biases extraction toward
            // unrelated providers and their fields. The App uses this same
            // provider-independent interpretation contract.
            let known = Vec::new();
            // A follow-up is parsed as the Ask page sends it: the previous
            // request is context only, and its values stay literal.
            let asked = match task.conversation.last() {
                Some(_) => format!("Previous conversation (context only): {}\n\nCurrent request: {question}", task.parser_context()),
                None => question.to_owned(),
            };
            let literals = capability_binding::literal_context(question, task, None);
            let unavailable = |error: anyhow::Error| {
                tracing::warn!(surface = "ask-intake", reason = %error, "Ask v2 capability parse failed");
                anyhow::Error::from(ModelFailure::Unavailable)
            };
            // One correction, as the Ask page gives its parser.
            let first = client.parse(&asked, &known, None).await.map_err(unavailable)?;
            match intake::normalize_with_context(&first.text, question, &literals, &first.model) {
                Ok(parsed) => parsed,
                Err(error) => {
                    let second = client.parse(&asked, &known, Some(&error.to_string())).await.map_err(unavailable)?;
                    intake::normalize_with_context(&second.text, question, &literals, &second.model).map_err(unavailable)?
                }
            }
        };
        let mut matches = crate::ask_v2::search::search(&parsed, catalog);
        let mut selected_capabilities: std::collections::BTreeSet<String> = matches.iter().flat_map(|m| m.operations.iter().cloned()).collect();
        // A follow-up about the same subject continues the earlier request
        // ("yes, search by name first"): the operations found for it remain
        // valid ways to answer. A new subject starts from its own search.
        if let Some(earlier) = earlier.filter(|_| !task.conversation.is_empty()) {
            let subjects: std::collections::BTreeSet<_> = earlier.parsed_request.subjects.iter().map(|s| s.label.to_lowercase()).collect();
            for found in &mut matches {
                let subject = parsed.capabilities.iter().find(|c| c.slug == found.slug).and_then(|c| c.subject_id.as_deref())
                    .and_then(|id| parsed.subjects.iter().find(|s| s.id == id));
                if subject.is_some_and(|s| subjects.contains(&s.label.to_lowercase())) {
                    found.operations.extend(earlier.selected_capabilities.iter().cloned());
                    selected_capabilities.extend(earlier.selected_capabilities.iter().cloned());
                }
            }
        }
        Ok(intake::Intake {
            selected_capabilities, parsed_request: parsed, matches,
            apiosk_sources_searched: intake::source_count(catalog), catalog_version: catalog.version.clone(),
        })
    }

    /// Ask must finish capability extraction before any source search starts.
    pub async fn prepare_ask(&self, question: &str, selection: Option<&ModelSelection>, task: &Task, catalog: &Catalog) -> anyhow::Result<crate::capability_intake::Intake> {
        self.prepare_ask_with_bazaar(question, selection, task, catalog, &self.bazaar_search_url).await
    }

    pub(crate) async fn prepare_ask_with_bazaar(&self, question: &str, selection: Option<&ModelSelection>, task: &Task, catalog: &Catalog, bazaar: &str) -> anyhow::Result<crate::capability_intake::Intake> {
        use crate::capability_intake as intake;
        let known: Vec<String> = Vec::new();
        let literals = capability_binding::literal_context(question, task, selection);
        let payload = json!({"model":self.model,"temperature":0,"max_tokens":3000,
            "reasoning":{"effort":"minimal","exclude":true},"response_format":{"type":"json_object"},
            "messages":[{"role":"system","content":include_str!("../contracts/capability-intake-prompt.txt")},
                {"role":"user","content":json!({"question":question,"known_slugs":known,"previous_context":task.parser_context(),"selection":selection.map(|s| &s.view)}).to_string()}]});
        let parsed = self.completion("capability-intake", &payload, |raw| intake::normalize_with_context(raw, question, &literals, &self.model)).await?;
        let mut input = intake::search_model_input(&intake::search_input(&parsed, task, catalog));
        if let Some(selection) = selection { input["selection"] = selection.view.clone(); }
        let queries = intake::queries(&parsed, task);
        let selected = if queries.is_empty() || input["candidates"].as_object().is_some_and(|c| c.is_empty()) {
            queries.keys().map(|slug| (slug.clone(), Default::default())).collect()
        } else {
            let payload = json!({"model":self.model,"temperature":0,"max_tokens":2500,
                "reasoning":{"effort":"minimal","exclude":true},"provider":{"require_parameters":true},
                "response_format":{"type":"json_schema","json_schema":{"name":"capability_search","strict":true,"schema":intake::search_reply_schema(&input)}},
                "messages":[{"role":"system","content":include_str!("../contracts/capability-source-search-prompt.txt")},
                    {"role":"user","content":input.to_string()}]});
            self.completion("capability-search", &payload, |raw| intake::decode_matches(raw, &input)).await?
        };
        let mut matches = intake::local_matches(&parsed, &queries, &selected, catalog);
        // Directory discovery is public and carries no user credentials. Never
        // follow redirects to an arbitrary host or fetch the discovered resource.
        intake::add_coinbase(&self.discovery_http, &mut matches, bazaar).await;
        Ok(intake::Intake { parsed_request: parsed, matches,
            apiosk_sources_searched: intake::source_count(catalog), catalog_version: catalog.version.clone(),
            selected_capabilities: selected.into_values().flatten().collect() })
    }

    pub async fn chat_from_capabilities(&self, question: &str, selection: Option<&ModelSelection>, task: &Task, catalog: &Catalog, intake: &crate::capability_intake::Intake) -> anyhow::Result<ChatReply> {
        if let Some(reply) = plan_coverage::uncovered_reply(intake) { return Ok(reply); }
        if selection.is_none_or(|selection|selection.view["mode"]=="include") {
            // The AI extraction owns the request. Plan any fully covered
            // retrieval from live contracts, irrespective of its wording,
            // company name, provider or membership of the example list.
            let literals = capability_binding::literal_context(question, task, selection);
            if let Some(reply) = plan_coverage::single_retrieval_reply(intake, catalog, question, &literals, task) { return Ok(reply); }
            if let Some(reply) = plan_coverage::multiple_retrieval_reply(intake, catalog, question, &literals, task) { return Ok(reply); }
        }
        let view = capability_planning_view(catalog.selected_parser_view(&intake.selected_capabilities));
        self.chat_with_view(question, selection, task, catalog, view, Some(intake)).await
    }

    async fn chat_with_view(&self, question: &str, selection: Option<&ModelSelection>, task: &Task, catalog: &Catalog, view: Value, intake: Option<&crate::capability_intake::Intake>) -> anyhow::Result<ChatReply> {
        // The chat route has no interpretation cache: every question is read anew.
        crate::telemetry::set_parser_cache(Cache::Miss);
        let mut input=chat_input(question, task, catalog);
        input["catalog"]=view;
        let bindings = intake.map(|i| Bindings::new(&i.parsed_request, question, task, selection));
        if let Some(bindings) = &bindings { input["bindings"] = bindings.view(); }
        if let Some(intake) = intake {
            input.as_object_mut().unwrap().remove("source_groups");
            input["parsed_request"] = json!(intake.parsed_request);
            input["source_fields"] = source_grounding::view(intake, catalog);
            input["source_matches"] = json!(intake.parsed_request.capabilities.iter().enumerate().filter_map(|(index,cap)| explicit_request::request_match(intake,index).map(|m|
                json!({"parsed_capability":cap.slug,"subject_ref":cap.subject_id,"apiosk_operations":m.operations,
                    "apiosk_results":m.apiosk,
                    "additional_coinbase_results":m.coinbase,"coinbase_status":m.coinbase_status})
            )).collect::<Vec<_>>());
            input["discovery_rules"] = json!("Interpret the complete original request using parsed_request and the matched Apiosk catalog. Cover every retrieval capability and its subject. A combined retrieval may use several matched goals only when their selected contracts jointly prove all requested outputs and each goal adds requested scope. One goal may cover several requirements only when one matched contract explicitly returns all their output facts; never duplicate a goal. Keep answer-only instructions in answer_operations. Dependencies belong to the backend resolver. Do not discard unmatched capabilities or coverage requirements. If an essential capability has no executable match, ask a focused clarification or explain the gap instead of quoting a partial substitute. A missing match means this specific requirement is not covered; do not claim an entire provider is absent. Apiosk results include registered endpoints without executable contracts; their discovery_only or unverified status must be preserved. Discovery results from Apiosk and Coinbase are metadata only: they cannot be quoted or executed, are not evidence answering the question, and may offer broader context rather than the subject-specific output requested. Only apiosk_operations identify executable matches. Preserve source selections and the single total-price approval flow.");
        }
        if let Some(selection) = selection { input["selection"] = selection.view.clone(); }
        // Entered values are literals too; the question is still sent as typed.
        let literals = if intake.is_some() {
            Some(capability_binding::literal_context(question, task, selection))
        } else {
            selection.map(|selection| std::iter::once(question).chain(selection.literals.iter().map(String::as_str)).collect::<Vec<_>>().join("\n"))
        };
        let system = if intake.is_some() {
            format!("{}\n\nThe language code for your message is {}. Write the entire message in that language, including clarification questions.", include_str!("../contracts/capability-planning-prompt.txt"), intake.unwrap().parsed_request.language)
        } else {
            format!("{}\n\nIntent interpretation rules (place their output INSIDE the intent field of the chat reply):\n{}\n\nReturn one object with message, discover, intent. For a concrete request, discover=true and intent is the complete Intent object. For conversation or clarification, discover=false and intent=null. Never return a bare Intent object.", include_str!("../contracts/ask-chat-prompt.txt"), include_str!("../contracts/parser-prompt.txt"))
        };
        let response_format = if let Some(intake) = intake {
            json!({"type":"json_schema","json_schema":{"name":"capability_plan","strict":true,
                "schema":capability_reply_schema(&input["catalog"], bindings.as_ref().unwrap(), &intake.selected_capabilities, plan_coverage::retrievals_matched(intake), intake.parsed_request.capabilities.iter().filter(|cap|!crate::capability_intake::answer_instruction(cap)).map(|cap|cap.outputs.len().max(1)).sum())}})
        } else {
            json!({"type":"json_object"})
        };
        let payload = json!({
            "model": self.model, "temperature": 0, "max_tokens": 4096,
            "reasoning": {"effort":"minimal", "exclude":true},
            "provider": {"require_parameters":true},
            "response_format": response_format,
            "messages": [
                {"role":"system","content":system},
                {"role":"user","content":input.to_string()}
            ]
        });
        // An explicit data request declined without a genuine reason gets one
        // repair; declined again, it is planned from the source search.
        let declined = std::sync::atomic::AtomicBool::new(false);
        let surface = if intake.is_some() { "capability-plan" } else { "chat" };
        let result = self.completion(surface, &payload, |raw| {
            let mut reply: ChatReply = match &bindings {
                Some(bindings) => bindings.decode_reply(raw)?,
                None => serde_json::from_str(raw)?,
            };
            reply.validate()?;
            if let Some(intake) = intake { source_grounding::validate(&reply.output_bindings, intake, catalog)?; }
            if let Some(intake) = intake.filter(|intake| !reply.discover && explicit_request::data_request(intake)) {
                if !explicit_request::genuine_decline(reply.decline_reason.as_deref(), &reply.message) {
                    anyhow::ensure!(declined.swap(true, Ordering::Relaxed), explicit_request::DECLINED);
                    tracing::warn!(surface, "planner declined an explicit request twice; planning from the source search");
                    reply = explicit_request::planned_reply(intake, catalog).ok_or_else(|| anyhow::anyhow!(explicit_request::DECLINED))?;
                }
            }
            if intake.is_some() {
                anyhow::ensure!(!reply.discover || reply.intent.is_some(), "discovery requires a matched intent");
                anyhow::ensure!(!reply.discover || intake.is_none_or(|i| plan_coverage::retrievals_matched(i)), "unmatched capabilities need clarification before a quote");
            }
            if let Some(intent) = &mut reply.intent {
                anyhow::ensure!(reply.discover, "unexpected intent for conversational reply");
                if intake.is_some() {
                    anyhow::ensure!(intent.goals.iter().all(|goal| input["catalog"]["capabilities"].as_array().is_some_and(|caps| caps.iter().any(|c| c == &goal.capability))), "intent must use discovered operations");
                }
                intent.restore_question_literals(question);
                intent.bind_constraint_inputs(literals.as_deref().unwrap_or(question), catalog)?;
                if let Some(intake) = intake {
                    capability_binding::bind_source_requirements(intent, intake, catalog);
                    plan_coverage::prepare_grounded(intent, intake, catalog, &reply.output_bindings)?;
                    for instruction in source_grounding::answer_instructions(&reply.output_bindings, intake) {
                        if !intent.answer_operations.contains(&instruction) { intent.answer_operations.push(instruction); }
                    }
                    if intake.parsed_request.subjects.is_empty() {
                        capability_binding::bind_ecb_target_literal(intent, question, catalog);
                    }
                }
                intent.validate(literals.as_deref().unwrap_or(question), task, catalog)?;
                // New schema replies get coverage feedback inside the repair
                // loop. Legacy replies without the mapping field retain their
                // final engine coverage check before any quote is created.
                if let Some(intake) = intake.filter(|_| serde_json::from_str::<Value>(raw).ok().is_some_and(|reply|reply.get("output_bindings").is_some())) {
                    source_grounding::verify_plan(intent, task, intake, catalog, &reply.output_bindings)?;
                }
            }
            Ok(reply)
        })
        .await;
        match (result, intake) {
            (Err(error), Some(intake)) if error.is::<RequirementRejection>() => Ok(
                plan_coverage::single_retrieval_reply(intake,catalog,question,literals.as_deref().unwrap_or(question),task)
                    .unwrap_or_else(||plan_coverage::rejected_reply(intake))),
            (Err(error), None) if error.is::<RequirementRejection>() => Err(ModelFailure::Unavailable.into()),
            (result, _) => result,
        }
    }

    /// Collect a workflow's field values from the conversation; each value
    /// must be one the person supplied.
    pub async fn workflow_chat(&self, question: &str, task: &Task, recipe: &crate::workflow_catalog::Recipe) -> anyhow::Result<WorkflowChatReply> {
        let workflow = task.workflow.as_ref().ok_or_else(|| anyhow::anyhow!("Missing workflow context"))?;
        let payload = json!({
            "model": self.model, "temperature": 0, "max_tokens": 2048,
            "reasoning": {"effort":"minimal", "exclude":true},
            "provider": {"require_parameters":true},
            "response_format": {"type":"json_object"},
            "messages": [
                {"role":"system","content":include_str!("../contracts/workflow-chat-prompt.txt")},
                {"role":"user","content":json!({"workflow":recipe.slug,"name":recipe.name,"description":recipe.description,"fields":recipe.fields,"limitations":recipe.limitations,"collected":workflow.input,"conversation":task.conversation,"question":question}).to_string()}
            ]
        });
        self.completion("workflow-chat", &payload, |raw| {
            let reply: WorkflowChatReply = serde_json::from_str(raw)?;
            anyhow::ensure!(!reply.message.trim().is_empty() && reply.message.len() <= 3000, "Invalid workflow reply");
            for (key, value) in &reply.input {
                anyhow::ensure!(recipe.fields.iter().any(|field| field.key == *key), "Unknown workflow field");
                anyhow::ensure!(value.is_null() || value.is_number() || value.as_str().is_some_and(|value| value.len() <= 500), "Invalid workflow value");
                let literal = value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string());
                let normalize = |s: &str| s.to_lowercase().replace("https://", "").chars().filter(|c| c.is_alphanumeric()).collect::<String>();
                let text = std::iter::once(question).chain(task.conversation.iter().map(|turn| turn.question.as_str())).collect::<Vec<_>>().join(" ");
                let country_aliases: &[&str] = match literal.as_str() {
                    "NL" => &["Netherlands", "Nederland", "Dutch", "Holland"],
                    "GB" => &["UK", "United Kingdom", "British", "Verenigd Koninkrijk", "Engeland"],
                    "FR" => &["France", "Frankrijk", "French"],
                    "No" => &["nee", "without", "geen", "no"],
                    "Yes" => &["ja", "yes", "include", "inclusief"],
                    _ => &[],
                };
                anyhow::ensure!(value.is_null() || workflow.input.get(key) == Some(value) || (!normalize(&literal).is_empty() && normalize(&text).contains(&normalize(&literal))) || country_aliases.iter().any(|alias| normalize(&text).contains(&normalize(alias))), "Workflow value must be supplied by the user");
            }
            Ok(reply)
        }).await
    }

    pub async fn review_plan(
        &self,
        question: &str,
        task: &Task,
        catalog: &Catalog,
        intent: &Intent,
        reason: &str,
    ) -> anyhow::Result<PlanningReview> {
        let payload = json!({"model":self.model,"temperature":0,"max_tokens":4096,
            "reasoning":{"effort":"minimal","exclude":true},
            "provider":{"require_parameters":true},"response_format":{"type":"json_object"},
            "messages":[
                {"role":"system","content":include_str!("../contracts/planning-prompt.txt")},
                {"role":"user","content":json!({"surface":"planning-review","question":question,
                    "context":task.parser_context(),"intent":intent,"compiler_feedback":reason,
                    "available":catalog.planning_view(intent, task.capability_intake.as_ref().map(|i| &i.selected_capabilities))}).to_string()}]});
        self.completion("planning", &payload, |raw| {
            let review: PlanningReview = serde_json::from_str(raw)?;
            review.validate(intent, question, task, catalog)?;
            Ok(review)
        })
        .await
    }

    /// Retrying interpretation is safe: this boundary cannot call a data
    /// provider, authorize a quote or execute a purchase. Both attempts share
    /// a single bounded stage deadline.
    async fn completion<T>(
        &self,
        surface: &'static str,
        payload: &Value,
        decode: impl Fn(&str) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let started = std::time::Instant::now();
        let payload = self.routed_payload(surface, payload);
        let input_bytes = serde_json::to_vec(&payload)?.len();
        if self.key.is_empty() {
            return Err(ModelFailure::Unavailable.into());
        }
        let (attempt_seconds, total_seconds) = completion_budget(surface);
        let work = async {
            let mut attempt_payload = payload.clone();
            let mut rejected_requirement = None;
            for attempt in 1..=2 {
                // Only the final complete response can establish a semantic
                // rejection. A later timeout, transport or JSON failure is an
                // availability failure even when an earlier plan was rejected.
                rejected_requirement = None;
                let mut call = crate::telemetry::model_call(surface, attempt, payload["model"].as_str().unwrap_or_default(), input_bytes);
                let response = self
                    .http
                    .post(&self.url)
                    .bearer_auth(&self.key)
                    .timeout(Duration::from_secs(attempt_seconds))
                    .json(&attempt_payload)
                    .send()
                    .await;
                let outcome: Result<(), &str> = match response {
                    Ok(response) if response.status().is_success() => {
                        match bounded_json_sized(response, 65536).await {
                            Ok((body, output_bytes)) => {
                                call.received(&body, output_bytes);
                                let choice = body.pointer("/choices/0");
                                let finish = choice.and_then(|c| c["finish_reason"].as_str());
                                let refused = choice.is_some_and(|c| {
                                    c["message"]["refusal"]
                                        .as_str()
                                        .is_some_and(|v| !v.is_empty())
                                }) || finish == Some("content_filter");
                                if refused {
                                    call.finish(Outcome::Failed(ErrorClass::Validation), "refused");
                                    return Err(ModelFailure::Unavailable.into());
                                }
                                let raw = body
                                    .pointer("/choices/0/message/content")
                                    .and_then(Value::as_str)
                                    .filter(|text| !text.trim().is_empty());
                                if !body["error"].is_null() || finish == Some("length") {
                                    Err("incomplete_response")
                                } else if let Some(raw) = raw {
                                    match decode(raw) {
                                        Ok(value) => {
                                            tracing::info!(trace = crate::telemetry::id(), surface, attempt,
                                                model_ms = started.elapsed().as_millis() as u64, input_bytes,
                                                model = payload["model"].as_str().unwrap_or_default(),
                                                provider = body["provider"].as_str().unwrap_or_default(),
                                                prompt_tokens = body["usage"]["prompt_tokens"].as_u64(),
                                                completion_tokens = body["usage"]["completion_tokens"].as_u64(),
                                                "model response validated");
                                            call.finish(Outcome::Ok, "validated");
                                            return Ok(value);
                                        },
                                        Err(error)
                                            if matches!(
                                                error.downcast_ref::<ModelFailure>(),
                                                Some(ModelFailure::Unsupported)
                                            ) =>
                                        {
                                            call.finish(Outcome::Failed(ErrorClass::Unsupported), "unsupported");
                                            return Err(ModelFailure::Unsupported.into());
                                        }
                                        Err(error) => {
                                            #[cfg(test)]
                                            if surface == "capability-plan" { crate::live_ask_planning_tests::record_validation_failure(raw,&error); }
                                            if surface == "capability-intake" || surface == "capability-search" {
                                                let reason = match error.to_string().as_str() {
                                                    "missing capability matches" => "missing_matches",
                                                    "unknown or duplicate parsed capability" => "unknown_capability",
                                                    "unknown candidate id" => "unknown_candidate",
                                                    "incomplete capability matching" => "incomplete_matches",
                                                    "subject must be literal and have a type" => "non_literal_subject",
                                                    _ => "invalid_schema",
                                                };
                                                tracing::warn!(surface, attempt, reason, "capability validation failed");
                                                if let Some(messages) = attempt_payload["messages"].as_array_mut() {
                                                    messages.push(json!({"role":"assistant","content":raw}));
                                                    messages.push(json!({"role":"user","content":format!("The JSON failed validation: {}. Return one complete corrected object following the original schema. Do not invent identifiers, capabilities or operations.",error)}));
                                                }
                                            }
                                            if matches!(surface, "intent" | "chat" | "capability-plan") {
                                                let coverage = error.downcast_ref::<source_grounding::CoverageError>();
                                                let reason = if coverage.is_some() { "missing_source_coverage" } else { intent_validation_reason(&error) };
                                                if matches!(surface, "chat" | "capability-plan") && requirement_rejection(reason) {
                                                    rejected_requirement = Some(reason);
                                                }
                                                tracing::warn!(surface, attempt, reason, "intent validation failed");
                                                if let Some(messages) = attempt_payload["messages"].as_array_mut() {
                                                    messages.push(json!({"role":"assistant","content":raw}));
                                                    messages.push(json!({"role":"user","content":intent_repair_message(surface, reason)}));
                                                    if let Some(coverage) = coverage {
                                                        messages.push(json!({"role":"user","content":format!("The proposed source plan does not yet support outputs {} or preserve supplied inputs {}. Recheck source_fields and the exact input definitions. Correct source selection, literal bindings and output_bindings using only admitted evidence. Do not remove requirements to pass this check. If an essential item is genuinely unavailable, explain it briefly with discover=false and intent=null.",json!(coverage.outputs),json!(coverage.inputs))}));
                                                    }
                                                }
                                            }
                                            if surface == "planning" {
                                                // Fixed diagnostics only; never log model text or user facts.
                                                let reason = match error.to_string().as_str() {
                                                    "planner changed the request" => "changed_request",
                                                    "planner changed a goal" => "changed_goal",
                                                    "planner relaxed a requirement" => "relaxed_requirement",
                                                    "corrected plan is still unsupported; ask a clarification instead" => "still_unsupported",
                                                    _ => "invalid_review",
                                                };
                                                tracing::warn!(
                                                    surface,
                                                    attempt,
                                                    reason,
                                                    "planning validation failed"
                                                );
                                                if let Some(messages) =
                                                    attempt_payload["messages"].as_array_mut()
                                                {
                                                    messages.push(json!({"role":"user","content":format!("The proposed review failed validation ({reason}). Preserve the original subjects, goal count, capabilities and constraints exactly. Do not add identifier lookup goals; the compiler supplies dependencies. If no executable correction exists, return {{\"message\":\"one focused clarification about the unsupported requirement, in the user's language\",\"intent\":null}}. Do not claim retrieval is planned or that an available filing is necessarily the latest.")}));
                                                }
                                            }
                                            if matches!(surface, "analysis" | "multi-source-analysis") {
                                                // Fixed validator classifications only; never log the model answer.
                                                let reason = analysis_validation_reason(&error);
                                                tracing::warn!(
                                                    surface,
                                                    attempt,
                                                    reason,
                                                    "analysis validation failed"
                                                );
                                                if let Some(messages) =
                                                    attempt_payload["messages"].as_array_mut()
                                                {
                                                    // The repair must see what failed so it can fix the
                                                    // cited field or quote, rather than regenerate blind.
                                                    // This remains inside the bounded model request;
                                                    // neither the answer nor source data is logged.
                                                    messages.push(json!({"role":"assistant","content":raw}));
                                                    let repair = if surface == "multi-source-analysis" {
                                                        format!("The response failed validation ({reason}). Correct the complete JSON. Keep concise observations with at most eight fact_id references each. Cite the supplied field IDs only; do not write pointers or quotes. Text and limitations must contain no literal digits or URLs: use {{{{a}}}} through {{{{h}}}} in observations for cited exact values, and make limitations qualitative. Cover every requested source finding and give a joined, evidence-supported conclusion. Do not substitute a format-example sentence for the answer. Preserve each provider's substantive evidence or its visible specific source_gaps limitation. requirement_status answered_by uses o1 for the first observation; do not repeat evidence arrays.")
                                                    } else {
                                                        format!("The previous response failed validation ({reason}). Return a corrected, smaller JSON response. Use at most four observations with at most four scalar field references each. Every pointer must exist under /data/ in its named document. For a long text field include a short exact contiguous quote copied from that field, preserving whitespace; never paraphrase a quote. Prefer short scalar title, URL or date fields when possible. No digits, dates, amounts, numeric codes, URLs or markdown in text OR limitations. Insert exact values using {{{{a}}}}, {{{{b}}}}, {{{{c}}}}, {{{{d}}}} with matching evidence references. Do not reproduce the invalid response. Preserve evidence from each distinct provider in observations or presentation data cells; do not drop a provider to make the response smaller. If a provider cannot support this answer, add a visible limitation and a source_gaps entry with its exact result_ref, an allowed reason and the matching limitation_index.")
                                                    };
                                                    messages.push(json!({"role":"user","content":repair}));
                                                }
                                            }
                                            Err("invalid_output")
                                        }
                                    }
                                } else {
                                    Err("empty_output")
                                }
                            }
                            Err(error) => Err(model_envelope_failure(&error)),
                        }
                    }
                    Ok(response) => {
                        let status = response.status().as_u16();
                        tracing::warn!(surface, attempt, status, "model HTTP failure");
                        call.finish(Outcome::Failed(ErrorClass::upstream(status)), "http_status");
                        if status != 408 && status != 429 && status < 500 {
                            return Err(ModelFailure::Unavailable.into());
                        }
                        Err("upstream_unavailable")
                    }
                    Err(error) => Err(if error.is_timeout() { "timeout" } else { "transport_failure" }),
                };
                if let Err(reason) = outcome {
                    // No prompt, answer, reasoning or credentials in diagnostics.
                    tracing::warn!(surface, attempt, reason, "model response unavailable");
                    let class = match reason { "timeout" => ErrorClass::Timeout, "transport_failure" => ErrorClass::Transport, _ => ErrorClass::Validation };
                    call.finish(Outcome::Failed(class), reason);
                    if surface == "multi-source-analysis" && reason == "timeout" {
                        // Another full synthesis cannot fit in the remaining
                        // stage budget. Keep the saved results retryable.
                        return Err(ModelFailure::Unavailable.into());
                    }
                }
            }
            if let Some(reason) = rejected_requirement {
                return Err(RequirementRejection(reason).into());
            }
            Err(ModelFailure::Unavailable.into())
        };
        let result = tokio::time::timeout(Duration::from_secs(total_seconds), work)
            .await
            .unwrap_or_else(|_| Err(ModelFailure::Unavailable.into()));
        tracing::info!(trace = crate::telemetry::id(), surface, input_bytes, model = payload["model"].as_str().unwrap_or_default(), model_ms = started.elapsed().as_millis() as u64,
            success = result.is_ok(), "model stage completed");
        result
    }
    pub async fn parse(
        &self,
        owner: Uuid,
        question: &str,
        task: &Task,
        catalog: &Catalog,
    ) -> anyhow::Result<Intent> {
        let context = task.parser_context();
        let key = digest(&(
            owner,
            question.trim(),
            &context,
            &catalog.version,
            &self.model,
            "parser-v2.6",
        ));
        crate::telemetry::set_parser_cache(if self.cache.contains_key(&key) { Cache::Hit } else { Cache::Miss });
        let intent=self.cache.try_get_with(key,async {
            self.calls.fetch_add(1,Ordering::Relaxed);
            self.parse_baseline(question, task, catalog).await.map(Arc::new)
        }).await.map_err(|e| anyhow::Error::new(e.downcast_ref::<ModelFailure>().cloned().unwrap_or(ModelFailure::Unavailable)))?;
        Ok((*intent).clone())
    }

    async fn parse_baseline(&self, question: &str, task: &Task, catalog: &Catalog) -> anyhow::Result<Intent> {
            let context = task.parser_context();
            let started=std::time::Instant::now();
            let system=include_str!("../contracts/parser-prompt.txt");
            let view=self.planning_view(question,task,catalog).await?;
            let payload=json!({
                "model":self.model,"temperature":0,"max_tokens":4096,
                "reasoning":{"effort":"minimal","exclude":true},
                "provider":{"require_parameters":true},
                "response_format":{"type":"json_object"},
                "messages":[{"role":"system","content":system},{"role":"user","content":json!({"question":question,"context":context,"catalog":view}).to_string()}]
            });
            let intent=self.completion("intent", &payload, |raw| {
                let mut intent=decode_intent(raw)?;
                intent.restore_question_literals(question);
                intent.bind_constraint_inputs(question,catalog)?;
                intent.validate(question,task,catalog)?;
                Ok(intent)
            }).await?;
            tracing::info!(parser_ms=started.elapsed().as_millis() as u64,"intent parsed");
            Ok(intent)
    }

    async fn planning_view(&self, question: &str, task: &Task, catalog: &Catalog) -> anyhow::Result<Value> {
        if catalog.services.len()<=400 {return Ok(catalog.parser_view());}
        let presets=catalog.parser_view()["workflow_presets"].clone();
        let payload=json!({"model":self.model,"temperature":0,"max_tokens":4096,
            "reasoning":{"effort":"minimal","exclude":true},"provider":{"require_parameters":true},
            "response_format":{"type":"json_object"},"messages":[
                {"role":"system","content":include_str!("../contracts/catalog-selection-prompt.txt")},
                {"role":"user","content":json!({"question":question,"context":task.parser_context(),"operations":catalog.operation_index_for(question),"workflow_presets":presets}).to_string()}
            ]});
        let selected=self.completion("catalog-selection",&payload,|raw| {
            let value:Value=serde_json::from_str(raw)?;
            let choices=value["capabilities"].as_array().ok_or_else(||anyhow::anyhow!("missing catalog choices"))?;
            anyhow::ensure!(choices.len()<=32,"too many catalog choices");
            let mut selected=std::collections::BTreeSet::new();
            for choice in choices {
                let cap=choice.as_str().ok_or_else(||anyhow::anyhow!("invalid catalog choice"))?;
                anyhow::ensure!(catalog.by_capability.contains_key(cap)||presets.as_array().is_some_and(|rows|rows.iter().any(|v|v["capability"]==cap)),"unknown catalog choice");
                selected.insert(cap.to_owned());
            }
            Ok(selected)
        }).await?;
        let mut selected = selected;
        // FilingPulse is a member of Pulse Network. Its operation slugs are
        // opaque, so a first-pass selector can miss the relevant member. Keep
        // the filing alternatives in the second-pass view for filing questions;
        // the parser still decides which operation actually fits.
        if question.to_ascii_lowercase().contains("filing") {
            selected.extend(catalog.services.iter()
                .filter(|s| s.api_slug == "filingspulse")
                .map(|s| s.contract.capability_slug.clone()));
        }
        Ok(catalog.selected_parser_view(&selected))
    }
}

fn empty_source_analysis(sources: &[Value]) -> Option<Value> {
    (!sources.is_empty() && sources.iter().all(|source| source["document"]["coverage"] == "empty"))
        .then(|| json!({"status":"partial","observations":[],"limitations":[
            "The returned source response contains no usable records for this question. No finding can be supported from it."
        ]}))
}
pub async fn bounded_json(response: reqwest::Response, limit: usize) -> anyhow::Result<Value> {
    Ok(bounded_json_sized(response, limit).await?.0)
}
/// [`bounded_json`] plus the body's size in bytes.
pub async fn bounded_json_sized(mut response: reqwest::Response, limit: usize) -> anyhow::Result<(Value, usize)> {
    if let Some(n) = response.content_length() {
        anyhow::ensure!(n <= limit as u64, "response exceeds size limit");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        anyhow::ensure!(
            bytes.len() + chunk.len() <= limit,
            "response exceeds size limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok((serde_json::from_slice(&bytes)?, bytes.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "local capture of saved planning payload; no external network or credentials"]
    async fn capture_saved_capability_planning_payload_locally() {
        let saved: Value=serde_json::from_slice(&std::fs::read(std::env::var("ASK_SAVED_INTAKE").unwrap()).unwrap()).unwrap();
        let question=saved["question"].as_str().unwrap();
        let parsed:crate::capability_intake::ParsedRequest=serde_json::from_value(saved["parsed"].clone()).unwrap();
        let catalog=Catalog::build(serde_json::from_slice(&std::fs::read(std::env::var("ASK_SAVED_INTAKE_CATALOG").unwrap()).unwrap()).unwrap());
        let matches=crate::ask_v2::search::search(&parsed,&catalog);
        let selected_capabilities=matches.iter().flat_map(|matched|matched.operations.iter().cloned()).collect();
        let intake=crate::capability_intake::Intake {parsed_request:parsed,matches,selected_capabilities,
            catalog_version:catalog.version.clone(),apiosk_sources_searched:crate::capability_intake::source_count(&catalog)};
        let (parser,_,server,requests)=model_fixture_with_requests(vec![(400,json!({"error":"local capture only"}))]).await;
        assert!(parser.chat_with_view(question,None,&Task::default(),&catalog,capability_planning_view(catalog.selected_parser_view(&intake.selected_capabilities)),Some(&intake)).await.is_err());
        let requests=requests.lock().unwrap();assert_eq!(requests.len(),1);
        std::fs::write(std::env::var("ASK_SAVED_INTAKE_CAPTURE").unwrap(),serde_json::to_vec_pretty(&requests[0]).unwrap()).unwrap();
        server.abort();
    }

    #[tokio::test]
    #[ignore = "uses saved AI intake, live planning model and read-only catalog; no source calls or writes"]
    async fn live_saved_capability_intake_forms_a_complete_quote() {
        dotenvy::dotenv().ok();
        let _ = tracing_subscriber::fmt().with_env_filter("info,sqlx=error").try_init();
        let saved: Value = serde_json::from_slice(&std::fs::read(std::env::var("ASK_SAVED_INTAKE").expect("saved intake path required")).unwrap()).unwrap();
        let question = saved["question"].as_str().unwrap();
        let parsed: crate::capability_intake::ParsedRequest = serde_json::from_value(saved["parsed"].clone()).unwrap();
        let catalog = if let Ok(path)=std::env::var("ASK_SAVED_INTAKE_CATALOG") {
            std::sync::Arc::new(Catalog::build(serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()))
        } else {
            let pool=sqlx::postgres::PgPoolOptions::new().max_connections(1).connect(&std::env::var("DATABASE_URL").unwrap()).await.unwrap();
            let catalog=crate::catalog::CatalogCache::new(pool.clone()).get().await.unwrap();
            pool.close().await;catalog
        };
        let matches = crate::ask_v2::search::search(&parsed, &catalog);
        let selected_capabilities = matches.iter().flat_map(|matched|matched.operations.iter().cloned()).collect();
        let intake = crate::capability_intake::Intake { parsed_request: parsed, matches, selected_capabilities,
            catalog_version:catalog.version.clone(),apiosk_sources_searched:crate::capability_intake::source_count(&catalog) };
        let saved_reply=std::env::var("ASK_SAVED_MODEL_REPLY").ok();
        let key = if saved_reply.is_some() { "local-replay".into() } else {
            std::env::var("OPENROUTER_API_KEY").ok().filter(|key|!key.is_empty()).unwrap_or_else(||std::env::var("INTENT_API_KEY").unwrap())
        };
        let http = reqwest::Client::new();
        let parser = Parser::new(http.clone(),"https://openrouter.ai/api/v1/chat/completions".into(),key,MODEL.into());
        let task=Task::default();
        let literals=capability_binding::literal_context(question,&task,None);
        assert!(plan_coverage::single_retrieval_reply(&intake,&catalog,question,&literals,&task).is_none(),"this replay must require the planning model");
        let unreachable=sqlx::postgres::PgPoolOptions::new().connect_lazy("postgresql://postgres@127.0.0.1:1/unused").unwrap();
        let store=crate::store::Store {pool:unreachable.clone()};
        let mut engine=crate::engine::Engine {store:store.clone(),catalog:crate::catalog::CatalogCache::new(unreachable),parser,
            signer:crate::state::Signer::new(vec![8;32]).unwrap(),app_url:"http://localhost".into(),
            executor:crate::execution::Executor {store,http,supabase_url:"http://127.0.0.1:1".into(),enabled:false}};
        let started=std::time::Instant::now();
        // Record the exact validated model intent even if the quote resolver
        // refuses it. Replay that same semantic reply locally through guest
        // planning so this diagnostic never pays for a second model answer.
        let reply=if let Some(path)=&saved_reply {
            let saved:Value=serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            ChatReply { output_bindings: vec![],message:"Review the complete source price.".into(),discover:true,
                intent:Some(serde_json::from_value(saved["decoded_intent"].clone()).unwrap()),decline_reason:Some("none".into())}
        } else { engine.parser.chat_from_capabilities(question,None,&task,&catalog,&intake).await.unwrap() };
        let decoded_intent=serde_json::to_value(&reply.intent).unwrap();
        let mut bound_intent=decoded_intent.clone();
        if let Some(subjects)=bound_intent.get_mut("subjects").and_then(Value::as_array_mut) {
            for subject in subjects {
                let parsed=intake.parsed_request.subjects.iter().find(|parsed|subject["label"]==parsed.label).unwrap();
                let values=subject["values"].as_object().unwrap().iter().map(|(key,value)|(key.clone(),json!({"literal":value}))).collect::<serde_json::Map<_,_>>();
                *subject=json!({"subject_ref":parsed.id,"values":values});
            }
        }
        let body=json!({"message":reply.message,"discover":reply.discover,"intent":bound_intent,"decline_reason":reply.decline_reason});
        let envelope=json!({"choices":[{"finish_reason":"stop","message":{"content":body.to_string()}}]});
        let (replay_parser,calls,server)=model_fixture(vec![(200,envelope)]).await;
        engine.parser=replay_parser;
        let quote=engine.guest_plan(question,intake,&catalog).await.unwrap();
        assert_eq!(calls.load(Ordering::Relaxed),1);server.abort();
        use base64::Engine as _;
        let preview=quote["preview_token"].as_str().and_then(|token|token.split_once('.'))
            .and_then(|(payload,_)|base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).ok())
            .and_then(|bytes|serde_json::from_slice::<Value>(&bytes).ok()).unwrap_or(Value::Null);
        let evidence=json!({"question":question,"parsed":saved["parsed"],"model":CAPABILITY_PLAN_MODEL,
            "replayed_saved_model_response":saved_reply.is_some(),
            "status":quote["status"],"message":quote["message"],"sources":quote["sources"],"total_atomic":quote["total_atomic"],
            "decoded_intent":decoded_intent,"intent":preview["task"]["intent"],"facts":preview["task"]["facts"],"elapsed_ms":started.elapsed().as_millis()});
        std::fs::write(std::env::var("ASK_SAVED_INTAKE_EVIDENCE").expect("evidence output required"),serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
        assert_eq!(quote["status"],"ready","{evidence}");
        println!("saved intake reached a complete quote with {CAPABILITY_PLAN_MODEL} in {}ms",started.elapsed().as_millis());
    }

    #[test]
    fn completion_budget_preserves_other_stages_and_bounds_longer_synthesis() {
        assert_eq!(completion_budget("multi-source-analysis"), (60, 65));
        assert_eq!(completion_budget("analysis"), (30, 65));
        assert_eq!(completion_budget("chat"), (20, 30));
        assert_eq!(completion_budget("capability-plan"), (40, 65));
        assert_eq!(completion_budget("capability-intake"), (10, 20));
        assert_eq!(completion_budget("capability-search"), (10, 20));
        assert_eq!(completion_budget("clarification"), (6, 12));
        for surface in ["intent", "planning"] { assert_eq!(completion_budget(surface), (12, 25)); }
    }

    #[tokio::test]
    async fn response_body_timeout_is_not_reported_as_an_invalid_model_envelope() {
        use axum::{body::Body, routing::get, Router};
        use futures_util::StreamExt;
        let app = Router::new().route("/", get(|| async {
            Body::from_stream(futures_util::stream::once(async { Ok::<_, std::convert::Infallible>(" ") })
                .chain(futures_util::stream::once(async {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    Ok::<_, std::convert::Infallible>("{}")
                })))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let response = reqwest::Client::new().get(url).timeout(Duration::from_millis(100)).send().await.unwrap();
        let error = bounded_json_sized(response, 65536).await.unwrap_err();
        assert_eq!(model_envelope_failure(&error), "timeout");
        let malformed = serde_json::from_str::<Value>("not json").unwrap_err();
        assert_eq!(model_envelope_failure(&malformed.into()), "invalid_envelope");
        server.abort();
    }

    #[test]
    fn sixteen_goals_are_preserved_but_execution_limits_still_refuse_oversized_plans() {
        let mut raw: Value = serde_json::from_str(include_str!("../tests/fixtures/integration-catalog.json")).unwrap();
        let template = raw["services"][1].clone();
        raw["services"] = json!((0..17).map(|index| {
            let mut service = template.clone();
            service["id"] = json!(Uuid::from_u128(index + 1));
            service["endpoint_id"] = service["id"].clone();
            service["contract"]["capability_slug"] = json!(format!("company.audit{index}"));
            service["contract"]["requires"] = json!([]);
            service["contract"]["input_map"] = json!({});
            service
        }).collect::<Vec<_>>());
        let catalog = Catalog::build(serde_json::from_value(raw).unwrap());
        assert_eq!(catalog.services.len(), 17);
        let goal_schema = &intent_reply_schema("intent").unwrap()["properties"]["goals"];
        assert_eq!(goal_schema["maxItems"], 16);
        assert_eq!(crate::protocol::MAX_STEPS, 12);
        assert_eq!(crate::protocol::MAX_DEPTH, 5);
        for count in [9, 16, 17] {
            let mut intent = Intent {
                subjects: vec![Subject {entity_ref:None,label:"Coolblue".into(),values:BTreeMap::new()}],
                goals: (0..count).map(|index|Goal {capability:format!("company.audit{index}"),subject:0,constraints:BTreeMap::new()}).collect(),
                answer_operations: vec![], unresolved_references: vec![],
            };
            let mut task = Task::default();
            assert_eq!(intent.validate("Coolblue", &task, &catalog).is_ok(), count <= 16);
            assert_eq!(intent.goals.len(), count, "validation never truncates the request");
            if count > 16 {continue;}
            crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
            match crate::resolver::resolve(&catalog, &task, &intent) {
                crate::resolver::Resolution::Plan(plan) if count == 9 => assert_eq!(plan.steps.len(), count),
                crate::resolver::Resolution::Unsupported(_) if count == 16 => {},
                result => panic!("{count} goals: unexpected {result:?}"),
            }
            assert_eq!(intent.goals.len(), count, "the execution cap refuses the complete plan instead of discarding goals");
        }
        assert!(include_str!("../contracts/capability-planning-prompt.txt").contains("goals: one to sixteen objects"));
        assert!(intent_repair_message("chat", "fixture").contains("between one and sixteen goals"));
    }

    #[tokio::test]
    async fn a_blocked_purchase_without_sources_keeps_its_reason_without_a_model_call() {
        let parser = Parser::new(reqwest::Client::new(), "http://127.0.0.1:1".into(), "unused".into(), "unused".into());
        let failure = crate::billing::public_error(&crate::billing::refunded_failure(&json!({
            "execution":{"paid":false,"status":403,"body":{"error":"policy_denied","message":"private policy details"}}
        })));
        let failures = BTreeMap::from([("first".into(), failure.clone()), ("second".into(), failure.clone())]);
        let answer = parser.analyze(&json!(["Find charging stations"]), &[], &failures, &[]).await.unwrap();
        assert_eq!(answer["status"], "partial");
        assert_eq!(answer["observations"], json!([]));
        assert_eq!(answer["limitations"], json!([failure["message"]]));
        assert!(answer["follow_up_question"].is_null());
        assert!(!answer.to_string().contains("private policy"));
        let empty = parser.analyze(&json!([]), &[], &BTreeMap::new(), &[]).await.unwrap();
        assert_eq!(empty["limitations"], json!(["No source result is available to support an analysis."]));
    }

    #[test]
    fn missing_context_must_be_one_conversational_question() {
        assert_eq!(missing_context_message(r#"{"message":"I found a different UK company. Which Apple filing topic do you mean?"}"#).unwrap(),
            "I found a different UK company. Which Apple filing topic do you mean?");
        assert!(missing_context_message(r#"{"message":"No data. Try again."}"#).is_err());
        assert!(missing_context_message(r#"{"message":"Which form? Which year?"}"#).is_err());
    }

    #[test]
    fn no_supported_observations_remain_partial() {
        let answer = validate_analysis(r#"{"observations":[],"limitations":["The record belongs to a different company."],"follow_up_question":"Which Apple filing topic should I check?"}"#, &[], false).unwrap();
        assert_eq!(answer["status"], "partial");
        assert_eq!(answer["follow_up_question"], "Which Apple filing topic should I check?");
    }

    #[test]
    fn partly_supported_answer_keeps_model_follow_up() {
        let source = json!({"result_ref":"saved","document":{"data":{"name":"Apple Inc"}}});
        let answer = json!({"observations":[{"text":"The named issuer is {{a}}.",
            "evidence":[{"result_ref":"saved","pointer":"/data/name"}]}],
            "limitations":["The source did not provide the requested filing form."],
            "follow_up_question":"Which filing form should I look for?"});
        let checked = validate_analysis(&answer.to_string(), &[source], false).unwrap();
        assert_eq!(checked["status"], "partial");
        assert_eq!(checked["observations"][0]["text"], "The named issuer is Apple Inc.");
        assert_eq!(checked["follow_up_question"], "Which filing form should I look for?");
    }

    #[test]
    fn broad_filing_question_requires_scope_before_any_quote() {
        assert_eq!(broad_filing_subject("i need filings from apple inc"), Some("apple inc"));
        assert_eq!(broad_filing_subject("Filings from Apple Inc."), Some("Apple Inc"));
        assert_eq!(broad_filing_subject("Filings from Apple Inc last year"), None);
        assert_eq!(broad_filing_subject("Filings from Companies House"), None);
        assert_eq!(broad_filing_subject("What do Apple Inc filings say about China supply chain risk?"), None);
    }

    #[test]
    fn us_issuer_filing_request_cannot_quote_companies_house_without_uk_scope() {
        let snapshot = serde_json::from_str(include_str!("../tests/fixtures/company-sources/catalog.json")).unwrap();
        let catalog = Catalog::build(snapshot);
        let intent = Intent {
            subjects: vec![Subject { entity_ref: None, label: "Apple Inc".into(), values: BTreeMap::new() }],
            goals: vec![Goal { capability: "company.filing-history".into(), subject: 0, constraints: BTreeMap::new() }],
            answer_operations: vec![], unresolved_references: vec![],
        };
        assert!(intent.validate("Filings from Apple Inc", &Task::default(), &catalog).is_err());
        assert!(intent.validate("Companies House filings from Apple Inc", &Task::default(), &catalog).is_ok());
    }

    #[test]
    fn explicitly_empty_source_skips_slow_analysis_and_cannot_support_findings() {
        let source = json!({"document":{"coverage":"empty","source":{"provider":"companies-house"},"data":{"items":[]}}});
        let answer = empty_source_analysis(&[source]).unwrap();
        assert_eq!(answer["status"], "partial");
        assert_eq!(answer["observations"], json!([]));
        assert!(empty_source_analysis(&[json!({"document":{"coverage":"source_response","data":{"items":[{"date":"2026-01-01"}]}}})]).is_none());
    }

    #[test]
    fn live_vat_plan_cannot_hide_invented_source_inputs_in_constraints() {
        let fixture: Value = serde_json::from_str(include_str!("../tests/fixtures/profession-case09-constraints.json")).unwrap();
        let catalog = Catalog::build(serde_json::from_value(fixture["catalog"].clone()).unwrap());
        let question = fixture["question"].as_str().unwrap();
        let original: Intent = serde_json::from_value(fixture["intent"].clone()).unwrap();
        let mut task = Task::default();
        let mut original = original;
        for subject in &mut original.subjects { subject.entity_ref = None; }
        let mut bad = original.clone();
        assert_eq!(bad.bind_constraint_inputs(question,&catalog).unwrap_err().to_string(),"parser invented a value");
        // Each invented input is refused independently, including zero which
        // only occurs as part of the supplied VAT identifier.
        for index in 1..original.goals.len() {
            for (field,value) in &original.goals[index].constraints {
                if !catalog.vocabulary.contains_key(field) {continue;}
                let mut isolated = original.clone();
                isolated.goals = vec![original.goals[index].clone()];
                isolated.goals[0].constraints.retain(|key,_| matches!(key.as_str(),"source"|"endpoint_id"));
                isolated.goals[0].constraints.insert(field.clone(),value.clone());
                assert_eq!(isolated.validate(question,&task,&catalog).unwrap_err().to_string(),"parser invented a value", "{field}");
                assert_eq!(isolated.bind_constraint_inputs(question,&catalog).unwrap_err().to_string(),"parser invented a value", "{field}");
            }
        }
        // The genuine literal VIES request stays executable after moving its
        // redundant typed constraints to the existing matching subject values.
        let mut valid = original.clone();
        valid.goals.truncate(1);
        valid.bind_constraint_inputs(question,&catalog).unwrap();
        assert_eq!(valid.subjects[0].values["vat.eu.country_code"],"NL");
        assert_eq!(valid.subjects[0].values["vat.eu.number"],"002033354B01");
        assert_eq!(valid.goals[0].constraints.len(),2);
        valid.validate(question,&task,&catalog).unwrap();
        crate::engine::bind_subjects(&mut task,&catalog,&mut valid).unwrap();
        let crate::resolver::Resolution::Plan(plan)=crate::resolver::resolve(&catalog,&task,&valid) else {panic!("VIES must remain priceable when its real inputs are supplied");};
        assert_eq!(plan.steps.len(),1);
        assert_eq!(plan.steps[0].service.api_slug,"vies");
    }

    #[test]
    fn typed_constraint_binding_preserves_exact_filters_policy_and_subject_identity() {
        let fixture: Value = serde_json::from_str(include_str!("../tests/fixtures/profession-case09-constraints.json")).unwrap();
        let mut catalog = Catalog::build(serde_json::from_value(fixture["catalog"].clone()).unwrap());
        let mut intent: Intent = serde_json::from_value(fixture["intent"].clone()).unwrap();
        intent.subjects[0].entity_ref = None;
        intent.goals = vec![intent.goals[2].clone()];
        let filter = "$filter=Sector eq '1010' and Periodo eq '2023'";
        let question = format!("{} Apply {filter}, order by Periodo desc, skip 0; complete coverage",fixture["question"].as_str().unwrap());
        intent.goals[0].constraints.insert("coverage".into(),json!("complete"));
        let mut bound = intent.clone();
        bound.bind_constraint_inputs(&question,&catalog).unwrap();
        assert_eq!(bound.subjects[0].values["cbs.input.odata_filter"],filter);
        assert_eq!(bound.subjects[0].values["cbs.input.odata_skip"],0);
        assert_eq!(bound.goals[0].constraints["coverage"],"complete");
        bound.validate(&question,&Task::default(),&catalog).unwrap();
        let mut conflict = intent.clone();
        conflict.subjects[0].values.insert("cbs.input.odata_filter".into(),json!("Other literal"));
        assert_eq!(conflict.bind_constraint_inputs(&question,&catalog).unwrap_err().to_string(),"input belongs to a different parsed subject");
        let mut wrong_type = intent.clone();
        wrong_type.goals[0].constraints.insert("cbs.input.odata_skip".into(),json!("0"));
        assert!(wrong_type.bind_constraint_inputs(&question,&catalog).is_err());
        let mut not_declared = intent.clone();
        not_declared.goals[0].constraints.insert("source.eurlex_cellar.celex".into(),json!("32006L0029"));
        assert_eq!(not_declared.bind_constraint_inputs(&format!("{question} 32006L0029"),&catalog).unwrap_err().to_string(),"source input is not declared for the selected operation");
        let mut ambiguous = intent.clone();
        ambiguous.goals[0].constraints.remove("endpoint_id");
        let mut variant = catalog.services.iter().find(|service| service.api_slug == "cbs-statline").unwrap().clone();
        variant.endpoint_id = Uuid::new_v4();
        variant.id = Uuid::new_v4();
        variant.contract.input_map.remove("cbs.input.odata_filter");
        catalog.services.push(variant);
        assert_eq!(ambiguous.bind_constraint_inputs(&question,&catalog).unwrap_err().to_string(),"source input is not declared for the selected operation", "all possible endpoint variants must declare a promoted input");
        let mut pinned = intent.clone();
        pinned.bind_constraint_inputs(&question,&catalog).unwrap();
        assert_eq!(pinned.subjects[0].values["cbs.input.odata_filter"],filter);
        catalog.vocabulary.get_mut("cbs.input.odata_filter").unwrap().value_schema["default"] = json!(filter);
        let mut invented = intent;
        invented.goals[0].constraints.remove("coverage");
        assert_eq!(invented.bind_constraint_inputs(fixture["question"].as_str().unwrap(),&catalog).unwrap_err().to_string(),"parser invented a value", "schema defaults do not authorize planner facts");
    }

    #[test]
    fn separator_repair_copies_only_contiguous_words_from_the_question() {
        let question = "Zoek nieuws voor 7521 BE, Enschede en café, München.";
        assert_eq!(question_literal(question, "7521 BE Enschede"), Some("7521 BE, Enschede"));
        assert_eq!(question_literal(question, "café München"), Some("café, München"));
        for invented in ["7522 BE Enschede", "7521 BE Amsterdam", "Enschede 7521 BE", "7521 Enschede", "7521BE", "BE Enschede nieuws", "", ",;"] {
            assert_eq!(question_literal(question, invented), None, "{invented}");
        }
        assert_eq!(question_literal("Bedrag 10,00", "10.00"), Some("10,00"), "restores the source, never the model's altered punctuation");
    }

    #[tokio::test]
    async fn crunchbase_profile_url_binds_without_a_paid_identity_lookup() {
        let question = "Use Crunchbase to retrieve the full company profile for https://www.crunchbase.com/organization/starbucks, including available founders, employees, board members, funding rounds and investors.";
        let catalog = Catalog::build(serde_json::from_str(include_str!("../tests/fixtures/crunchbase/official-mcp.json")).unwrap());
        let parsed = crate::capability_intake::normalize(&json!({"language":"en","subjects":[{"id":"s1","type":"company","label":"starbucks","identifiers":{"url":"https://www.crunchbase.com/organization/starbucks"}}],"capabilities":[{"slug":"company.profile","name":"Company profile","description":"Retrieve the company profile and its available relationships.","domain":"company","inputs":["url"],"outputs":["profile","founders","employees","board_members","funding_rounds","investors"],"subject_id":"s1","source_hint":"crunchbase","confidence":1.0}],"deliverable":{"format":"chat","operations":[]}}).to_string(), question, "fixture").unwrap();
        let matches = crate::ask_v2::search::search(&parsed, &catalog);
        let intake = crate::capability_intake::Intake { selected_capabilities: matches.iter().flat_map(|m| m.operations.iter().cloned()).collect(),
            parsed_request:parsed, matches, apiosk_sources_searched:1, catalog_version:catalog.version.clone() };
        let raw = json!({"message":"I will check the price first.","discover":true,"decline_reason":"none","intent":{
            "subjects":[{"subject_ref":"s1","values":{"company.crunchbase_entity_id":{"ref":"s1.identifiers.url"}}}],
            "goals":[{"capability":"company.crunchbase-profile","subject":0,"constraints":{}}],"answer_operations":[],"unresolved_references":[]}}).to_string();
        let (parser, calls, server) = model_fixture(vec![(200,json!({"choices":[{"finish_reason":"stop","message":{"content":raw}}]}))]).await;
        let mut task = Task::default();
        let reply = parser.chat_from_capabilities(question, None, &task, &catalog, &intake).await.unwrap();
        let mut intent = reply.intent.unwrap();
        assert_eq!(intent.subjects[0].values["company.crunchbase_entity_id"], "starbucks");
        crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan) = crate::resolver::resolve(&catalog,&task,&intent) else {panic!("profile must compile")};
        assert_eq!(plan.steps.len(),1); assert_eq!(plan.cap_atomic,54_347);
        task.capability_intake = Some(intake.clone());
        assert!(plan_coverage::uncovered_outputs(&task,&catalog,&intent,&plan).is_empty(), "a resolver plan must also cover all six requested outputs before quoting");
        task.capability_intake.as_mut().unwrap().parsed_request.capabilities[0].outputs.push("customer_ratings".into());
        assert_eq!(plan_coverage::uncovered_outputs(&task,&catalog,&intent,&plan),["Company profile: customer_ratings"]);
        assert_eq!(calls.load(Ordering::Relaxed),0, "the compiler binds the parsed profile URL without a second model call or source purchase");
        server.abort();
    }

    #[tokio::test]
    async fn split_profile_requirements_recover_after_invented_model_constraints() {
        let question = "Use Crunchbase to retrieve the full company profile for https://www.crunchbase.com/organization/starbucks, including available founders, employees, board members, funding rounds and investors.";
        let catalog = Catalog::build(serde_json::from_str(include_str!("../tests/fixtures/crunchbase/official-mcp.json")).unwrap());
        let parsed_request = serde_json::from_str(include_str!("../tests/fixtures/crunchbase/profile-intake.json")).unwrap();
        let matches = crate::ask_v2::search::search(&parsed_request, &catalog);
        let intake = crate::capability_intake::Intake { selected_capabilities: matches.iter().flat_map(|m| m.operations.iter().cloned()).collect(),
            parsed_request, matches, apiosk_sources_searched: 1, catalog_version: catalog.version.clone() };
        let raw = json!({"message":"I will check the price.","discover":true,"decline_reason":"none","intent":{
            "subjects":[{"subject_ref":"s1","values":{}}],
            "goals":[{"capability":"company.crunchbase-profile","subject":0,"constraints":{"period":"2024"}}],
            "answer_operations":[],"unresolved_references":[]}}).to_string();
        let response = json!({"choices":[{"finish_reason":"stop","message":{"content":raw}}]});
        let (parser, calls, server) = model_fixture(vec![(200,response.clone()),(200,response)]).await;
        let mut task = Task::default();
        let reply = parser.chat_with_view(question, None, &task, &catalog, capability_planning_view(catalog.selected_parser_view(&intake.selected_capabilities)), Some(&intake)).await.unwrap();
        assert!(reply.discover);
        let mut intent = reply.intent.expect("one matched profile covers every field requirement");
        assert_eq!(intent.goals.len(), 1);
        assert!(!intent.goals[0].constraints.contains_key("period"));
        assert_eq!(intent.subjects[0].values["company.crunchbase_entity_id"], "starbucks");
        crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan) = crate::resolver::resolve(&catalog, &task, &intent) else { panic!("profile must compile"); };
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.cap_atomic, 54_347);
        task.capability_intake = Some(intake);
        assert!(plan_coverage::uncovered_outputs(&task, &catalog, &intent, &plan).is_empty());
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        server.abort();
    }

    #[tokio::test]
    async fn postcode_chat_restores_literal_search_text_without_a_second_model_call() {
        let question = "Ik zoek informatie over deze postcode : 7521 BE, Enschede. Zoek nieuws items, overheids data etc";
        let raw = include_str!("../tests/fixtures/postcode-chat-reply.json");
        let mut catalog = Catalog::build(serde_json::from_str(include_str!("../tests/fixtures/integration-catalog.json")).unwrap());
        for kind in ["building.postcode", "government.publication.keyword", "web.query"] {
            catalog.vocabulary.insert(kind.into(), serde_json::from_value(json!({"slug":kind,"value_schema":{"type":"string","minLength":1}})).unwrap());
        }
        for capability in ["area.demographics", "building.registry-search", "government.publication.search", "web.search"] {
            catalog.by_capability.insert(capability.into(), vec![]);
        }
        let original: ChatReply = serde_json::from_str(raw).unwrap();
        let original = original.intent.unwrap();
        assert!(original.validate(question, &Task::default(), &catalog).is_err());
        let (parser, calls, server) = model_fixture(vec![(200, json!({"choices":[{"finish_reason":"stop","message":{"content":raw}}]}))]).await;
        let reply = parser.chat(question, &Task::default(), &catalog).await.unwrap();
        let mut intent = reply.intent.unwrap();
        assert_eq!(intent.subjects[0].values["web.query"], "7521 BE, Enschede");
        assert_eq!(intent.subjects[0].values["building.postcode"], "7521 BE");
        assert_eq!(serde_json::to_value(&intent.goals).unwrap(), serde_json::to_value(&original.goals).unwrap());
        assert_eq!(intent.answer_operations, original.answer_operations);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        catalog.vocabulary.get_mut("web.query").unwrap().value_schema = json!({"type":"string","pattern":"^[^,]+$"});
        assert!(intent.validate(question, &Task::default(), &catalog).is_err(), "restored values must still satisfy the input schema");
        catalog.vocabulary.get_mut("web.query").unwrap().value_schema = json!({"type":"string"});
        intent.subjects[0].values.insert("web.query".into(), json!("7521 BE Amsterdam"));
        intent.restore_question_literals(question);
        assert!(intent.validate(question, &Task::default(), &catalog).is_err());
        server.abort();
    }


    #[test]
    fn model_routes_preserve_parameters_and_scope_openrouter_extensions() {
        let payload = json!({"provider":{"require_parameters":true},"response_format":{"type":"json_object"}});
        for (url, routed) in [("https://openrouter.ai/api/v1/chat/completions", true), ("https://example.com/completions", false)] {
            let parser = Parser::new(reqwest::Client::new(), url.into(), "fixture".into(), "fast-intent".into())
                .with_routing(Some("analysis-model".into()), Some("latency".into()), Some("throughput".into())).unwrap();
            for (surface, model, sort) in [("chat", "fast-intent", "latency"), ("intent", "fast-intent", "latency"), ("capability-plan", CAPABILITY_PLAN_MODEL, "latency"), ("analysis", "analysis-model", "throughput"), ("multi-source-analysis", MULTISOURCE_ANALYSIS_MODEL, "throughput")] {
                let value = parser.routed_payload(surface, &payload);
                assert_eq!(value["model"], model);
                assert_eq!(value["provider"]["require_parameters"], true);
                assert_eq!(value["response_format"], payload["response_format"]);
                assert_eq!(value["provider"]["sort"], if routed { json!(sort) } else { Value::Null });
            }
        }
        assert!(Parser::new(reqwest::Client::new(), "".into(), "".into(), "fixture".into())
            .with_routing(None, Some("typo".into()), None).is_err());
    }
    #[test]
    fn capability_planning_uses_stronger_model_without_altering_other_stage_parameters() {
        let parser = Parser::new(reqwest::Client::new(), "https://openrouter.ai/api/v1/chat/completions".into(), "fixture".into(), MODEL.into());
        let payload = json!({"temperature":0,"reasoning":{"effort":"minimal","exclude":true},"provider":{"require_parameters":true},
            "response_format":{"type":"json_schema","json_schema":{"name":"capability_plan","strict":true,"schema":{"type":"object"}}}});
        let routed = parser.routed_payload("capability-plan", &payload);
        assert_eq!(routed["model"], CAPABILITY_PLAN_MODEL);
        assert!(routed.get("temperature").is_none());
        assert_eq!(routed["reasoning"], payload["reasoning"]);
        assert_eq!(routed["response_format"]["json_schema"]["schema"], payload["response_format"]["json_schema"]["schema"]);
        assert_eq!(routed["response_format"]["json_schema"]["strict"], false);
        assert_eq!(routed["provider"], payload["provider"]);
        assert_eq!(parser.routed_payload("chat", &payload)["model"], MODEL);
        assert_eq!(parser.routed_payload("analysis", &payload)["model"], MODEL);
        assert!(intent_repair_message("capability-plan", "non_literal_input").contains("message, discover and intent"));
    }

    #[test]
    fn gemma_3_uses_supported_parameters_and_keeps_output_validation() {
        let parser = Parser::new(reqwest::Client::new(), "https://openrouter.ai/api/v1/chat/completions".into(), "fixture".into(), "google/gemma-3-12b-it".into());
        let payload = json!({"reasoning":{"effort":"minimal","exclude":true},"provider":{"require_parameters":true},"response_format":{"type":"json_object"}});
        for surface in ["chat", "intent", "analysis", "clarification"] {
            let routed = parser.routed_payload(surface, &payload);
            assert_eq!(routed["model"], "google/gemma-3-12b-it");
            assert!(routed.get("reasoning").is_none());
            assert_eq!(routed["provider"]["require_parameters"], true);
            assert_eq!(routed["response_format"]["type"], if matches!(surface, "chat" | "intent") { "json_schema" } else { "json_object" });
        }
    }
    #[test]
    fn apiosks_one_model_gets_no_reasoning_parameter_and_the_intent_schema() {
        // Qwen3 Instruct 2507 providers list no `reasoning` parameter: with
        // require_parameters it would match no provider and every call fails.
        let qwen = Parser::new(reqwest::Client::new(), "https://openrouter.ai/api/v1/chat/completions".into(), "fixture".into(), MODEL.into());
        let payload = json!({"reasoning":{"effort":"minimal","exclude":true},"provider":{"require_parameters":true},"response_format":{"type":"json_object"}});
        for surface in ["chat", "intent", "analysis", "capability-intake"] {
            let routed = qwen.routed_payload(surface, &payload);
            assert_eq!(routed["model"], MODEL);
            assert!(routed.get("reasoning").is_none(), "{surface}");
            assert_eq!(routed["provider"]["require_parameters"], true);
        }
        assert_eq!(qwen.routed_payload("chat", &payload)["response_format"]["type"], "json_schema");
        let multi = qwen.routed_payload("multi-source-analysis", &payload);
        assert_eq!(multi["model"], MULTISOURCE_ANALYSIS_MODEL);
        assert_eq!(multi["reasoning"], payload["reasoning"]);
        assert!(multi.get("temperature").is_none());
        assert_eq!(multi["response_format"]["type"], "json_object");
    }
    #[test]
    fn gemma_answers_without_thinking_and_other_models_keep_their_effort() {
        let payload = json!({"reasoning":{"effort":"minimal","exclude":true}});
        let routed = |model: &str| Parser::new(reqwest::Client::new(), "https://openrouter.ai/api/v1/chat/completions".into(), "fixture".into(), model.into())
            .routed_payload("chat", &payload);
        assert_eq!(routed("google/gemma-4-26b-a4b-it")["reasoning"], json!({"enabled": false}));
        assert_eq!(routed("google/gemini-3.8-flash")["reasoning"], payload["reasoning"]);
        assert!(routed("google/gemma-4-26b-a4b-it").pointer("/reasoning").is_some());
        let without = Parser::new(reqwest::Client::new(), "https://openrouter.ai/api/v1/chat/completions".into(), "fixture".into(), "google/gemma-4-26b-a4b-it".into())
            .routed_payload("chat", &json!({}));
        assert!(without.get("reasoning").is_none(), "a payload without reasoning gets none added");
        let json_mode = json!({"response_format":{"type":"json_object"}});
        let gemma = Parser::new(reqwest::Client::new(), "https://openrouter.ai/api/v1/chat/completions".into(), "fixture".into(), "google/gemma-4-26b-a4b-it".into());
        let chat = gemma.routed_payload("chat", &json_mode);
        assert_eq!(chat["response_format"]["type"], "json_schema");
        assert_eq!(chat["response_format"]["json_schema"]["schema"]["properties"]["intent"]["anyOf"][0]["properties"]["answer_operations"]["items"]["type"], "string");
        assert_eq!(gemma.routed_payload("analysis", &json_mode)["response_format"], json_mode["response_format"], "only intent-carrying replies get the schema");
        let gemini = Parser::new(reqwest::Client::new(), "https://openrouter.ai/api/v1/chat/completions".into(), "fixture".into(), "google/gemini-3.8-flash".into());
        assert_eq!(gemini.routed_payload("chat", &json_mode)["response_format"], json_mode["response_format"]);
    }
    #[test]
    #[ignore = "local catalog snapshot inspection only; no network"]
    fn inspect_production_planning_context() {
        let catalog = Catalog::build(serde_json::from_slice(&std::fs::read(std::env::var("ASK_CATALOG_SNAPSHOT").unwrap()).unwrap()).unwrap());
        for capability in ["company.registry-profile", "company.sanctions-screen"] {
            let selected = [capability.to_owned()].into_iter().collect();
            let original = catalog.selected_parser_view(&selected);
            let view = capability_planning_view(original.clone());
            let schema = capability_reply_schema(&view, &Bindings::default(), &selected, true,1);
            println!("{capability}: planning catalog {} -> {} bytes; constrained schema {} bytes, input types {}", original.to_string().len(), view.to_string().len(), schema.to_string().len(), view["input_types"].as_array().unwrap().len());
            for (key, value) in view.as_object().unwrap() { println!("  {key}: {} bytes", value.to_string().len()); }
            assert!(view.to_string().len() < 35_000);
            assert!(view["input_types"].as_array().unwrap().contains(&json!("company.name")));
            assert_eq!(schema["properties"]["intent"]["anyOf"][0]["properties"]["subjects"]["items"]["properties"]["values"]["additionalProperties"], false);
            // Offline contract check only: no model, task persistence or source
            // call. The exact production contracts must accept the bound name.
            let question = "Check Coolblue B.V.";
            let parsed = crate::capability_intake::normalize(&json!({"language":"en",
                "subjects":[{"id":"s1","label":"Coolblue B.V.","type":"company","identifiers":{}}],
                "capabilities":[{"slug":capability,"name":"Company check","description":"Check company data","domain":"company","inputs":["company_name"],"outputs":[],"subject_id":"s1","source_hint":null,"confidence":1}],
                "deliverable":{"format":"chat","operations":[]}}).to_string(), question, "offline fixture").unwrap();
            let mut task = Task::default();
            let bindings = Bindings::new(&parsed, question, &task, None);
            let mut intent = bindings.decode(&json!({"message":"Price first","discover":true,"intent":{
                "subjects":[{"subject_ref":"s1","values":{"company.name":{"ref":"s1.label"}}}],
                "goals":[{"capability":capability,"subject":0,"constraints":{}}],"answer_operations":[],"unresolved_references":[]}}).to_string()).unwrap().intent.unwrap();
            intent.validate(question, &task, &catalog).unwrap();
            crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
            assert!(matches!(crate::resolver::resolve(&catalog, &task, &intent), crate::resolver::Resolution::Plan(_)));
            println!("{capability}: parsed subject binding compiles against the local contract snapshot; no model or source request");
        }
    }

    #[test]
    fn capability_plan_schema_rejects_guessed_input_names_and_keeps_dependencies() {
        let catalog = Catalog::build(serde_json::from_str(include_str!("../tests/fixtures/integration-catalog.json")).unwrap());
        let selected = ["company.accounts".to_owned()].into_iter().collect();
        let view = capability_planning_view(catalog.selected_parser_view(&selected));
        let schema = capability_reply_schema(&view, &Bindings::default(), &selected, true,1);
        let intent = &schema["properties"]["intent"]["anyOf"][0];
        let values = &intent["properties"]["subjects"]["items"]["properties"]["values"];
        assert_eq!(values["additionalProperties"], false);
        assert!(values["properties"].get("company.name").is_some());
        assert!(values["properties"].get("company_name").is_none());
        assert!(values["properties"].get("company.kvk_number").is_none());
        let literal = &values["properties"]["company.name"]["anyOf"][1];
        catalog_core::facts::schema::validate(literal, &json!({"literal":"Coolblue"})).unwrap();
        assert_eq!(intent["properties"]["goals"]["items"]["properties"]["capability"]["enum"], json!(["company.accounts"]));
        assert_eq!(intent["properties"]["goals"]["maxItems"],json!(1),"candidate alternatives cannot become extra requested goals");
        assert!(view["capabilities"].as_array().unwrap().contains(&json!("company.search")), "dependency still supplies the name input");
        let parser = Parser::new(reqwest::Client::new(), "https://openrouter.ai/api/v1/chat/completions".into(), "fixture".into(), "google/gemma-3-12b-it".into());
        let payload = json!({"response_format":{"type":"json_schema","json_schema":{"name":"capability_plan","strict":true,"schema":schema}}});
        assert_eq!(parser.routed_payload("chat", &payload)["response_format"], payload["response_format"], "routing must retain per-request vocabulary constraints");
        let empty = capability_reply_schema(&view, &Bindings::default(), &Default::default(), true,1);
        assert_eq!(empty["properties"]["intent"], json!({"type":"null"}));
        let partial = capability_reply_schema(&view, &Bindings::default(), &selected, false,1);
        assert_eq!(partial["properties"]["intent"], json!({"type":"null"}));
        assert_eq!(partial["properties"]["discover"]["enum"], json!([false]), "an unmatched essential capability requires a clarification, even if another capability has a match");
    }

    #[test]
    fn capability_plan_omits_unrelated_directory_but_preserves_restrictions() {
        let view = capability_planning_view(json!({"sources":[
            {"slug":"selected","available":true,"capabilities":["company.accounts"],"tags":["unrelated"],"readiness":{"status":"ready","restrictions":["latest filing only"],"operations":["large repeated catalog"]}},
            {"slug":"unrelated","available":true,"capabilities":[]},
            {"slug":"blocked","name":"Blocked source","available":false,"capabilities":[],"reason":"No active execution contract",
                "readiness":{"status":"blocked","restrictions":["Blocked source: No active execution contract","A separate restriction"],"coverage_notices":["Limited coverage"]}}
        ]}));
        assert_eq!(view["sources"].as_array().unwrap().len(), 2);
        assert_eq!(view["sources"][0]["readiness"]["restrictions"], json!(["latest filing only"]));
        assert!(view["sources"][0].get("tags").is_none());
        assert!(view["sources"][0]["readiness"].get("operations").is_none());
        assert_eq!(view["sources"][1]["reason"], "No active execution contract");
        assert_eq!(view["sources"][1]["readiness"]["restrictions"], json!(["A separate restriction"]));
        assert_eq!(view["sources"][1]["readiness"]["coverage_notices"], json!(["Limited coverage"]));
    }

    #[test]
    fn chat_keeps_the_complete_catalog_once() {
        let catalog = Catalog::build(serde_json::from_str(include_str!("../tests/fixtures/supplier-check/catalog.json")).unwrap());
        let task = Task::default();
        let input = chat_input("Wat is het adres van Mollie B.V.?", &task, &catalog);
        assert_eq!(input["catalog"], catalog.parser_view());
        assert!(input.get("sources").is_none());
        assert!(input.get("capabilities").is_none());
        let mut old = input.clone();
        old["sources"] = input["catalog"]["sources"].clone();
        old["capabilities"] = input["catalog"]["capabilities"].clone();
        eprintln!("chat input fixture: {} -> {} bytes", old.to_string().len(), input.to_string().len());
        assert!(input.to_string().len() < old.to_string().len());
    }
    #[test]
    fn planning_cannot_drop_periods_goals_or_change_an_explicit_source() {
        let catalog = Catalog::build(
            serde_json::from_str(include_str!("../tests/fixtures/integration-catalog.json"))
                .unwrap(),
        );
        let task = Task::default();
        let original: Intent = serde_json::from_value(json!({
            "subjects":[{"entity_ref":null,"label":"Example","values":{"company.name":"Example"}}],
            "goals":[{"subject":0,"capability":"company.accounts","constraints":{"source":"fixture","period":"2024 en 2025"}}],
            "answer_operations":["compare"],"unresolved_references":[]
        })).unwrap();
        for mutate in [
            "drop_period",
            "drop_goal",
            "change_source",
            "unsupported_plan",
        ] {
            let mut revised = original.clone();
            match mutate {
                "drop_period" => {
                    revised.goals[0].constraints.remove("period");
                }
                "drop_goal" => revised.goals.clear(),
                "change_source" => {
                    revised.goals[0]
                        .constraints
                        .insert("source".into(), json!("other"));
                }
                _ => (),
            }
            assert!(
                PlanningReview {
                    message: "Plan".into(),
                    intent: Some(revised)
                }
                .validate(&original, "Example 2024 en 2025", &task, &catalog)
                .is_err(),
                "{mutate}"
            );
        }
        assert!(PlanningReview {
            message: "Wil je de beschikbare jaarrekening ophalen?".into(),
            intent: None
        }
        .validate(&original, "Example 2024 en 2025", &task, &catalog)
        .is_ok());
    }
    async fn model_fixture(
        responses: Vec<(u16, Value)>,
    ) -> (Parser, Arc<AtomicU64>, tokio::task::JoinHandle<()>) {
        let (parser, calls, server, _) = model_fixture_with_requests(responses).await;
        (parser, calls, server)
    }

    async fn model_fixture_with_requests(
        responses: Vec<(u16, Value)>,
    ) -> (Parser, Arc<AtomicU64>, tokio::task::JoinHandle<()>, Arc<std::sync::Mutex<Vec<Value>>>) {
        use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
        use std::{collections::VecDeque, sync::Mutex};
        type ModelState = (Arc<Mutex<VecDeque<(u16, Value)>>>, Arc<AtomicU64>, Arc<Mutex<Vec<Value>>>);
        let calls = Arc::new(AtomicU64::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = (
            Arc::new(Mutex::new(VecDeque::from(responses))),
            calls.clone(),
            requests.clone(),
        );
        let app = Router::new()
            .route(
                "/",
                post(|State((queue, calls, requests)): State<ModelState>, Json(request): Json<Value>| async move {
                    calls.fetch_add(1, Ordering::Relaxed);
                    requests.lock().unwrap().push(request);
                    let (status, body) = queue
                        .lock()
                        .unwrap()
                        .pop_front()
                        .expect("unexpected extra model request");
                    (StatusCode::from_u16(status).unwrap(), Json(body))
                }),
            )
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (
            Parser::new(
                reqwest::Client::new(),
                url,
                "fixture".into(),
                "fixture".into(),
            ),
            calls,
            server,
            requests,
        )
    }

    #[tokio::test]
    async fn non_literal_intent_retries_with_feedback_without_relaxing_validation() {
        let catalog = Catalog::build(serde_json::from_str(include_str!("../tests/fixtures/integration-catalog.json")).unwrap());
        let question = "Find European fintech funding and expansion signals";
        let task = Task::default();
        let valid = json!({
            "subjects":[{"entity_ref":null,"label":"European fintech funding and expansion signals","values":{"company.name":"European fintech funding and expansion signals"}}],
            "goals":[{"subject":0,"capability":"company.accounts","constraints":{}}],
            "answer_operations":[],"unresolved_references":[]
        });
        let mut invalid = valid.clone();
        invalid["subjects"][0]["values"]["company.name"] = json!("European fintech funding expansion");
        let envelope = |intent: &Value| json!({"choices":[{"finish_reason":"stop","message":{"content":intent.to_string()}}]});
        for corrected in [true, false] {
            let second = if corrected { &valid } else { &invalid };
            let (parser, calls, server, requests) = model_fixture_with_requests(vec![
                (200, envelope(&invalid)), (200, envelope(second)),
            ]).await;
            let result = parser.parse(Uuid::new_v4(), question, &task, &catalog).await;
            assert_eq!(result.is_ok(), corrected);
            assert_eq!(calls.load(Ordering::Relaxed), 2);
            let requests = requests.lock().unwrap();
            let messages = requests[1]["messages"].as_array().unwrap();
            assert_eq!(messages[messages.len()-2]["content"], invalid.to_string());
            let feedback = messages.last().unwrap()["content"].as_str().unwrap();
            assert!(feedback.contains("non_literal_input"));
            assert!(feedback.contains("exact contiguous text"));
            assert!(feedback.contains("Preserve every requested retrieval"));
            assert_eq!(requests[0]["messages"][1], requests[1]["messages"][1]);
            server.abort();
        }
    }

    #[test]
    fn intent_diagnostics_are_fixed_and_chat_repair_preserves_envelope() {
        let error = anyhow::anyhow!("secret user value from model output");
        assert_eq!(intent_validation_reason(&error), "invalid_intent_schema");
        let repair = intent_repair_message("chat", "non_literal_input");
        assert!(repair.contains("message, discover and intent"));
        assert!(!repair.contains("secret user value"));
    }

    #[tokio::test]
    async fn invalid_planning_rewrite_recovers_with_clarification() {
        let catalog = Catalog::build(
            serde_json::from_str(include_str!("../tests/fixtures/integration-catalog.json"))
                .unwrap(),
        );
        let task = Task::default();
        let original: Intent = serde_json::from_value(json!({
            "subjects":[{"entity_ref":null,"label":"Example","values":{"company.name":"Example"}}],
            "goals":[{"subject":0,"capability":"company.accounts","constraints":{"filing_selection":"latest"}}],
            "answer_operations":[],"unresolved_references":[]
        })).unwrap();
        let mut invalid = original.clone();
        invalid.goals.push(original.goals[0].clone());
        let envelope = |review: Value| json!({"choices":[{"finish_reason":"stop","message":{"content":review.to_string()}}]});
        let (parser, calls, server) = model_fixture(vec![
            (200, envelope(json!({"message":"Retrieve latest","intent":invalid}))),
            (200, envelope(json!({"message":"Shall I retrieve the available filing and report its year?","intent":null}))),
        ]).await;
        let review = parser
            .review_plan(
                "Example latest",
                &task,
                &catalog,
                &original,
                "unsupported filing selection",
            )
            .await
            .unwrap();
        assert!(review.intent.is_none());
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(original.goals[0].constraints["filing_selection"], "latest");
        server.abort();
    }

    fn decode_chat(raw: &str) -> anyhow::Result<ChatReply> {
        let reply: ChatReply = serde_json::from_str(raw)?;
        reply.validate()?;
        Ok(reply)
    }

    #[tokio::test]
    async fn one_vat_retrieval_recovers_from_duplicate_alternatives_without_losing_scope() {
        let fixture: Value=serde_json::from_str(include_str!("../tests/fixtures/profession-vies-planning.json")).unwrap();
        let catalog=Catalog::build(serde_json::from_value(fixture["catalog"].clone()).unwrap());
        let intake:crate::capability_intake::Intake=serde_json::from_value(fixture["intake"].clone()).unwrap();
        let question=fixture["question"].as_str().unwrap();
        let invalid=json!({"message":"I will show the price.","discover":true,"decline_reason":"none","intent":{
            "subjects":[{"subject_ref":intake.parsed_request.subjects[0].id,"values":{}}],
            "goals":[{"subject":0,"capability":"vat.eu.check","constraints":{}},{"subject":0,"capability":"vat.eu.validate","constraints":{}}],
            "answer_operations":[],"unresolved_references":[]}});
        let envelope=json!({"choices":[{"finish_reason":"stop","message":{"content":invalid.to_string()}}]});
        for unsupported in [false,true] {
            let mut request=intake.clone();
            if unsupported {request.parsed_request.capabilities[0].outputs.push("vat_registration_date".into());}
            let (parser,calls,server)=model_fixture(vec![(200,envelope.clone()),(200,envelope.clone())]).await;
            let reply=parser.chat_with_view(question,None,&Task::default(),&catalog,capability_planning_view(catalog.selected_parser_view(&request.selected_capabilities)),Some(&request)).await.unwrap();
            assert_eq!(calls.load(Ordering::Relaxed),2,"bounded correction remains two calls");
            if unsupported {
                assert!(!reply.discover && reply.intent.is_none(),"recovery cannot remove an unsupported output");
            } else {
                let intent=reply.intent.expect("the displayed validity/date contract resolves with the supplied VAT identifier");
                assert!(reply.discover);assert_eq!(intent.goals.len(),1);
                assert!(matches!(intent.goals[0].capability.as_str(),"vat.eu.check"|"vat.eu.validate"));
                assert_eq!(intent.subjects[0].values["vat.eu.country_code"],json!("NL"));
                assert_eq!(intent.subjects[0].values["vat.eu.number"],json!("002033354B01"));
            }
            server.abort();
        }
        let (parser,calls,server)=model_fixture(vec![(503,json!({"error":"unavailable"})),(503,json!({"error":"unavailable"}))]).await;
        let error=parser.chat_with_view(question,None,&Task::default(),&catalog,capability_planning_view(catalog.selected_parser_view(&intake.selected_capabilities)),Some(&intake)).await.unwrap_err();
        assert!(matches!(error.downcast_ref::<ModelFailure>(),Some(ModelFailure::Unavailable)));
        assert_eq!(calls.load(Ordering::Relaxed),2,"existing bounded upstream retry is preserved without semantic fallback");
        server.abort();
    }

    #[tokio::test]
    async fn raw_api_alias_cannot_bypass_input_provenance_or_reach_an_invented_review() {
        let fixture: Value = serde_json::from_str(include_str!("../tests/fixtures/profession-case09-raw-constraints.json")).unwrap();
        let catalog = Catalog::build(serde_json::from_value(fixture["catalog"].clone()).unwrap());
        let intake: crate::capability_intake::Intake = serde_json::from_value(fixture["intake"].clone()).unwrap();
        let mut original: Intent = serde_json::from_value(fixture["intent"].clone()).unwrap();
        for subject in &mut original.subjects {subject.entity_ref=None;}
        assert_eq!(original.validate(fixture["question"].as_str().unwrap(),&Task::default(),&catalog).unwrap_err().to_string(),"unsupported retrieval constraint");
        let mut intent=fixture["intent"].clone();
        intent["subjects"] = json!(intent["subjects"].as_array().unwrap().iter().map(|subject|{
            let values = subject["values"].as_object().unwrap().iter().map(|(key,value)|(key.clone(),json!({"literal":value}))).collect::<serde_json::Map<_,_>>();
            json!({"subject_ref":intake.parsed_request.subjects[0].id,"values":values})
        }).collect::<Vec<_>>());
        let envelope=json!({"choices":[{"finish_reason":"stop","message":{"content":json!({"message":"I will show the price.","discover":true,"intent":intent,"decline_reason":"none"}).to_string()}}]});
        let (parser,calls,server)=model_fixture(vec![(200,envelope.clone()),(200,envelope)]).await;
        let reply=parser.chat_from_capabilities(fixture["question"].as_str().unwrap(),None,&Task::default(),&catalog,&intake).await.unwrap();
        assert!(!reply.discover && reply.intent.is_none() && reply.decline_reason.is_none());
        assert!(reply.message.contains("geen volledig plan"));
        assert!(!reply.message.contains("2023") && !reply.message.contains("5210"));
        assert_eq!(calls.load(Ordering::Relaxed),2,"no free-form planning review is called after refusal");
        server.abort();
    }

    #[tokio::test]
    async fn invented_live_constraint_inputs_never_reach_a_price_or_false_provider_review() {
        let fixture: Value = serde_json::from_str(include_str!("../tests/fixtures/profession-case09-constraints.json")).unwrap();
        let catalog = Catalog::build(serde_json::from_value(fixture["catalog"].clone()).unwrap());
        let intake: crate::capability_intake::Intake = serde_json::from_value(fixture["intake"].clone()).unwrap();
        let mut intent = fixture["intent"].clone();
        let values = intent["subjects"][0]["values"].as_object().unwrap().iter()
            .map(|(field,value)|(field.clone(),json!({"literal":value}))).collect::<serde_json::Map<_,_>>();
        intent["subjects"] = json!([{"subject_ref":intake.parsed_request.subjects[0].id,"values":values}]);
        let envelope = json!({"choices":[{"finish_reason":"stop","message":{"content":json!({"message":"I will show the price.","discover":true,"intent":intent,"decline_reason":"none"}).to_string()}}]});
        let (parser,calls,server) = model_fixture(vec![(200,envelope.clone()),(200,envelope)]).await;
        let reply = parser.chat_from_capabilities(fixture["question"].as_str().unwrap(),None,&Task::default(),&catalog,&intake).await.unwrap();
        assert!(!reply.discover && reply.intent.is_none() && reply.decline_reason.is_none());
        assert!(reply.message.contains("geen volledig plan"));
        assert!(!reply.message.contains("2023") && !reply.message.contains("32006L0029") && !reply.message.contains("geen geldige uitvoeringscontracten"));
        assert_eq!(calls.load(Ordering::Relaxed),2,"no extra planning review or source call");
        server.abort();
    }

    #[tokio::test]
    async fn semantic_rejections_are_distinct_from_service_and_malformed_response_failures() {
        let envelope = json!({"choices":[{"finish_reason":"stop","message":{"content":"{}"}}]});
        for (surface, rejection) in ["chat", "capability-plan"].into_iter().flat_map(|surface| ["duplicate retrieval goal", "every parsed subject needs its requested retrieval", "input belongs to a different parsed subject", "parser invented a value"].map(|reason|(surface,reason))) {
            let (parser, calls, server) = model_fixture(vec![(200,envelope.clone()),(200,envelope.clone())]).await;
            let error = parser.completion::<()>(surface, &json!({}), |_| anyhow::bail!(rejection)).await.unwrap_err();
            assert!(error.is::<RequirementRejection>(), "{rejection}: {error}");
            assert_eq!(calls.load(Ordering::Relaxed),2);
            server.abort();
        }
        for failed in [(503,json!({"error":"unavailable"})),(200,json!({"choices":[]})),(200,json!({"choices":[{"finish_reason":"length","message":{"content":"{"}}]}))] {
            let (parser, calls, server) = model_fixture(vec![(200,envelope.clone()),failed]).await;
            let error = parser.completion::<()>("chat", &json!({}), |_| anyhow::bail!("duplicate retrieval goal")).await.unwrap_err();
            assert!(matches!(error.downcast_ref::<ModelFailure>(),Some(ModelFailure::Unavailable)));
            assert_eq!(calls.load(Ordering::Relaxed),2);
            server.abort();
        }
        let (parser, _, server) = model_fixture(vec![(200,envelope.clone()),(200,envelope)]).await;
        let count = AtomicU64::new(0);
        let error = parser.completion::<()>("chat",&json!({}), |_| {
            if count.fetch_add(1,Ordering::Relaxed)==0 { anyhow::bail!("duplicate retrieval goal"); }
            anyhow::bail!("invalid JSON");
        }).await.unwrap_err();
        assert!(matches!(error.downcast_ref::<ModelFailure>(),Some(ModelFailure::Unavailable)));
        server.abort();
    }

    #[tokio::test]
    async fn empty_truncated_and_invalid_model_output_recovers_once() {
        let valid = json!({"error":null,"choices":[{"finish_reason":"stop","message":{"content":r#"{"message":"Which period do you mean?","discover":false}"#}}]});
        for failure in [
            json!({"choices":[{"message":{"content":null,"reasoning":r#"{"message":"not the answer","discover":true}"#}}]}),
            json!({"choices":[{"finish_reason":"length","message":{"content":r#"{"message":"truncated","discover":true}"#}}]}),
            json!({"choices":[{"message":{"content":"not JSON"}}]}),
            json!({"choices":[{"message":{"content":r#"{"message":"bad types","discover":"yes"}"#}}]}),
            json!({"error":{"code":503,"message":"private upstream detail"}}),
        ] {
            let (parser, calls, server) =
                model_fixture(vec![(200, failure), (200, valid.clone())]).await;
            let reply = parser
                .completion("chat", &json!({}), decode_chat)
                .await
                .unwrap();
            assert_eq!(reply.message, "Which period do you mean?");
            assert!(!reply.discover);
            assert_eq!(calls.load(Ordering::Relaxed), 2);
            server.abort();
        }
    }

    #[tokio::test]
    async fn repeated_empty_output_is_bounded_and_has_a_safe_error() {
        let empty = json!({"choices":[{"message":{"content":null}}]});
        let (parser, calls, server) = model_fixture(vec![(200, empty.clone()), (200, empty)]).await;
        let error = parser
            .completion("chat", &json!({}), decode_chat)
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<ModelFailure>(),
            Some(ModelFailure::Unavailable)
        ));
        assert!(!error.to_string().contains("JSON"));
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        server.abort();
    }

    #[tokio::test]
    async fn refusals_and_permanent_http_errors_are_not_retried() {
        for response in [
            (
                200,
                json!({"choices":[{"finish_reason":"content_filter","message":{"content":null}}]}),
            ),
            (
                401,
                json!({"error":{"message":"secret upstream configuration"}}),
            ),
        ] {
            let (parser, calls, server) = model_fixture(vec![response]).await;
            assert!(parser
                .completion("chat", &json!({}), decode_chat)
                .await
                .is_err());
            assert_eq!(calls.load(Ordering::Relaxed), 1);
            server.abort();
        }
    }

    #[test]
    fn chat_reply_is_bounded_and_cannot_authorize_or_fill_inputs() {
        let reply: ChatReply = serde_json::from_value(
            json!({"message":"Hallo! Hoe kan ik je helpen?","discover":false}),
        )
        .unwrap();
        assert!(reply.validate().is_ok());
        assert!(serde_json::from_value::<ChatReply>(
            json!({"message":"Yes","discover":true,"approved":true})
        )
        .is_err());
        assert!(ChatReply { output_bindings: vec![],
            message: " ".into(),
            discover: false,
            intent: None,
            decline_reason: None,
        }
        .validate()
        .is_err());
        assert!(ChatReply { output_bindings: vec![],
            message: "x".repeat(3001),
            discover: false,
            intent: None,
            decline_reason: None,
        }
        .validate()
        .is_err());
    }

    #[test]
    fn a_null_subject_is_repaired_only_when_there_is_one_unambiguous_subject() {
        let repaired = decode_intent(
            r#"{
            "subjects":[{"entity_ref":null,"label":"Acme","values":{}}],
            "goals":[{"capability":"company.search","subject":null,"constraints":{}}],
            "answer_operations":[],"unresolved_references":[]
        }"#,
        )
        .unwrap();
        assert_eq!(repaired.goals[0].subject, 0);
        assert!(decode_intent(r#"{
            "subjects":[{"entity_ref":null,"label":"Acme","values":{}},{"entity_ref":null,"label":"Beta","values":{}}],
            "goals":[{"capability":"company.search","subject":null,"constraints":{}}],
            "answer_operations":[],"unresolved_references":[]
        }"#).is_err());
    }
}
