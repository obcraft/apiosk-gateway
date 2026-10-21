use super::{Company, SavedSource};
use crate::{auth::Principal, engine::Engine, state::Task, store::StoreAccessError};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use std::collections::BTreeSet;
use uuid::Uuid;

pub struct Run {
    pub request_id: Uuid,
    pub task_id: Option<Uuid>,
    pub snapshot: Option<Value>,
}
fn run(row: sqlx::postgres::PgRow) -> Run {
    Run {
        request_id: row.get("request_id"),
        task_id: row.get("task_id"),
        snapshot: row.get("snapshot"),
    }
}

pub async fn companies(pool: &PgPool, p: &Principal, id: Uuid) -> anyhow::Result<Vec<Company>> {
    let value: Value = sqlx::query_scalar("SELECT companies FROM gateway_v2.company_checks WHERE id=$1 AND owner_id=$2 AND workspace_id IS NOT DISTINCT FROM $3 AND (connection_id IS NOT DISTINCT FROM $4 OR $4 IS NULL)")
        .bind(id).bind(p.owner).bind(p.workspace).bind(p.connection).fetch_optional(pool).await?
        .ok_or(StoreAccessError::TaskUnavailable)?;
    Ok(serde_json::from_value(value)?)
}

pub async fn begin(
    engine: &Engine,
    p: &Principal,
    id: Uuid,
    occurrence: &str,
    input: Option<Vec<Company>>,
) -> anyhow::Result<(Vec<Company>, Run)> {
    engine.store.check_workspace(p).await?;
    let mut tx = engine.store.pool.begin().await?;
    // Serializes both retries and creation without holding a transaction over
    // planning/provider network calls. UUID strings are never interpolated SQL.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(id.to_string())
        .execute(&mut *tx)
        .await?;
    if let Some(ref list) = input {
        super::validate_companies(list)?;
        sqlx::query("INSERT INTO gateway_v2.company_checks(id,owner_id,connection_id,workspace_id,companies) VALUES($1,$2,$3,$4,$5) ON CONFLICT(id) DO NOTHING")
            .bind(id).bind(p.owner).bind(p.connection).bind(p.workspace).bind(json!(list)).execute(&mut *tx).await?;
    }
    let value: Value = sqlx::query_scalar("SELECT companies FROM gateway_v2.company_checks WHERE id=$1 AND owner_id=$2 AND workspace_id IS NOT DISTINCT FROM $3 AND connection_id IS NOT DISTINCT FROM $4")
        .bind(id).bind(p.owner).bind(p.workspace).bind(p.connection).fetch_optional(&mut *tx).await?.ok_or(StoreAccessError::TaskUnavailable)?;
    let companies: Vec<Company> = serde_json::from_value(value)?;
    anyhow::ensure!(
        input.as_ref().is_none_or(|i| *i == companies),
        "This check has different saved companies. Use a new check_id to change the customer list."
    );
    if let Some(row) = sqlx::query("SELECT request_id,task_id,snapshot FROM gateway_v2.company_check_runs WHERE check_id=$1 AND occurrence=$2")
        .bind(id).bind(occurrence).fetch_optional(&mut *tx).await? {
        tx.commit().await?;
        return Ok((companies,run(row)));
    }
    let open: Option<String> = sqlx::query_scalar("SELECT occurrence FROM gateway_v2.company_check_runs WHERE check_id=$1 AND completed_at IS NULL")
        .bind(id).fetch_optional(&mut *tx).await?;
    anyhow::ensure!(
        open.is_none(),
        "Collect the previous occurrence before starting another check."
    );
    let request_id = Uuid::new_v4();
    sqlx::query("INSERT INTO gateway_v2.company_check_runs(check_id,occurrence,request_id) VALUES($1,$2,$3)")
        .bind(id).bind(occurrence).bind(request_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((
        companies,
        Run {
            request_id,
            task_id: None,
            snapshot: None,
        },
    ))
}

pub async fn load(pool: &PgPool, id: Uuid, occurrence: &str) -> anyhow::Result<Run> {
    Ok(run(sqlx::query("SELECT request_id,task_id,snapshot FROM gateway_v2.company_check_runs WHERE check_id=$1 AND occurrence=$2")
        .bind(id).bind(occurrence).fetch_optional(pool).await?.ok_or(StoreAccessError::TaskUnavailable)?))
}

pub async fn link(pool: &PgPool, id: Uuid, occurrence: &str, task: Uuid) -> anyhow::Result<()> {
    let result = sqlx::query("UPDATE gateway_v2.company_check_runs SET task_id=$3 WHERE check_id=$1 AND occurrence=$2 AND (task_id IS NULL OR task_id=$3)")
        .bind(id).bind(occurrence).bind(task).execute(pool).await?;
    anyhow::ensure!(
        result.rows_affected() == 1,
        "The saved task changed. Recover this occurrence."
    );
    Ok(())
}

/// A terminal refusal before task creation still completes this occurrence.
/// Otherwise a saved unsupported request would block every following Monday.
pub async fn finish_without_task(
    pool: &PgPool,
    id: Uuid,
    occurrence: &str,
    companies: &[Company],
    response: &crate::protocol::Response,
) -> anyhow::Result<Value> {
    anyhow::ensure!(
        super::can_snapshot(response) && response.state.is_none(),
        "Only terminal taskless responses may close an occurrence."
    );
    let mut tx = pool.begin().await?;
    let row=sqlx::query("SELECT task_id,snapshot FROM gateway_v2.company_check_runs WHERE check_id=$1 AND occurrence=$2 FOR UPDATE").bind(id).bind(occurrence).fetch_one(&mut *tx).await?;
    if let Some(saved) = row.get::<Option<Value>, _>("snapshot") {
        return Ok(saved);
    }
    anyhow::ensure!(
        row.get::<Option<Uuid>, _>("task_id").is_none(),
        "A task was linked; recover it instead."
    );
    let previous:Option<Value>=sqlx::query_scalar("SELECT snapshot FROM gateway_v2.company_check_runs WHERE check_id=$1 AND completed_at IS NOT NULL ORDER BY completed_at DESC LIMIT 1").bind(id).fetch_optional(&mut *tx).await?;
    let reported: Vec<String> = sqlx::query_scalar(
        "SELECT signal_id FROM gateway_v2.company_check_reported_signals WHERE check_id=$1",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    let mut saved = super::snapshot(
        companies,
        &[],
        previous.as_ref(),
        &reported.into_iter().collect(),
        &chrono::Utc::now().to_rfc3339(),
        Value::Null,
        "failed",
    );
    saved["check_id"] = json!(id);
    saved["occurrence"] = json!(occurrence);
    saved["execution_status"] = serde_json::to_value(&response.status)?;
    saved["execution_errors"] = serde_json::to_value(&response.errors)?;
    sqlx::query("UPDATE gateway_v2.company_check_runs SET snapshot=$3,completed_at=now() WHERE check_id=$1 AND occurrence=$2").bind(id).bind(occurrence).bind(&saved).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(saved)
}

/// Called only after task authorization and a terminal engine status. Source
/// documents are retained before the ordinary 30-day task expiry can remove them.
pub async fn collect(
    engine: &Engine,
    p: &Principal,
    id: Uuid,
    occurrence: &str,
    companies: &[Company],
    task: &Task,
    response: &crate::protocol::Response,
) -> anyhow::Result<Value> {
    anyhow::ensure!(
        super::can_snapshot(response) && task.pending_dispatches.is_empty(),
        "Execution or settlement is still pending. Recover the existing task."
    );
    let task_id = response
        .state
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No saved task."))?
        .state_ref;
    let docs = engine
        .store
        .results(p.owner, task_id, &task.result_refs)
        .await?;
    let mut sources = Vec::new();
    for (result_ref, document) in docs {
        let retrieved: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
            "SELECT created_at FROM results WHERE id=$1 AND owner_id=$2 AND task_id=$3",
        )
        .bind(result_ref)
        .bind(p.owner)
        .bind(task_id)
        .fetch_one(&engine.store.pool)
        .await?;
        sources.push(SavedSource {
            result_ref,
            retrieved_at: retrieved.to_rfc3339(),
            kvk_number: super::source_company(task, &document, companies),
            document: crate::report_assets::public_document(&document),
        });
    }
    let mut tx = engine.store.pool.begin().await?;
    let existing: Option<Value> = sqlx::query_scalar("SELECT snapshot FROM gateway_v2.company_check_runs WHERE check_id=$1 AND occurrence=$2 FOR UPDATE")
        .bind(id).bind(occurrence).fetch_one(&mut *tx).await?;
    if let Some(snapshot) = existing {
        return Ok(snapshot);
    }
    let previous: Option<Value> = sqlx::query_scalar("SELECT snapshot FROM gateway_v2.company_check_runs WHERE check_id=$1 AND completed_at IS NOT NULL ORDER BY completed_at DESC LIMIT 1")
        .bind(id).fetch_optional(&mut *tx).await?;
    let reported: Vec<String> = sqlx::query_scalar(
        "SELECT signal_id FROM gateway_v2.company_check_reported_signals WHERE check_id=$1",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    let status = serde_json::to_value(&response.status)?;
    let mut snapshot = super::snapshot(
        companies,
        &sources,
        previous.as_ref(),
        &reported.into_iter().collect::<BTreeSet<_>>(),
        &chrono::Utc::now().to_rfc3339(),
        response.billing.clone().unwrap_or(Value::Null),
        status.as_str().unwrap_or("unknown"),
    );
    snapshot["check_id"] = json!(id);
    snapshot["occurrence"] = json!(occurrence);
    snapshot["task_ref"] = json!(task_id);
    sqlx::query("UPDATE gateway_v2.company_check_runs SET snapshot=$3,completed_at=now() WHERE check_id=$1 AND occurrence=$2")
        .bind(id).bind(occurrence).bind(&snapshot).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(snapshot)
}

/// Full documents remain durable in the DB. Hosts receive exact cited values
/// and bounded source metadata, not megabytes of unrelated source records.
pub fn public_snapshot(mut value: Value) -> Value {
    value.as_object_mut().map(|o| o.remove("observations"));
    if let Some(sources) = value["sources"].as_array_mut() {
        for s in sources {
            *s = json!({"result_ref":s["result_ref"],"retrieved_at":s["retrieved_at"],"kvk_number":s["kvk_number"],"historical":s["historical"]==true,"source":s["document"]["source"],"charged_atomic":s["document"]["charged_atomic"],"currency":s["document"]["currency"]});
        }
    }
    value
}

/// Report snapshots are immutable, acknowledgements are not. A recovered tool
/// response must not invite the host to notify already-delivered signals again.
pub async fn present(pool: &PgPool, id: Uuid, mut snapshot: Value) -> anyhow::Result<Value> {
    let reported: BTreeSet<String> = sqlx::query_scalar(
        "SELECT signal_id FROM gateway_v2.company_check_reported_signals WHERE check_id=$1",
    )
    .bind(id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect();
    if let Some(signals) = snapshot["signals"].as_array_mut() {
        for signal in signals.iter_mut() {
            if signal["signal_id"]
                .as_str()
                .is_some_and(|id| reported.contains(id))
            {
                signal["previously_reported"] = json!(true);
                signal["change"] = json!("already_reported");
            }
        }
        let fresh: Vec<_> = signals
            .iter()
            .filter(|s| s["previously_reported"] != true)
            .cloned()
            .collect();
        snapshot["new_signal_count"] = json!(fresh.len());
        snapshot["notification_required"] = json!(!fresh.is_empty());
        snapshot["new_signals"] = json!(fresh);
    }
    Ok(public_snapshot(snapshot))
}
