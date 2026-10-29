//! Job rows: creating one idempotently, claiming a node under a lease, and
//! writing down what happened (migrations 114 and 115).
//!
//! Two statements in here carry the durability guarantees, and both work by
//! putting the condition in the WHERE clause rather than in Rust:
//!
//! * [`Database::create_job`] leans on the unique index over
//!   `(owner, idempotency_key)`. A double click conflicts instead of starting a
//!   second reservation, and the existing job is returned.
//! * [`Database::claim_node`] updates a node only while its lease is absent or
//!   expired. Two workers issuing that update at the same instant produce one
//!   winner and one zero-row update, because Postgres serialises the row.
//!   Checking the lease in Rust and then updating would leave the gap that
//!   guarantee exists to close.

use super::*;

use crate::job::schedule::{NodeState, NodeStatus};

/// A job as the API and the worker see it.
#[derive(Debug, Clone, FromRow)]
pub struct JobRow {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub plan_version_id: Uuid,
    pub plan_hash: String,
    pub user_id: Option<Uuid>,
    pub connection_id: Option<Uuid>,
    pub status: String,
    pub pending_question: Option<serde_json::Value>,
    pub buyer_total_ceiling_atomic: i64,
    pub max_requests: i32,
    pub requests_made: i32,
    pub cancelled_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    /// What this job has ACTUALLY cost the buyer so far, in atomic USDC:
    /// the sum of `settled_atomic` over nodes whose payment is `settled`.
    ///
    /// The ceiling is what was approved; this is what was spent, and the two
    /// are not interchangeable. Until this existed the only number any caller
    /// could read was the ceiling, so the App — which holds the buyer's money
    /// in its own ledger — had nothing authoritative to settle a reservation
    /// against and nothing to show as the job's cost. A UI in that position
    /// either shows the ceiling as if it were the bill or invents a figure,
    /// and inventing money is the one thing the money rules forbid outright.
    pub settled_atomic: i64,
    /// What is spent but UNPROVEN, in atomic USDC: the sum over nodes whose
    /// payment status is `unresolved`. Not a cost and not a refund — the third
    /// answer. It stays held until reconciliation decides, and it is reported
    /// separately so a caller can never quietly fold it into either.
    pub unresolved_atomic: i64,
}

/// Whether a create call made the job or found it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Created {
    /// This call created it.
    Fresh,
    /// An identical call had already created it. The caller gets the same job
    /// and nothing new is reserved — which is the whole point.
    Existing,
}

#[derive(Debug, Clone, FromRow)]
struct NodeRow {
    node_key: String,
    depends_on: Vec<String>,
    status: String,
    lease_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    attempts: i32,
    max_attempts: i32,
    required: bool,
}

impl Database {
    /// Create a job for an approved plan version, or return the one this exact
    /// call already created.
    ///
    /// The nodes are inserted in the same transaction: a job with no nodes is a
    /// job the worker would report as finished the moment it looked at it.
    pub async fn create_job(
        &self,
        plan: &crate::plan::Plan,
        claim: &crate::plan::token::PlanClaim,
        conversation_id: Option<Uuid>,
        idempotency_key: &str,
        required_nodes: &[String],
        max_attempts: i32,
    ) -> Result<(JobRow, Created)> {
        let mut tx = self.pool.begin().await?;

        // Serialize retries before reserving; a duplicate start reuses its existing hold.
        let owner = claim
            .user_id
            .ok_or_else(|| anyhow::anyhow!("funded_user_required"))?;
        sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("research-start:{owner}:{idempotency_key}"))
            .execute(&mut *tx)
            .await?;
        let existing: Option<Uuid> = sqlx::query_scalar(
            "select id from research_jobs where user_id = $1 and idempotency_key = $2",
        )
        .bind(owner)
        .bind(idempotency_key)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(id) = existing {
            tx.commit().await?;
            let row = self
                .job_for_owner(Some(id), claim.user_id, claim.connection_id, None)
                .await?
                .ok_or_else(|| anyhow::anyhow!("job missing"))?;
            if row.plan_hash != claim.plan_hash || row.plan_version_id != claim.version_id {
                return Err(anyhow::anyhow!(
                    "idempotency key belongs to a different approved plan"
                ));
            }
            return Ok((row, Created::Existing));
        }
        let new_job_id = Uuid::new_v4();
        let budget: serde_json::Value = sqlx::query_scalar(
            "select public.reserve_job_budget($1, $2, $3, $4, $5, $5, $6, p_metadata => '{\"stage\":\"started\"}'::jsonb)")
            .bind(owner).bind(new_job_id).bind(claim.plan_id).bind(claim.version_id)
            .bind(super::plans::atomic_i64(claim.buyer_total_atomic)?).bind(&claim.plan_hash)
            .fetch_one(&mut *tx).await?;
        if let Some(code) = budget.get("error").and_then(serde_json::Value::as_str) {
            return Err(anyhow::anyhow!("{code}"));
        }
        // The hold, job and nodes commit together. Rollback returns every cent.

