//! Read-only projection of the existing App ledger. Never a second balance.
use crate::store::Store;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

pub fn valid_capture(body: &Value) -> bool {
    let (Some(p), Some(f), Some(t)) = (
        body["provider_micro_usd"].as_i64(),
        body["fee_micro_usd"].as_i64(),
        body["charged_micro_usd"].as_i64(),
    ) else {
        return false;
    };
    if !(0..1_000_000_000_000).contains(&p) || f < 0 {
        return false;
    }
    // Standard calls carry 15%, workflow runs 20%; nothing else is a valid capture.
    [catalog_core::fees::BUYER_MARKUP_BPS, catalog_core::fees::WORKFLOW_MARKUP_BPS].iter().any(|bps| {
        let q = catalog_core::fees::quote_with_markup(p.into(), catalog_core::fees::ProviderClass::Federated, *bps);
        q.buyer_markup_atomic == i128::from(f) && q.buyer_total_atomic == i128::from(t)
    })
}

/// Only explicit refusals BEFORE reservation may finish without a ledger row.
pub fn refused_before_reservation(body: &Value) -> bool {
    body["ok"] == false
        && body.get("execution_id").is_none_or(Value::is_null)
        && matches!(
            body["stage"].as_str(),
            Some("validating" | "pricing" | "checking_balance")
        )
        && matches!(
            body["error"].as_str(),
            Some(
                "invalid_input"
                    | "unauthorized"
                    | "selection_not_found"
                    | "price_unverified"
                    | "insufficient_funds"
                    | "limit_exceeded"
                    | "rate_limited"
                    | "not_configured"
            )
        )
}
pub fn public_error(body: &Value) -> Value {
    let (code, message) = match body["error"].as_str() {
        Some("insufficient_funds") => (
            "insufficient_balance",
            "Your Apiosk balance is insufficient. Nothing was purchased.",
        ),
        Some("limit_exceeded") => (
            "spending_limit_exceeded",
            "Deze aankoop past niet binnen je ingestelde bestedingslimiet.",
        ),
        Some("price_unverified") => (
            "price_changed",
            "This price has expired. Request a new quote.",
        ),
        Some("quote_changed") => (
            "quote_changed",
            "A source or price in this request changed. This step was not purchased. Request a new quote; previously saved results remain available.",
        ),
        Some("rate_limited") => (
            "rate_limited",
            "Please wait before requesting another quote.",
        ),
        Some("rate_limited_refunded") => (
            "rate_limited_refunded",
            "The source temporarily limited this request (429). The full amount, including the fee, was refunded.",
        ),
        Some("kvk_http_404") => (
            "annual_account_unavailable",
            "KVK has no annual account available for this company through this source. Nothing was purchased.",
        ),
        Some(code) if code.starts_with("kvk_") => (
            "annual_account_unavailable",
            "KVK could not supply this annual account. Nothing was purchased. Please try again later.",
        ),
        Some("refunded") => (
            "refunded",
            "The payment ledger confirms that the amount, including the fee, was refunded.",
        ),
        Some("source_input_refunded") => (
            "source_input_refunded",
            "The source request was missing or contained invalid required input. The full amount, including the fee, was refunded.",
        ),
        Some("source_not_found_refunded") => (
            "source_not_found",
            "The source has no record for this input (404). The full amount, including the fee, was refunded.",
        ),
        Some("source_schema_refunded") => (
            "source_schema_refunded",
            "The source response could not be processed. The full amount, including the fee, was refunded.",
        ),
        Some("source_access_refunded") => (
            "source_access_blocked",
            "The source blocked Apiosk's request (403). The full amount, including the fee, was refunded. Apiosk must resolve access to this source; changing your question will not fix this error.",
        ),
        Some("policy_denied_refunded") => (
            "purchase_policy_denied",
            "An Apiosk purchase rule or limit blocked this request before the source was called. The full amount, including the fee, was refunded. The rule or limit must be resolved before retrying; changing the search query will not resolve it.",
        ),
        Some("policy_unavailable_refunded") => (
            "purchase_policy_unavailable",
            "Apiosk could not check the purchase rules, so the source was not called. The full amount, including the fee, was refunded. Please try again once the purchase rules are available.",
        ),
        _ => (
            "execution_refused",
            "Execution was refused. Check the payment status before continuing.",
        ),
    };
    json!({"code":code,"message":message})
}

