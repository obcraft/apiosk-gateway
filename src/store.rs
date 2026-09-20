use crate::{protocol::Response, state::Task};
use serde_json::Value;
use sqlx::{PgPool, Row};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Clone)]
pub struct Store {
    pub pool: PgPool,
}
#[derive(Debug, thiserror::Error)]
pub enum StoreAccessError {
    #[error("task unavailable")]
    TaskUnavailable,
    #[error("workspace unavailable")]
    WorkspaceUnavailable,
}

pub fn is_workspace_access_denied(error: &sqlx::Error) -> bool {
    error.as_database_error().is_some_and(|database| {
        database.code().as_deref() == Some("42501") && database.message() == "forbidden"
    })
}

fn workspace_error(error: sqlx::Error) -> anyhow::Error {
    if is_workspace_access_denied(&error) {
        StoreAccessError::WorkspaceUnavailable.into()
    } else {
        error.into()
    }
}
pub enum Claim {
    New,
    Replay(Box<Response>),
    Busy,
    Conflict,
}
impl Store {
    pub async fn list_tasks(&self, owner: Uuid) -> anyhow::Result<Vec<Value>> { self.list_tasks_scoped(owner,None).await }
    pub async fn list_tasks_scoped(&self, owner: Uuid, workspace: Option<Uuid>) -> anyhow::Result<Vec<Value>> {
        // Summaries only, bounded and ordered through tasks_owner_recent.
        Ok(sqlx::query_scalar(
            "SELECT jsonb_build_object('id',id,'title',left(coalesce(nullif(document #>> '{conversation,0,question}',''),nullif(document #>> '{proposal,label}',''),'Untitled chat'),160),'updated_at',updated_at) FROM tasks WHERE owner_id=$1 AND (document->>'workspace_id')::uuid IS NOT DISTINCT FROM $2 AND expires_at>now() AND (jsonb_array_length(coalesce(document->'conversation','[]'::jsonb))>0 OR document->'intent' <> 'null'::jsonb) ORDER BY updated_at DESC,id DESC LIMIT 100",
        ).bind(owner).bind(workspace).fetch_all(&self.pool).await?)
    }
    pub async fn claim(&self, owner: Uuid, request: Uuid, hash: &str) -> anyhow::Result<Claim> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(owner.to_string())
            .execute(&mut *tx)
            .await?;
        if let Some(row)=sqlx::query("SELECT payload_hash,response FROM requests WHERE owner_id=$1 AND request_id=$2").bind(owner).bind(request).fetch_optional(&mut *tx).await? {
            if row.get::<String,_>("payload_hash")!=hash {return Ok(Claim::Conflict)}
            return Ok(match row.get::<Option<Value>,_>("response") {Some(v)=>Claim::Replay(Box::new(serde_json::from_value(v)?)),None=>Claim::Busy})
        }
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM requests WHERE owner_id=$1 AND created_at>now()-interval '1 minute'").bind(owner).fetch_one(&mut *tx).await?;
        anyhow::ensure!(count < 60, "request rate limit; wait one minute");
        sqlx::query(
            "INSERT INTO requests(owner_id,request_id,payload_hash) VALUES($1,$2,$3)",
        )
        .bind(owner)
        .bind(request)
        .bind(hash)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Claim::New)
    }
    pub async fn finish(&self, owner: Uuid, response: &Response) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE requests SET response=$3 WHERE owner_id=$1 AND request_id=$2",
        )
        .bind(owner)
        .bind(response.request_id)
        .bind(serde_json::to_value(response)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
    pub async fn create(&self, owner: Uuid, connection: Option<Uuid>) -> anyhow::Result<Uuid> {
        self.create_scoped(owner, connection, None).await
    }
    pub async fn create_scoped(&self, owner: Uuid, connection: Option<Uuid>, workspace: Option<Uuid>) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO tasks(id,owner_id,connection_id,document) VALUES($1,$2,$3,$4)",
        )
        .bind(id)
        .bind(owner)
        .bind(connection)
        .bind(serde_json::to_value(Task { workspace_id: workspace, ..Task::default() })?)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }
    pub async fn check_workspace(&self, p: &crate::auth::Principal) -> anyhow::Result<()> {
        if p.workspace.is_some() {
            let _: Value = sqlx::query_scalar("SELECT public.workspace_gateway_context($1,$2,$3)")
                .bind(p.owner).bind(p.connection).bind(p.workspace).fetch_one(&self.pool).await.map_err(workspace_error)?;
        }
        Ok(())
    }
    pub async fn check_spending(&self, p: &crate::auth::Principal) -> anyhow::Result<()> {
        if p.workspace.is_some() {
            let ctx: Value = sqlx::query_scalar("SELECT public.workspace_gateway_context($1,$2,$3)")
                .bind(p.owner).bind(p.connection).bind(p.workspace).fetch_one(&self.pool).await?;
            anyhow::ensure!(ctx["can_spend"] == true, "workspace spending is not permitted");
        }
        Ok(())
    }
    pub async fn load_scoped(&self, p: &crate::auth::Principal, id: Uuid) -> anyhow::Result<(i64, Task, Option<Uuid>)> {
        self.check_workspace(p).await?;
        let row = self.load(p.owner, id).await?;
        if row.1.workspace_id != p.workspace {
            return Err(StoreAccessError::TaskUnavailable.into());
        }
        Ok(row)
    }
    pub async fn load(&self, owner: Uuid, id: Uuid) -> anyhow::Result<(i64, Task, Option<Uuid>)> {
        let row=sqlx::query("SELECT revision,document,connection_id FROM tasks WHERE id=$1 AND owner_id=$2 AND expires_at>now()")
            .bind(id).bind(owner).fetch_optional(&self.pool).await?.ok_or(StoreAccessError::TaskUnavailable)?;
        Ok((
            row.get("revision"),
            serde_json::from_value(row.get("document"))?,
            row.get("connection_id"),
        ))
    }
    pub async fn save(
        &self,
        owner: Uuid,
        id: Uuid,
        revision: i64,
        task: &Task,
    ) -> anyhow::Result<i64> {
        anyhow::ensure!(task.bounded(), "task capacity reached; start a new task");
        let n=sqlx::query("UPDATE tasks SET document=$4,revision=revision+1,updated_at=now() WHERE id=$1 AND owner_id=$2 AND revision=$3 AND (document->>'workspace_id') IS NOT DISTINCT FROM ($4::jsonb->>'workspace_id') AND expires_at>now()")
            .bind(id).bind(owner).bind(revision).bind(serde_json::to_value(task)?).execute(&self.pool).await?.rows_affected();
        anyhow::ensure!(n == 1, "state_conflict");
        crate::server_events::task_changed(id);
        Ok(revision + 1)
    }
    /// Attach paid outcomes even when cancellation wins a race with a worker.
    /// Other state changes still conflict; cancellation never discards receipts.
    pub async fn save_progress(&self, owner: Uuid, id: Uuid, revision: i64, progress: &Task) -> anyhow::Result<i64> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query("SELECT revision,document FROM tasks WHERE id=$1 AND owner_id=$2 AND expires_at>now() FOR UPDATE")
            .bind(id).bind(owner).fetch_one(&mut *tx).await?;
        let current: i64 = row.get("revision");
        let mut saved: Task = serde_json::from_value(row.get("document"))?;
        anyhow::ensure!(saved.workspace_id == progress.workspace_id, "immutable workspace");
        if current == revision { saved = progress.clone(); }
        else {
            anyhow::ensure!(saved.cancelled && saved.proposal == progress.proposal, "state_conflict");
            for result in &progress.result_refs { if !saved.result_refs.contains(result) { saved.result_refs.push(*result); } }
            for result in &progress.active_results { if !saved.active_results.contains(result) { saved.active_results.push(*result); } }
            for key in &progress.completed { if !saved.completed.contains(key) { saved.completed.push(key.clone()); } }
            saved.failed_steps.extend(progress.failed_steps.clone());
            let plan: crate::resolver::Plan = serde_json::from_value(saved.proposal.clone().ok_or_else(||anyhow::anyhow!("missing plan"))?)?;
            let finished: Vec<_> = plan.steps.iter().enumerate().filter(|(_,step)|saved.step_finished(&step.key())).map(|(index,_)|index).collect();
            saved.pending_dispatches.retain(|d|!finished.contains(&d.step));
        }
        anyhow::ensure!(saved.bounded(), "task capacity reached");
        sqlx::query("UPDATE tasks SET document=$3,revision=revision+1,updated_at=now() WHERE id=$1 AND owner_id=$2")
            .bind(id).bind(owner).bind(serde_json::to_value(saved)?).execute(&mut *tx).await?;
        tx.commit().await?;
        crate::server_events::task_changed(id);
        Ok(current + 1)
    }
    /// A new intent may not replace a plan while a dispatch is in flight or
    /// its paid result is waiting to be attached to the task after a crash.
    pub async fn save_intent(
        &self,
        owner: Uuid,
        id: Uuid,
        revision: i64,
        task: &Task,
    ) -> anyhow::Result<i64> {
        anyhow::ensure!(task.bounded(), "task capacity reached");
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT id FROM tasks WHERE id=$1 AND owner_id=$2 FOR UPDATE")
            .bind(id)
            .bind(owner)
            .fetch_one(&mut *tx)
            .await?;
        let pending:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM executions e JOIN tasks t ON t.id=e.task_id WHERE e.task_id=$1 AND (e.status IN ('dispatching','unresolved') OR (e.status='succeeded' AND NOT EXISTS(SELECT 1 FROM results r WHERE r.execution_id=e.id AND (t.document->'result_refs') ? r.id::text))))").bind(id).fetch_one(&mut *tx).await?;
        anyhow::ensure!(
            !pending,
            "existing execution needs recovery before a new intent"
        );
        let n=sqlx::query("UPDATE tasks SET document=$4,revision=revision+1,updated_at=now() WHERE id=$1 AND owner_id=$2 AND revision=$3 AND (document->>'workspace_id') IS NOT DISTINCT FROM ($4::jsonb->>'workspace_id') AND expires_at>now()")
            .bind(id).bind(owner).bind(revision).bind(serde_json::to_value(task)?).execute(&mut *tx).await?.rows_affected();
        anyhow::ensure!(n == 1, "state_conflict");
        tx.commit().await?;
        crate::server_events::task_changed(id);
        Ok(revision + 1)
    }
    pub async fn result(&self, owner: Uuid, task: Uuid, id: Uuid) -> anyhow::Result<Value> {
        Ok(sqlx::query_scalar("SELECT r.document FROM results r JOIN tasks t ON t.id=r.task_id WHERE r.id=$1 AND r.owner_id=$2 AND r.task_id=$3 AND t.expires_at>now()")
            .bind(id).bind(owner).bind(task).fetch_one(&self.pool).await?)
    }
    /// One request-scoped snapshot, with the same ownership and expiry boundary
    /// as `result`. A missing reference fails instead of hiding a saved result.
    pub async fn results(&self, owner: Uuid, task: Uuid, ids: &[Uuid]) -> anyhow::Result<BTreeMap<Uuid, Value>> {
        if ids.is_empty() { return Ok(BTreeMap::new()); }
        let ids: Vec<_> = ids.iter().copied().collect::<BTreeSet<_>>().into_iter().collect();
        let rows: Vec<(Uuid, Value)> = sqlx::query_as("SELECT r.id,r.document FROM results r JOIN tasks t ON t.id=r.task_id WHERE r.id=ANY($1) AND r.owner_id=$2 AND r.task_id=$3 AND t.expires_at>now()")
            .bind(&ids).bind(owner).bind(task).fetch_all(&self.pool).await?;
        if rows.len() != ids.len() { return Err(sqlx::Error::RowNotFound.into()); }
        Ok(rows.into_iter().collect())
    }
    /// Compatibility lookup for pre-timeline tasks. Uses requests_owner_time
    /// and the task's creation bound; returns only presentation metadata.
    pub async fn original_result_turn(&self, owner: Uuid, task: Uuid, result: Uuid) -> anyhow::Result<Option<Value>> {
        Ok(sqlx::query_scalar("SELECT jsonb_build_object('conversation',r.response->'context_view'->'conversation','proposal',r.response->'proposal') FROM requests r WHERE r.owner_id=$1 AND r.created_at >= (SELECT created_at FROM tasks WHERE id=$2 AND owner_id=$1 AND expires_at>now()) AND r.response->'state'->>'state_ref'=$2::text AND (r.response->'context_view'->'result_refs') ? $3 ORDER BY r.created_at LIMIT 1")
            .bind(owner).bind(task).bind(result.to_string()).fetch_optional(&self.pool).await?)
    }
}
