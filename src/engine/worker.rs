use super::*;
use futures_util::{stream::FuturesUnordered, StreamExt};
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};
static WORKERS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();

impl Engine {
    pub async fn worker_active(&self, task: Uuid) -> anyhow::Result<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM execution_leases WHERE task_id=$1 AND expires_at>now())",
        )
        .bind(task)
        .fetch_one(&self.store.pool)
        .await?)
    }

    /// One server owns the continuation; the browser is only an observer.
    /// A crash releases the lease after 120s. Recovery reuses saved actions and
    /// the payment journal, and never stores the caller's bearer in the database.
    pub async fn start_worker(
        self: &Arc<Self>,
        p: Principal,
        bearer: String,
        id: Uuid,
    ) -> anyhow::Result<()> {
        let (_, task, connection) = self.store.load(p.owner, id).await?;
        anyhow::ensure!(
            connection == p.connection,
            "task belongs to another connection"
        );
        if task.cancelled || !task.pending_candidates.is_empty() || task.execution_error.is_some() {
            return Ok(());
        }
        let Some(plan) = task
            .proposal
            .as_ref()
            .map(|v| serde_json::from_value::<Plan>(v.clone()))
            .transpose()?
        else {
            return Ok(());
        };
        let authorized: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM authorizations WHERE task_id=$1 AND owner_id=$2 AND quote_ref=$3 AND plan_hash=$4 AND revoked_at IS NULL AND expires_at>now())")
            .bind(id).bind(p.owner).bind(plan.quote_ref).bind(&plan.hash).fetch_one(&self.store.pool).await?;
        if !authorized
            || !task.actions.iter().any(|a| {
                matches!(
                    a["kind"].as_str(),
                    Some("run" | "poll" | "wait" | "analyze")
                )
            })
        {
            return Ok(());
        }
        let permit = WORKERS
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(32)))
            .clone()
            .try_acquire_owned()?;
        let worker = Uuid::new_v4();
        let claimed = sqlx::query("INSERT INTO execution_leases(task_id,worker_id,expires_at) VALUES($1,$2,now()+interval '120 seconds') ON CONFLICT(task_id) DO UPDATE SET worker_id=EXCLUDED.worker_id,expires_at=EXCLUDED.expires_at WHERE execution_leases.expires_at<=now()")
            .bind(id).bind(worker).execute(&self.store.pool).await?.rows_affected();
        if claimed == 0 {
            return Ok(());
        }
        let engine = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let result: anyhow::Result<()> = {
                let renew = async {
                    let mut interval = tokio::time::interval(Duration::from_secs(15));
                    loop {
                        interval.tick().await;
                        let lease = sqlx::query("UPDATE execution_leases SET expires_at=now()+interval '120 seconds' WHERE task_id=$1 AND worker_id=$2 AND expires_at>now()")
                            .bind(id).bind(worker).execute(&engine.store.pool).await?;
                        anyhow::ensure!(lease.rows_affected() == 1, "execution lease lost");
                    }
                    #[allow(unreachable_code)]
                    Ok::<(), anyhow::Error>(())
                };
                tokio::select! {
                    result = engine.drive(&p, &bearer, id) => result,
                    result = renew => result,
                    _ = tokio::time::sleep(Duration::from_secs(900)) => Err(anyhow::anyhow!("execution time limit reached")),
                }
            };
            if let Err(error) = result {
                tracing::warn!(task=%id, error=%error, "server execution stopped; recover saved task");
            }
            let _ = sqlx::query("DELETE FROM execution_leases WHERE task_id=$1 AND worker_id=$2")
                .bind(id)
                .bind(worker)
                .execute(&engine.store.pool)
                .await;
        });
        Ok(())
    }

    async fn drive(self: &Arc<Self>, p: &Principal, bearer: &str, id: Uuid) -> anyhow::Result<()> {
        loop {
            let (revision, task, _) = self.store.load(p.owner, id).await?;
            if task.cancelled
                || !task.pending_candidates.is_empty()
                || task.execution_error.is_some()
            {
                return Ok(());
            }
            if !task.pending_dispatches.is_empty()
                || task.actions.iter().any(|a| a["kind"] == "run")
            {
                if !self.drive_wave(p, bearer, id, revision, task).await? {
                    return Ok(());
                }
                continue;
            }
            let action = task
                .actions
                .iter()
                .filter_map(|a| serde_json::from_value::<Action>(a.clone()).ok())
                .find(|a| {
                    matches!(
                        a,
                        Action::Run { .. }
                            | Action::Poll { .. }
                            | Action::Wait { .. }
                            | Action::Analyze { .. }
                    )
                });
            let Some(action) = action else {
                return Ok(());
            };
            if let Action::Wait { until, .. } = action {
                if until > Utc::now() {
                    tokio::time::sleep(Duration::from_millis(
                        (until - Utc::now()).num_milliseconds().clamp(50, 1000) as u64,
                    ))
                    .await;
                    continue;
                }
            }
            let polling = matches!(action, Action::Poll { .. });
            let req = ExecuteRequest {
                request_id: Uuid::new_v4(),
                action_id: action.id(),
                state: self.signer.issue(p.owner, id, revision, task.focus),
                quote_ref: task
                    .proposal
                    .as_ref()
                    .and_then(|v| v["quote_ref"].as_str())
                    .and_then(|s| s.parse().ok()),
                authorization_ref: None,
                idempotency_key: action.id(),
                input: Value::Null,
            };
            // Internal transitions do not consume the public request rate limit.
            let response = self.execute_inner(p, bearer, &req).await?;
            if matches!(
                response.status,
                Status::RequiresApproval
                    | Status::StateConflict
                    | Status::Failed
                    | Status::Unsupported
            ) {
                return Ok(());
            }
            if polling {
                tokio::time::sleep(Duration::from_millis(750)).await;
            }
        }
    }

    async fn drive_wave(
        self: &Arc<Self>,
        p: &Principal,
        bearer: &str,
        id: Uuid,
        mut revision: i64,
        mut task: Task,
    ) -> anyhow::Result<bool> {
        let plan: Plan = serde_json::from_value(
            task.proposal
                .clone()
                .ok_or_else(|| anyhow::anyhow!("missing plan"))?,
        )?;
        if single_call_mode() && plan.steps.len() > 1 {
            return Ok(false);
        }
        let auth: Option<Uuid> = sqlx::query_scalar("SELECT id FROM authorizations WHERE task_id=$1 AND owner_id=$2 AND quote_ref=$3 AND revoked_at IS NULL AND expires_at>now()")
            .bind(id).bind(p.owner).bind(plan.quote_ref).fetch_optional(&self.store.pool).await?;
        let Some(auth) = auth else {
            return Ok(false);
        };
        let catalog = self.catalog.fresh().await?;
        let queued = enqueue_ready(&mut task, &plan, &catalog)?;
        if task.pending_dispatches.is_empty() {
            self.advance(&mut task)?;
            self.store.save(p.owner, id, revision, &task).await?;
            return Ok(false);
        }
        if !queued.is_empty() {
            revision = self.store.save(p.owner, id, revision, &task).await?;
        }
        let mut calls = FuturesUnordered::new();
        for pending in &task.pending_dispatches {
            calls.push(self.spawn_dispatch(
                p.clone(),
                bearer.to_owned(),
                id,
                revision,
                pending.clone(),
                auth,
                plan.clone(),
            ));
        }
        let mut failure = None;
        let mut waiting_until = None;
        while let Some(joined) = calls.next().await {
            let (pending, outcome) = joined?;
            let outcome = match outcome {
                Ok(outcome) => outcome,
                Err(error) => {
                    failure = Some(error);
                    continue;
                }
            };
            // Publish each completed source immediately, preserving choices or
            // cancellation saved while the other providers are still busy.
            let (current_revision, mut task, _) = self.store.load(p.owner, id).await?;
            anyhow::ensure!(
                task.proposal
                    .as_ref()
                    .is_some_and(|p| p["quote_ref"] == json!(plan.quote_ref)),
                "plan changed during execution"
            );
            if !task.pending_candidates.is_empty() {
                continue;
            }
            let step = &plan.steps[pending.step];
            match outcome {
                Dispatch::Done { execution_id, body } => {
                    self.ingest(p, id, &mut task, step, execution_id, body, &catalog)
                        .await?;
                    task.pending_dispatches
                        .retain(|d| d.action_id != pending.action_id);
                }
                Dispatch::Failed { body } => {
                    self.fail_step(&mut task, step, crate::billing::public_error(&body))?;
                    task.pending_dispatches
                        .retain(|d| d.action_id != pending.action_id);
                }
                Dispatch::Waiting { until } => {
                    waiting_until = Some(
                        waiting_until
                            .map_or(until, |prior: chrono::DateTime<Utc>| prior.min(until)),
                    );
                }
                Dispatch::Pending { .. } => {}
            }
            let added = if failure.is_none() {
                enqueue_ready(&mut task, &plan, &catalog)?
            } else {
                Vec::new()
            };
            if !task.pending_dispatches.is_empty()
                && task.pending_candidates.is_empty()
                && !task.cancelled
            {
                task.actions = vec![serde_json::to_value(Action::Wait {
                    action_id: Uuid::new_v4(),
                    until: waiting_until.unwrap_or(Utc::now() + chrono::Duration::seconds(1)),
                })?];
            }
            let revision = self
                .store
                .save_progress(p.owner, id, current_revision, &task)
                .await?;
            for pending in added {
                calls.push(self.spawn_dispatch(
                    p.clone(),
                    bearer.to_owned(),
                    id,
                    revision,
                    pending,
                    auth,
                    plan.clone(),
                ));
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        let (_, task, _) = self.store.load(p.owner, id).await?;
        if !task.pending_dispatches.is_empty() {
            tokio::time::sleep(Duration::from_millis(750)).await;
        }
        Ok(!task.cancelled && task.pending_candidates.is_empty())
    }
    #[allow(clippy::too_many_arguments)]
    fn spawn_dispatch(
        self: &Arc<Self>,
        p: Principal,
        bearer: String,
        id: Uuid,
        revision: i64,
        pending: crate::state::PendingDispatch,
        auth: Uuid,
        plan: Plan,
    ) -> tokio::task::JoinHandle<(crate::state::PendingDispatch, anyhow::Result<Dispatch>)> {
        let engine = self.clone();
        tokio::spawn(async move {
            engine
                .dispatch_pending(&p, &bearer, id, revision, pending, auth, &plan)
                .await
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn dispatch_pending(
        &self,
        p: &Principal,
        bearer: &str,
        id: Uuid,
        revision: i64,
        pending: crate::state::PendingDispatch,
        auth: Uuid,
        plan: &Plan,
    ) -> (crate::state::PendingDispatch, anyhow::Result<Dispatch>) {
        let outcome = async {
            let step = plan
                .steps
                .get(pending.step)
                .ok_or_else(|| anyhow::anyhow!("invalid pending step"))?;
            let mut outcome = self
                .executor
                .dispatch(
                    p,
                    bearer,
                    id,
                    revision,
                    pending.action_id,
                    pending.action_id,
                    auth,
                    plan,
                    step,
                    &pending.inputs,
                )
                .await?;
            if let Dispatch::Pending { execution_id } = outcome {
                if let Some(body) = self.executor.poll(p, id, execution_id).await? {
                    outcome = if body["ok"] == true {
                        Dispatch::Done { execution_id, body }
                    } else {
                        Dispatch::Failed { body }
                    };
                }
            }
            Ok(outcome)
        }
        .await;
        (pending, outcome)
    }
}

/// Refill free slots as soon as a dependency finishes. A slow independent
/// source cannot hold up a newly resolvable financial-statements request.
fn enqueue_ready(
    task: &mut Task,
    plan: &Plan,
    catalog: &Catalog,
) -> anyhow::Result<Vec<crate::state::PendingDispatch>> {
    let mut added = Vec::new();
    if task.cancelled || !task.pending_candidates.is_empty() {
        return Ok(added);
    }
    for (index, step) in plan.steps.iter().enumerate() {
        if task.pending_dispatches.len() >= 4 {
            break;
        }
        if task.step_finished(&step.key())
            || task.pending_dispatches.iter().any(|d| d.step == index)
        {
            continue;
        }
        let Ok(inputs) = step
            .service
            .inputs(task, step.entity_ref, &catalog.vocabulary)
        else {
            continue;
        };
        anyhow::ensure!(catalog.services.iter().any(|s|s.id==step.service.id&&s.version_hash()==step.service.version_hash()),"catalog or price changed; request a new quote");
        let existing = task
            .actions
            .iter()
            .find(|a| a["kind"] == "run" && a["step"] == json!(index))
            .and_then(|a| a["action_id"].as_str())
            .and_then(|s| s.parse().ok());
        let pending = crate::state::PendingDispatch {
            step: index,
            action_id: existing.unwrap_or_else(Uuid::new_v4),
            inputs,
        };
        task.pending_dispatches.push(pending.clone());
        added.push(pending);
    }
    Ok(added)
}