        // `on conflict do nothing` plus a follow-up select, rather than
        // `do update`: an update would touch a row that may already be running
        // and whose ceiling somebody is spending against.
        let inserted: Option<Uuid> = sqlx::query_scalar(
            r#"insert into research_jobs
                 (id, plan_id, plan_version_id, plan_hash, user_id, connection_id,
                  conversation_id, status, buyer_total_ceiling_atomic,
                  max_requests, idempotency_key)
               values ($10, $1, $2, $3, $4, $5, $6, 'queued', $7, $8, $9)
               on conflict do nothing
               returning id"#,
        )
        .bind(claim.plan_id)
        .bind(claim.version_id)
        .bind(&claim.plan_hash)
        .bind(claim.user_id)
        .bind(claim.connection_id)
        .bind(conversation_id)
        .bind(super::plans::atomic_i64(claim.buyer_total_atomic)?)
        // Same rule as the amount: a request ceiling that cannot be represented
        // is refused, not widened. Clamping upward turns "this job may make at
        // most N calls" into "as many as it likes".
        .bind(i32::try_from(claim.max_requests).map_err(|_| {
            anyhow::anyhow!("request budget {} is not representable", claim.max_requests)
        })?)
        .bind(idempotency_key)
        .bind(new_job_id)
        .fetch_optional(&mut *tx)
        .await?;

        let Some(job_id) = inserted else {
            tx.rollback().await?;
            let existing = self
                .job_for_owner(
                    None,
                    claim.user_id,
                    claim.connection_id,
                    Some(idempotency_key),
                )
                .await?
                .ok_or_else(|| anyhow::anyhow!("job conflicted but could not be read back"))?;
            if existing.plan_hash != claim.plan_hash || existing.plan_version_id != claim.version_id
            {
                return Err(anyhow::anyhow!(
                    "idempotency key belongs to a different approved plan"
                ));
            }
            return Ok((existing, Created::Existing));
        };

        for node in &plan.nodes {
            sqlx::query(
                r#"insert into research_job_nodes
                     (job_id, node_key, contract_id, endpoint_id, capability_slug,
                      subject_role, depends_on, reserved_atomic, max_attempts,
                      payment_status)
                   values ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'reserved')"#,
            )
            .bind(job_id)
            .bind(&node.id)
            .bind(Uuid::parse_str(&node.contract_id)?)
            .bind(Uuid::parse_str(&node.endpoint_id)?)
            .bind(&node.capability_slug)
            .bind(&node.subject_role)
            .bind(&node.depends_on)
            .bind(super::plans::atomic_i64(node.buyer_total_atomic)?)
            .bind(max_attempts)
            .execute(&mut *tx)
            .await?;
        }

        sqlx::query(
            r#"insert into research_job_events (job_id, seq, kind, summary, detail)
               values ($1, 1, 'job_queued', $2, $3)"#,
        )
        .bind(job_id)
        .bind(format!("{} steps approved", plan.nodes.len()))
        .bind(serde_json::json!({
            "required_nodes": required_nodes,
            "plan_hash": claim.plan_hash,
        }))
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        let row = self
            .job_for_owner(Some(job_id), claim.user_id, claim.connection_id, None)
            .await?
            .ok_or_else(|| anyhow::anyhow!("job inserted but could not be read back"))?;
        Ok((row, Created::Fresh))
    }

    /// One job, for its owner only.
    ///
    /// Ownership is in the WHERE clause, not a check on the result: a query
    /// that can return another buyer's job before deciding not to show it is
    /// one refactor away from showing it.
    pub async fn job_for_owner(
        &self,
        job_id: Option<Uuid>,
        user_id: Option<Uuid>,
        connection_id: Option<Uuid>,
        idempotency_key: Option<&str>,
    ) -> Result<Option<JobRow>> {
        Ok(sqlx::query_as::<_, JobRow>(
            r#"select j.id, j.plan_id, j.plan_version_id, j.plan_hash, j.user_id,
                      j.connection_id, j.status, j.pending_question,
                      j.buyer_total_ceiling_atomic, j.max_requests, j.requests_made,
                      j.cancelled_at, j.created_at, j.finished_at,
                      -- Summed here rather than kept as a running column on the
                      -- job: the node rows are the record of what was paid, and
                      -- a second copy of that number is a second thing that can
                      -- be wrong. `filter` rather than `case` so a node that is
                      -- settled-but-null contributes nothing instead of zero.
                      coalesce((select sum(n.settled_atomic)
                                  from research_job_nodes n
                                 where n.job_id = j.id
                                   and n.payment_status = 'settled'), 0)::bigint
                        as settled_atomic,
                      coalesce((select sum(n.settled_atomic)
                                  from research_job_nodes n
                                 where n.job_id = j.id
                                   and n.payment_status = 'unresolved'), 0)::bigint
                        as unresolved_atomic
                 from research_jobs j
                where ($1::uuid is null or j.id = $1)
                  and ($4::text is null or j.idempotency_key = $4)
                  and ( ($2::uuid is not null and j.user_id = $2)
                     or ($3::uuid is not null and j.connection_id = $3) )
                order by j.created_at desc
                limit 1"#,
        )
        .bind(job_id)
        .bind(user_id)
        .bind(connection_id)
        .bind(idempotency_key)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// The job's nodes, in the shape the scheduler reasons about.
    pub async fn job_nodes(&self, job_id: Uuid, required: &[String]) -> Result<Vec<NodeState>> {
        let rows = sqlx::query_as::<_, NodeRow>(
            r#"select node_key, depends_on, status, lease_expires_at,
                      attempts, max_attempts, (node_key = any($2)) as required
                 from research_job_nodes
                where job_id = $1
                order by created_at"#,
        )
        .bind(job_id)
        .bind(required)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| NodeState {
                key: row.node_key,
                depends_on: row.depends_on,
                status: parse_status(&row.status),
                lease_expires_at: row.lease_expires_at,
                attempts: row.attempts,
                max_attempts: row.max_attempts,
                required: row.required,
            })
            .collect())
    }

    /// Take a node, or find out someone else already has it.
    ///
    /// The lease check is in the WHERE clause. Two workers issuing this at the
    /// same instant produce one winner and one zero-row update; Postgres
    /// serialises the row, and neither worker ever sees a window in which the
    /// node looks free while the other holds it.
    pub async fn claim_node(
        &self,
        job_id: Uuid,
        node_key: &str,
        worker: &str,
        lease: chrono::Duration,
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        // Serialize claims for one job so concurrent workers cannot each use
        // the same remaining concurrency/request slot or miss cancellation.
        sqlx::query("select 1 from research_jobs where id = $1 for update")
            .bind(job_id)
            .execute(&mut *tx)
            .await?;
        let claimed = sqlx::query(
            r#"update research_job_nodes n
                  set status = 'running',
                      leased_by = $3,
                      lease_expires_at = now() + $4::interval,
                      attempts = attempts + 1,
                      started_at = coalesce(started_at, now())
                where job_id = $1
                  and node_key = $2
                  and status = 'queued'
                  and attempts < max_attempts
                  and (lease_expires_at is null or lease_expires_at < now())
                  and not exists (
                      select 1 from unnest(n.depends_on) dependency(key)
                      left join research_job_nodes d on d.job_id = n.job_id and d.node_key = dependency.key
                      where d.status is distinct from 'succeeded')
                  and exists (
                      select 1 from research_jobs j
                      join research_plan_versions v on v.id = j.plan_version_id
                      where j.id = n.job_id and j.status in ('queued', 'running') and j.cancelled_at is null
                        and not exists (select 1 from research_job_nodes u where u.job_id = j.id and u.payment_status = 'unresolved')
                        and j.requests_made + (select count(*) from research_job_nodes r where r.job_id = j.id and r.status = 'running') < j.max_requests
                        and (select count(*) from research_job_nodes r where r.job_id = j.id and r.status = 'running') < least(3, coalesce((v.limits->>'max_concurrency')::int, 0)))"#,
        )
        .bind(job_id)
        .bind(node_key)
        .bind(worker)
        .bind(
            sqlx::postgres::types::PgInterval::try_from(lease)
                .map_err(|e| anyhow::anyhow!("{e}"))?,
        )
        .execute(&mut *tx)
        .await?;
        if claimed.rows_affected() == 1 {
            event_in_tx(
                &mut tx,
                job_id,
                Some(node_key),
                "node_running",
                "started",
                serde_json::json!({ "worker": worker }),
            )
            .await?;
        }
        tx.commit().await?;
        Ok(claimed.rows_affected() == 1)
    }

    /// Append an event, numbering it under the job's own row lock so two
    /// writers cannot take the same `seq`.
    pub async fn append_event(
        &self,
        job_id: Uuid,
        node_key: Option<&str>,
        kind: &str,
        summary: &str,
        detail: serde_json::Value,
    ) -> Result<i64> {
        append_event_on(&self.pool, job_id, node_key, kind, summary, detail).await
    }

    /// Events after a cursor, for a browser picking the story back up.
    pub async fn job_events_after(
        &self,
        job_id: Uuid,
        after: i64,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>> {
        Ok(sqlx::query_scalar::<_, serde_json::Value>(
            r#"select jsonb_build_object(
                        'seq', seq, 'node_key', node_key, 'kind', kind,
                        'summary', summary, 'detail', detail, 'at', created_at)
                 from research_job_events
                where job_id = $1 and seq > $2
                order by seq
                limit $3"#,
        )
        .bind(job_id)
        .bind(after)
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.pool)
        .await?)
    }

    /// Ask for cancellation.
    ///
    /// A server action, and only the first half of one: it stops future
    /// dispatches. Calls already in flight are settled by the worker, because
    /// a request that has left cannot be unsent and pretending otherwise would
    /// leave money unaccounted for.
    pub async fn cancel_job(
        &self,
        job_id: Uuid,
        user_id: Option<Uuid>,
        connection_id: Option<Uuid>,
    ) -> Result<bool> {
        let updated = sqlx::query(
            r#"update research_jobs
                  set cancelled_at = coalesce(cancelled_at, now()),
                      status = case when status in ('succeeded','partial','failed','cancelled','expired')
                                    then status else 'cancelled' end
                where id = $1
                  and ( ($2::uuid is not null and user_id = $2)
                     or ($3::uuid is not null and connection_id = $3) )"#,
        )
        .bind(job_id)
        .bind(user_id)
        .bind(connection_id)
        .execute(&self.pool)
        .await?;
        Ok(updated.rows_affected() == 1)
    }
}

