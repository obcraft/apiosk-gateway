//! Ask model transport and canonical interpretation, shared by the App and
//! public API. This client never plans or executes a data-source request.

pub mod search;
pub mod external;
pub mod purchase;
pub(crate) mod input_values;
#[cfg(test)]
pub(crate) mod test_catalog;

use anyhow::{anyhow, ensure, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub const DEFAULT_MODEL: &str = crate::parser::MODEL;
const ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";
const PROMPT: &str = include_str!("../../contracts/ask-v2-webgpu-prompt.txt");
// The parse contract: connected chatbots fill in exactly this shape
// (contracts/tool-schemas.json), and the App validates the model's parse
// against it. The model itself is asked for a JSON object; see `parse`.
#[cfg_attr(not(test), allow(dead_code))]
const SCHEMA: &str = include_str!("../../contracts/ask-v2-webgpu-schema.json");
/// Qwen's own provider first. Measured on 2026-09-28 for this parse: Alibaba
/// 1.4–2.4 s; the providers with strict json_schema 3–14 s (Nebius degraded
/// and rate-limited, StreamLake 3–7 s, SiliconFlow 7 s). The others stay as
/// fallbacks.
const PARSE_PROVIDERS: &[&str] = &["alibaba"];
// Qwen3 30B streams this parse in about 1–4 s on the preferred provider; the
// deadline leaves room for a slower fallback provider.
const DEADLINE: Duration = Duration::from_secs(20);
const MAX_STREAM_BYTES: usize = 256 * 1024;
const WINDOW: Duration = Duration::from_secs(60);
const MAX_OWNERS: usize = 4096;

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    key: String,
    model: String,
    endpoint: String,
    deadline: Duration,
    limits: Arc<Mutex<HashMap<String, OwnerLimits>>>,
}

#[derive(Debug, Serialize)]
pub struct Output {
    pub text: String,
    pub model: String,
    pub timing: Timing,
}

#[derive(Debug, Serialize)]
pub struct Timing {
    pub parse_ms: u64,
}

struct Window {
    start: Instant,
    count: u32,
}
struct OwnerLimits {
    parse: Window,
    search: Window,
    reply: Window,
}
#[derive(Clone, Copy)]
enum Operation {
    Parse,
    Search,
    Reply,
}

impl Client {
    /// Canonical interpretation for the App and public Ask API. Both callers
    /// consume this saved shape rather than normalizing the model differently.
    pub async fn interpret(&self, question: &str, known_slugs: &[String]) -> Result<Value> {
        let started = Instant::now();
        let mut correction = None;
        for attempt in 1..=2 {
            let output = self.parse(question, known_slugs, correction.as_deref()).await?;
            match crate::capability_intake::normalize(&output.text, question, &output.model) {
                Ok(parsed) => {
                    let mut parsed = serde_json::to_value(parsed)?;
                    for capability in parsed["capabilities"].as_array_mut().into_iter().flatten() {
                        capability["status"] = Value::Null;
                    }
                    return Ok(json!({"text":output.text,"model":output.model,"parsed_request":parsed,
                        "attempts":attempt,"timing":{"parse_ms":started.elapsed().as_millis() as u64}}));
                }
                Err(error) if attempt == 1 => correction = Some(error.to_string()),
                Err(error) => return Err(error),
            }
        }
        unreachable!()
    }

