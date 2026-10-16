use crate::workflow_catalog;
use super::*;
use crate::workflows::{run_status, SUPPLIER_CHECK};
use sqlx::Row;
use std::sync::OnceLock;

const RUN_COLUMNS: &str = "id,workflow_slug,surface,status,task_id,quote_ref,document,input,question,result->'verdict' AS verdict,result->'verdict'->>'decision' AS decision,created_at,updated_at";

/// Registration follows the task's own state transitions rather than every
/// view build: a view only re-registers a run once per revision it has not
/// registered before. The cache is best-effort (per instance, evicted after
/// a day of task inactivity); a miss after eviction or on a fresh instance
/// just re-registers, which is a no-op UPDATE if nothing actually changed.
/// Never losing a run status therefore does not depend on the cache: it is
/// only there to skip the redundant query on repeat reads of an unchanged
/// revision (SSE polling, status refreshes).
fn registration_cache() -> &'static moka::future::Cache<(Uuid, Uuid), i64> {
    static CACHE: OnceLock<moka::future::Cache<(Uuid, Uuid), i64>> = OnceLock::new();
    CACHE.get_or_init(|| {
        moka::future::Cache::builder()
            .max_capacity(50_000)
            .time_to_idle(std::time::Duration::from_secs(86_400))
            .build()
    })
}

fn named_run_summary(task: &Task) -> Option<Value> {
    task.analysis.as_ref().map(|analysis| json!({
        "status": analysis["status"],
        "observations_count": analysis["observations"].as_array().map_or(0, Vec::len),
        "results_count": task.active_results.len(),
        "detail_location": "saved_task"
    }))
}

fn run_json(row: &sqlx::postgres::PgRow) -> Value {
    json!({"id":row.get::<Uuid,_>("id"),"workflow_slug":row.get::<String,_>("workflow_slug"),"surface":row.get::<String,_>("surface"),
        "status":row.get::<String,_>("status"),"task_id":row.get::<Option<Uuid>,_>("task_id"),"quote_ref":row.get::<Option<Uuid>,_>("quote_ref"),
        "document":row.get::<Option<Value>,_>("document"),"input":row.get::<Value,_>("input"),"question":row.get::<String,_>("question"),
        "verdict":row.get::<Option<Value>,_>("verdict"),"decision":row.get::<Option<String>,_>("decision"),
        "created_at":row.get::<chrono::DateTime<Utc>,_>("created_at"),"updated_at":row.get::<chrono::DateTime<Utc>,_>("updated_at")})
}

impl Engine {
    /// Collect conversational input before building the existing, code-owned recipe.
    pub(crate) async fn collect_workflow(&self, question: &str, task: &mut crate::state::Task, catalog: &crate::catalog::Catalog) -> anyhow::Result<Option<crate::parser::Intent>> {
        let workflow = task.workflow.as_mut().ok_or_else(|| anyhow::anyhow!("Missing workflow context"))?;
        workflow.collecting = true;
        let recipe = workflow_catalog::recipe(&workflow.slug).ok_or_else(|| anyhow::anyhow!("Unknown workflow"))?;
        let reply = self.parser.workflow_chat(question, task, &recipe).await?;
        let workflow = task.workflow.as_mut().unwrap();
        for (key, value) in reply.input {
            if value.is_null() || value.as_str() == Some("") { workflow.input.remove(&key); }
            else { workflow.input.insert(key, value); }
        }
        let prepared = workflow_catalog::prepare(&workflow.slug, &workflow.input, catalog);
        let mut message = reply.message;
        let mut intent = None;
        if reply.ready {
            match prepared {
                Ok((plan, context, _)) => {
                    task.workflow = Some(context);
                    intent = Some(plan);
                }
                Err(error) => {
                    // A missing or invalid value is a normal chat turn, not a failed request.
                    message = format!("Please check this detail so I can prepare the workflow: {error}");
                }
            }
        }
        task.record_turn(question, Some(message));
        task.intent = None;
        task.proposal = None;
        task.actions.clear();
        task.completed.clear();
        task.failed_steps.clear();
        task.skipped_steps.clear();
        task.active_results.clear();
        task.analysis = None;
        task.supplier_check = None;
        task.pending_candidates.clear();
        task.original_subject = None;
        task.execution_error = None;
        task.cancelled = false;
        task.focus.goal_refs.clear();
        Ok(intent)
    }

