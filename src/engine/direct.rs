use super::*;
use crate::parser::{Goal, Subject};
use std::collections::BTreeMap;

impl Engine {
    /// A direct source call: the caller names the source, the operation and
    /// the inputs, so no model interprets a question. It is a one-goal intent
    /// pinned to that source, priced by the same planner and run by the same
    /// executor and ledger as an Ask — not a second execution path.
    pub async fn direct(
        &self,
        p: &Principal,
        request_id: Uuid,
        source: &str,
        operation: &str,
        input: &BTreeMap<String, Value>,
    ) -> anyhow::Result<Response> {
        if let Some(r) = self
            .request_claim(p, request_id, &digest(&("direct", p.connection, source, operation, input)))
            .await?
        {
            return Ok(r);
        }
        let label: String = input
            .values()
            .find_map(Value::as_str)
            .unwrap_or(source)
            .chars()
            .take(120)
            .collect();
        let intent = Intent {
            subjects: vec![Subject { entity_ref: None, label, values: input.clone() }],
            goals: vec![Goal {
                capability: operation.to_owned(),
                subject: 0,
                constraints: BTreeMap::from([("source".to_owned(), json!(source))]),
            }],
            answer_operations: vec![],
            unresolved_references: vec![],
        };
        let req = DiscoverRequest {
            request_id,
            question: format!("Run {operation} on {source}"),
            state: None,
            context_delta: ContextDelta::default(),
        };
        let result = self.intake(p, &req, false, Some((intent, None))).await;
        Ok(self.complete(p, request_id, result).await)
    }
}