    pub fn new(http: reqwest::Client, key: String, model: String) -> Self {
        Self {
            http,
            key,
            model: if model.trim().is_empty() {
                DEFAULT_MODEL.into()
            } else {
                model
            },
            endpoint: ENDPOINT.into(),
            deadline: DEADLINE,
            limits: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Local fixtures only: production has a fixed OpenRouter endpoint.
    #[doc(hidden)]
    pub fn with_local_endpoint(mut self, endpoint: String) -> Result<Self> {
        let url =
            url::Url::parse(&endpoint).map_err(|_| anyhow!("invalid local model endpoint"))?;
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

    pub fn admit(&self, owner: &str) -> bool {
        self.admit_at(owner, Operation::Parse, Instant::now())
    }
    pub fn admit_search(&self, owner: &str) -> bool {
        self.admit_at(owner, Operation::Search, Instant::now())
    }
    /// A written conversation reply: at most one per parse, so the same budget.
    pub fn admit_reply(&self, owner: &str) -> bool {
        self.admit_at(owner, Operation::Reply, Instant::now())
    }

    fn admit_at(&self, owner: &str, operation: Operation, now: Instant) -> bool {
        if owner.is_empty() || owner.len() > 256 {
            return false;
        }
        let Ok(mut limits) = self.limits.lock() else {
            return false;
        };
        limits.retain(|_, entry| {
            now.duration_since(entry.parse.start) < WINDOW
                || now.duration_since(entry.search.start) < WINDOW
                || now.duration_since(entry.reply.start) < WINDOW
        });
        if !limits.contains_key(owner) && limits.len() >= MAX_OWNERS {
            return false;
        }
        let entry = limits
            .entry(owner.to_owned())
            .or_insert_with(|| OwnerLimits {
                parse: Window {
                    start: now,
                    count: 0,
                },
                search: Window {
                    start: now,
                    count: 0,
                },
                reply: Window {
                    start: now,
                    count: 0,
                },
            });
        let (window, maximum) = match operation {
            Operation::Parse => (&mut entry.parse, 30),
            Operation::Search => (&mut entry.search, 60),
            Operation::Reply => (&mut entry.reply, 30),
        };
        if now.duration_since(window.start) >= WINDOW {
            window.start = now;
            window.count = 0;
        }
        if window.count >= maximum {
            return false;
        }
        window.count += 1;
        true
    }

    pub async fn parse(
        &self,
        question: &str,
        known_slugs: &[String],
        correction: Option<&str>,
    ) -> Result<Output> {
        let started = Instant::now();
        let text = tokio::time::timeout(
            self.deadline,
            self.generate(question, known_slugs, correction),
        )
        .await
        .map_err(|_| anyhow!("ask_v2_model_timeout"))??;
        Ok(Output {
            text,
            model: self.model.clone(),
            timing: Timing {
                parse_ms: started.elapsed().as_millis() as u64,
            },
        })
    }

    async fn generate(
        &self,
        question: &str,
        known_slugs: &[String],
        correction: Option<&str>,
    ) -> Result<String> {
        ensure!(!self.key.is_empty(), "ask_v2_model_unavailable");
        ensure!(
            !question.trim().is_empty() && question.encode_utf16().count() <= 16_000,
            "ask_v2_invalid_question"
        );
        ensure!(
            known_slugs.len() <= 100
                && known_slugs.iter().all(|slug| slug.len() <= 256
                    && !slug.is_empty()
                    && slug.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'.')),
            "ask_v2_invalid_known_slugs"
        );
        ensure!(
            correction.is_none_or(|text| text.len() <= 16_384),
            "ask_v2_invalid_correction"
        );
        let mut prompt = if known_slugs.is_empty() {
            PROMPT.to_owned()
        } else {
            format!(
                "{}Known slugs: {}",
                PROMPT
                    .strip_suffix("Known slugs: none")
                    .ok_or_else(|| anyhow!("ask_v2_model_unavailable"))?,
                known_slugs.join(", ")
            )
        };
        if let Some(correction) = correction {
            prompt.push_str(&format!("\nPrevious output failed validation: {correction}\nParse the user's request again. Return only one complete JSON object with every required field."));
        }
        // A JSON object, not a strict json_schema: Alibaba, the fastest Qwen
        // provider, has no strict schema mode. The prompt spells out the shape,
        // and `interpret` validates and mechanically repairs the parse, with
        // one correction attempt for anything else.
        let body = json!({
            "model": self.model,
            "messages": [{"role":"system","content":prompt},{"role":"user","content":question}],
            "stream": true, "temperature": 0, "max_tokens": 4096,
            "provider": {"order": PARSE_PROVIDERS, "require_parameters": true},
            "response_format": {"type": "json_object"}
        })
        .to_string();
        let mut response = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.key)
            .header("X-Title", "Apiosk Ask v2")
            .timeout(self.deadline)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.clone())
            .send()
            .await
            .map_err(|_| anyhow!("ask_v2_model_unavailable"))?;
        // Match WebGPU's two bounded retries for a rejected free/provider pool.
        // The complete operation, including waits and the stream, stays within DEADLINE.
        for delay in [1500, 4000] {
            if response.status() != reqwest::StatusCode::TOO_MANY_REQUESTS {
                break;
            }
            drop(response);
            tokio::time::sleep(Duration::from_millis(delay)).await;
            response = self
                .http
                .post(&self.endpoint)
                .bearer_auth(&self.key)
                .header("X-Title", "Apiosk Ask v2")
                .timeout(self.deadline)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.clone())
                .send()
                .await
                .map_err(|_| anyhow!("ask_v2_model_unavailable"))?;
        }
        if !response.status().is_success() {
            tracing::warn!(status = response.status().as_u16(), "Ask v2 model request refused");
            return Err(anyhow!("ask_v2_model_unavailable"));
        }
        ensure!(
            response
                .content_length()
                .is_none_or(|size| size <= MAX_STREAM_BYTES as u64),
            "ask_v2_model_response_too_large"
        );
        let mut stream = Sse::default();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow!("ask_v2_model_stream_unavailable"))?
        {
            stream.push(&chunk)?;
            if stream.done {
                break;
            }
        }
        stream.finish()
    }
}