fn parse_status(raw: &str) -> NodeStatus {
    match raw {
        "running" => NodeStatus::Running,
        "needs_input" => NodeStatus::NeedsInput,
        "paused" => NodeStatus::Paused,
        "succeeded" => NodeStatus::Succeeded,
        "failed" | "partial" | "expired" => NodeStatus::Failed,
        "blocked" => NodeStatus::Blocked,
        "skipped" => NodeStatus::Skipped,
        "cancelled" => NodeStatus::Cancelled,
        // Anything else — including a status added by a later migration this
        // build has not heard of — is treated as not yet run. Guessing it is
        // finished would let a job report success it has not earned.
        _ => NodeStatus::Queued,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_stored_status_maps_to_the_scheduler_and_unknown_means_unfinished() {
        for (raw, expected) in [
            ("queued", NodeStatus::Queued),
            ("running", NodeStatus::Running),
            ("needs_input", NodeStatus::NeedsInput),
            ("paused", NodeStatus::Paused),
            ("succeeded", NodeStatus::Succeeded),
            ("failed", NodeStatus::Failed),
            ("partial", NodeStatus::Failed),
            ("expired", NodeStatus::Failed),
            ("blocked", NodeStatus::Blocked),
            ("skipped", NodeStatus::Skipped),
            ("cancelled", NodeStatus::Cancelled),
        ] {
            assert_eq!(parse_status(raw), expected, "{raw}");
        }
        // A status from a newer migration must not read as finished.
        assert_eq!(parse_status("something_new"), NodeStatus::Queued);
        assert!(!parse_status("something_new").is_terminal());
    }
}

impl Database {
    /// Record the answer to a job's pending question and let its branch run on.
    ///
    /// Saves the selected candidate's validated facts before marking its node
    /// successful. The job lock, fact inserts, node transition and next question
    /// share one transaction, so dependants cannot run with a missing identity.
    /// A repeated answer changes nothing and never repeats the lookup.
    ///
    /// Nothing is charged here. The lookup that produced the candidates was
    /// already made and paid for; this reads one of them.
    pub async fn record_answer(
        &self,
        job_id: Uuid,
        node_key: &str,
        chosen: &serde_json::Value,
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await?;

        let cleared = sqlx::query(
            r#"with picked as (
                 select j.id as job_id, n.id as node_id, n.node_key,
                        n.subject_role, n.endpoint_id, n.contract_id, n.started_at,
                        candidate.value as candidate
                   from research_jobs j
                   join research_job_nodes n
                     on n.job_id = j.id and n.node_key = $2
                   cross join lateral jsonb_array_elements(j.pending_question->'candidates') candidate(value)
                  where j.id = $1 and j.status = 'needs_input'
                    and j.cancelled_at is null and n.status = 'needs_input'
                    and j.pending_question->>'node_key' = $2
                    and candidate.value->'identity' = $3::jsonb
                    and exists (
                      select 1 from jsonb_array_elements(candidate.value->'facts') fact(value)
                       where fact.value->>'fact_type' = j.pending_question->>'fact_type'
                         and fact.value->'value' = candidate.value->'identity')
                  for update of j, n
               ), saved as (
                 insert into research_job_facts
                   (job_id, node_id, node_key, fact_type, subject_key, subject_role,
                    value, source_endpoint_id, source_contract_id, extraction_base,
                    observed_at, validation)
                 select p.job_id, p.node_id, p.node_key, fact.value->>'fact_type',
                        p.candidate->>'identity', p.subject_role, fact.value->'value',
                        p.endpoint_id, p.contract_id, 'pending_question.candidates',
                        coalesce(p.started_at, now()), 'valid'
                   from picked p
                   cross join lateral jsonb_array_elements(p.candidate->'facts') fact(value)
                 returning job_id
               )
               update research_jobs
                  set pending_question = null,
                      status = 'queued'
                where id = $1
                  and exists (select 1 from saved where job_id = $1)"#,
        )
        .bind(job_id)
        .bind(node_key)
        .bind(chosen)
        .execute(&mut *tx)
        .await?;

        if cleared.rows_affected() != 1 {
            tx.rollback().await?;
            return Ok(false);
        }

        sqlx::query(
            r#"update research_job_nodes
                  set status = 'succeeded',
                      bound_inputs = coalesce(bound_inputs, '{}'::jsonb) || jsonb_build_object('chosen', $3::jsonb),
                      finished_at = coalesce(finished_at, now())
                where job_id = $1 and node_key = $2"#,
        )
        .bind(job_id)
        .bind(node_key)
        .bind(chosen)
        .execute(&mut *tx)
        .await?;
        event_in_tx(
            &mut tx,
            job_id,
            Some(node_key),
            "node_succeeded",
            "answered",
            serde_json::json!({}),
        )
        .await?;

        // The next node that is still asking takes the place just vacated. Its
        // question is the one it recorded when it parked; the job goes back to
        // `needs_input` rather than looking resumable while a branch of it is
        // waiting on an answer nobody has been asked for.
        sqlx::query(
            r#"update research_jobs
                  set pending_question = waiting.detail,
                      status = 'needs_input'
                 from (select e.detail
                         from research_job_events e
                         join research_job_nodes n
                           on n.job_id = e.job_id and n.node_key = e.node_key
                        where e.job_id = $1 and e.kind = 'question_asked'
                          and n.status = 'needs_input'
                        order by e.seq desc
                        limit 1) as waiting
                where id = $1 and pending_question is null"#,
        )
        .bind(job_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(true)
    }
}

/// Everything one node needs before it can be sent, in one read.
///
/// One query rather than several, for the same reason the planner takes its
/// whole candidate set at once: a dispatcher that can ask the database a
/// follow-up question mid-node can also ask a provider one, and the boundary
/// stops being structural.
#[derive(Debug, Clone)]
pub struct RunnableNode {
    pub required_facts: Vec<String>,
    pub job_id: Uuid,
    pub node_key: String,
    pub contract: crate::facts::contract::ExecutionContract,
    pub api_slug: String,
    pub endpoint_path: String,
    pub method: String,
    pub reserved_atomic: i64,
    pub attempts: i32,
    pub max_attempts: i32,
    /// Approved known inputs and facts from their declared dependency, keyed by
    /// fact type. Only `validation = 'valid'` rows: a rejected value is not a fact
    /// and must never be bound into a paid call.
    pub available_facts: std::collections::BTreeMap<String, serde_json::Value>,
    /// The identity fact the contract declares, if any.
    pub identifies: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
struct RunnableRow {
    dag: serde_json::Value,
    plan_hash: String,
    endpoint_price: BigDecimal,
    live_contract_id: Uuid,
    live_endpoint_id: Uuid,
    job_id: Uuid,
    node_key: String,
    api_slug: String,
    endpoint_path: String,
    method: String,
    reserved_atomic: i64,
    attempts: i32,
    max_attempts: i32,
    capability_slug: String,
    version: i32,
    cardinality: String,
    requires: serde_json::Value,
    input_map: serde_json::Value,
    output_map: serde_json::Value,
    identifies: Option<String>,
    max_fan_out: i32,
}

impl Database {
    /// Load a node the caller has already claimed.
    ///
    /// Separate from [`Self::claim_node`] on purpose: claiming is one
    /// conditional UPDATE and nothing else, so its guarantee is easy to read.
    /// Reading what was claimed is a different concern and a different query.
    pub async fn runnable_node(
        &self,
        job_id: Uuid,
        node_key: &str,
    ) -> Result<Option<RunnableNode>> {
        let Some(row) = sqlx::query_as::<_, RunnableRow>(
            r#"select n.job_id, n.node_key, a.slug as api_slug, e.path as endpoint_path,
                      e.method, n.reserved_atomic, n.attempts, n.max_attempts,
                      c.capability_slug, c.version, c.cardinality, c.requires,
                      c.input_map, c.output_map, c.identifies, c.max_fan_out,
                      v.dag, j.plan_hash, e.price as endpoint_price, c.id as live_contract_id, e.id as live_endpoint_id
                 from research_job_nodes n
                 join research_jobs j on j.id = n.job_id
                 join research_plan_versions v on v.id = j.plan_version_id
                 join capability_execution_contracts c on c.id = n.contract_id
                 join api_endpoints e on e.id = n.endpoint_id
                 join apis a on a.id = e.api_id
                where n.job_id = $1 and n.node_key = $2
                  -- An endpoint deactivated since the plan was approved is not
                  -- called. The approval was for work that could be done.
                  and e.active and c.status = 'active' and c.endpoint_id = n.endpoint_id"#,
        )
        .bind(job_id)
        .bind(node_key)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(None);
        };

        let contract = crate::facts::contract::ExecutionContract {
            capability_slug: row.capability_slug,
            version: row.version,
            cardinality: serde_json::from_value(serde_json::Value::String(row.cardinality))?,
            requires: serde_json::from_value(row.requires)?,
            input_map: serde_json::from_value(row.input_map)?,
            output_map: serde_json::from_value(row.output_map)?,
            identifies: row.identifies.clone(),
            max_fan_out: u32::try_from(row.max_fan_out).unwrap_or(1),
        };

        let plan: crate::plan::Plan = serde_json::from_value(row.dag)?;
        let vocabulary = self.fact_vocabulary().await?;
        if plan.hash() != row.plan_hash || crate::plan::validate::plan(&plan, &vocabulary).is_err()
        {
            return Ok(None);
        }
        let Some(approved) = plan.nodes.iter().find(|n| n.id == node_key) else {
            return Ok(None);
        };
        let price = crate::fees::usdc_to_atomic(&row.endpoint_price);
        if approved.contract_id != row.live_contract_id.to_string()
            || approved.endpoint_id != row.live_endpoint_id.to_string()
            || approved.endpoint_method != row.method
            || approved.contract.as_ref() != Some(&contract)
            || approved.api_slug != row.api_slug
            || approved.endpoint_path != row.endpoint_path
            || approved.buyer_total_atomic != row.reserved_atomic as i128
            || price <= 0
            || price > i64::MAX as i128 / 2
            || crate::plan::quote::node_buyer_total(price, crate::fees::ProviderClass::Managed)
                != approved.buyer_total_atomic
        {
            return Ok(None);
        }
        let facts: Vec<(String, String, serde_json::Value)> = sqlx::query_as(
            r#"select f.node_key, f.fact_type, f.value
                 from research_job_facts f
                 join research_job_nodes d on d.job_id = f.job_id and d.node_key = f.node_key
                where f.job_id = $1 and f.validation = 'valid' and d.status = 'succeeded'
                  and f.subject_role = $3 and (f.fresh_until is null or f.fresh_until > now())
                  and d.node_key = any(select unnest(depends_on) from research_job_nodes where job_id = $1 and node_key = $2)"#,
        ).bind(job_id).bind(node_key).bind(&approved.subject_role).fetch_all(&self.pool).await?;
        let available_facts = match crate::plan::validate::inputs(approved, &facts, &vocabulary) {
            Ok(facts) => facts,
            Err(error) => {
                tracing::warn!(job = %job_id, node = node_key, %error, "approved inputs unavailable");
                return Ok(None);
            }
        };

        let required_facts = crate::plan::validate::required_facts(&plan, node_key);
        Ok(Some(RunnableNode {
            required_facts,
            job_id: row.job_id,
            node_key: row.node_key,
            contract,
            api_slug: row.api_slug,
            endpoint_path: row.endpoint_path,
            method: row.method,
            reserved_atomic: row.reserved_atomic,
            attempts: row.attempts,
            max_attempts: row.max_attempts,
            available_facts,
            identifies: row.identifies,
        }))
    }

    /// Write down what a node is about to send, BEFORE it sends it.
    ///
    /// The commit here is the crash boundary. A process that dies after this
    /// and before the request leaves evidence of what it was about to do; one
    /// that dies without it leaves nothing, and nothing is indistinguishable
    /// from a call that was never made — which is the one thing a payment
    /// record may not be ambiguous about.
    pub async fn record_intent(
        &self,
        job_id: Uuid,
        node_key: &str,
        bound: &serde_json::Value,
        input_hash: &str,
        idempotency_key: &str,
    ) -> Result<bool> {
        let updated = sqlx::query(
            r#"update research_job_nodes
                  set bound_inputs = $3,
                      bound_input_hash = $4,
                      idempotency_key = $5,
                      payment_status = 'unresolved'
                where job_id = $1 and node_key = $2
                  and status = 'running'
                  and payment_status is distinct from 'unresolved'
                  -- The same key with different content is refused rather than
                  -- treated as a retry: reusing a key across two different
                  -- requests is how one payment answers for another.
                  and (idempotency_key is null
                       or (idempotency_key = $5 and bound_input_hash = $4))"#,
        )
        .bind(job_id)
        .bind(node_key)
        .bind(bound)
        .bind(input_hash)
        .bind(idempotency_key)
        .execute(&self.pool)
        .await?;
        Ok(updated.rows_affected() == 1)
    }

    /// Store what one node learned, and close it.
    ///
    /// Facts and the node's terminal status go in one transaction. A node
    /// marked succeeded whose facts were not written would release its
    /// dependants to bind against nothing.
    pub async fn finish_node(
        &self,
        job_id: Uuid,
        node_key: &str,
        status: &str,
        payment: crate::job::attempt::PaymentStatus,
        settled_atomic: Option<i128>,
        facts: &[(String, Option<String>, serde_json::Value, String)],
        rejected: &[(String, String)],
        error_code: Option<&str>,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;

        // The role comes off the node being closed, never from the extractor:
        // a contract describes "the subject" and knows nothing of which of a
        // plan's companies this node was planned for. Getting it from anywhere
        // else is how one company's facts bind into the other's call.
        for (fact_type, subject_key, value, base) in facts {
            sqlx::query(
                r#"insert into research_job_facts
                     (job_id, node_key, fact_type, subject_key, subject_role, value, extraction_base, validation)
                   select $1, $2, $3, $4, n.subject_role, $5, $6, 'valid'
                     from research_job_nodes n
                    where n.job_id = $1 and n.node_key = $2"#,
            )
            .bind(job_id)
            .bind(node_key)
            .bind(fact_type)
            .bind(subject_key)
            .bind(value)
            .bind(base)
            .execute(&mut *tx)
            .await?;
        }

        for (fact_type, reason) in rejected {
            sqlx::query(
                r#"insert into research_job_facts
                     (job_id, node_key, fact_type, subject_role, value, validation, rejected_reason)
                   select $1, $2, $3, n.subject_role, 'null'::jsonb, 'rejected', $4
                     from research_job_nodes n
                    where n.job_id = $1 and n.node_key = $2"#,
            )
            .bind(job_id)
            .bind(node_key)
            .bind(fact_type)
            .bind(reason)
            .execute(&mut *tx)
            .await?;
        }

        sqlx::query(
            r#"update research_job_nodes
                  set status = $3,
                      payment_status = $4,
                      settled_atomic = $5,
                      error_code = coalesce($6, error_code),
                      finished_at = now()
                where job_id = $1 and node_key = $2"#,
        )
        .bind(job_id)
        .bind(node_key)
        .bind(status)
        .bind(
            serde_json::to_value(payment)?
                .as_str()
                .unwrap_or("reserved"),
        )
        .bind(settled_atomic.map(super::plans::atomic_i64).transpose()?)
        .bind(error_code)
        .execute(&mut *tx)
        .await?;
        event_in_tx(
            &mut tx,
            job_id,
            Some(node_key),
            &node_event_kind(status),
            error_code.unwrap_or(status),
            serde_json::json!({
                "status": status,
                "error_code": error_code,
                "payment": serde_json::to_value(payment)?,
                "facts": facts.len(),
                "rejected": rejected.len(),
            }),
        )
        .await?;

        sqlx::query(
            r#"update research_jobs
                  set requests_made = requests_made + 1,
                      status = case when status = 'queued' then 'running' else status end,
                      started_at = coalesce(started_at, now())
                where id = $1 and requests_made < max_requests"#,
        )
        .bind(job_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    /// Park a node on a question, and stop its branch until it is answered.
    pub async fn ask_question(
        &self,
        job_id: Uuid,
        node_key: &str,
        question: &crate::job::ask::Question,
        payment: crate::job::attempt::PaymentStatus,
        settled_atomic: Option<i128>,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;

        // The lookup that produced these candidates was made and charged. Its
        // payment is settled here, with the question, so resuming later reads
        // a row instead of calling anyone.
        sqlx::query(
            r#"update research_job_nodes
                  set status = 'needs_input',
                      payment_status = $3,
                      settled_atomic = $4,
                      leased_by = null,
                      lease_expires_at = null
                where job_id = $1 and node_key = $2"#,
        )
        .bind(job_id)
        .bind(node_key)
        .bind(serde_json::to_value(payment)?.as_str().unwrap_or("settled"))
        .bind(settled_atomic.map(super::plans::atomic_i64).transpose()?)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"update research_jobs
                  set status = 'needs_input',
                      requests_made = requests_made + 1
                where id = $1"#,
        )
        .bind(job_id)
        .execute(&mut *tx)
        .await?;

        // ONE QUESTION IN FRONT OF SOMEBODY AT A TIME. A plan that researches
        // two companies has two lookups, and both may come back ambiguous
        // within the same round. Overwriting the first with the second would
        // leave a node parked on a question nobody was ever shown, so the
        // second waits and `record_answer` puts it up once the first is
        // answered. Its own node row already says it is asking.
        sqlx::query(
            r#"update research_jobs
                  set pending_question = $2
                where id = $1 and pending_question is null"#,
        )
        .bind(job_id)
        .bind(serde_json::to_value(question)?)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }
}

/// A job the worker may advance, with the two things scheduling needs.
#[derive(Debug, Clone)]
pub struct JobToAdvance {
    pub job_id: Uuid,
    /// Node keys a required output depends on. Read back from the job's own
    /// first event, where they were recorded when it was created, so a later
    /// change to how required nodes are derived cannot rewrite what an
    /// already-running job promised.
    pub required_nodes: Vec<String>,
    /// From the approved plan version. The concurrency limit is part of what
    /// the buyer approved, not a worker setting.
    pub max_concurrency: usize,
}

#[derive(Debug, Clone, FromRow)]
struct AdvanceRow {
    job_id: Uuid,
    required_nodes: Vec<String>,
    limits: serde_json::Value,
}

impl Database {
    /// Jobs with work left, oldest first.
    ///
    /// Cancelled jobs are excluded here rather than filtered later: cancel is a
    /// server action that stops FUTURE dispatches, and the cheapest way to make
    /// that true is for a cancelled job never to reach the dispatcher.
    pub async fn jobs_to_advance(&self, limit: i64) -> Result<Vec<JobToAdvance>> {
        let rows = sqlx::query_as::<_, AdvanceRow>(
            r#"select j.id as job_id,
                      coalesce(
                        (select array_agg(value)
                           from research_job_events e,
                                jsonb_array_elements_text(e.detail->'required_nodes') as value
                          where e.job_id = j.id and e.kind = 'job_queued'),
                        '{}') as required_nodes,
                      v.limits as limits
                 from research_jobs j
                 join research_plan_versions v on v.id = j.plan_version_id
                where j.status in ('queued', 'running')
                  and j.cancelled_at is null
                  and j.requests_made < j.max_requests
                order by j.created_at
                limit $1"#,
        )
        .bind(limit.clamp(1, 200))
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| JobToAdvance {
                job_id: row.job_id,
                required_nodes: row.required_nodes,
                max_concurrency: row
                    .limits
                    .get("max_concurrency")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|v| usize::try_from(v).ok())
                    .filter(|v| *v > 0)
                    // A version whose limits could not be read runs one node at
                    // a time. Guessing higher would spend faster than the plan
                    // said it would.
                    .unwrap_or(1),
            })
            .collect())
    }

    /// The active fact vocabulary, for validating what a provider sent.
    ///
    /// `status = 'active'` only. A draft fact type has no proven producer, and
    /// validating against one would let a job report a fact nobody has shown
    /// can be delivered.
    /// Every fact this conversation has already paid for and validated.
    ///
    /// Scoped to one owner AND one conversation, deliberately. That is the
    /// reuse boundary: a thread does not buy the same fact twice, and nothing
    /// crosses from one thread to another — a fact bought in one conversation
    /// is not evidence in the next, and treating it as such would leak both
    /// data and money between contexts that the buyer keeps separate.
    ///
    /// Only `valid` rows: a `rejected` row is the record that a provider sent
    /// something its type refused, and reading one as a fact is exactly the
    /// mistake that table was shaped to prevent.
    ///
    /// Only unexpired rows: `fresh_until` comes from the fact type's own
    /// freshness, and NULL there means the fact does not go stale (a
    /// registration number does not).
    pub async fn conversation_facts(
        &self,
        owner: Uuid,
        conversation_id: Uuid,
    ) -> Result<Vec<crate::plan::HeldFact>> {
        let rows: Vec<(String, Option<String>, String, serde_json::Value)> = sqlx::query_as(
            r#"SELECT f.fact_type, f.subject_key, f.subject_role, f.value
                 FROM research_job_facts f
                 JOIN research_jobs j ON j.id = f.job_id
                WHERE j.user_id = $1
                  AND j.conversation_id = $2
                  AND f.validation = 'valid'
                  AND (f.fresh_until IS NULL OR f.fresh_until > now())
                ORDER BY f.observed_at DESC"#,
        )
        .bind(owner)
        .bind(conversation_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(fact_type, subject_key, subject_role, value)| crate::plan::HeldFact {
                    fact_type,
                    subject_key,
                    subject_role,
                    value,
                },
            )
            .collect())
    }

    pub async fn fact_vocabulary(&self) -> Result<crate::facts::contract::Vocabulary> {
        let rows: Vec<(String, serde_json::Value)> =
            sqlx::query_as(r#"select slug, value_schema from fact_types where status = 'active'"#)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .into_iter()
            .map(|(slug, value_schema)| {
                (
                    slug.clone(),
                    crate::facts::contract::FactType { slug, value_schema },
                )
            })
            .collect())
    }
}

