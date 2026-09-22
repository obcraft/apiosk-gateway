//! Safe, actionable approval diagnostics. Internal errors stay in server logs.
use crate::protocol::{Response, Status};
use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
#[error("approval budget exceeded: {kind}, required {required}, available {available}")]
pub(crate) struct BudgetRefusal {
    kind: &'static str,
    required: i64,
    available: i64,
}

pub(crate) fn check_budget(
    cap: i64,
    per_request: i64,
    daily: i64,
    balance: i64,
) -> anyhow::Result<()> {
    anyhow::ensure!(cap >= 0, "invalid approval amount");
    for (kind, available) in [
        ("per_request", per_request),
        ("daily", daily),
        ("balance", balance),
    ] {
        if cap > available {
            return Err(BudgetRefusal {
                kind,
                required: cap,
                available,
            }
            .into());
        }
    }
    Ok(())
}

pub(crate) fn response(error: &anyhow::Error) -> (StatusCode, Response) {
    if error
        .downcast_ref::<crate::store::StoreAccessError>()
        .is_some()
        || error
            .downcast_ref::<sqlx::Error>()
            .is_some_and(crate::store::is_workspace_access_denied)
    {
        return (StatusCode::FORBIDDEN, Response::error(Uuid::new_v4(), Status::Failed,
            "approval_refused", "You do not have spending access to this request's workspace. Open the correct workspace or ask its administrator for access."));
    }
    if let Some(refusal) = error.downcast_ref::<BudgetRefusal>() {
        let (code, message) = match refusal.kind {
            "per_request" => ("approval_per_request_limit", "This request is above your connection's per-request spending limit. Increase it in Apiosk > Connections, or ask a smaller question. Nothing was purchased by this approval attempt."),
            "daily" => ("approval_daily_limit", "This request is above your connection's remaining daily budget. Increase the daily limit in Apiosk > Connections, wait for earlier spending to leave the 24-hour window, or ask a smaller question. Nothing was purchased by this approval attempt."),
            _ => ("approval_balance_insufficient", "Your Apiosk balance is too low for this request. Add funds in Apiosk > Balance, or ask a smaller question. Nothing was purchased by this approval attempt."),
        };
        let mut body = Response::error(Uuid::new_v4(), Status::Failed, code, message);
        body.errors[0]["details"] = json!({"required_atomic":refusal.required.to_string(), "available_atomic":refusal.available.max(0).to_string(), "currency":"USD"});
        return (StatusCode::FORBIDDEN, body);
    }
    // Only recognised guard failures are safe to describe as a refusal.
    // A database/transport failure may follow a saved approval: recover first.
    let reason = error.to_string();
    let message = match reason.as_str() {
        "quote changed or expired" => "This price has expired or changed. Use Check status, then Get new price and review the updated total before approving.",
        "state changed; check status before approving" | "state expired; recover task" | "invalid state version" | "invalid state signature" => "Your request needs to be refreshed. Use Check status to see the current plan before approving.",
        "set a positive per-request and daily limit in Connections, then reconnect if needed" => "Set a per-request limit and a daily limit in Apiosk > Connections before approving. Then return here and check status.",
        "purchases unavailable" => "Purchases are temporarily unavailable. Your request is saved; check status and try again later.",
        "workspace spending is not permitted" => "Your role cannot approve spending in this workspace. Ask a workspace administrator to update your access.",
        "connected account required" | "task belongs to another connection" | "task belongs to another workspace" => "This request belongs to a different connection or workspace. Open it using the account that created it.",
        "task cancelled" | "authorization revoked" => "This request or its approval was cancelled. It will not be restarted automatically. Ask again if you want a new request.",
        "no proposal" | "approval amount must match displayed cap" | "invalid approval amount" => "The displayed plan is no longer current. Use Check status and review the current price before approving.",
        _ => return (StatusCode::SERVICE_UNAVAILABLE, Response::error(Uuid::new_v4(), Status::Failed, "approval_unconfirmed", "We could not confirm your approval yet. Use Check status to recover your saved request before trying again. Do not submit a new purchase.")),
    };
    (
        StatusCode::FORBIDDEN,
        Response::error(Uuid::new_v4(), Status::Failed, "approval_refused", message),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_budget_block_has_a_safe_message_and_exact_amounts() {
        for (limits, code, action) in [
            ([25, 8, 1000], "approval_per_request_limit", "Connections"),
            ([100, 8, 1000], "approval_daily_limit", "24-hour"),
            ([100, 100, 8], "approval_balance_insufficient", "Balance"),
        ] {
            let error = check_budget(81, limits[0], limits[1], limits[2]).unwrap_err();
            let (status, body) = response(&error);
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_eq!(body.errors[0]["code"], code);
            assert_eq!(body.errors[0]["details"]["required_atomic"], "81");
            assert!(body.errors[0]["message"].as_str().unwrap().contains(action));
            assert!(body.errors[0]["message"]
                .as_str()
                .unwrap()
                .contains("Nothing was purchased by this approval attempt"));
        }
        assert!(check_budget(81, 81, 81, 81).is_ok());
        assert!(check_budget(-1, 100, 100, 100).is_err());
    }

    #[test]
    fn unknown_failures_recover_without_leaking_details_or_claiming_no_charge() {
        let (status, body) = response(&anyhow::anyhow!("database secret connection details"));
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body.errors[0]["code"], "approval_unconfirmed");
        assert!(!serde_json::to_string(&body).unwrap().contains("secret"));
        assert!(!body.errors[0]["message"]
            .as_str()
            .unwrap()
            .contains("Nothing was purchased"));
    }

    #[test]
    fn stale_price_has_a_specific_recovery_action() {
        let (_, body) = response(&anyhow::anyhow!("quote changed or expired"));
        assert!(body.errors[0]["message"]
            .as_str()
            .unwrap()
            .contains("Get new price"));
    }

    #[test]
    fn access_denial_is_not_reported_as_a_temporary_server_failure() {
        let error = crate::store::StoreAccessError::WorkspaceUnavailable.into();
        let (status, body) = response(&error);
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body.errors[0]["message"]
            .as_str()
            .unwrap()
            .contains("administrator"));
        let (status, _) = response(&anyhow::anyhow!("invalid state signature"));
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
}
