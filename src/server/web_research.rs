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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    question: String,
    #[serde(default)]
    earlier: String,
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
        let work = run(&client, &question, &request.earlier, &sender);
        tokio::select! {
            _ = sender.closed() => {},
            result = tokio::time::timeout(Duration::from_secs(76),work) => {
                if !matches!(result,Ok(Ok(()))) {
                    tracing::warn!(operation="web-research",status="failed","research operation");
                    let _=sender.send(json!({"phase":"failed","errors":[{"code":"web_research_failed","message":"Web research could not finish. Retry your question."}]})).await;
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
    sender: &mpsc::Sender<Value>,
) -> anyhow::Result<()> {
    phase(sender, "searching", 0).await?;
    let start = Instant::now();
    let (candidates, providers) = tokio::time::timeout(
        Duration::from_secs(20),
        crate::web_research::search(question),
    )
    .await
    .map_err(|error| {
        tracing::warn!(
            operation = "web-research-search",
            status = "timeout",
            "research operation"
        );
        error
    })?
    .map_err(|error| {
        tracing::warn!(
            operation = "web-research-search",
            status = "failed",
            "research operation"
        );
        error
    })?;
    let search_ms = start.elapsed().as_millis() as u64;
    tracing::info!(
        operation = "web-research-search",
        elapsed_ms = search_ms,
        count = candidates.len(),
        cost_usd = 0,
        "research operation"
    );
    phase(sender, "reading", candidates.len().min(5)).await?;
    let start = Instant::now();
    let (sources, failures) = crate::web_research::read(candidates, question).await;
    let extract_ms = start.elapsed().as_millis() as u64;
    phase(sender, "analyzing", sources.len()).await?;
    let start = Instant::now();
    let count = sources.len();
    let mut limitations = Vec::new();
    if count < 5 {
        limitations.push(format!("Only {count} of 5 requested sources were successfully read and analyzed. Other results were unavailable, blocked, duplicate or irrelevant."));
    }
    let input = json!({"question":question,"earlier_conversation_context_only":earlier,"current_date":chrono::Utc::now().date_naive(),"sources":sources});
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
        let output = client.synthesize_web(input).await.map_err(|error| {
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
        "provenance":{"query":question,"search_providers":providers,"skipped":failures},
        "operations":{"search":{"elapsed_ms":search_ms,"cost_usd":0},"extraction":{"elapsed_ms":extract_ms,"cost_usd":0},
            "processing":{"elapsed_ms":process_ms},"synthesis":{"elapsed_ms":synthesis_ms,"model":crate::parser::MULTISOURCE_ANALYSIS_MODEL,"usage":usage}}
    }})).await?;
    Ok(())
}