impl Database {
    /// The facts one job produced, for its owner only.
    ///
    /// Returns rejections alongside valid facts, and says which is which on
    /// every row. A reader that only ever saw the valid ones could not tell
    /// "the source answered and had nothing" from "the source sent something
    /// its type does not accept" — and those are different sentences to put in
    /// front of somebody. `normaliseFacts` on the App side drops anything not
    /// marked `valid`; it can only do that because the marking is here.
    ///
    /// Ownership is in the WHERE clause via the job, not checked on the way
    /// out, for the same reason as every other read in this file.
    pub async fn job_facts(
        &self,
        job_id: Uuid,
        user_id: Option<Uuid>,
        connection_id: Option<Uuid>,
    ) -> Result<Vec<serde_json::Value>> {
        Ok(sqlx::query_scalar::<_, serde_json::Value>(
            r#"select jsonb_build_object(
                        'fact_type', f.fact_type,
                        'subject_key', f.subject_key,
                        'subject_role', f.subject_role,
                        'value', f.value,
                        'node_key', f.node_key,
                        'observed_at', f.observed_at,
                        'fresh_until', f.fresh_until,
                        'validation', f.validation,
                        'rejected_reason', f.rejected_reason,
                        'extraction_base', f.extraction_base)
                 from research_job_facts f
                 join research_jobs j on j.id = f.job_id
                where f.job_id = $1
                  and ( ($2::uuid is not null and j.user_id = $2)
                     or ($3::uuid is not null and j.connection_id = $3) )
                order by f.created_at, f.fact_type"#,
        )
        .bind(job_id)
        .bind(user_id)
        .bind(connection_id)
        .fetch_all(&self.pool)
        .await?)
    }
}

