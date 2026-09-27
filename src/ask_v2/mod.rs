//! Isolated Ask v2 model transport. Validation and normalization stay in the
//! unchanged WebGPU TypeScript parser; this client never plans or executes data.

pub mod search;
pub mod external;
pub mod purchase;

use anyhow::{anyhow, ensure, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub const DEFAULT_MODEL: &str = "google/gemma-3-4b-it";
const ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";
const PROMPT: &str = include_str!("../../contracts/ask-v2-webgpu-prompt.txt");
const SCHEMA: &str = include_str!("../../contracts/ask-v2-webgpu-schema.json");
const DEADLINE: Duration = Duration::from_secs(12);
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
}

impl Client {
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
        self.admit_at(owner, false, Instant::now())
    }
    pub fn admit_search(&self, owner: &str) -> bool {
        self.admit_at(owner, true, Instant::now())
    }

    fn admit_at(&self, owner: &str, search: bool, now: Instant) -> bool {
        if owner.is_empty() || owner.len() > 256 {
            return false;
        }
        let Ok(mut limits) = self.limits.lock() else {
            return false;
        };
        limits.retain(|_, entry| {
            now.duration_since(entry.parse.start) < WINDOW
                || now.duration_since(entry.search.start) < WINDOW
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
            });
        let (window, maximum) = if search {
            (&mut entry.search, 60)
        } else {
            (&mut entry.parse, 30)
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
        let mut body = json!({
            "model": self.model,
            "messages": [{"role":"system","content":prompt},{"role":"user","content":question}],
            "stream": true, "temperature": 0, "max_tokens": 1024
        })
        .to_string();
        // Serialize all dynamic fields normally, but embed the trusted schema
        // verbatim: Value's sorted maps reorder its properties and can change
        // the model's generation order relative to WebGPU's requested shape.
        body.pop();
        body.push_str(",\"response_format\":{\"type\":\"json_schema\",\"json_schema\":{\"name\":\"capability_request\",\"strict\":true,\"schema\":");
        body.push_str(SCHEMA);
        body.push_str("}}}");
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
        // The complete operation, including waits and the stream, remains 12s.
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
    fn default_model_is_total_four_billion_and_fixture_endpoint_is_loopback_only() {
        assert_eq!(DEFAULT_MODEL, "google/gemma-3-4b-it");
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
        assert!(!original.admit(""));
        assert!(!original.admit(&"x".repeat(257)));
    }

    #[test]
    fn rate_cache_expires_and_never_evicts_live_owners_to_allow_bypassing_limits() {
        let client = client();
        let start = Instant::now();
        for i in 0..MAX_OWNERS {
            assert!(client.admit_at(&format!("owner-{i}"), false, start));
        }
        assert_eq!(client.limits.lock().unwrap().len(), MAX_OWNERS);
        assert!(!client.admit_at("overflow", false, start));
        for _ in 1..30 {
            assert!(client.admit_at("owner-0", false, start));
        }
        assert!(!client.admit_at("owner-0", false, start));
        assert!(client.admit_at("overflow", false, start + WINDOW));
        assert_eq!(client.limits.lock().unwrap().len(), 1);
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
        assert_eq!(PROMPT.len(), 3353);
        assert_eq!(
            captured[0]["messages"],
            json!([{"role":"system","content":PROMPT},{"role":"user","content":"Hoi!"}])
        );
        let schema: Value = serde_json::from_str(SCHEMA).unwrap();
        assert_eq!(
            captured[0],
            json!({
                "model":DEFAULT_MODEL,"messages":captured[0]["messages"],
                "stream":true,"temperature":0,"max_tokens":1024,
                "response_format":{"type":"json_schema","json_schema":{"name":"capability_request","strict":true,"schema":schema}}
            })
        );
        assert_eq!(captured[1]["messages"][0]["content"], format!("{}Known slugs: company.profile\nPrevious output failed validation: subjects must be an array.\nParse the user's request again. Return only one complete JSON object with every required field.", PROMPT.strip_suffix("Known slugs: none").unwrap()));
        assert_eq!(captured[1]["messages"].as_array().unwrap().len(), 2);
        assert_eq!(
            captured[1]["messages"][1]["content"],
            "Geef \"KVK\" details\né"
        );
        for request in raw_requests.lock().unwrap().iter() {
            let schema = request.split_once("\"schema\":").unwrap().1;
            assert!(schema.starts_with(SCHEMA));
            let positions = ["language", "subjects", "capabilities", "deliverable"]
                .map(|field| schema.find(&format!("\"{field}\":")).unwrap());
            assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
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
