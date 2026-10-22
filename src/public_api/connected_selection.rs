//! Manual-only connected source preparation and exact saved-task repricing.
use super::*;

pub async fn prepare(State(state):State<ServerState>,headers:HeaderMap,body:Bytes)->HttpResponse {
    prepare_inner(&state,&headers,&body).await.unwrap_or_else(|response|response)
}
async fn prepare_inner(state:&ServerState,headers:&HeaderMap,body:&[u8])->Answer {
    let ctx=context(state,headers,&["ask:create"]).await?;
    if ctx.principal.connection.is_none() {return Err(problem(StatusCode::FORBIDDEN,"connection_required","Use a connected account to prepare selected sources."));}
    let mut req:crate::engine::ConnectedPrepareRequest=parse(body)?;
    req.question=req.question.trim().to_owned();
    if req.request_id.is_nil() || req.question.trim().is_empty() || req.question.encode_utf16().count()>16_000 {
        return Err(problem(StatusCode::BAD_REQUEST,"invalid_request","Supply a request_id and the original question."));
    }
    if req.parsed_request.meta.text_sha256 != hex::encode(Sha256::digest(req.question.as_bytes())) {
        return Err(problem(StatusCode::BAD_REQUEST,"source_refresh_required","The question changed. Search again before selecting sources."));
    }
    let snapshot=crate::search_snapshot::Snapshot::open_connected(&state.engine.signer,&ctx.principal,&req.parsed_request,&req.search_id)
        .map_err(|_|problem(StatusCode::CONFLICT,"source_refresh_required","The source search is invalid for this connection. Search again before selecting sources."))?;
    let Ok(_permit)=state.permits.try_acquire() else{return Err(busy());};
    let response=state.engine.prepare_connected_sources(&ctx.principal,req,snapshot).await
        .map_err(|error|unavailable("selected source preparation",&error))?;
    Ok(Json(response).into_response())
}

pub async fn requote(State(state):State<ServerState>,headers:HeaderMap,body:Bytes)->HttpResponse {
    let result=async {
        let ctx=context(&state,&headers,&["ask:create"]).await?;
        let req:crate::protocol::RequoteRequest=parse(&body)?;
        if ctx.principal.connection.is_none() {return Err(problem(StatusCode::FORBIDDEN,"connection_required","Use the connection that prepared these selected sources."));}
        let Ok(_permit)=state.permits.try_acquire() else{return Err(busy());};
        let response=state.engine.requote_connected_sources(&ctx.principal,req).await
            .map_err(|_|problem(StatusCode::CONFLICT,"source_requote_unavailable","This selected-source quote cannot be refreshed. Read the saved task, or search sources again. Nothing was purchased."))?;
        Ok(Json(response).into_response())
    }.await;
    result.unwrap_or_else(|response|response)
}
