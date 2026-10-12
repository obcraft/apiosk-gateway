//! The written reply to a message that is not yet a clear data question:
//! small talk, a request with no subject, or a requirement no source covers.
//! It is the one place [`CHAT_MODEL`] is called. It only writes text for the
//! person to answer: it cannot plan, quote, look anything up or execute, and
//! every caller already has a reply of its own to show when this one fails.
use super::CHAT_MODEL;
use crate::{capability_intake::Intake, catalog::Catalog, state::Task, telemetry};
use anyhow::{anyhow, ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

const ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";
const PROMPT: &str = include_str!("../../contracts/conversation-reply-prompt.txt");
/// Alibaba first, Parasail second, OpenRouter's other providers as fallbacks.
/// Measured on 2026-09-29 with this prompt: Alibaba 0.6–1.5 s (p50 1.1 s, 18
/// calls); `sort: latency` chose DeepInfra and GMICloud at 1.4–5.6 s (p50 2.0
/// s), and the unrouted default 1.0–5.2 s.
const PROVIDERS: &[&str] = &["alibaba", "parasail"];
/// The person is waiting on this reply; past it the caller's own text is shown.
pub const DEADLINE: Duration = Duration::from_secs(6);
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_REPLY_CHARS: usize = 500;

/// Why no lookup was planned: the planner's `decline_reason`, or the App's
/// reading of a search that found nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Conversation,
    AmbiguousRequest,
    UnavailableRequirement,
}
impl Reason {
    /// The planner's stated reason; `none` and anything else get no reply.
    pub fn from_decline(reason: Option<&str>) -> Option<Self> {
        match reason? {
            "conversation" => Some(Self::Conversation),
            "ambiguous_request" => Some(Self::AmbiguousRequest),
            "unavailable_requirement" => Some(Self::UnavailableRequirement),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}
/// An earlier message in the conversation, context only.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Turn {
    pub role: Role,
    pub text: String,
}

pub struct Request<'a> {
    pub question: &'a str,
    pub earlier: Vec<Turn>,
    pub reason: Reason,
    /// A two-letter language code; anything else is left to the question.
    pub language: Option<&'a str>,
    /// The reply the caller would show without this one.
    pub draft: Option<&'a str>,
}

pub struct Reply {
    pub text: String,
    pub model: &'static str,
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    key: String,
    endpoint: String,
    deadline: Duration,
}

impl Client {
    pub fn new(http: reqwest::Client, key: String) -> Self {
        Self { http, key, endpoint: ENDPOINT.into(), deadline: DEADLINE }
    }

    /// Local fixtures only: production has a fixed OpenRouter endpoint.
    #[doc(hidden)]
    pub fn with_local_endpoint(mut self, endpoint: String) -> Result<Self> {
        let url = url::Url::parse(&endpoint).map_err(|_| anyhow!("invalid local model endpoint"))?;
        ensure!(
            url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "invalid local model endpoint"
        );
        self.endpoint = endpoint;
        Ok(self)
    }

    /// One bounded attempt: no retry fits the person's wait. Errors carry no
    /// prompt, reply, key or provider detail.
    pub async fn reply(&self, request: &Request<'_>, catalog: Value) -> Result<Reply> {
        ensure!(!self.key.is_empty(), "conversation_reply_unavailable");
        let payload = json!({
            "model": CHAT_MODEL, "temperature": 0.3, "max_tokens": 200,
            "provider": {"order": PROVIDERS, "allow_fallbacks": true},
            "messages": [{"role":"system","content":PROMPT},{"role":"user","content":input(request, catalog).to_string()}]
        });
        let input_bytes = serde_json::to_vec(&payload)?.len();
        let mut call = telemetry::model_call("conversation-reply", 1, CHAT_MODEL, input_bytes);
        let started = Instant::now();
        let fetched = tokio::time::timeout(self.deadline, self.fetch(&payload)).await
            .unwrap_or(Err((telemetry::ErrorClass::Timeout, "timeout")));
        let (body, output_bytes) = match fetched {
            Ok(fetched) => fetched,
            Err((class, reason)) => {
                tracing::warn!(surface = "conversation-reply", reason, model_ms = started.elapsed().as_millis() as u64, "conversation reply unavailable");
                call.finish(telemetry::Outcome::Failed(class), reason);
                return Err(anyhow!("conversation_reply_unavailable"));
            }
        };
        call.received(&body, output_bytes);
        match reply_text(&body).and_then(validate) {
            Ok(text) => {
                tracing::info!(trace = telemetry::id(), surface = "conversation-reply", model = CHAT_MODEL,
                    provider = body["provider"].as_str().unwrap_or_default(), model_ms = started.elapsed().as_millis() as u64,
                    "conversation reply written");
                call.finish(telemetry::Outcome::Ok, "validated");
                Ok(Reply { text, model: CHAT_MODEL })
            }
            Err(_) => {
                // Fixed classification only; never the model's text.
                tracing::warn!(surface = "conversation-reply", reason = "invalid_output", "conversation reply unavailable");
                call.finish(telemetry::Outcome::Failed(telemetry::ErrorClass::Validation), "invalid_output");
                Err(anyhow!("conversation_reply_unavailable"))
            }
        }
    }