/// Preserve a safe failure category when the ledger confirms a full refund.
pub fn refunded_failure(output: &Value) -> Value {
    let execution = &output["execution"];
    let error = if execution["paid"] == false {
        match (execution["status"].as_u64(), execution["body"]["error"].as_str()) {
            (Some(403), Some("policy_denied")) => "policy_denied_refunded",
            (_, Some("policy_unavailable")) => "policy_unavailable_refunded",
            (Some(403), _) => "source_access_refunded",
            (Some(429), _) => "rate_limited_refunded",
            (Some(404), _) => "source_not_found_refunded",
            (_, Some("invalid_source_query")) => "source_input_refunded",
            (_, Some("source_schema_mismatch")) => "source_schema_refunded",
            _ => "refunded",
        }
    } else { "refunded" };
    json!({"ok":false,"charged_micro_usd":0,"error":error,"message":"The payment ledger confirms this execution was refunded."})
}

/// Purchase restrictions and source access blocks need an operational change,
/// not a narrower query. Suppress the generic request-refinement follow-up.
pub fn purchase_policy_blocked(failures: &std::collections::BTreeMap<String, Value>) -> bool {
    failures.values().any(|failure| matches!(failure["code"].as_str(),
        Some("purchase_policy_denied" | "purchase_policy_unavailable" | "source_access_blocked")))
}

pub fn display_step_error(error: &Value) -> Value {
    if error["code"] == "dependency_failed" {
        let mut error = error.clone();
        error["message"] = json!("Not requested because a required earlier source step failed.");
        error
    } else { error.clone() }
}

#[cfg(test)]
mod refunded_error_tests {
    use super::*;
    #[test]
    fn html_source_access_block_is_safe_and_does_not_request_query_refinement() {
        let failure = refunded_failure(&json!({"execution":{"paid":false,"status":403,
            "body":"<html>Request blocked. Request ID: private-trace</html>"}}));
        let shown = public_error(&failure);
        assert_eq!(shown["code"], "source_access_blocked");
        assert_eq!(failure["charged_micro_usd"], 0);
        assert!(shown["message"].as_str().unwrap().contains("changing your question will not fix"));
        assert!(!shown.to_string().contains("private-trace"));
        assert!(purchase_policy_blocked(&std::collections::BTreeMap::from([("step".into(), shown)])));
    }
    #[test]
    fn refunded_policy_denial_keeps_the_reason_and_does_not_expose_internal_rules() {
        let output = json!({"execution":{"paid":false,"status":403,"body":{
            "error":"policy_denied","message":"200 purchases already today; private rule identifier"
        }}});
        let failure = refunded_failure(&output);
        assert_eq!(failure["charged_micro_usd"], 0);
        assert_eq!(failure["error"], "policy_denied_refunded");
        let shown = public_error(&failure);
        assert_eq!(shown["code"], "purchase_policy_denied");
        assert!(shown["message"].as_str().unwrap().contains("before the source was called"));
        assert!(!shown.to_string().contains("private rule"));
        assert!(purchase_policy_blocked(&std::collections::BTreeMap::from([("step".into(), shown)])));

        // An upstream 403 alone does not prove an Apiosk purchase restriction.
        assert_eq!(refunded_failure(&json!({"execution":{"paid":false,"status":403,"body":{"error":"forbidden"}}}))["error"], "source_access_refunded");
        assert_eq!(refunded_failure(&json!({"execution":{"paid":true,"status":403,"body":{"error":"policy_denied"}}}))["error"], "refunded");
        assert!(!purchase_policy_blocked(&std::collections::BTreeMap::from([("step".into(), public_error(&json!({"error":"source_input_refunded"})))])));
    }

