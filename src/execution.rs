use crate::{
    auth::Principal,
    parser::bounded_json,
    resolver::{Plan, Step},
    state::{digest, Task},
    store::Store,
};
use chrono::Utc;
use serde_json::{json, Value};
use sqlx::Row;
use std::time::Duration;
use uuid::Uuid;

pub struct Executor {
    pub store: Store,
    pub http: reqwest::Client,
    pub supabase_url: String,
    pub enabled: bool,
}
pub enum Dispatch {
    Waiting { until: chrono::DateTime<Utc> },
    Failed { body: Value },
    Done { execution_id: Uuid, body: Value },
    Pending { execution_id: Uuid },
}
impl Executor {
    pub async fn connection_can_approve(&self, p: &Principal) -> anyhow::Result<bool> {
        let Some(connection) = p.connection else {
            return Ok(false);
        };
        Ok(
            sqlx::query("SELECT 1 FROM gateway_v2.connection_budget($1,$2)")
                .bind(p.owner)
                .bind(connection)
                .fetch_optional(&self.store.pool)
                .await?
                .is_some(),
        )
    }

    /// The caller's existing connection mandate is the authority. A host's
    /// app-only button is not proof of human presence and must not widen it.
    pub async fn approve_connection(
        &self,
        p: &Principal,
        task_id: Uuid,
        revision: i64,
        quote: Uuid,
        cap: &str,
    ) -> anyhow::Result<Uuid> {
        anyhow::ensure!(self.enabled, "purchases unavailable");
        self.store.check_spending(p).await?;
        let connection = p
            .connection
            .ok_or_else(|| anyhow::anyhow!("connected account required"))?;
        let mut tx = self.store.pool.begin().await?;
        let row = sqlx::query("SELECT revision,connection_id,document FROM tasks WHERE id=$1 AND owner_id=$2 AND expires_at>now() FOR UPDATE")
            .bind(task_id).bind(p.owner).fetch_one(&mut *tx).await?;
        anyhow::ensure!(
            row.get::<Option<Uuid>, _>("connection_id") == Some(connection),
            "task belongs to another connection"
        );
        let task: Task = serde_json::from_value(row.get("document"))?;
        anyhow::ensure!(task.workspace_id == p.workspace, "task belongs to another workspace");
        anyhow::ensure!(!task.cancelled, "task cancelled");
        let plan: Plan = serde_json::from_value(
            task.proposal
                .ok_or_else(|| anyhow::anyhow!("no proposal"))?,
        )?;
        anyhow::ensure!(
            plan.quote_ref == quote && plan.expires_at > Utc::now(),
            "quote changed or expired"
        );
        anyhow::ensure!(
            cap == plan.cap_atomic.to_string(),
            "approval amount must match displayed cap"
        );
        let budget = sqlx::query("SELECT * FROM gateway_v2.connection_budget($1,$2)")
            .bind(p.owner).bind(connection).fetch_optional(&mut *tx).await?
            .ok_or_else(|| anyhow::anyhow!("set a positive per-request and daily limit in Connections, then reconnect if needed"))?;
        // A repeated click reads the same authorization, never renews a revoked
        // authorization or creates another reservation/payment identity.
        if let Some(existing) = sqlx::query(
            "SELECT id,revoked_at FROM authorizations WHERE task_id=$1 AND quote_ref=$2",
        )
        .bind(task_id)
        .bind(quote)
        .fetch_optional(&mut *tx)
        .await?
        {
            anyhow::ensure!(
                existing
                    .get::<Option<chrono::DateTime<Utc>>, _>("revoked_at")
                    .is_none(),
                "authorization revoked"
            );
            return Ok(existing.get("id"));
        }
        anyhow::ensure!(row.get::<i64, _>("revision") == revision, "state changed; check status before approving");
        anyhow::ensure!(
            plan.cap_atomic >= 0 && plan.cap_atomic <= budget.get::<i64, _>("per_call_atomic"),
            "plan exceeds this connection's per-request limit"
        );
        anyhow::ensure!(
            plan.cap_atomic <= budget.get::<i64, _>("daily_remaining_atomic"),
            "plan exceeds this connection's remaining daily limit"
        );
        anyhow::ensure!(
            plan.cap_atomic <= budget.get::<i64, _>("balance_atomic"),
            "insufficient Apiosk balance for this plan"
        );
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO authorizations(id,task_id,owner_id,quote_ref,plan_hash,cap_atomic,expires_at,approved_via,approved_connection_id) VALUES($1,$2,$3,$4,$5,$6,$7,'connection_budget',$8)")
            .bind(id).bind(task_id).bind(p.owner).bind(quote).bind(plan.hash).bind(plan.cap_atomic).bind(plan.expires_at).bind(connection).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(id)
    }

