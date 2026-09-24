//! Release-gate invariants on the payment boundary and on evidence. Every
//! release-gate scenario ends with all four, on every task it created:
//! one explicit approval per quote at the displayed cap, the approved cap
//! never exceeded, at most one settlement per execution, and every cited
//! value present in its saved source document.
//! Include with `#[path = "support/gate.rs"] mod gate;`.
#![allow(dead_code)]

use serde_json::Value;
use sqlx::{PgPool, Row};
use std::collections::BTreeMap;
use uuid::Uuid;

/// Exactly one authorization for the task, for the displayed quote and at the
/// displayed cap to the micro-USD, and every execution of the task under it.
pub async fn assert_one_approval(pool: &PgPool, task: Uuid, proposal: &Value) {
    let quote: Uuid = proposal["quote_ref"].as_str().and_then(|q| q.parse().ok()).expect("proposal quote_ref");
    let displayed: i64 = proposal["max_total_atomic"].as_str().and_then(|c| c.parse().ok()).expect("proposal max_total_atomic");
    let rows = sqlx::query("SELECT id,quote_ref,cap_atomic FROM authorizations WHERE task_id=$1")
        .bind(task).fetch_all(pool).await.unwrap();
    assert_eq!(rows.len(), 1, "task {task}: one approval, found {}", rows.len());
    let approval = &rows[0];
    assert_eq!(approval.get::<Uuid, _>("quote_ref"), quote, "task {task}: the approval names the displayed quote");
    assert_eq!(approval.get::<i64, _>("cap_atomic"), displayed, "task {task}: the approved cap is the displayed cap");
    let foreign: i64 = sqlx::query_scalar("SELECT count(*) FROM executions WHERE task_id=$1 AND authorization_id IS DISTINCT FROM $2")
        .bind(task).bind(approval.get::<Uuid, _>("id")).fetch_one(pool).await.unwrap();
    assert_eq!(foreign, 0, "task {task}: every execution belongs to the one approval");
}

/// No approval and nothing reserved, dispatched or charged for the task.
pub async fn assert_no_approval(pool: &PgPool, task: Uuid) {
    let approvals: i64 = sqlx::query_scalar("SELECT count(*) FROM authorizations WHERE task_id=$1").bind(task).fetch_one(pool).await.unwrap();
    let executions: i64 = sqlx::query_scalar("SELECT count(*) FROM executions WHERE task_id=$1").bind(task).fetch_one(pool).await.unwrap();
    assert_eq!((approvals, executions), (0, 0), "task {task}: no approval, no execution");
}