/// Append an event from a bare pool: the worker's bulk passes hold no `Db`.
pub async fn append_event_on(
    pool: &sqlx::PgPool,
    job_id: Uuid,
    node_key: Option<&str>,
    kind: &str,
    summary: &str,
    detail: serde_json::Value,
) -> Result<i64> {
    let mut tx = pool.begin().await?;
    let seq = event_in_tx(&mut tx, job_id, node_key, kind, summary, detail).await?;
    tx.commit().await?;
    Ok(seq)
}

/// The one way an event is written. Locks the job row first: the sequence is
/// per job, so the job row is the only thing that needs serialising. Called
/// inside the transaction that changes a node's status, so the event and the
/// status it reports commit together — a browser folding events into its
/// progress never sees a node it cannot account for.
async fn event_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    job_id: Uuid,
    node_key: Option<&str>,
    kind: &str,
    summary: &str,
    detail: serde_json::Value,
) -> Result<i64> {
    sqlx::query("select 1 from research_jobs where id = $1 for update")
        .bind(job_id)
        .execute(&mut **tx)
        .await?;
    Ok(sqlx::query_scalar(
        r#"insert into research_job_events (job_id, seq, node_key, kind, summary, detail)
           select $1,
                  coalesce(max(seq), 0) + 1,
                  $2, $3, $4, $5
             from research_job_events where job_id = $1
           returning seq"#,
    )
    .bind(job_id)
    .bind(node_key)
    .bind(kind)
    .bind(summary)
    .bind(detail)
    .fetch_one(&mut **tx)
    .await?)
}

