use crate::{protocol::Response, state::Task};
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Clone)]
pub struct Store {
    pub pool: PgPool,
}
pub enum Claim {
    New,
    Replay(Box<Response>),
    Busy,
    Conflict,
}
impl Store {
    pub async fn claim(&self, owner: Uuid, request: Uuid, hash: &str) -> anyhow::Result<Claim> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(owner.to_string())
            .execute(&mut *tx)
            .await?;
        if let Some(row)=sqlx::query("SELECT payload_hash,response FROM gateway_v2.requests WHERE owner_id=$1 AND request_id=$2").bind(owner).bind(request).fetch_optional(&mut *tx).await? {
            if row.get::<String,_>("payload_hash")!=hash {return Ok(Claim::Conflict)}
            return Ok(match row.get::<Option<Value>,_>("response") {Some(v)=>Claim::Replay(Box::new(serde_json::from_value(v)?)),None=>Claim::Busy})
        }
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM gateway_v2.requests WHERE owner_id=$1 AND created_at>now()-interval '1 minute'").bind(owner).fetch_one(&mut *tx).await?;
        anyhow::ensure!(count < 60, "request rate limit; wait one minute");
        sqlx::query(
            "INSERT INTO gateway_v2.requests(owner_id,request_id,payload_hash) VALUES($1,$2,$3)",
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
            "UPDATE gateway_v2.requests SET response=$3 WHERE owner_id=$1 AND request_id=$2",
        )
        .bind(owner)
        .bind(response.request_id)
        .bind(serde_json::to_value(response)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
    pub async fn create(&self, owner: Uuid, connection: Option<Uuid>) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO gateway_v2.tasks(id,owner_id,connection_id,document) VALUES($1,$2,$3,$4)",
        )
        .bind(id)
        .bind(owner)
        .bind(connection)
        .bind(serde_json::to_value(Task::default())?)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }
    pub async fn load(&self, owner: Uuid, id: Uuid) -> anyhow::Result<(i64, Task, Option<Uuid>)> {
        let row=sqlx::query("SELECT revision,document,connection_id FROM gateway_v2.tasks WHERE id=$1 AND owner_id=$2 AND expires_at>now()")
            .bind(id).bind(owner).fetch_optional(&self.pool).await?.ok_or_else(||anyhow::anyhow!("task unavailable"))?;
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
        let n=sqlx::query("UPDATE gateway_v2.tasks SET document=$4,revision=revision+1,updated_at=now() WHERE id=$1 AND owner_id=$2 AND revision=$3 AND expires_at>now()")
            .bind(id).bind(owner).bind(revision).bind(serde_json::to_value(task)?).execute(&self.pool).await?.rows_affected();
        anyhow::ensure!(n == 1, "state_conflict");
        Ok(revision + 1)
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
        sqlx::query("SELECT id FROM gateway_v2.tasks WHERE id=$1 AND owner_id=$2 FOR UPDATE")
            .bind(id)
            .bind(owner)
            .fetch_one(&mut *tx)
            .await?;
        let pending:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM gateway_v2.executions e JOIN gateway_v2.tasks t ON t.id=e.task_id WHERE e.task_id=$1 AND (e.status IN ('dispatching','unresolved') OR (e.status='succeeded' AND NOT EXISTS(SELECT 1 FROM gateway_v2.results r WHERE r.execution_id=e.id AND (t.document->'result_refs') ? r.id::text))))").bind(id).fetch_one(&mut *tx).await?;
        anyhow::ensure!(
            !pending,
            "existing execution needs recovery before a new intent"
        );
        let n=sqlx::query("UPDATE gateway_v2.tasks SET document=$4,revision=revision+1,updated_at=now() WHERE id=$1 AND owner_id=$2 AND revision=$3 AND expires_at>now()")
            .bind(id).bind(owner).bind(revision).bind(serde_json::to_value(task)?).execute(&mut *tx).await?.rows_affected();
        anyhow::ensure!(n == 1, "state_conflict");
        tx.commit().await?;
        Ok(revision + 1)
    }
    pub async fn result(&self, owner: Uuid, task: Uuid, id: Uuid) -> anyhow::Result<Value> {
        Ok(sqlx::query_scalar("SELECT r.document FROM gateway_v2.results r JOIN gateway_v2.tasks t ON t.id=r.task_id WHERE r.id=$1 AND r.owner_id=$2 AND r.task_id=$3 AND t.expires_at>now()")
            .bind(id).bind(owner).bind(task).fetch_one(&self.pool).await?)
    }
}