/// For every authorization of the task, what is reserved or spent and not
/// released stays within its cap, and the ledger never charges more.
pub async fn assert_cap(pool: &PgPool, task: Uuid) {
    let rows = sqlx::query("SELECT a.id,a.cap_atomic,
            coalesce(sum(coalesce(e.actual_atomic,e.reserved_atomic)) FILTER (WHERE e.status<>'failed'),0)::bigint allocated
        FROM authorizations a LEFT JOIN executions e ON e.authorization_id=a.id WHERE a.task_id=$1 GROUP BY a.id,a.cap_atomic")
        .bind(task).fetch_all(pool).await.unwrap();
    for row in rows {
        let (cap, allocated) = (row.get::<i64, _>("cap_atomic"), row.get::<i64, _>("allocated"));
        assert!(allocated <= cap, "task {task}: allocated {allocated} exceeds the approved cap {cap}");
        let charged: i64 = sqlx::query_scalar("SELECT coalesce(sum(c.charged_micro_usd),0)::bigint FROM executions e
                JOIN public.balance_charges c ON c.idempotency_key=e.id::text AND c.user_id=e.owner_id
            WHERE e.authorization_id=$1 AND c.status='charged'")
            .bind(row.get::<Uuid, _>("id")).fetch_one(pool).await.unwrap();
        assert!(charged <= cap, "task {task}: the ledger charged {charged} above the approved cap {cap}");
    }
    let unauthorized: i64 = sqlx::query_scalar("SELECT count(*) FROM executions WHERE task_id=$1 AND authorization_id IS NULL")
        .bind(task).fetch_one(pool).await.unwrap();
    assert_eq!(unauthorized, 0, "task {task}: no execution without an approval");
}

/// One execution per (owner, action), at most one ledger charge per
/// idempotency key, and a settled execution records exactly what the ledger
/// charged, within its reservation. A replayed dispatch adds no charge.
pub async fn assert_single_settlement(pool: &PgPool, task: Uuid) {
    let (executions, actions): (i64, i64) = sqlx::query_as("SELECT count(*),count(DISTINCT (owner_id,action_id)) FROM executions WHERE task_id=$1")
        .bind(task).fetch_one(pool).await.unwrap();
    assert_eq!(executions, actions, "task {task}: one execution per action");
    let rows = sqlx::query("SELECT e.id,e.status,e.reserved_atomic,e.actual_atomic,
            count(c.id) charges,
            coalesce(sum(c.charged_micro_usd) FILTER (WHERE c.status='charged'),0)::bigint charged
        FROM executions e LEFT JOIN public.balance_charges c ON c.idempotency_key=e.id::text AND c.user_id=e.owner_id
        WHERE e.task_id=$1 GROUP BY e.id,e.status,e.reserved_atomic,e.actual_atomic")
        .bind(task).fetch_all(pool).await.unwrap();
    for row in rows {
        let id = row.get::<Uuid, _>("id");
        let charges = row.get::<i64, _>("charges");
        assert!(charges <= 1, "task {task}: execution {id} has {charges} ledger charges");
        let charged = row.get::<i64, _>("charged");
        assert!(charged <= row.get::<i64, _>("reserved_atomic"), "task {task}: execution {id} charged above its reservation");
        match row.get::<String, _>("status").as_str() {
            "succeeded" => assert_eq!(row.get::<Option<i64>, _>("actual_atomic"), Some(charged), "task {task}: execution {id} records the ledger charge exactly"),
            "failed" => assert_eq!(charged, 0, "task {task}: failed execution {id} was charged"),
            _ => {}
        }
    }
}

/// The saved source documents of a task, by result id.
pub async fn saved_documents(pool: &PgPool, task: Uuid) -> BTreeMap<Uuid, Value> {
    sqlx::query("SELECT id,document FROM results WHERE task_id=$1").bind(task).fetch_all(pool).await.unwrap()
        .into_iter().map(|row| (row.get("id"), row.get("document"))).collect()
}

/// Every observation's evidence and every presentation cell that cites a
/// pointer resolves to a value in its saved document. Returns the number of
/// citations checked.
pub fn assert_evidence_complete(response: &Value, docs: &BTreeMap<Uuid, Value>) -> usize {
    let analysis = &response["context_view"]["analysis"];
    let mut cited: Vec<&Value> = analysis["observations"].as_array().into_iter().flatten()
        .flat_map(|observation| observation["evidence"].as_array().into_iter().flatten()).collect();
    cited.extend(analysis["presentation"]["rows"].as_array().into_iter().flatten()
        .filter_map(Value::as_array).flatten().filter(|cell| cell.get("pointer").is_some()));
    for citation in &cited {
        let result: Uuid = citation["result_ref"].as_str().and_then(|r| r.parse().ok())
            .unwrap_or_else(|| panic!("citation without a result: {citation}"));
        let document = docs.get(&result).unwrap_or_else(|| panic!("citation of an unsaved result: {citation}"));
        let pointer = citation["pointer"].as_str().unwrap_or_default();
        assert!(document.pointer(pointer).is_some_and(|value| !value.is_null()), "citation resolves to nothing: {citation}");
    }
    cited.len()
}

/// All four invariants for a task that was approved once.
pub async fn assert_all(pool: &PgPool, task: Uuid, proposal: &Value, response: &Value) -> usize {
    assert_one_approval(pool, task, proposal).await;
    assert_cap(pool, task).await;
    assert_single_settlement(pool, task).await;
    assert_evidence_complete(response, &saved_documents(pool, task).await)
}