    pub async fn approve(
        &self,
        p: &Principal,
        task_id: Uuid,
        quote: Uuid,
        cap: &str,
    ) -> anyhow::Result<Uuid> {
        anyhow::ensure!(
            p.is_person(),
            "approval requires an authenticated person in the App"
        );
        self.store.check_spending(p).await?;
        let (_, task, _) = self.store.load_scoped(&p, task_id).await?;
        anyhow::ensure!(task.workspace_id == p.workspace, "task belongs to another workspace");
        anyhow::ensure!(!task.cancelled, "task cancelled");
        let plan: Plan = serde_json::from_value(
            task.proposal
                .ok_or_else(|| anyhow::anyhow!("no proposal"))?,
        )?;
        anyhow::ensure!(
            plan.quote_ref == quote && plan.expires_at > Utc::now(),
            "quote changed or expired"
        );
        anyhow::ensure!(
            cap == plan.cap_atomic.to_string(),
            "approval amount must match displayed cap"
        );
        let id:Uuid=sqlx::query_scalar("INSERT INTO authorizations(id,task_id,owner_id,quote_ref,plan_hash,cap_atomic,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(task_id,quote_ref) DO UPDATE SET quote_ref=EXCLUDED.quote_ref RETURNING id")
            .bind(Uuid::new_v4()).bind(task_id).bind(p.owner).bind(quote).bind(plan.hash).bind(plan.cap_atomic).bind(plan.expires_at).fetch_one(&self.store.pool).await?;
        Ok(id)
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn dispatch(
        &self,
        p: &Principal,
        bearer: &str,
        task_id: Uuid,
        revision: i64,
        action_id: Uuid,
        idempotency: Uuid,
        authorization: Uuid,
        plan: &Plan,
        step: &Step,
        inputs: &Value,
    ) -> anyhow::Result<Dispatch> {
        anyhow::ensure!(
            self.enabled,
            "paid execution disabled until deployment verification"
        );
        let payload_hash = digest(&(task_id, action_id, &step.service, step.entity_ref, inputs));
        let mut tx = self.store.pool.begin().await?;
        let task=sqlx::query("SELECT revision,document FROM tasks WHERE id=$1 AND owner_id=$2 AND expires_at>now() FOR UPDATE").bind(task_id).bind(p.owner).fetch_one(&mut *tx).await?;
        if let Some(row)=sqlx::query("SELECT id,payload_hash,status,response FROM executions WHERE owner_id=$1 AND (action_id=$2 OR id=$3)").bind(p.owner).bind(action_id).bind(idempotency).fetch_optional(&mut *tx).await? {
            anyhow::ensure!(row.get::<String,_>("payload_hash")==payload_hash,"idempotency conflict");
            let id=row.get("id");return Ok(match row.get::<String,_>("status").as_str(){"succeeded"=>Dispatch::Done{execution_id:id,body:row.get("response")},"failed"=>Dispatch::Failed{body:row.get::<Option<Value>,_>("response").unwrap_or(json!({"error":"refunded"}))},_=>Dispatch::Pending{execution_id:id}})
        }
        anyhow::ensure!(task.get::<Value, _>("document")["workspace_id"] == json!(p.workspace), "task belongs to another workspace");
        let queued = task.get::<Value, _>("document")["pending_dispatches"].as_array()
            .is_some_and(|calls| calls.iter().any(|call| call["action_id"] == json!(action_id) && call["inputs"] == *inputs));
        // A completed sibling can advance the revision while this bounded,
        // already-saved wave is reserving its other independent calls.
        anyhow::ensure!(task.get::<i64, _>("revision") == revision || queued, "state_conflict");
        anyhow::ensure!(
            !task.get::<Value, _>("document")["cancelled"]
                .as_bool()
                .unwrap_or(false),
            "task cancelled"
        );
        let concurrent:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM executions WHERE task_id=$1 AND status IN ('dispatching','unresolved') AND authorization_id<>$2)").bind(task_id).bind(authorization).fetch_one(&mut *tx).await?;
        anyhow::ensure!(!concurrent, "another approved execution is pending reconciliation");
        // Parallel reservations serialize on the task and authorization rows.
        // Each reservation is included in allocated before the next is admitted.
        let auth=sqlx::query("SELECT cap_atomic,plan_hash,quote_ref FROM authorizations WHERE id=$1 AND task_id=$2 AND owner_id=$3 AND revoked_at IS NULL AND expires_at>now() FOR UPDATE").bind(authorization).bind(task_id).bind(p.owner).fetch_one(&mut *tx).await?;
        anyhow::ensure!(
            auth.get::<String, _>("plan_hash") == plan.hash
                && auth.get::<Uuid, _>("quote_ref") == plan.quote_ref
                && plan.expires_at > Utc::now(),
            "authorization does not match quote"
        );
        let allocated:i64=sqlx::query_scalar("SELECT coalesce(sum(coalesce(actual_atomic,reserved_atomic)),0)::bigint FROM executions WHERE authorization_id=$1 AND status!='failed'").bind(authorization).fetch_one(&mut *tx).await?;
        anyhow::ensure!(
            allocated
                .checked_add(step.service.total())
                .is_some_and(|total| total <= auth.get::<i64, _>("cap_atomic")),
            "budget exhausted"
        );
        let selection = Uuid::new_v4();
        if step.service.api_slug == kvk_accounts::PROVIDER
            && step.service.path == kvk_accounts::ENDPOINT
        {
            anyhow::ensure!(step.service.method == "GET" && inputs.as_object().is_some_and(|m| m.len() == 1),
                "KVK annual accounts only supports a KVK-number lookup; year/version selectors are not upstream inputs");
            let number = inputs["kvkNummer"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing KVK number"))?;
            // Admission follows authorization and budget validation, but precedes
            // a buyer reservation. Queue waiters never hold a payment open.
            match kvk_accounts::lookup_on(&mut tx, number).await? {
                kvk_accounts::Lookup::Ready(_) => {}
                kvk_accounts::Lookup::Pending { until } => {
                    tx.commit().await?;
                    return Ok(Dispatch::Waiting { until });
                }
                kvk_accounts::Lookup::Failed { code } => {
                    tx.commit().await?;
                    return Ok(Dispatch::Failed {
                        body: json!({"ok":false,"error":code,"charged_micro_usd":0,
                        "message":"KVK could not supply this annual account. No payment was reserved."}),
                    });
                }
            }
        }
        // The existing App ledger will reserve/debit this selection. This table
        // records only an authorized pick and never modifies a user balance.
        sqlx::query("INSERT INTO user_selected_apis(id,user_id,connection_id,prompt,kind,source,name,capability,api_slug,candidate_id,method,provider_price_micro_usd,charged_micro_usd,metadata) VALUES($1,$2,$3,$4,'reviewed','apiosk',$5,$6,$7,$8,$9,$10,$11,$12)")
            .bind(selection).bind(p.owner).bind(p.connection).bind(format!("Gateway v2: {}",step.service.contract.capability_slug)).bind(&step.service.name).bind(&step.service.contract.capability_slug).bind(&step.service.api_slug).bind(step.service.endpoint_id).bind(&step.service.method).bind(step.service.list_atomic).bind(step.service.total())
            .bind(json!({"gateway_v2":true,"task_id":task_id,"action_id":action_id,"inputs":inputs,"fields":[]})).execute(&mut *tx).await?;
        if let Some(workspace) = p.workspace {
            sqlx::query("SELECT public.workspace_bind_execution($1,$2,$3,$4,$5,$6,$7)")
                .bind(p.owner).bind(p.connection).bind(workspace).bind(selection).bind(task_id)
                .bind(step.service.total()).bind(plan.expires_at).execute(&mut *tx).await?;
        }
        sqlx::query("INSERT INTO executions(id,owner_id,task_id,action_id,authorization_id,payload_hash,selection_id,reserved_atomic,status) VALUES($1,$2,$3,$4,$5,$6,$7,$8,'dispatching')")
            .bind(idempotency).bind(p.owner).bind(task_id).bind(action_id).bind(authorization).bind(payload_hash).bind(selection).bind(step.service.total()).execute(&mut *tx).await?;
        tx.commit().await?;
        let path = if p.connection.is_some() {
            "agent-gateway/v1/run"
        } else {
            "use-selection"
        };
        let result=async {
            let r=self.http.post(format!("{}/functions/v1/{path}",self.supabase_url)).bearer_auth(bearer).timeout(Duration::from_secs(8)).json(&json!({"selection_id":selection,"inputs":inputs,"idempotency_key":idempotency,"deterministic":true})).send().await?;
            bounded_json(r,4*1024*1024).await
        }.await;
        let body = match result {
            Ok(body) => body,
            Err(_) => {
                sqlx::query("UPDATE executions SET status='unresolved',updated_at=now() WHERE id=$1").bind(idempotency).execute(&self.store.pool).await?;
                return Ok(Dispatch::Pending {
                    execution_id: idempotency,
                });
            }
        };
        let actual = body.get("charged_micro_usd").and_then(Value::as_i64);
        if crate::billing::refused_before_reservation(&body) {
            let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM balance_charges WHERE user_id=$1 AND idempotency_key=$2)").bind(p.owner).bind(idempotency.to_string()).fetch_one(&self.store.pool).await?;
            if !exists {
                sqlx::query("UPDATE executions SET status='failed',actual_atomic=0,response=$2,updated_at=now() WHERE id=$1").bind(idempotency).bind(&body).execute(&self.store.pool).await?;
                return Ok(Dispatch::Failed { body });
            }
        }
        let succeeded = body["ok"] == true && crate::billing::valid_capture(&body);
        // Unknown failures retain allocation. Only the ledger can prove zero
        // spent. Poll/reconciliation reads that ledger; never redispatch here.
        let status = if succeeded { "succeeded" } else { "unresolved" };
        anyhow::ensure!(
            actual.is_none_or(|n| n >= 0 && n <= step.service.total()),
            "executor returned invalid cost; reconcile before continuing"
        );
        sqlx::query("UPDATE executions SET status=$2,response=$3,actual_atomic=$4,updated_at=now() WHERE id=$1").bind(idempotency).bind(status).bind(&body).bind(if succeeded{actual}else{None}).execute(&self.store.pool).await?;
        if succeeded {
            Ok(Dispatch::Done {
                execution_id: idempotency,
                body,
            })
        } else {
            Ok(Dispatch::Pending {
                execution_id: idempotency,
            })
        }
    }
    pub async fn poll(&self, p: &Principal, task: Uuid, id: Uuid) -> anyhow::Result<Option<Value>> {
        let row=sqlx::query("SELECT status,response FROM executions WHERE id=$1 AND owner_id=$2 AND task_id=$3").bind(id).bind(p.owner).bind(task).fetch_one(&self.store.pool).await?;
        if row.get::<String, _>("status") == "succeeded" {
            return Ok(row.get("response"));
        }
        // The existing ledger's UUID string idempotency identity is stable.
        let charge=sqlx::query("SELECT id,status,charged_micro_usd,coalesce(actual_provider_micro_usd,provider_price_micro_usd) provider_micro_usd,fee_micro_usd FROM balance_charges WHERE user_id=$1 AND idempotency_key=$2").bind(p.owner).bind(id.to_string()).fetch_optional(&self.store.pool).await?;
        if let Some(c) = charge {
            if c.get::<String, _>("status") == "charged" {
                let run=sqlx::query("SELECT output,output_truncated FROM call_runs WHERE charge_id=$1 AND user_id=$2 AND status='succeeded' ORDER BY started_at DESC LIMIT 1")
                    .bind(c.get::<Uuid,_>("id")).bind(p.owner).fetch_optional(&self.store.pool).await?;
                if let Some(run) = run {
                    let actual = c.get::<i64, _>("charged_micro_usd");
                    let reserved:i64=sqlx::query_scalar("SELECT reserved_atomic FROM executions WHERE id=$1 AND owner_id=$2").bind(id).bind(p.owner).fetch_one(&self.store.pool).await?;
                    anyhow::ensure!(
                        (0..=reserved).contains(&actual),
                        "ledger cost exceeds authorization; operator reconciliation required"
                    );
                    let body = json!({"ok":true,"execution_id":c.get::<Uuid,_>("id"),"result":run.get::<Option<Value>,_>("output"),"result_truncated":run.get::<bool,_>("output_truncated"),"charged_micro_usd":actual,"provider_micro_usd":c.get::<Option<i64>,_>("provider_micro_usd"),"fee_micro_usd":c.get::<Option<i64>,_>("fee_micro_usd"),"reconciled":true});
                    anyhow::ensure!(
                        crate::billing::valid_capture(&body),
                        "ledger price breakdown requires reconciliation"
                    );
                    sqlx::query("UPDATE executions SET status='succeeded',actual_atomic=$2,response=$3,updated_at=now() WHERE id=$1").bind(id).bind(actual).bind(&body).execute(&self.store.pool).await?;
                    return Ok(Some(body));
                }
            }
            if c.get::<String, _>("status") == "refunded" {
                let failed: Option<Value> = sqlx::query_scalar("SELECT output FROM call_runs WHERE charge_id=$1 AND user_id=$2 AND status='failed' ORDER BY started_at DESC LIMIT 1")
                    .bind(c.get::<Uuid,_>("id")).bind(p.owner).fetch_optional(&self.store.pool).await?.flatten();
                let body = crate::billing::refunded_failure(&failed.unwrap_or(Value::Null));
                sqlx::query("UPDATE executions SET status='failed',actual_atomic=0,response=$2,updated_at=now() WHERE id=$1").bind(id).bind(&body).execute(&self.store.pool).await?;
                return Ok(Some(body));
            }
        }
        Ok(None)
    }
}
