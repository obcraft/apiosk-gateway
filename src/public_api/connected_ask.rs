//! Connected Ask adapters. App-only routes retain their existing guards.
//! Signed discovery is adopted without another question parse or a purchase.
use super::*;
use crate::{
    ask_v2::source_selection, capability_intake::ParsedRequest, search_snapshot::Snapshot,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Research {
    request_id: Uuid,
    question: String,
    parsed_request: ParsedRequest,
    search_id: String,
    #[serde(default)]
    discovery_id: Option<String>,
    #[serde(default)]
    selection: source_selection::Selection,
}

async fn connected(state: &ServerState, headers: &HeaderMap) -> Result<AuthContext, HttpResponse> {
    let ctx = context(state, headers, &["ask:create"]).await?;
    if ctx.principal.connection.is_none() {
        return Err(problem(
            StatusCode::FORBIDDEN,
            "connection_required",
            "Use the connected chatbot account for this Ask request.",
        ));
    }
    Ok(ctx)
}

pub async fn research(
    State(state): State<ServerState>,
    headers: HeaderMap,
    body: Bytes,
) -> HttpResponse {
    let result = async {
        let ctx = connected(&state, &headers).await?;
        let mut req: Research = parse(&body)?;
        req.question = req.question.trim().to_owned();
        if req.request_id.is_nil()
            || req.question.is_empty()
            || req.question.encode_utf16().count() > 16_000
            || req.parsed_request.meta.text_sha256
                != hex::encode(Sha256::digest(req.question.as_bytes()))
        {
            return Err(problem(
                StatusCode::BAD_REQUEST,
                "invalid_research_request",
                "Keep the original question and its complete signed search when planning.",
            ));
        }
        let refresh = || {
            problem(
                StatusCode::CONFLICT,
                "source_refresh_required",
                "Refresh this source search before planning. Nothing was purchased.",
            )
        };
        let snapshot = Snapshot::open_connected(
            &state.engine.signer,
            &ctx.principal,
            &req.parsed_request,
            &req.search_id,
        )
        .map_err(|_| refresh())?;
        let snapshot = if let Some(discovery) = &req.discovery_id {
            let discovery = Snapshot::open_connected(
                &state.engine.signer,
                &ctx.principal,
                &req.parsed_request,
                discovery,
            )
            .map_err(|_| refresh())?;
            snapshot.with_discovery(discovery).map_err(|_| refresh())?
        } else {
            snapshot
        };
        let receipt = crate::state::digest(&(&req.search_id, &req.discovery_id, &req.selection));
        let discover = DiscoverRequest {
            request_id: req.request_id,
            question: req.question.clone(),
            state: None,
            context_delta: Default::default(),
        };
        let Ok(_permit) = state.permits.try_acquire() else {
            return Err(busy());
        };
        if let Some(existing) = state
            .engine
            .existing_connected_research(&ctx.principal, &discover, &req.parsed_request, &receipt)
            .await
            .map_err(|error| unavailable("connected research recovery", &error))?
        {
            return Ok(Json(existing).into_response());
        }
        let catalog = state
            .engine
            .catalog
            .get()
            .await
            .map_err(|error| unavailable("connected research catalog", &error))?;
        if !snapshot.current(&catalog.version) {
            return Err(refresh());
        }
        let options = snapshot.source_options().to_vec();
        let planning_reply = snapshot.planning_reply();
        let intake = snapshot.intake(req.parsed_request);
        let source_context = source_selection::Context {
            question: req.question,
            original_intake: intake,
            search_id: req.search_id,
            discovery_id: req.discovery_id,
            options,
            selection: req.selection,
            planning_reply,
        };
        let intake = source_context.expanded(&catalog).map_err(|_| {
            problem(
                StatusCode::BAD_REQUEST,
                "invalid_source_selection",
                "Choose only offered sources and their declared inputs. Nothing was purchased.",
            )
        })?;
        let response = state
            .engine
            .discover_connected_searched(
                &ctx.principal,
                discover,
                intake,
                catalog,
                &receipt,
                source_context,
            )
            .await
            .map_err(|error| unavailable("connected research", &error))?;
        Ok(Json(response).into_response())
    }
    .await;
    result.unwrap_or_else(|response| response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Conversation {
    request_id: Uuid,
    question: String,
    task_refs: Vec<Uuid>,
}

pub async fn conversation(
    State(state): State<ServerState>,
    headers: HeaderMap,
    body: Bytes,
) -> HttpResponse {
    let result = async {
        let ctx = connected(&state,&headers).await?;
        let req: Conversation = parse(&body)?;
        if req.request_id.is_nil() || req.question.trim().is_empty() || req.question.encode_utf16().count()>16_000
            || req.task_refs.is_empty() || req.task_refs.len()>20 || req.task_refs.iter().any(Uuid::is_nil) {
            return Err(problem(StatusCode::BAD_REQUEST,"invalid_follow_up","Send the follow-up and the saved task references from this conversation."));
        }
        let Ok(_permit) = state.permits.try_acquire() else { return Err(busy()); };
        if state.engine.parser.ask_v2().is_some_and(|client| !client.admit_reply(&ctx.principal.owner.to_string())) { return Err(busy()); }
        let response = state.engine.connected_ask_follow_up(&ctx.principal,req.request_id,req.question.trim(),&req.task_refs)
            .await.map_err(|error|unavailable("connected follow-up",&error))?;
        if !response.errors.is_empty() {
            let code=response.errors[0]["code"].as_str().unwrap_or("follow_up_unavailable");
            return Err(problem(StatusCode::CONFLICT,code,"The saved follow-up could not be confirmed. Recover the existing task before continuing."));
        }
        let reply=&response.context_view["ask_conversation"];
        if reply["kind"] == "saved_answer" {
            for id in reply["task_refs"].as_array().into_iter().flatten().filter_map(|id|id.as_str().and_then(|id|id.parse::<Uuid>().ok())) {
                if let Err(error)=state.engine.start_worker(ctx.principal.clone(),token(&headers).unwrap_or_default().to_owned(),id).await {
                    tracing::warn!(error=%error,"connected saved answer will resume on recovery");
                }
            }
        }
        Ok(Json(reply.clone()).into_response())
    }.await;
    result.unwrap_or_else(|response| response)
}
