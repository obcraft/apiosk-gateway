//! The public question -> choices -> selected execution contract. Discovery
//! shares the App's interpreter and origin searches and never creates a task.
use super::*;
use crate::server::ask_v2::{self, Origin};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Question { question: String }

pub async fn ask(State(state): State<ServerState>, headers: HeaderMap, body: Bytes) -> HttpResponse {
    // Explicit recipes retain their established priced-workflow contract.
    if serde_json::from_slice::<Value>(&body).ok().is_some_and(|value| value.get("workflow").is_some()) {
        return super::plan_ask(State(state), headers, body).await;
    }
    discover(&state, &headers, &body).await.unwrap_or_else(|response| response)
}

async fn discover(state: &ServerState, headers: &HeaderMap, body: &[u8]) -> Answer {
    let ctx = context(state, headers, &["ask:create"]).await?;
    let req: Question = parse(body)?;
    let question = req.question.trim();
    if question.is_empty() || question.encode_utf16().count() > 16_000 {
        return Err(problem(StatusCode::BAD_REQUEST, "invalid_question", "question must be 1 to 16000 characters."));
    }
    let Some(client) = state.engine.parser.ask_v2() else {
        return Err(problem(StatusCode::SERVICE_UNAVAILABLE, "parser_unavailable", "Ask is temporarily unavailable."));
    };
    let Ok(_permit) = state.permits.try_acquire() else { return Err(busy()); };
    if !client.admit(&ctx.principal.owner.to_string()) || !client.admit_search(&ctx.principal.owner.to_string()) {
        return Err(busy());
    }
    let started=std::time::Instant::now();
    // Interpretation stays free; both origin searches are deterministic.
    let mut interpretation = if ctx.principal.connection.is_some() {
        let catalog = state.engine.catalog.get().await.map_err(|e| unavailable("Ask catalog", &e))?;
        retry_free_interpretation(|| client.interpret_connected_with_context(question, question, question, &[], &catalog)).await
            .map(|(mut interpretation, prior_attempts)| { interpretation.attempts += prior_attempts; interpretation })
    } else {
        client.interpret_with_context(question, question, question, &[]).await
    }.map_err(|e| unavailable("Ask interpretation", &e))?;
    if ctx.principal.connection.is_some() {
        let catalog = state.engine.catalog.get().await.map_err(|e| unavailable("Ask catalog", &e))?;
        crate::parser::connected_planning::anchor_collection_request(question, &mut interpretation.parsed_request, &catalog);
    }
    search_parsed(state, &ctx, question, interpretation.parsed_request, interpretation.output.model, interpretation.attempts, started).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchRetry { question: String, parsed_request: crate::capability_intake::ParsedRequest, search_id: String }

/// Retry discovery from the verified parse. An expired catalog snapshot can
/// be refreshed here; it never authorizes a quote or a source purchase.
pub async fn search(State(state): State<ServerState>, headers: HeaderMap, body: Bytes) -> HttpResponse {
    let result=async {
        let ctx=context(&state,&headers,&["ask:create"]).await?;
        if ctx.principal.connection.is_none() {
            return Err(problem(StatusCode::FORBIDDEN,"connection_required","Use the connected chatbot account to refresh its source search."));
        }
        let req:SearchRetry=parse(&body)?;
        let question=req.question.trim();
        if question.is_empty() || question.encode_utf16().count()>16_000
            || req.parsed_request.meta.text_sha256 != hex::encode(Sha256::digest(question.as_bytes())) {
            return Err(problem(StatusCode::BAD_REQUEST,"invalid_question","Keep the original question and signed parse when refreshing sources."));
        }
        crate::search_snapshot::Snapshot::open_connected(&state.engine.signer,&ctx.principal,&req.parsed_request,&req.search_id)
            .map_err(|_|problem(StatusCode::CONFLICT,"source_refresh_required","This signed search does not belong to this connection and question."))?;
        let Ok(_permit)=state.permits.try_acquire() else {return Err(busy());};
        if state.engine.parser.ask_v2().is_some_and(|client|!client.admit_search(&ctx.principal.owner.to_string())) {return Err(busy());}
        let model=req.parsed_request.meta.model.clone();
        search_parsed(&state,&ctx,question,req.parsed_request,model,0,std::time::Instant::now()).await
    }.await;
    result.unwrap_or_else(|response|response)
}

async fn search_parsed(state:&ServerState,ctx:&AuthContext,question:&str,parsed:crate::capability_intake::ParsedRequest,
    model:String,attempts:usize,started:std::time::Instant)->Answer {
    let parsed=&parsed;
    let (apiosk, coinbase) = tokio::join!(
        async {
            // Connected callers retain the canonical parse, but a generic
            // model category must not hide an exact documented output. The
            // normal App search keeps its existing matching behavior.
            if ctx.principal.connection.is_some() {
                if let Ok(catalog) = state.engine.catalog.get().await {
                    let matches = crate::ask_v2::search::search(parsed, &catalog);
                    let mut intake = crate::capability_intake::Intake {
                        parsed_request: parsed.clone(),
                        selected_capabilities: matches.iter().flat_map(|found| found.operations.iter().cloned()).collect(),
                        matches, catalog_version: catalog.version.clone(),
                        apiosk_sources_searched: crate::capability_intake::source_count(&catalog),
                    };
                    let refreshed = crate::parser::connected_planning::refresh_discovery(&mut intake, &catalog);
                    let input_context = crate::parser::connected_planning::input_context(&intake, &catalog);
                    if refreshed {
                        return ask_v2::search_connected_catalog_with_matches(state, &input_context.parsed_request, Origin::Apiosk, Some(&intake.matches)).await;
                    }
                    return ask_v2::search_connected_catalog_with_matches(state, &input_context.parsed_request, Origin::Apiosk, None).await;
                }
            }
            if ctx.principal.connection.is_some() {
                ask_v2::search_connected_catalog_with_matches(state, parsed, Origin::Apiosk, None).await
            } else { ask_v2::search_catalog(state, parsed, Origin::Apiosk).await }
        },
        ask_v2::search_catalog(state, parsed, Origin::Coinbase),
    );
    // An origin outage must never masquerade as a successful empty search.
    let mut searches = serde_json::Map::new();
    let mut apis = Vec::new();
    let mut errors = Vec::new();
    let mut search_id = None;
    let mut discovery_id = None;
    let mut source_options = json!([]);
    for (origin, result) in [("apiosk", apiosk), ("coinbase", coinbase)] {
        match result {
            Ok(value) => {
                if origin == "apiosk" && ctx.principal.connection.is_some() {
                    search_id = Some(crate::search_snapshot::Snapshot::issue_connected(&state.engine.signer, &ctx.principal, parsed, &value)
                        .map_err(|error| unavailable("source selection snapshot", &error))?);
                    source_options = value["source_options"].clone();
                }
                if origin == "coinbase" && ctx.principal.connection.is_some() {
                    discovery_id = Some(crate::search_snapshot::Snapshot::issue_connected(&state.engine.signer, &ctx.principal, parsed, &value)
                        .map_err(|error| unavailable("external discovery snapshot", &error))?);
                }
                for found in value["matches"].as_array().into_iter().flatten() {
                    for candidate in found[origin].as_array().into_iter().flatten() {
                        let mut api = candidate.clone();
                        api["requested_capability"] = found["slug"].clone();
                        api["discovery_query"] = found["query"].clone();
                        let executable = candidate["availability"] == "supported" && candidate["endpoint"].is_object();
                        api["executable"] = json!(executable);
                        if executable {
                            for field in ["inputs", "outputs", "input_values", "missing_inputs", "lookup"] {
                                api[field] = candidate["endpoint"][field].clone();
                            }
                            api["buyer_price"] = candidate["endpoint"]["price"].clone();
                        }
                        if !apis.contains(&api) { apis.push(api); }
                    }
                }
                if value["matches"].as_array().into_iter().flatten().any(|m| m["coinbase_status"] == "unavailable") {
                    errors.push(json!({"origin":origin,"code":"search_unavailable"}));
                }
                searches.insert(origin.into(), value);
            }
            Err(_) => { errors.push(json!({"origin":origin,"code":"search_unavailable"})); }
        }
    }
    let status = if !errors.is_empty() { "partial" } else if apis.is_empty() { "needs_input" } else { "choices" };
    let mut response = json!({"status":status,"question":question,"parsed_request":parsed,"model":model,
        "attempts":attempts,"timing":{"parse_ms":started.elapsed().as_millis() as u64,"model_http_requests":crate::telemetry::model_http_requests()},"apis":apis,"searches":searches,
        "errors":errors,"execution":{"method":"POST","path":"/v2/execute"}});
    if ctx.principal.connection.is_some() {
        response["search_id"] = json!(search_id);
        response["source_options"] = source_options;
        response["discovery_id"] = json!(discovery_id);
        response["company_context"] = json!(crate::subject_context::collect(parsed).await);
        response["context_status"] = json!("complete");
    }
    Ok(Json(response).into_response())
}

// Only an exhausted, strictly validated interpretation gets one fresh try.
// This is deliberately confined to free connected discovery: it never retries
// transport/rate failures or any task, quote, approval or purchase operation.
async fn retry_free_interpretation<T, F, Fut>(mut interpret: F) -> anyhow::Result<(T, usize)>
where F: FnMut() -> Fut, Fut: std::future::Future<Output = anyhow::Result<T>> {
    match interpret().await {
        Err(error) if error.is::<crate::ask_v2::UnverifiedInputGrounding>() => {
            tracing::warn!("Connected free discovery retries an unverified interpretation once");
            // Both responses failed literal grounding before any source-scope
            // requirement was pinned. A retry cannot erase a retained scope.
            interpret().await.map(|value| (value, 2))
        }
        result => result.map(|value| (value, 0)),
    }
}

#[cfg(test)]
mod free_interpretation_tests {
    use super::retry_free_interpretation;
    use std::collections::VecDeque;

    #[tokio::test]
    async fn connected_free_retry_is_bounded_to_exact_unverified_error() {
        for (replies, expected_calls, expected_prior_attempts) in [
            (vec![Ok(1)], 1, Some(0)),
            (vec![Err("eligible_grounding"), Ok(1)], 2, Some(2)),
            (vec![Err("eligible_grounding"), Err("eligible_grounding"), Ok(1)], 2, None),
            (vec![Err("ask_v2_interpretation_unverified"), Ok(1)], 1, None),
            (vec![Err("connection reset"), Ok(1)], 1, None),
            (vec![Err("request rate limit"), Ok(1)], 1, None),
            (vec![Err("ask_v2_interpretation_unverified: unrelated"), Ok(1)], 1, None),
        ] {
            let mut replies = VecDeque::from(replies);
            let mut calls = 0;
            let result = retry_free_interpretation(|| {
                calls += 1;
                std::future::ready(replies.pop_front().unwrap().map_err(|error| {
                    if error == "eligible_grounding" { crate::ask_v2::UnverifiedInputGrounding.into() }
                    else { anyhow::Error::msg(error) }
                }))
            }).await;
            assert_eq!(calls, expected_calls);
            assert_eq!(result.ok().map(|(_, attempts)| attempts), expected_prior_attempts);
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    api_id: String,
    capability: Option<String>,
    #[serde(default)]
    input: BTreeMap<String, Value>,
    max_spend: Option<Value>,
    wait_seconds: Option<u64>,
    limit: Option<usize>,
}

pub async fn execute_selected(State(state): State<ServerState>, headers: HeaderMap, body: Bytes) -> HttpResponse {
    selected(&state, &headers, &body).await.unwrap_or_else(|response| response)
}

async fn selected(state: &ServerState, headers: &HeaderMap, body: &[u8]) -> Answer {
    let ctx = context(state, headers, &["sources:execute"]).await?;
    let req: Selection = parse(body)?;
    let max = spend_limit(req.max_spend.as_ref())?
        .ok_or_else(|| problem(StatusCode::BAD_REQUEST, "max_spend_required", "Set max_spend to the total you approve for this selection."))?;
    if req.api_id.is_empty() || req.input.len() > 64 {
        return Err(problem(StatusCode::BAD_REQUEST, "invalid_input", "Select an api_id and supply its declared input fields."));
    }
    if !headers.contains_key("idempotency-key") {
        return Err(problem(StatusCode::BAD_REQUEST, "idempotency_key_required", "Send Idempotency-Key so retries recover the same execution."));
    }
    let limit = limit(req.limit)?;
    let request = request_id(headers, &ctx, "selected-execute")?;
    if req.api_id.starts_with("bazaar:") {
        return external_selected(state, headers, &ctx, &req, request, max).await;
    }
    let api_id = req.api_id.parse::<Uuid>().ok().filter(|id| !id.is_nil())
        .ok_or_else(|| problem(StatusCode::BAD_REQUEST, "invalid_api_id", "Copy api_id from the Ask result."))?;
    let response = {
        let Ok(_permit) = state.permits.try_acquire() else { return Err(busy()); };
        // The exact same pinned preparation used by the App's selected source.
        state.engine.prepare_endpoint(&ctx.principal, request, api_id, req.capability.as_deref(), &req.input)
            .await.map_err(|e| unavailable("selected endpoint", &e))?
    };
    if response.state.is_none() {
        if let Some(error) = response.errors.first().filter(|e| matches!(e["code"].as_str(), Some("invalid_endpoint_input" | "endpoint_unavailable" | "ambiguous_endpoint"))) {
            return Err(problem(StatusCode::UNPROCESSABLE_ENTITY, error["code"].as_str().unwrap(), error["message"].as_str().unwrap_or("Review the selected API and inputs.")));
        }
    }
    if let Some(refusal) = refused_by_engine(&response) { return Err(refusal); }
    let id = response.state.as_ref().map(|s| s.state_ref)
        .ok_or_else(|| problem(StatusCode::UNPROCESSABLE_ENTITY, "invalid_input", "The selected endpoint cannot run with these inputs."))?;
    // Missing or ambiguous inputs are returned to the caller, never replaced
    // by a different source or another interpretation of the question.
    if response.proposal.is_none() {
        return finish(state, &ctx, id, Duration::ZERO, limit, None).await;
    }
    authorize_and_run(state, &ctx, token(headers).map_err(|_| no_bearer())?, id, Some(max)).await?;
    finish(state, &ctx, id, wait_for(req.wait_seconds), limit, None).await
}

async fn external_selected(state: &ServerState, headers: &HeaderMap, ctx: &AuthContext, req: &Selection, id: Uuid, max: i64) -> Answer {
    use crate::ask_v2::{external, purchase};
    let (url, method, query) = external::selection(&req.api_id)
        .ok_or_else(|| problem(StatusCode::BAD_REQUEST, "invalid_api_id", "Copy api_id from the Ask result."))?;
    if req.capability.as_deref().is_some_and(|cap| cap != "external.request") {
        return Err(problem(StatusCode::BAD_REQUEST, "invalid_capability", "Use external.request for this selection."));
    }
    let input = json!(req.input);
    let hash = purchase::request_hash(&ctx.principal, &url, &method, &query, &input);
    let Ok(_permit) = state.permits.try_acquire() else { return Err(busy()); };
    let saved = purchase::prepared(&state.engine, &ctx.principal, id, &hash).await
        .map_err(external_error)?;
    let view = match saved {
        Some(view) => view,
        None => {
            let directory = state.commerce.as_ref()
                .ok_or_else(|| problem(StatusCode::SERVICE_UNAVAILABLE, "source_unavailable", "Source pricing is temporarily unavailable."))?;
            let offer = external::resolve(directory, &url, &method, &query).await
                .map_err(|_| problem(StatusCode::UNPROCESSABLE_ENTITY, "source_unavailable", "This selection is no longer executable."))?;
            let bound = external::validate_input(&offer, &input)
                .map_err(|e| problem(StatusCode::UNPROCESSABLE_ENTITY, "invalid_input", &e.to_string()))?;
            purchase::prepare(&state.engine, &ctx.principal, id, hash, offer, bound).await
                .map_err(external_error)?
        }
    };
    let cap = view["quote"]["max_total_atomic"].as_str().and_then(|v| v.parse::<i64>().ok())
        .ok_or_else(|| problem(StatusCode::SERVICE_UNAVAILABLE, "quote_unavailable", "Read the saved selection before retrying."))?;
    if view["status"] == "requires_approval" && cap > max {
        return Err(problem_with(StatusCode::PAYMENT_REQUIRED, "spend_limit_exceeded", "The current source price is above max_spend. Nothing was charged.", json!({"required":money(cap),"available":money(max)})));
    }
    let quote = view["quote"]["quote_ref"].as_str().and_then(|v| v.parse().ok())
        .ok_or_else(|| problem(StatusCode::SERVICE_UNAVAILABLE, "quote_unavailable", "Read the saved selection before retrying."))?;
    let view = purchase::authorize(state.engine.clone(), ctx.principal.clone(), token(headers).map_err(|_| no_bearer())?.to_owned(), id, quote, &cap.to_string()).await
        .map_err(external_error)?;
    Ok(external_response(view))
}

fn external_error(error: anyhow::Error) -> HttpResponse {
    match error.to_string().as_str() {
        "idempotency_conflict" => problem(StatusCode::CONFLICT, "idempotency_conflict", "This Idempotency-Key was already used with a different selection or input."),
        "quote_expired" => problem(StatusCode::CONFLICT, "plan_expired", "The unapproved quote expired. Use a new Idempotency-Key to prepare the current price."),
        _ => unavailable("external selection", &error),
    }
}

pub(super) fn external_response(mut view: Value) -> HttpResponse {
    view["id"] = view["purchase_id"].clone();
    let status = if view["status"] == "running" { StatusCode::ACCEPTED } else { StatusCode::OK };
    (status, Json(view)).into_response()
}
