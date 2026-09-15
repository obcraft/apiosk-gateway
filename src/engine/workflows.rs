use super::*;
use crate::workflows::{run_status, SUPPLIER_CHECK};
use sqlx::Row;

const RUN_COLUMNS: &str = "id,workflow_slug,surface,status,task_id,quote_ref,document,input,question,result->'verdict' AS verdict,result->'verdict'->>'decision' AS decision,created_at,updated_at";

fn run_json(row: &sqlx::postgres::PgRow) -> Value {
    json!({"id":row.get::<Uuid,_>("id"),"workflow_slug":row.get::<String,_>("workflow_slug"),"surface":row.get::<String,_>("surface"),
        "status":row.get::<String,_>("status"),"task_id":row.get::<Option<Uuid>,_>("task_id"),"quote_ref":row.get::<Option<Uuid>,_>("quote_ref"),
        "document":row.get::<Option<Value>,_>("document"),"input":row.get::<Value,_>("input"),"question":row.get::<String,_>("question"),
        "verdict":row.get::<Option<Value>,_>("verdict"),"decision":row.get::<Option<String>,_>("decision"),
        "created_at":row.get::<chrono::DateTime<Utc>,_>("created_at"),"updated_at":row.get::<chrono::DateTime<Utc>,_>("updated_at")})
}

impl Engine {
    pub async fn workflow_runs(&self, p: &Principal, workflow: Option<&str>) -> anyhow::Result<Value> {
        let rows = sqlx::query(&format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE owner_id=$1 AND workspace_id IS NOT DISTINCT FROM $2 AND ($3::text IS NULL OR workflow_slug=$3) AND expires_at>now() ORDER BY created_at DESC LIMIT 50"))
            .bind(p.owner).bind(p.workspace).bind(workflow).fetch_all(&self.store.pool).await?;
        Ok(json!({"runs": rows.iter().map(run_json).collect::<Vec<_>>()}))
    }

    /// A saved run with its live task while the task is retained (30 days).
    pub async fn workflow_run(&self, p: &Principal, id: Uuid) -> anyhow::Result<Option<Value>> {
        let Some(row) = sqlx::query(&format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE id=$1 AND owner_id=$2 AND workspace_id IS NOT DISTINCT FROM $3 AND expires_at>now()"))
            .bind(id).bind(p.owner).bind(p.workspace).fetch_optional(&self.store.pool).await? else { return Ok(None) };
        let mut run = run_json(&row);
        let task = match run["task_id"].as_str().and_then(|s| s.parse::<Uuid>().ok()) {
            Some(task_id) => self.view(p, task_id, Uuid::new_v4(), None).await.ok(),
            None => None,
        };
        if let Some(task) = &task {
            let live = run_status(&task.status, run["decision"].as_str());
            let quote = task.proposal.as_ref().map(|q| q.quote_ref);
            if run["decision"].is_null() && (live != run["status"] || json!(quote) != run["quote_ref"]) {
                sqlx::query("UPDATE workflow_runs SET status=$2,quote_ref=coalesce($3,quote_ref),updated_at=now() WHERE id=$1").bind(id).bind(live).bind(quote).execute(&self.store.pool).await?;
                run["status"] = json!(live);
            }
        }
        Ok(Some(json!({"run":run,"task":task})))
    }

    /// Save a finished supplier check as a workflow run, whichever surface asked it.
    pub(super) async fn record_supplier_run(&self, p: &Principal, task_id: Uuid, task: &Task, quote_ref: Uuid, analysis: &Value) -> anyhow::Result<()> {
        let surface = if p.connection.is_some() { "connected_agent" } else { "ask" };
        let question = task.conversation.last().map(|t| t.question.clone()).unwrap_or_default();
        let input = task.supplier_check.clone().map(|v| crate::supplier_check::record(&serde_json::from_value(v).unwrap_or_default())).unwrap_or(json!({}));
        sqlx::query("INSERT INTO workflow_runs(id,owner_id,workspace_id,workflow_slug,surface,status,task_id,quote_ref,input,question,result) VALUES($1,$2,$3,$4,$5,'completed',$6,$7,$8,$9,$10) ON CONFLICT(task_id,quote_ref) DO UPDATE SET result=EXCLUDED.result,status='completed',updated_at=now()")
            .bind(Uuid::new_v4()).bind(p.owner).bind(task.workspace_id).bind(SUPPLIER_CHECK).bind(surface).bind(task_id).bind(quote_ref).bind(input).bind(question).bind(analysis)
            .execute(&self.store.pool).await?;
        Ok(())
    }
}
