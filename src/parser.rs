use crate::{
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
    /// Copy an exact question span back into model strings that only changed
    /// separators. No words may be added, dropped, reordered or inferred; the
    /// original literal and the catalog schema are still validated afterwards.
    fn restore_question_literals(&mut self, question: &str) {
        for subject in &mut self.subjects {
            if subject.entity_ref.is_none() {
                if let Some(original) = question_literal(question, &subject.label) {
                    subject.label = original.to_owned();
                }
            }
            for value in subject.values.values_mut() {
                if let Some(original) = value.as_str().and_then(|text| question_literal(question, text)) {
                    *value = json!(original);
                }
            }
        }
    }

    pub fn validate(&self, question: &str, task: &Task, catalog: &Catalog) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.subjects.len() <= 8 && !self.goals.is_empty() && self.goals.len() <= 8,
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
                let literal = match value {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    _ => anyhow::bail!("explicit values must be scalar literals"),
                };
                anyhow::ensure!(
                    !literal.is_empty()
                        && question.to_lowercase().contains(&literal.to_lowercase()),
                    "parser invented a value"
                );
                let ft = catalog
                    .vocabulary
                    .get(kind)
                    .ok_or_else(|| anyhow::anyhow!("unknown input type"))?;
                catalog_core::facts::schema::validate(&ft.value_schema, value)?;
            }
        }
        for g in &self.goals {
            anyhow::ensure!(g.subject < self.subjects.len(), "invalid subject index");
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
        "unknown input type" => "unknown_input_type",
        "explicit values must be scalar literals" => "non_scalar_input",
        "invalid subject index" => "invalid_subject_index",
        "invalid intent size" => "invalid_intent_size",
        _ => "invalid_intent_schema",
    }
}

fn intent_repair_message(surface: &str, reason: &str) -> String {
    let shape = if surface == "chat" {
        "Return the chat envelope with message, discover and intent; place the corrected intent inside intent."
    } else {
        "Return only the Intent object with subjects, goals, answer_operations and unresolved_references."
    };
    format!("The previous response failed validation ({reason}). {shape} New subject labels and every value in subjects.values must be exact contiguous text copied from the original question. This includes search queries: copy the relevant phrase without dropping or reordering words, rather than composing new keywords. Do not invent company names, identifiers, country codes or entity references. Use only catalog input types and capabilities. Preserve every requested retrieval and constraint; do not hide a missing retrieval in answer_operations. If a downstream company or person cannot be identified by a supported dependency, explain that requirement in unresolved_references instead of inventing the entity. Correct the JSON shape, keeping goal subject indices within subjects and between one and eight goals.")
}
/// App-only conversational routing; never an executable action or approval.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatReply {
    pub message: String,
    pub discover: bool,
    #[serde(default)]
    pub intent: Option<Intent>,
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
    http: reqwest::Client,
    url: String,
    key: String,
    model: String,
    analysis_model: String,
    intent_provider_sort: Option<String>,
    analysis_provider_sort: Option<String>,
    typesafe: Option<crate::typesafe::Client>,
    pub usage: crate::typesafe::Usage,
    cache: moka::future::Cache<String, Arc<Intent>>,
    pub calls: AtomicU64,
}

fn chat_input(question: &str, task: &Task, catalog: &Catalog) -> Value {
    json!({"surface":"ask-chat","question":question,"context":task.parser_context(),"catalog":catalog.parser_view()})
}

pub(crate) fn validate_analysis(raw: &str, sources: &[Value], partial: bool) -> anyhow::Result<Value> {
    validate_analysis_limits(raw, sources, partial, 8, 6)
}

