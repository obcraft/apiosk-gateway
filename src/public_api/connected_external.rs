//! Connected Bazaar review and purchase recovery. Inspection and preparation
//! read only the configured directory; only explicit authorization dispatches.
use super::*;
use crate::ask_v2::{external, purchase};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceRequest {
    api_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrepareRequest {
    request_id: Uuid,
    api_id: String,
    input: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Approval {
    quote_ref: Uuid,
    max_total_atomic: String,
}

async fn connected(
    state: &ServerState,
    headers: &HeaderMap,
    scopes: &[&str],
) -> Result<AuthContext, HttpResponse> {
    let ctx = context(state, headers, scopes).await?;
    if ctx.principal.connection.is_none() {
        return Err(problem(
            StatusCode::FORBIDDEN,
            "connection_required",
            "Use the connected account that selected this source.",
        ));
    }
    Ok(ctx)
}

fn selection(api_id: &str) -> Result<(String, String, String), HttpResponse> {
    external::selection(api_id).ok_or_else(|| {
        problem(
            StatusCode::BAD_REQUEST,
            "invalid_source",
            "Copy the source's api_id from the Ask results.",
        )
    })
}

async fn resolve(
    state: &ServerState,
    resource: &str,
    method: &str,
    query: &str,
) -> Result<external::Offer, HttpResponse> {
    let directory = state.commerce.as_ref().ok_or_else(|| {
        problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "source_unavailable",
            "Source pricing is temporarily unavailable. Try again shortly.",
        )
    })?;
    external::resolve(
        directory,
        resource,
        method,
        query,
    )
    .await
    .map_err(|error| {
        problem(
            StatusCode::UNPROCESSABLE_ENTITY,
            &error.code,
            &error.message,
        )
    })
}

fn rate_limit(state: &ServerState, ctx: &AuthContext) -> Result<(), HttpResponse> {
    if state
        .engine
        .parser
        .ask_v2()
        .is_some_and(|client| !client.admit_search(&ctx.principal.owner.to_string()))
    {
        return Err(problem(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Wait a minute before reviewing another source.",
        ));
    }
    Ok(())
}

pub async fn source(
    State(state): State<ServerState>,
    headers: HeaderMap,
    body: Bytes,
) -> HttpResponse {
    let result: Answer = async {
        let ctx = connected(&state, &headers, &["sources:read"]).await?;
        let req: SourceRequest = parse(&body)?;
        let (resource, method, query) = selection(&req.api_id)?;
        let Ok(_permit) = state.permits.try_acquire() else {
            return Err(busy());
        };
        rate_limit(&state, &ctx)?;
        Ok(Json(resolve(&state, &resource, &method, &query).await?).into_response())
    }
    .await;
    result.unwrap_or_else(|response| response)
}

pub async fn prepare(
    State(state): State<ServerState>,
    headers: HeaderMap,
    body: Bytes,
) -> HttpResponse {
    let result: Answer = async {
        let ctx = connected(&state, &headers, &["ask:create"]).await?;
        let req: PrepareRequest = parse(&body)?;
        if req.request_id.is_nil() || !req.input.as_object().is_some_and(|input| input.len() <= 64)
        {
            return Err(problem(
                StatusCode::BAD_REQUEST,
                "invalid_input",
                "Supply a request_id and the selected source's input fields.",
            ));
        }
        let (resource, method, query) = selection(&req.api_id)?;
        let hash = purchase::request_hash(&ctx.principal, &resource, &method, &query, &req.input);
        let Ok(_permit) = state.permits.try_acquire() else {
            return Err(busy());
        };
        // Recover the same quote first. A retry never re-resolves or dispatches.
        if let Some(view) = purchase::prepared(&state.engine, &ctx.principal, req.request_id, &hash)
            .await
            .map_err(purchase_failure)?
        {
            return Ok(Json(view).into_response());
        }
        rate_limit(&state, &ctx)?;
        let offer = resolve(&state, &resource, &method, &query).await?;
        let input = external::validate_input(&offer, &req.input).map_err(|error| {
            problem(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_input",
                &error.to_string(),
            )
        })?;
        let view = purchase::prepare(
            &state.engine,
            &ctx.principal,
            req.request_id,
            hash,
            offer,
            input,
        )
        .await
        .map_err(purchase_failure)?;
        Ok(Json(view).into_response())
    }
    .await;
    result.unwrap_or_else(|response| response)
}

pub async fn status(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> HttpResponse {
    let result: Answer = async {
        let ctx = connected(&state, &headers, &["sources:read"]).await?;
        let view = purchase::view(&state.engine, &ctx.principal, id)
            .await
            .map_err(purchase_failure)?;
        Ok(Json(view).into_response())
    }
    .await;
    result.unwrap_or_else(|response| response)
}

pub async fn authorize(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Bytes,
) -> HttpResponse {
    let result: Answer = async {
        let ctx = connected(&state, &headers, &["ask:execute"]).await?;
        let req: Approval = parse(&body)?;
        if id.is_nil()
            || req.quote_ref.is_nil()
            || req.max_total_atomic.is_empty()
            || req.max_total_atomic.len() > 20
            || !req
                .max_total_atomic
                .bytes()
                .all(|byte| byte.is_ascii_digit())
        {
            return Err(problem(
                StatusCode::BAD_REQUEST,
                "invalid_quote",
                "Review the saved price before approving.",
            ));
        }
        let Ok(_permit) = state.permits.try_acquire() else {
            return Err(busy());
        };
        let view = purchase::authorize(
            state.engine.clone(),
            ctx.principal,
            token(&headers).map_err(|_| no_bearer())?.to_owned(),
            id,
            req.quote_ref,
            &req.max_total_atomic,
        )
        .await
        .map_err(purchase_failure)?;
        Ok(Json(view).into_response())
    }
    .await;
    result.unwrap_or_else(|response| response)
}

fn purchase_failure(error: anyhow::Error) -> HttpResponse {
    if error
        .downcast_ref::<crate::store::StoreAccessError>()
        .is_some()
    {
        return problem(
            StatusCode::NOT_FOUND,
            "purchase_unavailable",
            "This purchase is not available to this connection.",
        );
    }
    if error
        .downcast_ref::<sqlx::Error>()
        .is_some_and(crate::store::is_workspace_access_denied)
        || error.to_string() == "workspace spending is not permitted"
    {
        return problem(
            StatusCode::FORBIDDEN,
            "spending_not_permitted",
            "You cannot spend from this workspace.",
        );
    }
    match error.to_string().as_str() {
        "quote_expired" | "quote_changed" => problem(
            StatusCode::CONFLICT,
            "quote_changed",
            "The quote changed or expired. Review a new quote before approving.",
        ),
        "idempotency_conflict" => problem(
            StatusCode::CONFLICT,
            "idempotency_conflict",
            "This request_id has different inputs. Use a new request_id for changed inputs.",
        ),
        "purchase_cancelled" => problem(
            StatusCode::CONFLICT,
            "purchase_cancelled",
            "This purchase was cancelled.",
        ),
        "purchases_unavailable" => problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "purchases_unavailable",
            "Purchases are disabled for this environment.",
        ),
        _ => unavailable("connected external purchase", &error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_preparation_never_accepts_an_approval_or_client_price() {
        let request = json!({"request_id":Uuid::new_v4(),"api_id":"bazaar:fixture","input":{}});
        assert!(serde_json::from_value::<PrepareRequest>(request.clone()).is_ok());
        for key in [
            "max_total_atomic",
            "quote_ref",
            "approved",
            "provider_atomic",
            "execution_mode",
        ] {
            let mut invalid = request.clone();
            invalid[key] = json!("115");
            assert!(serde_json::from_value::<PrepareRequest>(invalid).is_err());
        }
    }

    #[test]
    fn approval_cannot_change_the_selected_source_or_input() {
        let request = json!({"quote_ref":Uuid::new_v4(),"max_total_atomic":"115"});
        assert!(serde_json::from_value::<Approval>(request.clone()).is_ok());
        for key in ["api_id", "input", "resource_url", "request_id"] {
            let mut invalid = request.clone();
            invalid[key] = json!("replacement");
            assert!(serde_json::from_value::<Approval>(invalid).is_err());
        }
        assert_eq!(
            purchase_failure(crate::store::StoreAccessError::TaskUnavailable.into()).status(),
            StatusCode::NOT_FOUND
        );
    }
}