/// The event kind a node status maps to, as the App reads it: `succeeded`,
/// `failed`, `blocked`, `skipped`, `needs_input`, `paused`, `queued`,
/// `running`. `partial` and `expired` are failures to a reader; the exact
/// word travels in the summary.
pub fn node_event_kind(status: &str) -> String {
    match status {
        "partial" | "expired" => "node_failed".to_string(),
        other => format!("node_{other}"),
    }
}

impl Database {
    /// Reuse only the same owner's exact request and contract, observed within
    /// five minutes. Failed, ambiguous, expired or incomplete results never qualify.
    pub async fn reuse_node(&self, job_id: Uuid, node_key: &str, input_hash: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let source: Option<(Uuid,String)> = sqlx::query_as(r#"
          select previous.job_id, previous.node_key
          from research_job_nodes target
          join research_jobs current_job on current_job.id=target.job_id
          join research_job_nodes previous on previous.contract_id=target.contract_id and previous.endpoint_id=target.endpoint_id
          join research_jobs previous_job on previous_job.id=previous.job_id
          join research_plan_versions pv on pv.id=previous_job.plan_version_id
          join research_plan_versions cv on cv.id=current_job.plan_version_id
          where target.job_id=$1 and target.node_key=$2 and target.status='running'
            and previous.job_id<>target.job_id and previous.status='succeeded'
            and previous.bound_input_hash=$3 and previous.finished_at>now()-interval '5 minutes'
            and previous_job.user_id=current_job.user_id
            and previous_job.connection_id is not distinct from current_job.connection_id
            and (select value->'contract' from jsonb_array_elements(pv.dag->'nodes') where value->>'id'=previous.node_key)
                = (select value->'contract' from jsonb_array_elements(cv.dag->'nodes') where value->>'id'=target.node_key)
            and exists(select 1 from research_job_facts f where f.job_id=previous.job_id and f.node_key=previous.node_key and f.validation='valid')
            and not exists(select 1 from research_job_facts f where f.job_id=previous.job_id and f.node_key=previous.node_key
                and (f.validation<>'valid' or f.observed_at is null or f.observed_at<now()-interval '5 minutes' or f.fresh_until<=now()))
          order by previous.finished_at desc limit 1"#)
            .bind(job_id).bind(node_key).bind(input_hash).fetch_optional(&mut *tx).await?;
        let Some((source_job, source_node)) = source else {
            return Ok(false);
        };
        sqlx::query(r#"insert into research_job_facts
          (job_id,node_id,node_key,fact_type,subject_key,subject_role,value,source_endpoint_id,source_contract_id,extraction_base,observed_at,fresh_until,validation)
          select $1,n.id,$2,f.fact_type,f.subject_key,n.subject_role,f.value,
            coalesce(f.source_endpoint_id,n.endpoint_id),coalesce(f.source_contract_id,n.contract_id),f.extraction_base,
            f.observed_at,coalesce(f.fresh_until,f.observed_at+interval '5 minutes'),'valid'
          from research_job_facts f join research_job_nodes n on n.job_id=$1 and n.node_key=$2
          where f.job_id=$3 and f.node_key=$4 and f.validation='valid'"#)
            .bind(job_id).bind(node_key).bind(source_job).bind(&source_node).execute(&mut *tx).await?;
        sqlx::query("update research_job_nodes set status='succeeded',payment_status='released',settled_atomic=0,finished_at=now(),leased_by=null,lease_expires_at=null,bound_input_hash=$3 where job_id=$1 and node_key=$2")
            .bind(job_id).bind(node_key).bind(input_hash).execute(&mut *tx).await?;
        event_in_tx(&mut tx,job_id,Some(node_key),"node_succeeded","reused recent result",
            serde_json::json!({"reused_from_job":source_job,"reused_from_node":source_node,"charged_atomic":0})).await?;
        tx.commit().await?;
        Ok(true)
    }
}