fn validate_analysis_limits(raw: &str, sources: &[Value], partial: bool, max_observations: usize, max_limitations: usize) -> anyhow::Result<Value> {
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
            !refs.is_empty() && refs.len() <= 4,
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
        for (index, letter) in ["a", "b", "c", "d"].iter().enumerate() {
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
        let answer = parser.analyze_with_scope(&json!(["Find the price"]), &sources, &BTreeMap::new(), true).await.unwrap();
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
            let answer = parser.analyze_with_scope(&json!(["Identify the company"]), &sources, &BTreeMap::new(), workflow).await.unwrap();
            assert_eq!(answer["status"], "completed");
            assert_eq!(answer["observations"][0]["text"], "The company is Example B.V..");
            assert_eq!(answer["observations"][0]["evidence"][0]["value"], "Example B.V.");
        }
        server.abort();
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

    pub async fn analyze(
        &self,
        operations: &Value,
        sources: &[Value],
        failures: &BTreeMap<String, Value>,
    ) -> anyhow::Result<Value> {
        if failures.is_empty() {
            if let Some(client) = &self.typesafe {
                if client.mode() == crate::typesafe::Mode::On {
                    if let Some(answer) = client.analyze(operations, sources).await { return Ok(answer); }
                } else {
                    let (candidate, baseline) = tokio::join!(client.analyze(operations, sources), self.analyze_with_scope(operations, sources, failures, false));
                    tracing::info!(surface = "analysis", candidate_available = candidate.is_some(), baseline_available = baseline.is_ok(), "typesafe shadow comparison");
                    return baseline;
                }
            }
        }
        if failures.is_empty() && self.typesafe.is_none() {
            if let Some(answer) = empty_source_analysis(sources) { return Ok(answer); }
        }
        self.analyze_with_scope(operations, sources, failures, false).await
    }

    pub async fn analyze_workflow(&self, operations: &Value, sources: &[Value], failures: &BTreeMap<String, Value>) -> anyhow::Result<Value> {
        if failures.is_empty() {
            if let Some(answer) = empty_source_analysis(sources) { return Ok(answer); }
        }
        self.analyze_with_scope(operations, sources, failures, true).await
    }

    async fn analyze_with_scope(&self, operations: &Value, sources: &[Value], failures: &BTreeMap<String, Value>, workflow: bool) -> anyhow::Result<Value> {
        let (max_observations, max_limitations) = if workflow { (24, 12) } else { (8, 6) };
        // Large saved documents are narrowed to question-relevant evidence;
        // citations are still validated against the originals.
        let mut input = json!({"answer_operations":operations,"sources":[],"unavailable_steps":failures});
        let budget = crate::evidence_selection::ANALYSIS_INPUT_BYTES.saturating_sub(serde_json::to_vec(&input)?.len());
        input["sources"] = json!(crate::evidence_selection::project(sources, operations, budget));
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
        let mut prompt = include_str!("../contracts/analysis-prompt.txt").to_owned();
        if workflow {
            prompt = prompt.replace("At most eight observations, four evidence fields each, and six limitations.", "At most twenty-four observations, four evidence fields each, and twelve limitations. Cover every requested check with a supported finding or an explicit unknown in limitations. For shortlists, name each candidate and explain its fit; for comparisons, group findings by company. A completed retrieval is not a passed check.");
        }
        // Gemini rejects this analysis schema at the provider boundary. JSON mode
        // is portable; validate_analysis_limits remains the authority on output
        // shape, evidence pointers, bounds, and numeric claims.
        let payload = json!({"model":self.model,"temperature":0,"max_tokens":if workflow {8192} else {4096},
            "reasoning":{"effort":"low","exclude":true},"provider":{"require_parameters":true},
            "response_format":{"type":"json_object"},"messages":[
                {"role":"system","content":prompt},
                {"role":"user","content":input.to_string()}]});
        self.completion("analysis", &payload, |raw| {
            validate_analysis_limits(raw, sources, !failures.is_empty(), max_observations, max_limitations)
        })
        .await
    }
    pub fn new(http: reqwest::Client, url: String, key: String, model: String) -> Self {
        Self {
            http,
            url,
            key,
            analysis_model: model.clone(),
            model,
            intent_provider_sort: None,
            analysis_provider_sort: None,
            typesafe: None,
            usage: crate::typesafe::Usage::default(),
            cache: moka::future::Cache::builder()
                .max_capacity(1024)
                .time_to_live(Duration::from_secs(600))
                .build(),
            calls: AtomicU64::new(0),
        }
    }
    pub fn with_typesafe(mut self, config: Option<crate::typesafe::Config>) -> anyhow::Result<Self> {
        self.typesafe = config.map(crate::typesafe::Client::new).transpose()?;
        Ok(self)
    }
    pub fn typesafe_usage(&self) -> crate::typesafe::UsageSnapshot {
        self.typesafe.as_ref().map(|client| client.usage.snapshot()).unwrap_or_default()
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
        let analysis = surface == "analysis";
        payload["model"] = json!(if analysis { &self.analysis_model } else { &self.model });
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
        if let Some(client) = &self.typesafe {
            if client.mode() == crate::typesafe::Mode::On {
                if let Some(decision) = client.parse(question, task, catalog).await {
                    return Ok(ChatReply { message: decision.message, discover: true, intent: Some(decision.intent) });
                }
            } else {
                let (candidate, baseline) = tokio::join!(client.parse(question, task, catalog), self.chat_baseline(question, task, catalog));
                shadow_intent(candidate.as_ref().map(|d| &d.intent), baseline.as_ref().ok().and_then(|r| r.intent.as_ref()));
                return baseline;
            }
        }
        self.chat_baseline(question, task, catalog).await
    }

    async fn chat_baseline(&self, question: &str, task: &Task, catalog: &Catalog) -> anyhow::Result<ChatReply> {
        // The chat route has no interpretation cache: every question is read anew.
        crate::telemetry::set_parser_cache(Cache::Miss);
        let mut input=chat_input(question, task, catalog);
        input["catalog"]=self.planning_view(question,task,catalog).await?;
        let payload = json!({
            "model": self.model, "temperature": 0, "max_tokens": 4096,
            "reasoning": {"effort":"minimal", "exclude":true},
            "provider": {"require_parameters":true},
            "response_format": {"type":"json_object"},
            "messages": [
                {"role":"system","content":format!("{}\n\nIntent interpretation rules (place their output INSIDE the intent field of the chat reply):\n{}\n\nReturn one object with message, discover, intent. For a concrete request, discover=true and intent is the complete Intent object. For conversation or clarification, discover=false and intent=null. Never return a bare Intent object.",include_str!("../contracts/ask-chat-prompt.txt"),include_str!("../contracts/parser-prompt.txt"))},
                {"role":"user","content":input.to_string()}
            ]
        });
        self.completion("chat", &payload, |raw| {
            let mut reply: ChatReply = serde_json::from_str(raw)?;
            reply.validate()?;
            if let Some(intent) = &mut reply.intent {
                anyhow::ensure!(reply.discover, "unexpected intent for conversational reply");
                intent.restore_question_literals(question);
                intent.validate(question, task, catalog)?;
            }
            Ok(reply)
        })
        .await
    }

    /// Propose invoice fields for review. Never stored, never executable.
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

    pub async fn extract_invoice(&self, document: crate::documents::Content<'_>) -> anyhow::Result<Value> {
        use crate::documents::Content;
        use crate::invoice_document::{schema, PROMPT};
        let content = match document {
            Content::Text(text) => json!(format!("Invoice text:\n{text}")),
            Content::Image(image) => json!([{"type":"text","text":"Invoice page image:"},{"type":"image_url","image_url":{"url":image}}]),
            Content::File { file_name, data_url } => json!([{"type":"text","text":"Invoice document:"},{"type":"file","file":{"filename":file_name,"file_data":data_url}}]),
        };
        let payload = json!({"model":self.model,"temperature":0,"max_tokens":1500,
            "reasoning":{"effort":"minimal","exclude":true},"provider":{"require_parameters":true},
            "response_format":{"type":"json_schema","json_schema":{"name":"invoice_fields","strict":true,"schema":schema()}},
            "messages":[{"role":"system","content":PROMPT},{"role":"user","content":content}]});
        self.completion("invoice", &payload, |raw| {
            let value: Value = serde_json::from_str(raw)?;
            anyhow::ensure!(value.is_object(), "invalid invoice fields");
            Ok(value)
        })
        .await
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
                    "available":catalog.planning_view(intent)}).to_string()}]});
        self.completion("planning", &payload, |raw| {
            let review: PlanningReview = serde_json::from_str(raw)?;
            review.validate(intent, question, task, catalog)?;
            Ok(review)
        })
        .await
    }

    /// Retrying interpretation is safe: this boundary cannot call a data
    /// provider, authorize a quote or execute a purchase. Both attempts share
    /// the original 25-second stage deadline.
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
        // Analysing saved multi-source documents takes longer than intent parsing.
        // Keep both retries bounded and below the app's 80-second request limit.
        let (attempt_seconds, total_seconds) = if surface == "clarification" {
            (6, 12)
        } else if surface == "analysis" || surface == "invoice" {
            (30, 65)
        } else {
            (12, 25)
        };
        let work = async {
            let mut attempt_payload = payload.clone();
            for attempt in 1..=2 {
                self.usage.request();
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
                                self.usage.completion(&body);
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
                                            if surface == "intent" || surface == "chat" {
                                                let reason = intent_validation_reason(&error);
                                                tracing::warn!(surface, attempt, reason, "intent validation failed");
                                                if let Some(messages) = attempt_payload["messages"].as_array_mut() {
                                                    messages.push(json!({"role":"assistant","content":raw}));
                                                    messages.push(json!({"role":"user","content":intent_repair_message(surface, reason)}));
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
                                            if surface == "analysis" {
                                                // Fixed validator classifications only; never log the model answer.
                                                let reason = match error.to_string().as_str() {
                                                    "numbers and links must come from source evidence" => "narrative_contains_numbers_or_links",
                                                    "missing source field" => "non_scalar_or_missing_pointer",
                                                    "invented source" => "unknown_source_reference",
                                                    "invalid evidence count" => "evidence_count",
                                                    "long source text needs a short exact evidence quote" => "exact_quote_required",
                                                    "evidence quote is not in the saved source" => "quote_not_verbatim",
                                                    "invalid evidence quote" => "quote_empty_or_too_long",
                                                    "missing answer value" | "invalid answer value slot" => "invalid_value_placeholder",
                                                    _ => "schema_or_pointer",
                                                };
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
                                                    messages.push(json!({"role":"user","content":format!("The previous response failed validation ({reason}). Return a corrected, smaller JSON response. Use at most four observations with at most four scalar field references each. Every pointer must exist under /data/ in its named document. For a long text field include a short exact contiguous quote copied from that field, preserving whitespace; never paraphrase a quote. Prefer short scalar title, URL or date fields when possible. No digits, dates, amounts, numeric codes, URLs or markdown in text OR limitations. Insert exact values using {{{{a}}}}, {{{{b}}}}, {{{{c}}}}, {{{{d}}}} with matching evidence references. Do not reproduce the invalid response.")}));
                                                }
                                            }
                                            Err("invalid_output")
                                        }
                                    }
                                } else {
                                    Err("empty_output")
                                }
                            }
                            Err(_) => Err("invalid_envelope"),
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
                    Err(_) => Err("transport_failure"),
                };
                if let Err(reason) = outcome {
                    // No prompt, answer, reasoning or credentials in diagnostics.
                    tracing::warn!(surface, attempt, reason, "model response unavailable");
                    call.finish(Outcome::Failed(if reason == "transport_failure" { ErrorClass::Transport } else { ErrorClass::Validation }), reason);
                }
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
            self.typesafe.as_ref().map(|client| client.fingerprint()),
            "parser-v2.6",
        ));
        crate::telemetry::set_parser_cache(if self.cache.contains_key(&key) { Cache::Hit } else { Cache::Miss });
        let intent=self.cache.try_get_with(key,async {
            self.calls.fetch_add(1,Ordering::Relaxed);
            if let Some(client) = &self.typesafe {
                if client.mode() == crate::typesafe::Mode::On {
                    if let Some(decision) = client.parse(question, task, catalog).await {
                        return Ok::<_,anyhow::Error>(Arc::new(decision.intent));
                    }
                } else {
                    let (candidate, baseline) = tokio::join!(client.parse(question, task, catalog), self.parse_baseline(question, task, catalog));
                    shadow_intent(candidate.as_ref().map(|d| &d.intent), baseline.as_ref().ok());
                    return baseline.map(Arc::new);
                }
            }
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
fn shadow_intent(candidate: Option<&Intent>, baseline: Option<&Intent>) {
    let core = |intent: &Intent| digest(&(&intent.subjects, &intent.goals, &intent.unresolved_references));
    let agreement = candidate.zip(baseline).map(|(left, right)| core(left) == core(right));
    tracing::info!(surface = "intent", candidate_available = candidate.is_some(), baseline_available = baseline.is_some(), core_agreement = agreement, "typesafe shadow comparison");
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
    async fn a_blocked_purchase_without_sources_keeps_its_reason_without_a_model_call() {
        let parser = Parser::new(reqwest::Client::new(), "http://127.0.0.1:1".into(), "unused".into(), "unused".into());
        let failure = crate::billing::public_error(&crate::billing::refunded_failure(&json!({
            "execution":{"paid":false,"status":403,"body":{"error":"policy_denied","message":"private policy details"}}
        })));
        let failures = BTreeMap::from([("first".into(), failure.clone()), ("second".into(), failure.clone())]);
        let answer = parser.analyze(&json!(["Find charging stations"]), &[], &failures).await.unwrap();
        assert_eq!(answer["status"], "partial");
        assert_eq!(answer["observations"], json!([]));
        assert_eq!(answer["limitations"], json!([failure["message"]]));
        assert!(answer["follow_up_question"].is_null());
        assert!(!answer.to_string().contains("private policy"));
        let empty = parser.analyze(&json!([]), &[], &BTreeMap::new()).await.unwrap();
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
            for (surface, model, sort) in [("chat", "fast-intent", "latency"), ("intent", "fast-intent", "latency"), ("analysis", "analysis-model", "throughput")] {
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
        assert!(ChatReply {
            message: " ".into(),
            discover: false,
            intent: None,
        }
        .validate()
        .is_err());
        assert!(ChatReply {
            message: "x".repeat(3001),
            discover: false,
            intent: None,
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