    async fn fetch(&self, payload: &Value) -> std::result::Result<(Value, usize), (telemetry::ErrorClass, &'static str)> {
        let response = self.http.post(&self.endpoint).bearer_auth(&self.key)
            .header("X-Title", "Apiosk conversation reply")
            .timeout(self.deadline).json(payload).send().await
            .map_err(|error| if error.is_timeout() { (telemetry::ErrorClass::Timeout, "timeout") } else { (telemetry::ErrorClass::Transport, "transport_failure") })?;
        if !response.status().is_success() {
            return Err((telemetry::ErrorClass::upstream(response.status().as_u16()), "http_status"));
        }
        super::bounded_json_sized(response, MAX_RESPONSE_BYTES).await
            .map_err(|_| (telemetry::ErrorClass::Validation, "invalid_envelope"))
    }
}

/// What the model is told: the message, the conversation so far, why no
/// lookup was planned, and the live catalog it may name.
fn input(request: &Request<'_>, catalog: Value) -> Value {
    let skip = request.earlier.len().saturating_sub(6);
    let earlier: Vec<Value> = request.earlier.iter().skip(skip)
        .map(|turn| json!({"role":turn.role,"text":clip(&turn.text, 600)})).collect();
    json!({
        "question": clip(request.question, 2000),
        "earlier_turns": earlier,
        "reason": request.reason,
        "language": request.language.filter(|code| valid_language(code)),
        "draft": request.draft.map(|draft| clip(draft, 600)),
        "catalog": catalog,
    })
}

pub fn valid_language(code: &str) -> bool {
    code.len() == 2 && code.bytes().all(|c| c.is_ascii_lowercase())
}

