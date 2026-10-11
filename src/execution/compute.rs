use super::*;
use crate::compute;

impl Executor {
    /// Fixed destination and server-built input only. The reservation, charge,
    /// receipt and reconciliation all use the existing App payment ledger.
    pub async fn dispatch_compute(
        &self,
        p: &Principal,
        bearer: &str,
        task_id: Uuid,
        revision: i64,
        action: Uuid,
        authorization: Uuid,
        plan: &Plan,
        inputs: &Value,
    ) -> anyhow::Result<Dispatch> {
        anyhow::ensure!(
            self.enabled && p.is_person(),
            "Extended analysis execution unavailable"
        );
        anyhow::ensure!(
            plan.compute.as_ref().is_some_and(|q| q.valid()) && plan.fee_bps == 1500,
            "Invalid compute quote"
        );
        let model = plan
            .compute
            .as_ref()
            .and_then(compute::Quote::model)
            .unwrap();
        let hash = digest(&(task_id, action, model.resource, inputs));
        let mut tx = self.store.pool.begin().await?;
        let row = sqlx::query("SELECT revision,document FROM tasks WHERE id=$1 AND owner_id=$2 AND expires_at>now() FOR UPDATE")
            .bind(task_id).bind(p.owner).fetch_one(&mut *tx).await?;
        let saved: Task = serde_json::from_value(row.get("document"))?;
        anyhow::ensure!(
            saved.workspace_id == p.workspace,
            "task belongs to another workspace"
        );
        if let Some(row) = sqlx::query("SELECT payload_hash FROM executions WHERE id=$1 AND owner_id=$2 AND task_id=$3 AND action_id=$1")
            .bind(action).bind(p.owner).bind(task_id).fetch_optional(&mut *tx).await? {
            anyhow::ensure!(row.get::<String,_>("payload_hash") == hash, "compute idempotency conflict");
            tx.commit().await?;
            return Ok(match self.poll_compute(p, task_id, action).await? {
                Some(body) if body["ok"] == true => Dispatch::Done { execution_id:action, body },
                Some(body) => Dispatch::Failed { body },
                None => Dispatch::Pending { execution_id: action },
            });
        }
        anyhow::ensure!(
            compute::enabled_for(p.owner)
                && row.get::<i64, _>("revision") == revision
                && !saved.cancelled,
            "state_conflict"
        );
        let run = saved
            .compute
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("missing compute choice"))?;
        anyhow::ensure!(
            run.action_id == action
                && run.quote_ref == plan.quote_ref
                && run.model_id == model.id
                && run
                    .prepared
                    .as_ref()
                    .is_some_and(|r| r.payload == *inputs && r.model_id == model.id),
            "compute input changed"
        );
        let auth = sqlx::query("SELECT cap_atomic,plan_hash,quote_ref FROM authorizations WHERE id=$1 AND task_id=$2 AND owner_id=$3 AND revoked_at IS NULL AND expires_at>now() FOR UPDATE")
            .bind(authorization).bind(task_id).bind(p.owner).fetch_one(&mut *tx).await?;
        anyhow::ensure!(
            auth.get::<String, _>("plan_hash") == plan.hash
                && auth.get::<Uuid, _>("quote_ref") == plan.quote_ref
                && plan.expires_at > Utc::now(),
            "authorization does not match quote"
        );
        let allocated: i64 = sqlx::query_scalar("SELECT coalesce(sum(coalesce(actual_atomic,reserved_atomic)),0)::bigint FROM executions WHERE authorization_id=$1 AND status!='failed'")
            .bind(authorization).fetch_one(&mut *tx).await?;
        anyhow::ensure!(
            allocated
                .checked_add(model.buyer_cap())
                .is_some_and(|n| n <= auth.get::<i64, _>("cap_atomic")),
            "budget exhausted"
        );
        let selection = Uuid::new_v4();
        sqlx::query("INSERT INTO user_selected_apis(id,user_id,prompt,kind,source,name,resource_url,method,provider_price_micro_usd,charged_micro_usd,metadata) VALUES($1,$2,'Extended analysis of saved results','external','coinbase-x402-bazaar',$3,$4,'POST',$5,$6,$7)")
            .bind(selection).bind(p.owner).bind(model.name).bind(model.resource).bind(model.provider_cap).bind(model.buyer_cap())
            .bind(json!({"gateway_v2":true,"analysis_compute":true,"task_id":task_id,"quote_ref":plan.quote_ref,"inputs":inputs,"fields":[]})).execute(&mut *tx).await?;
        if let Some(workspace) = p.workspace {
            sqlx::query("SELECT public.workspace_bind_execution($1,$2,$3,$4,$5,$6,$7)")
                .bind(p.owner)
                .bind(p.connection)
                .bind(workspace)
                .bind(selection)
                .bind(task_id)
                .bind(model.buyer_cap())
                .bind(plan.expires_at)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("INSERT INTO executions(id,owner_id,task_id,action_id,authorization_id,payload_hash,selection_id,reserved_atomic,status) VALUES($1,$2,$3,$1,$4,$5,$6,$7,'dispatching')")
            .bind(action).bind(p.owner).bind(task_id).bind(authorization).bind(hash).bind(selection).bind(model.buyer_cap()).execute(&mut *tx).await?;
        tx.commit().await?;
        let result = async {
            let response = self.http.post(format!("{}/functions/v1/use-selection",self.supabase_url))
                .bearer_auth(bearer).timeout(Duration::from_secs(8))
                .json(&json!({"selection_id":selection,"inputs":inputs,"idempotency_key":action,"deterministic":true})).send().await?;
            bounded_json_sized(response, 4*1024*1024).await
        }.await;
        if let Ok((body, _)) = result {
            if crate::billing::refused_before_reservation(&body) {
                let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM balance_charges WHERE user_id=$1 AND idempotency_key=$2)")
                    .bind(p.owner).bind(action.to_string()).fetch_one(&self.store.pool).await?;
                if !exists {
                    sqlx::query("UPDATE executions SET status='failed',actual_atomic=0,response=$2,updated_at=now() WHERE id=$1")
                        .bind(action).bind(&body).execute(&self.store.pool).await?;
                    return Ok(Dispatch::Failed { body });
                }
            }
            if body["ok"] == true
                && crate::billing::valid_capture(&body)
                && body["charged_micro_usd"]
                    .as_i64()
                    .is_some_and(|n| (0..=model.buyer_cap()).contains(&n))
            {
                sqlx::query("UPDATE executions SET status='succeeded',response=$2,actual_atomic=$3,updated_at=now() WHERE id=$1")
                    .bind(action).bind(&body).bind(body["charged_micro_usd"].as_i64()).execute(&self.store.pool).await?;
                return Ok(Dispatch::Done {
                    execution_id: action,
                    body,
                });
            }
        }
        // Unknown outcome keeps its allocation. Poll the journal, never POST again.
        sqlx::query("UPDATE executions SET status='unresolved',updated_at=now() WHERE id=$1")
            .bind(action)
            .execute(&self.store.pool)
            .await?;
        Ok(Dispatch::Pending {
            execution_id: action,
        })
    }
}