    pub async fn start_workflow(&self, p: &Principal, slug: &str, request: crate::workflow_catalog::StartRequest) -> anyhow::Result<Response> {
        if let Some(response) = crate::workflows::start_rejection(request.request_id) { return Ok(response); }
        if let Some(response)=self.request_claim(p,request.request_id,&digest(&("start-workflow",slug,&request)), "start-workflow").await? { return Ok(response); }
        let result=async {
            let catalog=self.catalog.get().await?;
            let (intent,context,question)=match crate::workflow_catalog::prepare(slug,&request.input,&catalog) {
                Ok(prepared)=>prepared,
                Err(error)=>return Ok(Response::error(request.request_id,Status::NeedsInput,"invalid_workflow_input",&error.to_string())),
            };
            if !matches!(crate::workflow_catalog::plan_preview(&catalog,&intent),Resolution::Plan(_)) {
                return Ok(Response::error(request.request_id,Status::Unsupported,"workflow_unavailable","These details cannot currently be completed with the available source operations."));
            }
            self.intake(p,&DiscoverRequest {request_id:request.request_id,question,state:None,context_delta:ContextDelta::default()},p.is_person(),Some((intent,Some(context)))).await
        }.await;
        Ok(self.complete(p,request.request_id,result).await)
    }

    /// Called from a view build, but only actually writes when this task
    /// revision has not been registered before: registration therefore
    /// tracks state transitions (a revision changes only on save), not
    /// every read that happens to observe the same, already-registered
    /// state. Failures never fail the view: a run row is a secondary index
    /// over the task, and the task itself remains the source of truth.
    pub(super) async fn record_named_workflow_run(&self, p: &Principal, task_id: Uuid, revision: i64, task: &Task, response: &Response) -> anyhow::Result<()> {
        let Some(workflow)=&task.workflow else {return Ok(())};
        let Some(quote_ref)=task.proposal.as_ref().and_then(|p|p["quote_ref"].as_str()).and_then(|q|q.parse::<Uuid>().ok()) else {return Ok(())};
        if registration_cache().get(&(task_id, quote_ref)).await == Some(revision) { return Ok(()); }
        let mut input=serde_json::to_value(&workflow.input)?;
        input["workflow_title"]=json!(workflow.title);
        let question=task.conversation.last().map(|t|t.question.as_str()).unwrap_or("");
        let status=if task.analysis.as_ref().is_some_and(|a|a["status"]=="unavailable") {"failed"} else if response.status==Status::Partial {"partial"} else {run_status(&response.status,None)};
        sqlx::query("INSERT INTO workflow_runs(id,owner_id,workspace_id,workflow_slug,surface,status,task_id,quote_ref,input,question,result) VALUES($1,$2,$3,$4,$11,$5,$6,$7,$8,$9,$10) ON CONFLICT(task_id,quote_ref) DO UPDATE SET status=EXCLUDED.status,result=EXCLUDED.result,updated_at=now() WHERE (workflow_runs.status,workflow_runs.result) IS DISTINCT FROM (EXCLUDED.status,EXCLUDED.result)")
            .bind(Uuid::new_v4()).bind(p.owner).bind(task.workspace_id).bind(&workflow.slug).bind(status).bind(task_id).bind(quote_ref).bind(input).bind(question).bind(named_run_summary(task)).bind(if p.is_person() {"workflows"} else {"connected_agent"}).execute(&self.store.pool).await?;
        registration_cache().insert((task_id, quote_ref), revision).await;
        Ok(())
    }
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
            let live = if run["workflow_slug"] != SUPPLIER_CHECK && task.context_view["analysis"]["status"] == "unavailable" { "failed" }
                else if run["workflow_slug"] != SUPPLIER_CHECK && task.status == Status::Partial { "partial" }
                else { run_status(&task.status, run["decision"].as_str()) };
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

#[cfg(test)]
mod summary_tests {
    use super::*;
    #[test]
    fn history_index_does_not_duplicate_large_saved_evidence() {
        let task = Task {
            analysis: Some(json!({"status":"completed","observations":[{"text":"large saved answer", "evidence":[{"value":"page ".repeat(100000)}]}]})),
            ..Task::default()
        };
        let summary = named_run_summary(&task).unwrap();
        assert_eq!(summary["observations_count"], 1);
        assert!(summary.to_string().len() < 1024);
        assert!(task.analysis.unwrap().to_string().len() > 262144);
    }
}