fn clip(text: &str, chars: usize) -> String {
    let text = text.trim();
    match text.char_indices().nth(chars) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

/// The earlier turns of a saved conversation: each question and its reply.
pub fn earlier_turns(task: &Task) -> Vec<Turn> {
    task.conversation.iter().flat_map(|turn| {
        std::iter::once(Turn { role: Role::User, text: turn.question.clone() })
            .chain(turn.reply.clone().map(|text| Turn { role: Role::Assistant, text }))
    }).collect()
}

/// What Apiosk can retrieve right now, as names the reply may use: the
/// curated source groups and the source categories of the live catalog, and
/// the sources a request's search matched. No prices, coverage or contracts.
pub fn catalog_summary(catalog: &Catalog, intake: Option<&Intake>) -> Value {
    let available: BTreeSet<&str> = catalog.services.iter().map(|service| service.api_slug.as_str()).collect();
    let name = |slug: &str| catalog.known_sources.iter().find(|source| source.slug == slug).map(|source| source.name.as_str())
        .or_else(|| catalog.services.iter().find(|service| service.api_slug == slug).map(|service| service.name.as_str()));
    let groups: Vec<Value> = crate::eu_capabilities::definitions().iter().filter_map(|group| {
        let sources: Vec<&str> = group.primary_sources.iter()
            .filter(|slug| available.contains(slug.as_str())).filter_map(|slug| name(slug)).take(4).collect();
        (!sources.is_empty()).then(|| json!({"name":group.name,"sources":sources}))
    }).collect();
    let mut categories: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for source in catalog.known_sources.iter().filter(|source| available.contains(source.slug.as_str())) {
        let category = source.metadata.category.trim();
        if !category.is_empty() {
            categories.entry(category).or_default().push(source.name.as_str());
        }
    }
    let mut categories: Vec<_> = categories.into_iter().collect();
    categories.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
    let categories: Vec<Value> = categories.into_iter().take(10)
        .map(|(category, sources)| json!({"category":category,"sources":sources.into_iter().take(3).collect::<Vec<_>>()})).collect();
    let mut summary = json!({"source_groups":groups,"categories":categories});
    let mut related: Vec<Value> = Vec::new();
    let mut seen = BTreeSet::new();
    for candidate in intake.iter().flat_map(|intake| &intake.matches).flat_map(|found| &found.apiosk)
        .filter(|candidate| candidate.availability == "supported")
    {
        let source = candidate.source_slug.as_deref().and_then(name).unwrap_or(&candidate.name);
        if related.len() < 3 && seen.insert(source.to_owned()) {
            related.push(json!({"source":source,"offers":clip(&candidate.description, 200)}));
        }
    }
    if !related.is_empty() {
        summary["related_sources"] = json!(related);
    }
    summary
}

fn reply_text(body: &Value) -> Result<&str> {
    let choice = body.pointer("/choices/0").ok_or_else(|| anyhow!("missing choice"))?;
    ensure!(body["error"].is_null(), "provider error");
    ensure!(!matches!(choice["finish_reason"].as_str(), Some("length" | "content_filter")), "incomplete reply");
    ensure!(choice["message"]["refusal"].as_str().is_none_or(str::is_empty), "refused");
    choice["message"]["content"].as_str().ok_or_else(|| anyhow!("missing reply"))
}

/// Plain text, one to three short sentences, ending in its one question. A
/// reply that is not is dropped for the caller's own text, never repaired.
fn validate(raw: &str) -> Result<String> {
    let mut text = raw.trim();
    for (open, close) in [('"', '"'), ('“', '”')] {
        if text.len() > 2 && text.starts_with(open) && text.ends_with(close) {
            text = text[open.len_utf8()..text.len() - close.len_utf8()].trim();
        }
    }
    ensure!(!text.is_empty() && text.chars().count() <= MAX_REPLY_CHARS, "reply length");
    ensure!(!text.starts_with(['{', '[']), "structured output");
    ensure!(!["://", "```", "**", "__", "USDC"].iter().any(|mark| text.contains(mark)) && !text.contains(['€', '$', '£']),
        "links, markup or prices");
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    ensure!(lines.len() <= 3, "too many lines");
    ensure!(!lines.iter().any(|line| line.starts_with(['-', '*', '•', '#']) || numbered(line)), "list");
    let questions = text.matches(['?', '？']).count();
    ensure!((1..=2).contains(&questions) && text.ends_with(['?', '？']), "one question at the end");
    Ok(text.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn numbered(line: &str) -> bool {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && matches!(line.as_bytes().get(digits), Some(b'.' | b')')) && line.as_bytes().get(digits + 1) == Some(&b' ')
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{http::StatusCode, routing::post, Json, Router};
    use std::sync::{Arc, Mutex};

    async fn server(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        (endpoint, tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }))
    }
    fn completion(text: &str) -> Value {
        json!({"provider":"Alibaba","choices":[{"finish_reason":"stop","message":{"content":text}}],"usage":{"prompt_tokens":10,"completion_tokens":5}})
    }
    fn request<'a>() -> Request<'a> {
        Request { question: "How are you?", earlier: vec![], reason: Reason::Conversation, language: Some("en"), draft: Some("What data do you need?") }
    }

    #[tokio::test]
    async fn sends_the_chat_model_with_explicit_fast_providers_and_returns_the_validated_text() {
        let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
        let seen = captured.clone();
        let app = Router::new().route("/", post(move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                assert_eq!(headers["authorization"], "Bearer fixture-key");
                seen.lock().unwrap().push(body);
                Json(completion("  I'm well, thank you!\nWhich company, person or place would you like data about?  "))
            }
        }));
        let (endpoint, task) = server(app).await;
        let client = Client::new(reqwest::Client::new(), "fixture-key".into()).with_local_endpoint(endpoint).unwrap();
        let mut asked = request();
        asked.earlier = (0..8).map(|index| Turn { role: Role::User, text: format!("turn {index}") }).collect();
        let reply = client.reply(&asked, json!({"source_groups":[]})).await.unwrap();
        assert_eq!(reply.text, "I'm well, thank you! Which company, person or place would you like data about?");
        assert_eq!(reply.model, CHAT_MODEL);
        let body = captured.lock().unwrap()[0].clone();
        assert_eq!(body["model"], "qwen/qwen3-235b-a22b-2507");
        assert_eq!(body["provider"], json!({"order":["alibaba","parasail"],"allow_fallbacks":true}));
        assert_eq!(body["messages"][0]["content"], PROMPT);
        let input: Value = serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(input["question"], "How are you?");
        assert_eq!(input["reason"], "conversation");
        assert_eq!(input["language"], "en");
        assert_eq!(input["draft"], "What data do you need?");
        assert_eq!(input["earlier_turns"].as_array().unwrap().len(), 6, "only the latest turns are context");
        assert_eq!(input["earlier_turns"][0]["text"], "turn 2");
        task.abort();
    }

    #[tokio::test]
    async fn a_slow_failing_or_invalid_reply_is_an_error_within_the_deadline() {
        let app = Router::new()
            .route("/slow", post(|| async { tokio::time::sleep(Duration::from_secs(5)).await; Json(completion("Which company?")) }))
            .route("/error", post(|| async { (StatusCode::SERVICE_UNAVAILABLE, "private provider detail") }))
            .route("/list", post(|| async { Json(completion("I can find:\n- company profiles\n- tenders\nWhich one?")) }))
            .route("/empty", post(|| async { Json(json!({"choices":[]})) }));
        let (endpoint, task) = server(app).await;
        for (path, deadline) in [("slow", 50), ("error", 2000), ("list", 2000), ("empty", 2000)] {
            let mut client = Client::new(reqwest::Client::new(), "fixture-key".into()).with_local_endpoint(format!("{endpoint}{path}")).unwrap();
            client.deadline = Duration::from_millis(deadline);
            let started = Instant::now();
            let error = client.reply(&request(), json!({})).await.err().unwrap();
            assert_eq!(error.to_string(), "conversation_reply_unavailable", "{path}");
            assert!(started.elapsed() < Duration::from_millis(deadline + 500), "{path}");
        }
        let keyless = Client::new(reqwest::Client::new(), String::new());
        assert!(keyless.reply(&request(), json!({})).await.is_err());
        task.abort();
    }

    #[tokio::test]
    async fn a_genuine_decline_gets_the_written_reply_and_any_failure_keeps_the_planner_text() {
        use super::super::{ChatReply, Parser, MODEL};
        let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
        let seen = calls.clone();
        let app = Router::new()
            .route("/ok", post(move |Json(body): Json<Value>| {
                let seen = seen.clone();
                async move { seen.lock().unwrap().push(body); Json(completion("Hoi! Met mij gaat het goed. Over welk bedrijf wil je gegevens?")) }
            }))
            .route("/error", post(|| async { StatusCode::INTERNAL_SERVER_ERROR }))
            .route("/slow", post(|| async { tokio::time::sleep(Duration::from_secs(5)).await; Json(completion("Welk bedrijf?")) }));
        let (endpoint, server) = server(app).await;
        let catalog = Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/workflows/catalog.json")).unwrap());
        let parser = |path: &str, deadline: u64| {
            let mut client = Client::new(reqwest::Client::new(), "fixture-key".into()).with_local_endpoint(format!("{endpoint}{path}")).unwrap();
            client.deadline = Duration::from_millis(deadline);
            // The planner keeps its own endpoint and model; it is never called here.
            Parser::new(reqwest::Client::new(), "http://127.0.0.1:1".into(), "fixture".into(), MODEL.into()).with_conversation_replies(client)
        };
        let declined = |reason: Option<&str>, discover: bool| ChatReply { output_bindings: vec![],
            message: "Waar kan ik je mee helpen?".into(), discover, intent: None, decline_reason: reason.map(str::to_owned),
        };
        let mut task = Task::default();
        task.record_turn("Registered address of Coolblue", Some("1 source can answer this.".into()));
        let written = parser("ok", 2000).decline_reply("Hoe gaat het?", &task, &catalog, &declined(Some("conversation"), false)).await;
        assert_eq!(written.as_deref(), Some("Hoi! Met mij gaat het goed. Over welk bedrijf wil je gegevens?"));
        let sent = calls.lock().unwrap()[0].clone();
        assert_eq!(sent["model"], CHAT_MODEL);
        let input: Value = serde_json::from_str(sent["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(input["reason"], "conversation");
        assert_eq!(input["draft"], "Waar kan ik je mee helpen?", "the planner's text is the draft");
        assert_eq!(input["earlier_turns"], json!([{"role":"user","text":"Registered address of Coolblue"},{"role":"assistant","text":"1 source can answer this."}]));
        // No genuine reason, or a plan: no written reply, no call.
        for reply in [declined(None, false), declined(Some("none"), false), declined(Some("conversation"), true)] {
            assert!(parser("ok", 2000).decline_reply("Hoe gaat het?", &task, &catalog, &reply).await.is_none());
        }
        assert_eq!(calls.lock().unwrap().len(), 1);
        // A failure or the deadline keeps the planner's own text, promptly.
        for (path, deadline) in [("error", 2000), ("slow", 100)] {
            let started = Instant::now();
            assert!(parser(path, deadline).decline_reply("Hoe gaat het?", &task, &catalog, &declined(Some("ambiguous_request"), false)).await.is_none());
            assert!(started.elapsed() < Duration::from_millis(deadline + 500));
        }
        let unconfigured = Parser::new(reqwest::Client::new(), "http://127.0.0.1:1".into(), "fixture".into(), MODEL.into());
        assert!(unconfigured.decline_reply("Hoe gaat het?", &task, &catalog, &declined(Some("conversation"), false)).await.is_none());
        server.abort();
    }

    #[test]
    fn a_reply_is_plain_text_ending_in_its_question() {
        assert_eq!(validate("\"Hallo! Over welk bedrijf wil je informatie?\"").unwrap(), "Hallo! Over welk bedrijf wil je informatie?");
        assert_eq!(validate("Hola, ¿en qué puedo ayudarte? ¿Sobre qué empresa necesitas información?").unwrap(),
            "Hola, ¿en qué puedo ayudarte? ¿Sobre qué empresa necesitas información?");
        for invalid in [
            "", "Hello there.", "Which company? Which year? Which register?", "{\"message\":\"Which company?\"}",
            "See https://apiosk.com. Which company?", "**Hi!** Which company?", "1. Companies\n2. Tenders\nWhich one?",
            "It costs €2. Which company?", "Which company?\nWhich year?\nWhich register?\nWhich source?",
        ] {
            assert!(validate(invalid).is_err(), "{invalid}");
        }
        assert!(validate(&format!("{}?", "a".repeat(MAX_REPLY_CHARS))).is_err());
        assert_eq!(Reason::from_decline(Some("ambiguous_request")), Some(Reason::AmbiguousRequest));
        assert_eq!(Reason::from_decline(Some("none")), None);
        assert_eq!(Reason::from_decline(None), None);
    }

    #[test]
    fn the_catalog_summary_names_only_live_sources_and_the_request_matches() {
        let mut catalog = Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/workflows/catalog.json")).unwrap());
        let live = catalog.services[0].api_slug.clone();
        catalog.known_sources = vec![
            crate::catalog::KnownSource { slug: live.clone(), name: "Live Register".into(), block_reason: None,
                metadata: crate::readiness::Metadata { category: "Company data".into(), ..Default::default() } },
            crate::catalog::KnownSource { slug: "retired-source".into(), name: "Retired Source".into(), block_reason: Some("blocked".into()),
                metadata: crate::readiness::Metadata { category: "Company data".into(), ..Default::default() } },
        ];
        let summary = catalog_summary(&catalog, None);
        assert_eq!(summary["categories"], json!([{"category":"Company data","sources":["Live Register"]}]));
        assert!(!summary.to_string().contains("Retired Source"));
        assert!(!summary.to_string().contains("price"));
        assert!(summary.get("related_sources").is_none());
    }

    #[test]
    fn stronger_models_are_confined_to_explicit_conversation_and_synthesis_stages() {
        // Intent/planning and single-provider analysis keep the pinned model.
        // Multi-provider synthesis has its own explicit constant in parser.rs.
        assert_ne!(CHAT_MODEL, super::super::MODEL);
        std::env::set_var("ASK_V2_MODEL_CONVERSATION_POLICY_PROBE", CHAT_MODEL);
        assert_eq!(super::super::pinned_model("ASK_V2_MODEL_CONVERSATION_POLICY_PROBE"), super::super::MODEL);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = vec![root.clone()];
        let mut users = BTreeSet::new();
        while let Some(path) = files.pop() {
            if path.is_dir() {
                files.extend(std::fs::read_dir(&path).unwrap().map(|entry| entry.unwrap().path()));
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                if text.contains("CHAT_MODEL") || text.contains("qwen3-235b") {
                    users.insert(path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/"));
                }
            }
        }
        assert_eq!(users, BTreeSet::from(["parser.rs".to_owned(), "parser/conversation_reply.rs".to_owned()]));
    }
}