#[derive(Default)]
struct Sse {
    pending: Vec<u8>,
    data: Vec<String>,
    text: String,
    bytes: usize,
    done: bool,
}

impl Sse {
    fn push(&mut self, chunk: &[u8]) -> Result<()> {
        if self.done {
            return Ok(());
        }
        self.bytes = self
            .bytes
            .checked_add(chunk.len())
            .ok_or_else(|| anyhow!("ask_v2_model_response_too_large"))?;
        ensure!(
            self.bytes <= MAX_STREAM_BYTES,
            "ask_v2_model_response_too_large"
        );
        self.pending.extend_from_slice(chunk);
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line = self.pending.drain(..=end).collect::<Vec<_>>();
            self.line(&line[..line.len() - 1])?;
            if self.done {
                break;
            }
        }
        Ok(())
    }

    fn line(&mut self, line: &[u8]) -> Result<()> {
        let line = std::str::from_utf8(line)
            .map_err(|_| anyhow!("ask_v2_model_stream_unavailable"))?
            .trim_end_matches('\r');
        if line.is_empty() {
            return self.event();
        }
        if let Some(data) = line.strip_prefix("data:") {
            self.data
                .push(data.strip_prefix(' ').unwrap_or(data).to_owned());
        }
        Ok(())
    }

    fn event(&mut self) -> Result<()> {
        if self.data.is_empty() {
            return Ok(());
        }
        let data = std::mem::take(&mut self.data).join("\n");
        if data.trim() == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        let chunk: Value =
            serde_json::from_str(&data).map_err(|_| anyhow!("ask_v2_model_stream_unavailable"))?;
        ensure!(
            chunk.get("error").is_none_or(Value::is_null),
            "ask_v2_model_stream_unavailable"
        );
        if let Some(content) = chunk
            .pointer("/choices/0/delta/content")
            .and_then(Value::as_str)
        {
            self.text.push_str(content);
        }
        Ok(())
    }

    fn finish(mut self) -> Result<String> {
        if !self.done {
            let line = std::mem::take(&mut self.pending);
            if !line.is_empty() {
                self.line(&line)?;
            }
            self.event()?;
        }
        ensure!(!self.text.trim().is_empty(), "ask_v2_model_empty_response");
        Ok(self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::{
        body::{Body, Bytes},
        http::{HeaderMap, StatusCode},
        response::Response,
        routing::post,
        Router,
    };
    use futures_util::StreamExt;
    use std::convert::Infallible;

    fn client() -> Client {
        Client::new(
            reqwest::Client::new(),
            "fixture-key".into(),
            DEFAULT_MODEL.into(),
        )
    }

    async fn server(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (endpoint, task)
    }

    fn delta(text: &str) -> String {
        format!(
            "data: {}\r\n\r\n",
            json!({"choices":[{"delta":{"content":text}}]})
        )
    }

    #[test]
    fn connected_chatbots_fill_in_the_ask_parse_schema_exactly() {
        let tools: serde_json::Value = serde_json::from_str(include_str!("../../contracts/tool-schemas.json")).unwrap();
        let parse: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
        assert_eq!(tools["search"]["properties"]["parsed_request"], parse);
        assert_eq!(tools["search"]["required"], serde_json::json!(["parsed_request"]));
    }

    #[test]
    fn default_model_is_apiosks_one_model_and_fixture_endpoint_is_loopback_only() {
        assert_eq!(DEFAULT_MODEL, "qwen/qwen3-30b-a3b-instruct-2507");
        assert_eq!(
            Client::new(reqwest::Client::new(), "fixture".into(), String::new()).model,
            DEFAULT_MODEL
        );
        for endpoint in [
            "https://example.com/",
            "http://localhost.example.com/",
            "http://user@localhost/",
            "http://localhost/?x=1",
        ] {
            assert!(client().with_local_endpoint(endpoint.into()).is_err());
        }
        for endpoint in [
            "http://127.0.0.1:1234/",
            "http://localhost:1234/",
            "http://[::1]:1234/",
        ] {
            assert!(client().with_local_endpoint(endpoint.into()).is_ok());
        }
    }

    #[test]
    fn stream_handles_crlf_split_utf8_comments_multiline_and_unterminated_done() {
        let first = json!({"choices":[{"delta":{"content":"{\"label\":\"Utrécht"}}]}).to_string();
        let payload = format!(
            ": keep-alive\r\nevent: message\r\ndata: {first}\r\n\r\n{}data: [DONE]",
            delta("\"}")
        );
        let mut parsed = Sse::default();
        // One-byte chunks guarantee a UTF-8 code point is split across chunks.
        for byte in payload.as_bytes() {
            parsed.push(&[*byte]).unwrap();
        }
        assert_eq!(parsed.finish().unwrap(), "{\"label\":\"Utrécht\"}");

        let mut multiline = Sse::default();
        multiline
            .push(b"data: {\"choices\":\ndata: [{\"delta\":{\"content\":\"ok\"}}]}\n\n")
            .unwrap();
        assert_eq!(multiline.finish().unwrap(), "ok");
    }

    #[test]
    fn stream_stops_at_done_and_flushes_final_data_without_newline() {
        let mut done = Sse::default();
        done.push(
            format!(
                "{}data: [DONE]\n\ndata: invalid provider detail\n\n",
                delta("{}")
            )
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(done.finish().unwrap(), "{}");
        let mut eof = Sse::default();
        eof.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"{}\"}}]}")
            .unwrap();
        assert_eq!(eof.finish().unwrap(), "{}");
    }

    #[test]
    fn stream_rejects_empty_errors_invalid_json_and_oversize_without_provider_details() {
        assert_eq!(
            Sse::default().finish().unwrap_err().to_string(),
            "ask_v2_model_empty_response"
        );
        for payload in [
            "data: {\"error\":{\"message\":\"private provider account detail\"}}\n\n",
            "data: invalid provider account detail\n\n",
        ] {
            assert_eq!(
                Sse::default()
                    .push(payload.as_bytes())
                    .unwrap_err()
                    .to_string(),
                "ask_v2_model_stream_unavailable"
            );
        }
        assert_eq!(
            Sse::default()
                .push(&vec![b'x'; MAX_STREAM_BYTES + 1])
                .unwrap_err()
                .to_string(),
            "ask_v2_model_response_too_large"
        );
        let mut invalid_utf8 = Sse::default();
        assert_eq!(
            invalid_utf8.push(b"data: \xff\n").unwrap_err().to_string(),
            "ask_v2_model_stream_unavailable"
        );
    }

    #[test]
    fn rate_limits_are_per_owner_per_operation_and_shared_by_clones() {
        let original = client();
        let cloned = original.clone();
        for _ in 0..30 {
            assert!(original.admit("owner-a"));
        }
        assert!(!cloned.admit("owner-a"));
        assert!(cloned.admit("owner-b"));
        for _ in 0..60 {
            assert!(cloned.admit_search("owner-a"));
        }
        assert!(!original.admit_search("owner-a"));
        for _ in 0..30 {
            assert!(original.admit_reply("owner-a"), "replies have their own window");
        }
        assert!(!cloned.admit_reply("owner-a"));
        assert!(!original.admit(""));
        assert!(!original.admit(&"x".repeat(257)));
    }

    #[test]
    fn rate_cache_expires_and_never_evicts_live_owners_to_allow_bypassing_limits() {
        let client = client();
        let start = Instant::now();
        for i in 0..MAX_OWNERS {
            assert!(client.admit_at(&format!("owner-{i}"), Operation::Parse, start));
        }
        assert_eq!(client.limits.lock().unwrap().len(), MAX_OWNERS);
        assert!(!client.admit_at("overflow", Operation::Parse, start));
        for _ in 1..30 {
            assert!(client.admit_at("owner-0", Operation::Parse, start));
        }
        assert!(!client.admit_at("owner-0", Operation::Parse, start));
        assert!(client.admit_at("overflow", Operation::Parse, start + WINDOW));
        assert_eq!(client.limits.lock().unwrap().len(), 1);
    }

    fn scope_capability(slug: &str, inputs: &[&str], outputs: &[&str], subject: Option<&str>, source: Option<&str>) -> Value {
        json!({"slug":slug,"name":"Requested data","description":"Retrieve the requested factual data for the supplied scope.",
            "domain":slug.split('.').next().unwrap(),"inputs":inputs,"outputs":outputs,"subject_id":subject,"source_hint":source,"confidence":0.95})
    }

    // These transport fixtures exercise the canonical parse path and its
    // literal-value normalization. Real-model preservation is checked again
    // on the live profession scenarios; a fixture cannot establish that.
    #[tokio::test]
    async fn interpret_preserves_literal_geography_occupation_and_distinct_sector_cohorts() {
        let salary_question = "Geef salarisbenchmarks voor softwareontwikkelaars in Utrecht, arbeidsmarktkrapte, vacaturevolume en officiële werkloosheidscijfers voor Nederland. Maak onderscheid tussen brondata en AI-inschattingen.";
        let market_question = "Vergelijk marktomvang en groei van de Nederlandse en Duitse softwaresector op basis van CBS en Eurostat. Geef concurrenten, peers en bronvermeld deskresearch.";
        let salary = json!({"language":"nl","subjects":[
            {"id":"occupation","label":"softwareontwikkelaars","type":"occupation","identifiers":{"job_category":"softwareontwikkelaars","location":"Utrecht"}},
            {"id":"national","label":"Nederland","type":"country","identifiers":{"country":"Nederland"}}],
            "capabilities":[
                scope_capability("research.salary.benchmarks", &["job_category","location"], &["average_salary","salary_range"], Some("occupation"), None),
                scope_capability("research.labor.market.scarcity", &["job_category","location"], &["scarcity_index"], Some("occupation"), None),
                scope_capability("research.job.vacancy.volume", &["job_category","location"], &["vacancy_count"], Some("occupation"), None),
                scope_capability("research.unemployment.rate", &["country"], &["unemployment_rate"], Some("national"), None)],
            "deliverable":{"format":"chat","operations":[]}});
        let market = json!({"language":"nl","subjects":[
            {"id":"first","label":"Nederlandse","type":"sector","identifiers":{"country":"Nederlandse","sector":"softwaresector"}},
            {"id":"second","label":"Duitse","type":"sector","identifiers":{"country":"Duitse","sector":"softwaresector"}}],
            "capabilities":[
                scope_capability("research.market.size", &["country","sector"], &["market_size"], Some("first"), Some("cbs")),
                scope_capability("research.market.size", &["country","sector"], &["market_size"], Some("second"), Some("eurostat")),
                scope_capability("research.market.growth", &["country","sector"], &["growth_rate"], Some("first"), Some("cbs")),
                scope_capability("research.market.growth", &["country","sector"], &["growth_rate"], Some("second"), Some("eurostat"))],
            "deliverable":{"format":"chat","operations":["compare"]}});
        let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
        let handler_capture = captured.clone();
        let app = Router::new().route("/", post(move |body: axum::Json<Value>| {
            let captured = handler_capture.clone();
            let salary = salary.clone();
            let market = market.clone();
            async move {
                let question = body["messages"][1]["content"].as_str().unwrap();
                let result = if question == salary_question { salary } else { assert_eq!(question, market_question); market };
                captured.lock().unwrap().push(body.0);
                Response::builder().header("content-type", "text/event-stream")
                    .body(Body::from(format!("{}data: [DONE]\n\n", delta(&result.to_string())))).unwrap()
            }
        }));
        let (endpoint, server) = server(app).await;
        let client = client().with_local_endpoint(endpoint).unwrap();
        let salary = client.interpret(salary_question, &[]).await.unwrap();
        let parsed = &salary["parsed_request"];
        assert_eq!(parsed["subjects"][0]["identifiers"], json!({"job_category":"softwareontwikkelaars","location":"Utrecht"}));
        assert_eq!(parsed["subjects"][1]["identifiers"], json!({"country":"Nederland"}));
        for (index, subject) in ["s1", "s1", "s1", "s2"].iter().enumerate() {
            assert_eq!(parsed["capabilities"][index]["subject_id"], *subject);
            assert_eq!(parsed["capabilities"][index]["missing"], json!([]));
        }
        let market = client.interpret(market_question, &[]).await.unwrap();
        let parsed = &market["parsed_request"];
        assert_eq!(parsed["subjects"][0]["identifiers"], json!({"country":"Nederlandse","sector":"softwaresector"}));
        assert_eq!(parsed["subjects"][1]["identifiers"], json!({"country":"Duitse","sector":"softwaresector"}));
        assert_eq!(parsed["capabilities"].as_array().unwrap().len(), 4);
        for (index, subject) in ["s1", "s2", "s1", "s2"].iter().enumerate() {
            assert_eq!(parsed["capabilities"][index]["subject_id"], *subject);
        }
        for request in captured.lock().unwrap().iter() {
            assert_eq!(request["messages"][0]["content"], PROMPT);
        }
        assert_eq!(salary["attempts"], 1);
        assert_eq!(market["attempts"], 1);
        server.abort();
    }

    #[test]
    fn literal_normalization_keeps_requested_factual_fields_and_does_not_infer_a_country_from_language() {
        for (question, label, kind, inputs, outputs, source) in [
            ("Geef voor Dam 1 Amsterdam de BAG-objectgegevens, bouwjaar, oppervlakte en gekoppelde kadastrale percelen via PDOK. Toon bronverwijzingen en ontbrekende velden.", "Dam 1 Amsterdam", "address", vec!["address"], vec!["construction_year","area","cadastral_parcel_ids","source_references","missing_fields"], "pdok"),
            ("Zoek Reuters-nieuws over ASML. Geef de gevonden titels, publicatiedatums en oorspronkelijke bronlinks en vermeld de zoekdekking.", "ASML", "company", vec!["keywords"], vec!["titles","publication_dates","original_links","search_coverage"], "reuters"),
        ] {
            let raw = json!({"language":"nl","subjects":[{"id":"s1","label":label,"type":kind,"identifiers":{}}],
                "capabilities":[scope_capability("research.data", &inputs, &outputs, Some("s1"), Some(source))],
                "deliverable":{"format":"chat","operations":[]}});
            let parsed = crate::capability_intake::normalize(&raw.to_string(), question, "fixture").unwrap();
            assert_eq!(parsed.capabilities[0].outputs, outputs);
            assert_eq!(parsed.capabilities[0].subject_id.as_deref(), Some("s1"));
            assert!(parsed.subjects[0].identifiers.is_empty(), "request language must not inject country identifiers");
            assert!(parsed.deliverable.operations.is_empty(), "factual outputs must not become answer operations");
        }
        let raw = json!({"language":"nl","subjects":[],
            "capabilities":[scope_capability("research.salary.benchmarks", &["country","sector"], &["salary_range"], None, None)],
            "deliverable":{"format":"chat","operations":[]}});
        let parsed = crate::capability_intake::normalize(&raw.to_string(), "Geef salarisbenchmarks.", "fixture").unwrap();
        assert!(parsed.subjects.is_empty());
        assert_eq!(parsed.capabilities[0].missing, vec!["country", "sector"]);
    }

    #[tokio::test]
    async fn sends_only_exact_prompt_question_and_shape_with_no_planner_fields() {
        let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
        let handler_capture = captured.clone();
        let raw_requests = Arc::new(Mutex::new(Vec::<String>::new()));
        let handler_raw = raw_requests.clone();
        let app = Router::new().route(
            "/",
            post(move |headers: HeaderMap, body: Bytes| {
                let captured = handler_capture.clone();
                let raw_requests = handler_raw.clone();
                async move {
                    assert_eq!(headers.get("authorization").unwrap(), "Bearer fixture-key");
                    assert_eq!(headers.get("content-type").unwrap(), "application/json");
                    captured
                        .lock()
                        .unwrap()
                        .push(serde_json::from_slice(&body).unwrap());
                    raw_requests
                        .lock()
                        .unwrap()
                        .push(String::from_utf8(body.to_vec()).unwrap());
                    Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(Body::from(format!(
                            "{}data: [DONE]\n\n",
                            delta("{\"language\":\"nl\"}")
                        )))
                        .unwrap()
                }
            }),
        );
        let (endpoint, server) = server(app).await;
        let client = client().with_local_endpoint(endpoint).unwrap();
        let first = client.parse("Hoi!", &[], None).await.unwrap();
        client
            .parse(
                "Geef \"KVK\" details\né",
                &["company.profile".into()],
                Some("subjects must be an array."),
            )
            .await
            .unwrap();
        let captured = captured.lock().unwrap();
        assert!(PROMPT.contains("capabilities: one per piece of data or action requested, at most sixteen."));
        assert_eq!(
            captured[0]["messages"],
            json!([{"role":"system","content":PROMPT},{"role":"user","content":"Hoi!"}])
        );
        assert_eq!(
            captured[0],
            json!({
                "model":DEFAULT_MODEL,"messages":captured[0]["messages"],
                "stream":true,"temperature":0,"max_tokens":4096,
                "provider":{"order":["alibaba"],"require_parameters":true},
                "response_format":{"type":"json_object"}
            })
        );
        assert_eq!(captured[1]["messages"][0]["content"], format!("{}Known slugs: company.profile\nPrevious output failed validation: subjects must be an array.\nParse the user's request again. Return only one complete JSON object with every required field.", PROMPT.strip_suffix("Known slugs: none").unwrap()));
        assert_eq!(captured[1]["messages"].as_array().unwrap().len(), 2);
        assert_eq!(
            captured[1]["messages"][1]["content"],
            "Geef \"KVK\" details\né"
        );
        // The shape reaches the model through the prompt alone; no strict schema.
        for request in raw_requests.lock().unwrap().iter() {
            assert!(!request.contains("json_schema"));
        }
        assert_eq!(first.text, "{\"language\":\"nl\"}");
        assert_eq!(first.model, DEFAULT_MODEL);
        assert!(serde_json::to_value(first).unwrap()["timing"]["parse_ms"].is_u64());
        server.abort();
    }

    #[tokio::test]
    async fn deadline_covers_an_open_stream_after_partial_content() {
        let app = Router::new().route(
            "/",
            post(|| async {
                let initial = futures_util::stream::iter([Ok::<Bytes, Infallible>(Bytes::from(
                    delta("partial"),
                ))]);
                let body =
                    initial.chain(futures_util::stream::pending::<Result<Bytes, Infallible>>());
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(Body::from_stream(body))
                    .unwrap()
            }),
        );
        let (endpoint, server) = server(app).await;
        let mut client = client().with_local_endpoint(endpoint).unwrap();
        client.deadline = Duration::from_millis(30);
        let started = Instant::now();
        let error = client.parse("question", &[], None).await.unwrap_err();
        // Reqwest and the outer timer share this deadline; both discard partial output.
        assert!(matches!(
            error.to_string().as_str(),
            "ask_v2_model_timeout" | "ask_v2_model_stream_unavailable"
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
        server.abort();
    }

    #[tokio::test]
    async fn http_failures_never_return_private_provider_details() {
        let app = Router::new().route(
            "/",
            post(|| async { (StatusCode::BAD_REQUEST, "private provider account detail") }),
        );
        let (endpoint, server) = server(app).await;
        let client = client().with_local_endpoint(endpoint).unwrap();
        assert_eq!(
            client
                .parse("question", &[], None)
                .await
                .unwrap_err()
                .to_string(),
            "ask_v2_model_unavailable"
        );
        server.abort();
    }
}
