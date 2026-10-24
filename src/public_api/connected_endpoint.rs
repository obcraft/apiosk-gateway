//! The Ask page's explicit per-source quote, with durable manual approval for
//! connected callers. This adapter never interprets a question or executes it.
use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepare {
    request_id: Uuid,
    endpoint_id: Uuid,
    question: Option<String>,
    capability: Option<String>,
    #[serde(default)]
    input: BTreeMap<String, Value>,
}

impl Prepare {
    fn valid(&self) -> bool {
        !self.request_id.is_nil() && !self.endpoint_id.is_nil()
            && self.question.as_deref().is_none_or(|q| !q.trim().is_empty() && q.len() <= 8_000)
            && self.capability.as_deref().is_none_or(|value| !value.is_empty() && value.len() <= 200
                && value.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_')))
            && self.input.len() <= 64
            && self.input.keys().all(|field| !field.is_empty() && field.len() <= 200)
    }
}

pub async fn prepare(State(state): State<ServerState>, headers: HeaderMap, body: Bytes) -> HttpResponse {
    let result = async {
        let ctx = context(&state, &headers, &["ask:create"]).await?;
        if ctx.principal.connection.is_none() {
            return Err(problem(StatusCode::FORBIDDEN, "connection_required", "Use a connected account to prepare selected sources."));
        }
        let req: Prepare = parse(&body)?;
        if !req.valid() {
            return Err(problem(StatusCode::BAD_REQUEST, "invalid_endpoint_input", "Choose an endpoint and review its input fields. Nothing was charged."));
        }
        let Ok(permit) = state.permits.clone().try_acquire_owned() else { return Err(busy()); };
        let engine = state.engine.clone();
        // Keep the quote and its manual policy together if the client leaves.
        // No bearer reaches this work, and no automatic continuation is called.
        let response = crate::telemetry::spawn(async move {
            let _permit = permit;
            let person = &ctx.principal;
            engine.prepare_manual_endpoint_answer(person, req.request_id, req.endpoint_id,
                req.capability.as_deref(), &req.input, req.question.as_deref()).await
        }).await.map_err(|_| problem(StatusCode::SERVICE_UNAVAILABLE, "prepare_unavailable",
            "The quote could not be confirmed. Retry the same request to recover it."))?
            .map_err(|error| unavailable("manual source preparation", &error))?;
        Ok(Json(response).into_response())
    }.await;
    result.unwrap_or_else(|response| response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Value {
        json!({"request_id":Uuid::new_v4(),"endpoint_id":Uuid::new_v4(),
            "capability":"company.accounts","input":{"company.id":"30204462"},"question":"Show the registered company profile."})
    }

    #[test]
    fn source_prepare_keeps_app_input_bounds_and_rejects_execution_fields() {
        assert!(serde_json::from_value::<Prepare>(request()).unwrap().valid());
        for field in ["approved", "max_spend", "price", "execution_mode", "state", "selections"] {
            let mut value = request(); value[field] = json!(true);
            assert!(serde_json::from_value::<Prepare>(value).is_err(), "{field}");
        }
        for (field, value) in [
            ("request_id", json!(Uuid::nil())), ("endpoint_id", json!(Uuid::nil())),
            ("question", json!(" ")), ("question", json!("a".repeat(8001))),
            ("capability", json!("company.accounts/execute")), ("input", json!({"":1})),
        ] {
            let mut candidate = request(); candidate[field] = value;
            assert!(!serde_json::from_value::<Prepare>(candidate).unwrap().valid(), "{field}");
        }
    }
}
