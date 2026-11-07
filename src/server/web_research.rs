//! Authenticated App adapter. Streaming phases follow actual operation
//! boundaries; disconnecting cancels the work and releases its permit.
use super::{app_person, failure, ServerState};
use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    convert::Infallible,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

#[derive(Default, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
enum SearchProvider {
    #[default]
    Combined,
    Exa,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    question: String,
    #[serde(default)]
    earlier: String,
    #[serde(default)]
    provider: SearchProvider,
    #[serde(default)]
    stream_text: bool,
}

pub(super) async fn research(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<Request>,
) -> Response {
    let person = match app_person(&state, &headers).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let question = request.question.trim().to_owned();
    if question.is_empty()
        || question.encode_utf16().count() > 16_000
        || request.earlier.chars().count() > 4000
    {
        return failure(
            StatusCode::BAD_REQUEST,
            "invalid_web_question",
            "Shorten your web research question to 16,000 characters.",
        );
    }
    let Some(client) = state.engine.parser.ask_v2().cloned() else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "web_research_unavailable",
            "Web research is temporarily unavailable.",
        );
    };
    if !client.admit_reply(&person.owner.to_string()) {
        return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "Try again shortly.");
    }
    let Ok(permit) = state.permits.clone().try_acquire_owned() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "Try again shortly.");
    };
    let (sender, receiver) = mpsc::channel::<Value>(8);
    crate::telemetry::spawn(async move {
        let _permit = permit;
        let work = run(&client, &question, &request.earlier, request.provider, request.stream_text, &sender);
        tokio::select! {
            _ = sender.closed() => {},
            result = tokio::time::timeout(Duration::from_secs(78),work) => {
                if !matches!(result,Ok(Ok(()))) {
                    tracing::warn!(operation="web-research",status="failed","research operation");
                    let message = if request.provider == SearchProvider::Exa {
                        "Exa web search could not finish. Try again, or send your question to use the other search providers."
                    } else {
                        "Web research could not finish. Retry your question."
                    };
                    let _=sender.send(json!({"phase":"failed","errors":[{"code":"web_research_failed","message":message}]})).await;
                }
            }
        }
    });
    let body = ReceiverStream::new(receiver)
        .map(|value| Ok::<_, Infallible>(Bytes::from(format!("{value}\n"))));
    (
        [
            ("content-type", "application/x-ndjson"),
            ("cache-control", "no-store"),
            ("x-accel-buffering", "no"),
        ],
        Body::from_stream(body),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_research_provider_is_optional_and_strictly_validated() {
        let default: Request = serde_json::from_value(json!({"question":"Evidence"})).unwrap();
        assert!(matches!(default.provider, SearchProvider::Combined));
        let exa: Request = serde_json::from_value(json!({"question":"Evidence","provider":"exa"})).unwrap();
        assert!(matches!(exa.provider, SearchProvider::Exa));
        assert!(serde_json::from_value::<Request>(json!({"question":"Evidence","provider":"unknown"})).is_err());
        assert!(serde_json::from_value::<Request>(json!({"question":"Evidence","api_key":"browser-key"})).is_err());
    }

    /// Explicit operator opt-in. Exercises the actual search, page reader and
    /// streamed answer path without accounts, purchases or saved task writes.
    #[tokio::test]
    #[ignore = "uses configured search/model credentials and public webpages"]
    async fn live_core_web_research() {
        dotenvy::dotenv().ok();
        let questions: Vec<String> = serde_json::from_slice(&std::fs::read(
            std::env::var("ASK_WEB_TEST_QUESTIONS").expect("explicit questions file required")
        ).unwrap()).unwrap();
        let evidence = std::env::var("ASK_WEB_TEST_EVIDENCE").expect("evidence path required");
        let key = std::env::var("OPENROUTER_API_KEY").or_else(|_|std::env::var("INTENT_API_KEY")).unwrap();
        let client = crate::ask_v2::Client::new(reqwest::Client::new(), key, crate::ask_v2::DEFAULT_MODEL.into());
        let provider = if std::env::var("ASK_WEB_TEST_PROVIDER").as_deref() == Ok("exa") { SearchProvider::Exa } else { SearchProvider::Combined };
        let mut results = Vec::new();
        for question in questions {
            let started = Instant::now();
            let (sender, mut receiver) = mpsc::channel(8);
            let work = async {
                let outcome = tokio::time::timeout(Duration::from_secs(78), run(&client, &question, "", provider, true, &sender)).await;
                drop(sender);
                outcome
            };
            let collect = async {
                let mut completed = None;
                let mut first_text_ms = None;
                let mut first_sources_ms = None;
                let mut source_updates = 0;
                let mut first_draft_ms = None;
                let mut updates = 0;
                while let Some(event) = receiver.recv().await {
                    if event["phase"] == "sources" {
                        first_sources_ms.get_or_insert(started.elapsed().as_millis() as u64);
                        source_updates += 1;
                    }
                    if event["phase"] == "answer" {
                        updates += 1;
                        first_text_ms.get_or_insert(started.elapsed().as_millis() as u64);
                        let answer = event.get("answer").unwrap_or(&event["result"]["answer"]);
                        if answer.get("draft").is_some() { first_draft_ms.get_or_insert(started.elapsed().as_millis() as u64); }
                    }
                    if event["phase"] == "complete" {
                        let mut result = event["result"].clone();
                        result["streaming_verification"] = json!({"first_sources_ms":first_sources_ms,"source_updates":source_updates,"first_text_ms":first_text_ms,"first_draft_ms":first_draft_ms,"complete_ms":started.elapsed().as_millis() as u64,"updates":updates});
                        completed = Some(result); break;
                    }
                }
                completed
            };
            let (outcome, completed) = tokio::join!(work, collect);
            let row = match completed {
                Some(result) => result,
                None => json!({"question":question,"error":format!("{outcome:?}")}),
            };
            println!("{}: {} sources, {} blocks", question, row["sources"].as_array().map_or(0, Vec::len), row["answer"]["blocks"].as_array().map_or(0, Vec::len));
            results.push(row);
            std::fs::write(&evidence, serde_json::to_vec_pretty(&results).unwrap()).unwrap();
        }
        assert!(results.iter().all(|result| result["sources"].as_array().is_some_and(|sources| !sources.is_empty())
            && result["answer"]["blocks"].as_array().is_some_and(|blocks| !blocks.is_empty())), "inspect live research evidence");
    }
}
async fn phase(sender: &mpsc::Sender<Value>, name: &str, count: usize) -> anyhow::Result<()> {
    sender
        .send(json!({"phase":name,"source_count":count}))
        .await?;
    Ok(())
}
async fn run(
    client: &crate::ask_v2::Client,
    question: &str,
    earlier: &str,
    provider: SearchProvider,
    stream_text: bool,
    sender: &mpsc::Sender<Value>,
) -> anyhow::Result<()> {
    phase(sender, "searching", 0).await?;
    let query = crate::web_research::search_query(client, question, earlier).await;
    let evidence = crate::web_research::streaming::retrieve(&query.query, question, query.subject.as_deref(),
        provider == SearchProvider::Exa, |sources| async move {
            // Only read, validated evidence is streamed, never search snippets.
            sender.send(json!({"phase":"sources","question":question,"sources":sources})).await?;
            Ok(())
        }).await?;
    let sources = evidence.sources;
    let failures = evidence.failures;
    let providers = evidence.providers;
    let search_operation = evidence.search;
    let extract_ms = evidence.elapsed_ms;
    phase(sender, "analyzing", sources.len()).await?;
    let start = Instant::now();
    let count = sources.len();
    let mut limitations = Vec::new();
    if count < crate::web_research::TARGET {
        limitations.push(format!("Only {count} of {} requested sources were successfully read and analyzed. Other results were unavailable, blocked, duplicate or irrelevant.", crate::web_research::TARGET));
    }
    let input = json!({"question":question,"search_query":query.query,"compliance_subject":query.subject,"earlier_conversation_context_only":earlier,"current_date":chrono::Utc::now().date_naive(),"sources":sources});
    let synthesis_model = crate::ask_v2::web_synthesis_model(&input);
    let process_ms = start.elapsed().as_millis() as u64;
    let start = Instant::now();
    let (answer, usage) = if count == 0 {
        limitations.push("No readable, relevant pages were found. There is not enough retrieved evidence to answer this question.".into());
        (
            crate::web_research::Answer {
                blocks: vec![],
                limitations: vec![],
            },
            Value::Null,
        )
    } else {
        phase(sender, "generating", count).await?;
        let mut sent_evidence = false;
        let mut completed_blocks = 0;
        let output = client.synthesize_web_streaming(input, |mut answer| {
            let blocks = answer["blocks"].as_array().map_or(0, Vec::len);
            let skip = !stream_text && blocks <= completed_blocks;
            completed_blocks = blocks;
            if !stream_text { answer.as_object_mut().unwrap().remove("draft"); }
            tracing::debug!(operation="web-research-answer-stream",elapsed_ms=start.elapsed().as_millis() as u64,
                block_count=answer["blocks"].as_array().map_or(0,Vec::len),source_count=count,"cited paragraphs streamed");
            let snapshot = if stream_text && sent_evidence {
                // Evidence is immutable. Do not resend all page excerpts on
                // every token; only the growing answer crosses the wire.
                json!({"phase":"answer","answer":answer})
            } else { json!({"phase":"answer","source_count":count,"result":{"question":question,
                "answer":answer,"sources":sources,"provenance":{"query":query.query,"search_providers":providers,"skipped":failures},
                "operations":{"query_resolution":{"elapsed_ms":query.elapsed_ms,"method":query.method,"model":crate::ask_v2::WEB_QUERY_MODEL,"usage":query.usage},
                    "search":search_operation,"extraction":{"elapsed_ms":extract_ms,"overlapped_with_search":true,"cost_usd":0},
                    "processing":{"elapsed_ms":process_ms},"synthesis":{"elapsed_ms":start.elapsed().as_millis() as u64,"model":synthesis_model,"usage":null}}}}) };
            if !skip { sent_evidence = true; }
            async move { if !skip { sender.send(snapshot).await?; } Ok(()) }
        }).await.map_err(|error| {
            tracing::warn!(
                operation = "web-research-synthesis",
                status = "failed",
                "research operation"
            );
            error
        })?;
        (
            crate::web_research::validate_answer(&output, count).map_err(|error| {
                tracing::warn!(
                    operation = "web-research-synthesis",
                    status = "invalid_citations_or_output",
                    "research operation"
                );
                error
            })?,
            output["usage"].clone(),
        )
    };
    let synthesis_ms = start.elapsed().as_millis() as u64;
    limitations.extend(answer.limitations);
    let answer = crate::web_research::Answer {
        blocks: answer.blocks,
        limitations,
    };
    tracing::info!(
        operation = "web-research-synthesis",
        elapsed_ms = synthesis_ms,
        count,
        "research operation"
    );
    sender.send(json!({"phase":"complete","result":{"question":question,"answer":answer,"sources":sources,
        "provenance":{"query":query.query,"search_providers":providers,"skipped":failures},
        "operations":{"query_resolution":{"elapsed_ms":query.elapsed_ms,"method":query.method,"model":crate::ask_v2::WEB_QUERY_MODEL,"usage":query.usage},
            "search":search_operation,"extraction":{"elapsed_ms":extract_ms,"overlapped_with_search":true,"cost_usd":0},
            "processing":{"elapsed_ms":process_ms},"synthesis":{"elapsed_ms":synthesis_ms,"model":synthesis_model,"usage":usage}}
    }})).await?;
    Ok(())
}