    #[test]
    fn unavailable_purchase_rules_remain_a_separate_refunded_failure() {
        let failure = refunded_failure(&json!({"execution":{"paid":false,"status":503,"body":{"error":"policy_unavailable"}}}));
        let shown = public_error(&failure);
        assert_eq!(shown["code"], "purchase_policy_unavailable");
        assert!(purchase_policy_blocked(&std::collections::BTreeMap::from([("step".into(), shown)])));
    }
    #[test]
    fn refund_keeps_safe_input_failure_without_exposing_upstream_body() {
        let failure = refunded_failure(&json!({"execution":{"paid":false,"status":400,"body":{"error":"invalid_source_query","message":"Missing addresses","private":"not for display"}}}));
        assert_eq!(failure["error"], "source_input_refunded");
        assert!(public_error(&failure)["message"].as_str().unwrap().contains("required input"));
        assert!(!failure.to_string().contains("private"));
        assert_eq!(refunded_failure(&json!({"execution":{"paid":false,"status":429}}))["error"], "rate_limited_refunded");
        assert_eq!(public_error(&refunded_failure(&json!({"execution":{"paid":false,"status":404}})))["code"], "source_not_found");
        assert_eq!(refunded_failure(&json!(null))["error"], "refunded");
    }
    #[test]
    fn older_dependency_failures_do_not_claim_the_company_was_missing() {
        let shown=display_step_error(&json!({"code":"dependency_failed","message":"Not requested because the company could not be resolved."}));
        assert_eq!(shown["message"], "Not requested because a required earlier source step failed.");
    }
}
impl Store {
    pub async fn billing(
        &self,
        owner: Uuid,
        task: Uuid,
        quote: Option<Uuid>,
        cap: i64,
    ) -> anyhow::Result<Value> {
        let _db = crate::telemetry::db("billing");
        let rows=sqlx::query("SELECT e.id,e.status execution_status,e.reserved_atomic,e.response,c.id receipt_ref,c.status ledger_status,c.charged_micro_usd,c.actual_provider_micro_usd,c.provider_price_micro_usd,c.fee_micro_usd FROM executions e JOIN authorizations a ON a.id=e.authorization_id LEFT JOIN balance_charges c ON c.user_id=e.owner_id AND c.idempotency_key=e.id::text WHERE e.owner_id=$1 AND e.task_id=$2 AND a.quote_ref=$3 ORDER BY e.created_at,e.id")
            .bind(owner).bind(task).bind(quote).fetch_all(&self.pool).await?;
        // The existing balance projection already deducts held reservations.
        // Subtracting them again here would display an incorrect balance.
        let balance: Option<i64> = sqlx::query_scalar(
            "SELECT b.balance_micro_usd FROM user_balances b JOIN tasks t ON t.id=$2 AND t.owner_id=$1 WHERE b.account_id=coalesce((t.document->>'workspace_id')::uuid,$1)",
        )
        .bind(owner).bind(task)
        .fetch_optional(&self.pool)
        .await?;
        let authorized:Option<i64>=sqlx::query_scalar("SELECT cap_atomic FROM authorizations WHERE owner_id=$1 AND task_id=$2 AND quote_ref=$3 AND revoked_at IS NULL AND expires_at>now()").bind(owner).bind(task).bind(quote).fetch_optional(&self.pool).await?;
        let mut executions = vec![];
        let (mut total, mut provider, mut fee, mut held) = (0i64, 0i64, 0i64, 0i64);
        let (mut pending, mut refunded, mut reserved, mut charged, mut breakdown_known) =
            (false, false, false, false, true);
        for row in rows {
            let ledger = row.get::<Option<String>, _>("ledger_status");
            let status = match ledger.as_deref() {
                Some("charged") => "captured",
                Some("refunded") => "refunded",
                Some("reserved") => "reserved",
                _ if row.get::<String, _>("execution_status") == "failed" && ledger.is_none() => {
                    "released"
                }
                _ => "pending_reconciliation",
            };
            let amount = if status == "captured" {
                row.get::<Option<i64>, _>("charged_micro_usd").unwrap_or(0)
            } else {
                0
            };
            let p = row
                .get::<Option<i64>, _>("actual_provider_micro_usd")
                .or(row.get::<Option<i64>, _>("provider_price_micro_usd"));
            let f = row.get::<Option<i64>, _>("fee_micro_usd");
            if status == "captured" {
                total = total
                    .checked_add(amount)
                    .ok_or_else(|| anyhow::anyhow!("billing overflow"))?;
                if let (Some(p), Some(f)) = (p, f) {
                    if p >= 0 && f >= 0 && p.checked_add(f) == Some(amount) {
                        provider += p;
                        fee += f;
                    } else {
                        breakdown_known = false;
                    }
                } else {
                    breakdown_known = false;
                }
                charged = true;
            }
            pending |= status == "pending_reconciliation";
            reserved |= status == "reserved";
            refunded |= status == "refunded";
            let allocation = if matches!(status, "reserved" | "pending_reconciliation") {
                row.get::<Option<i64>, _>("charged_micro_usd")
                    .unwrap_or(row.get("reserved_atomic"))
            } else {
                0
            };
            held += allocation;
            executions.push(json!({"execution_id":row.get::<Uuid,_>("id"),"status":status,"receipt_ref":row.get::<Option<Uuid>,_>("receipt_ref"),"total_charged":amount.to_string(),"reserved":allocation.to_string(),"max_authorized":row.get::<i64,_>("reserved_atomic").to_string(),"provider_cost":if status=="captured"{p.map(|n|n.to_string())}else{None},"apiosk_fee":if status=="captured"{f.map(|n|n.to_string())}else{None}}));
        }
        let status = if pending {
            "pending_reconciliation"
        } else if reserved {
            "reserved"
        } else if charged {
            "captured"
        } else if refunded {
            "refunded"
        } else if executions.is_empty() {
            "not_started"
        } else {
            "released"
        };
        Ok(
            json!({"scope":"quote","quote_ref":quote,"status":status,"funding_source":"apiosk_balance","currency":"USD","unit":"micro","pricing_rule":"provider_cost_plus_15_v1","cost_basis":"existing_gateway_provider_tariff","apiosk_fee_bps":1500,"provider_cost":if breakdown_known{Some(provider.to_string())}else{None},"apiosk_fee":if breakdown_known{Some(fee.to_string())}else{None},"total_charged":total.to_string(),"reserved":held.to_string(),"quote_cap":cap.to_string(),"authorization_active":authorized.is_some(),"max_authorized":authorized.unwrap_or(0).to_string(),"balance_available":balance.map(|n|n.to_string()),"executions":executions,"external_settlement":"managed_by_existing_gateway"}),
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_proven_pre_reservation_refusals_are_terminal() {
        let mut v = json!({"ok":false,"error":"insufficient_funds","stage":"checking_balance"});
        assert!(refused_before_reservation(&v));
        v["execution_id"] = json!(Uuid::new_v4());
        assert!(!refused_before_reservation(&v));
        assert!(!refused_before_reservation(
            &json!({"ok":false,"error":"timeout","stage":"executing"})
        ));
        assert!(!refused_before_reservation(
            &json!({"ok":false,"error":"provider_error","stage":"paying"})
        ));
    }
    #[test]
    fn capture_uses_the_existing_fifteen_percent_rule() {
        assert!(valid_capture(
            &json!({"provider_micro_usd":200000,"fee_micro_usd":30000,"charged_micro_usd":230000})
        ));
        assert!(valid_capture(
            &json!({"provider_micro_usd":3,"fee_micro_usd":1,"charged_micro_usd":4})
        ));
        assert!(!valid_capture(
            &json!({"provider_micro_usd":200000,"fee_micro_usd":34500,"charged_micro_usd":234500})
        ));
        assert!(!valid_capture(&json!({"charged_micro_usd":230000})));
    }
}
