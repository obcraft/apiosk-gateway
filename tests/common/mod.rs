//! Fixtures shared by the flow tests for the answer written after results land.
//!
//! Every finished question is answered from its own saved documents, so each
//! test's fake model must reply to that request as well as to planning. The
//! answer here is deliberately generic: it cites the first scalar field of each
//! supplied document, which is what the gateway validates evidence against.
#![allow(dead_code)]
use apiosk_gateway_v2::{
    auth::Principal,
    engine::Engine,
    protocol::{ExecuteRequest, Response},
};
use serde_json::{json, Value};
use uuid::Uuid;

/// Whether this model request asks for the answer rather than for a plan.
pub fn asks_for_answer(input: &Value) -> bool {
    input.get("answer_operations").is_some()
}

/// A valid answer over whatever documents the gateway supplied.
pub fn answer(input: &Value) -> Value {
    let evidence: Vec<Value> = input["sources"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|source| {
            let (key, _) = source["document"]["data"]
                .as_object()?
                .iter()
                .find(|(_, value)| value.is_string() || value.is_number() || value.is_boolean())?;
            Some(json!({"result_ref":source["result_ref"],"pointer":format!("/data/{key}")}))
        })
        .collect();
    let observations = if evidence.is_empty() {
        json!([])
    } else {
        json!([{"text":"The source answers the question with the fields cited here.","evidence":evidence}])
    };
    let limitations = if evidence.is_empty() {
        json!(["No source field supports an answer to this question."])
    } else {
        json!([])
    };
    json!({"choices":[{"message":{"content":json!({"observations":observations,"limitations":limitations}).to_string()}}]})
}

/// Run the free follow-up actions a finished request leaves behind — the answer
/// above all — so a test can assert the state the person finally sees.
pub async fn settle(engine: &Engine, p: &Principal, mut response: Response) -> Response {
    for _ in 0..4 {
        let Some(action) = response
            .next_actions
            .iter()
            .find(|a| a.kind == "poll")
            .cloned()
        else {
            break;
        };
        let state = response.state.clone().expect("a pollable task keeps its state");
        response = engine
            .execute(
                p,
                "fixture",
                ExecuteRequest {
                    request_id: Uuid::new_v4(),
                    action_id: action.action_id,
                    state,
                    quote_ref: None,
                    authorization_ref: None,
                    idempotency_key: action.action_id,
                    input: Value::Null,
                },
            )
            .await
            .expect("the saved task answers its own follow-up action");
    }
    response
}
