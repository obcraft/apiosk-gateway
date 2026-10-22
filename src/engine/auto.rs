//! The one place a standing automatic-approval rule is applied: right after a
//! priced plan is returned to a connected chatbot. See `crate::auto_approval`.
use super::Engine;
use crate::{
    auth::Principal,
    execution::AutoApproval,
    protocol::{Response, Status},
};
use std::sync::Arc;

/// Whether a response is a complete priced plan that is waiting for approval
/// on a connection whose rule is live: the only state a rule may act on. The
/// view already read the rule (`context_view.auto_approval`), so a connection
/// without one costs no further query here.
pub(crate) fn waiting_for_auto_approval(p: &Principal, r: &Response) -> bool {
    p.connection.is_some()
        && r.state.is_some()
        && r.proposal.is_some()
        && r.errors.is_empty()
        && r.context_view["manual_approval_required"] != true
        && !r.context_view["connected_coverage"]["gaps"].as_array().is_some_and(|gaps| !gaps.is_empty())
        && matches!(r.status, Status::Ready | Status::RequiresApproval)
        && r.next_actions.iter().any(|action| action.kind == "execute_quoted_step")
        && !r.billing.as_ref().is_some_and(|billing| billing["authorization_active"] == true)
        && r.context_view["auto_approval"]["active"] == true
}

impl Engine {
    /// Continue a freshly priced plan without a click when the connection's
    /// rule covers it: approve under the mandate (`connection_auto`), start
    /// the server worker, and answer with the running task. Anything that does
    /// not qualify — or any failure here — returns the response unchanged, so
    /// the plan waits for Approve exactly as it always has.
    pub async fn continue_automatically(self: &Arc<Self>, p: &Principal, bearer: &str, response: Response) -> Response {
        if !waiting_for_auto_approval(p, &response) {
            return response;
        }
        let Some(id) = response.state.as_ref().map(|state| state.state_ref) else {
            return response;
        };
        match self.executor.auto_approve_connection(p, id).await {
            Ok(AutoApproval::Approved(_) | AutoApproval::AlreadyAuthorized(_)) => {
                if let Err(error) = self.start_worker(p.clone(), bearer.to_owned(), id).await {
                    // The approval is saved; status or the card resumes it.
                    tracing::warn!(task = %id, error = %error, "automatically approved task could not start");
                }
                match self.view(p, id, response.request_id, None).await {
                    Ok(view) => view,
                    Err(error) => {
                        tracing::warn!(task = %id, error = %error, "automatically approved task view unavailable");
                        response
                    }
                }
            }
            Ok(AutoApproval::Declined(denial)) => {
                tracing::info!(task = %id, reason = denial.code(), "automatic approval not applied");
                response
            }
            Err(error) => {
                tracing::warn!(task = %id, error = %error, "automatic approval unavailable");
                response
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{NextAction, Proposal};
    use serde_json::json;
    use uuid::Uuid;

    fn connection() -> Principal {
        Principal { owner: Uuid::new_v4(), connection: Some(Uuid::new_v4()), workspace: None }
    }

    fn priced() -> Response {
        let mut r = Response::new(Uuid::new_v4(), Status::Ready);
        r.state = Some(crate::state::Envelope {
            schema_version: "1".into(), state_ref: Uuid::new_v4(), revision: 1,
            expires_at: chrono::Utc::now(), focus: Default::default(), state_token: String::new(),
        });
        r.proposal = Some(Proposal {
            label: "Data request".into(), quote_ref: Uuid::new_v4(), price_status: "capped".into(), currency: "USD".into(),
            max_total_atomic: "115000".into(), expires_at: chrono::Utc::now(), approval_url: String::new(), steps: vec![], step_details: vec![],
        });
        r.next_actions = vec![NextAction { action_id: Uuid::new_v4(), kind: "execute_quoted_step".into(), label: String::new(),
            requires_authorization: true, input_schema: json!({}) }];
        r.billing = Some(json!({"authorization_active": false}));
        r.context_view = json!({"auto_approval": {"active": true}});
        r
    }

    #[test]
    fn only_a_waiting_plan_on_a_connection_with_a_live_rule_qualifies() {
        assert!(waiting_for_auto_approval(&connection(), &priced()));
        let mut approval = priced();
        approval.status = Status::RequiresApproval;
        assert!(waiting_for_auto_approval(&connection(), &approval));
    }

    #[test]
    fn a_person_in_the_app_is_never_approved_for() {
        let person = Principal { connection: None, ..connection() };
        assert!(!waiting_for_auto_approval(&person, &priced()));
    }

    #[test]
    fn anything_else_behaves_exactly_as_before() {
        let cases: Vec<fn(&mut Response)> = vec![
            |r| r.context_view = json!({"auto_approval": {"active": false}}),
            |r| r.context_view = json!({}),
            |r| r.context_view["manual_approval_required"] = json!(true),
            |r| r.context_view["connected_coverage"] = json!({"gaps":[{"output":"ownership_structure"}]}),
            |r| r.billing = Some(json!({"authorization_active": true})),
            |r| r.status = Status::NeedsSelection,
            |r| r.status = Status::Running,
            |r| r.status = Status::Succeeded,
            |r| r.next_actions.clear(),
            |r| r.proposal = None,
            |r| r.state = None,
            |r| r.errors.push(json!({"code": "unsupported"})),
        ];
        for (index, change) in cases.iter().enumerate() {
            let mut r = priced();
            change(&mut r);
            assert!(!waiting_for_auto_approval(&connection(), &r), "case {index}");
        }
    }
}