impl Executor {
    /// A settled provider error can still cost money. Preserve its actual
    /// charge while finishing the analysis as unavailable, instead of waiting
    /// forever or treating a paid failure as a refund.
    pub async fn poll_compute(
        &self,
        p: &Principal,
        task: Uuid,
        id: Uuid,
    ) -> anyhow::Result<Option<Value>> {
        if let Some(body) = self.poll(p, task, id).await? {
            return Ok(Some(body));
        }
        let row=sqlx::query("SELECT c.charged_micro_usd,coalesce(c.actual_provider_micro_usd,c.provider_price_micro_usd) provider_micro_usd,c.fee_micro_usd,e.reserved_atomic FROM executions e JOIN balance_charges c ON c.user_id=e.owner_id AND c.idempotency_key=e.id::text WHERE e.id=$1 AND e.owner_id=$2 AND e.task_id=$3 AND c.status='charged' AND EXISTS(SELECT 1 FROM call_runs r WHERE r.charge_id=c.id AND r.user_id=c.user_id AND r.status='failed')")
            .bind(id).bind(p.owner).bind(task).fetch_optional(&self.store.pool).await?;
        if let Some(row) = row {
            let actual: i64 = row.get("charged_micro_usd");
            let body = json!({"ok":false,"error":"compute_provider_failed","charged_micro_usd":actual,"provider_micro_usd":row.get::<Option<i64>,_>("provider_micro_usd"),"fee_micro_usd":row.get::<Option<i64>,_>("fee_micro_usd"),"message":"The provider settled payment but did not return a usable answer."});
            anyhow::ensure!(
                (0..=row.get::<i64, _>("reserved_atomic")).contains(&actual)
                    && crate::billing::valid_capture(&body),
                "Compute charge requires reconciliation"
            );
            sqlx::query("UPDATE executions SET status='succeeded',response=$2,actual_atomic=$3,updated_at=now() WHERE id=$1")
                .bind(id).bind(&body).bind(actual).execute(&self.store.pool).await?;
            return Ok(Some(body));
        }
        Ok(None)
    }
}
