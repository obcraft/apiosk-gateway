//! The research job worker: a separate process that runs approved plans.
//!
//! Why a process and not a request handler. A paid job must survive a closed
//! browser, a dropped stream and a crash, and Supabase Edge Functions have
//! runtime and CPU limits that make them a gateway rather than a place to
//! wait. So the Edge is the authenticated front door and short transactional
//! steps; the durable work runs here, out of a Postgres work list.
//!
//! ```text
//!   loop:
//!     1. reconcile   undecided payments FIRST, from the gateway's own ledger,
//!                    before anything new is spent
//!     2. recover     retake nodes whose lease has lapsed
//!     3. block       mark nodes whose branch has died, so nothing waits
//!                    forever on something that will never arrive
//!     4. dispatch    claim what is ready, up to the plan's own concurrency
//!     5. settle      write each job's status from its nodes
//! ```
//!
//! Step 1 is first on purpose. A worker that does not know what it already
//! spent cannot know whether the next call fits under the ceiling, and
//! "retry on timeout" is precisely how one intent becomes two payments.
//!
//! ---------------------------------------------------------------------------
//! What this process is and is not allowed to be
//! ---------------------------------------------------------------------------
//!
//! It holds NO buyer's browser token. It runs under a restricted service
//! identity, and for every job it checks the stored grant, the owner and
//! whether the approval has since been withdrawn — a token that was valid when
//! the job was queued is not evidence about now.
//!
//! It uses the same database functions, price rules and settlement path as the
//! existing single-call execution. A second way to pay for a call is a second
//! set of rounding, a second set of caps and a second thing to reconcile.
//!
//! ---------------------------------------------------------------------------
//! One pass or a loop
//! ---------------------------------------------------------------------------
//!
//! `RESEARCH_WORKER_ONCE=1` does a single pass and exits, which is what a
//! scheduled invocation wants and what makes this testable by running it.
//! Otherwise it loops on `RESEARCH_WORKER_INTERVAL_MS` (default 2000).
//! Several instances may run at once: every claim is conditional in SQL, so
//! two workers produce one winner and one no-op rather than two payments.
//!
//! ---------------------------------------------------------------------------
//! Why this is a module and not a `src/bin`
//! ---------------------------------------------------------------------------
//!
//! It was written as one first. A `src/bin` target is its OWN crate and cannot
//! see `crate::db`, `crate::facts` or `crate::plan` — so a worker there would
//! have to re-implement binding, extraction, pricing and settlement against raw
//! SQL. A second implementation of the price rules is a second set of rounding
//! and a second thing to reconcile, and the plan is explicit that the worker
//! uses the same DB functions, price rules and settlement path as the existing
//! execution.
//!
//! So it lives in the gateway's own crate and runs as a PROCESS ROLE: the same
//! image, started with `RESEARCH_WORKER_ENABLED=1`. On Fly that is a second
//! process group on the same release — a separate process with its own
//! lifecycle and its own scaling, which is what "a separate deployable worker"
//! asks for, without a second copy of the money code.

use std::env;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tracing::{error, info, warn};

use crate::AppState;

/// How long a worker holds a node before another may take it.
///
/// Long enough that a slow provider does not lose its claim mid-call, short
/// enough that a crashed worker's node is picked up while anyone still cares.
/// A lease is not a timeout on the call; it is a bound on how long a dead
/// process can hold work hostage.
const LEASE_SECONDS: i64 = 120;

/// How long one call through the proxy may take before it is called undecided.
///
/// LONGER than the proxy's own upstream timeout (`GATEWAY_UPSTREAM_TIMEOUT_SECS`,
/// 60 s unless set), on purpose. The proxy is the side that knows whether money
/// moved, and it says so in a header on its own response — including the
/// error it writes when the provider was the slow one. A worker that gives up
/// first throws that verdict away and manufactures an undecided payment out of
/// a call the proxy would have settled, or refused, seconds later; and one
/// undecided payment stops every other node of the job from being claimed.
/// 45 s against a 60 s proxy was how a slow but honest provider froze a
/// research.
const SEND_TIMEOUT_SECONDS: u64 = 75;

// The lease has to outlive the call it protects, or a live worker's node is
// "recovered" from under it mid-call and run twice.
const _: () = assert!(LEASE_SECONDS as u64 > SEND_TIMEOUT_SECONDS);

/// Whether this process should run the worker loop at all.
pub fn enabled() -> bool {
    env::var("RESEARCH_WORKER_ENABLED").is_ok_and(|v| v == "1")
}

/// Run the worker until the process ends.
///
/// Owned by `CommerceRuntime` when [`enabled`] is true. It uses native
/// authenticated router dispatch and shares the sole v2 process lifecycle.
pub async fn run(state: Arc<AppState>) -> Result<()> {
    let worker_id = format!(
        "{}-{}",
        env::var("FLY_MACHINE_ID").unwrap_or_else(|_| "local".into()),
        std::process::id()
    );
    let once = env::var("RESEARCH_WORKER_ONCE").is_ok_and(|v| v == "1");
    let interval = env::var("RESEARCH_WORKER_INTERVAL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(2_000);

    info!(worker = %worker_id, once, interval_ms = interval, "research worker starting");

    loop {
        if let Err(error) = pass(&state, &worker_id).await {
            // A failed pass is not a reason to stop. The next one re-reads
            // everything from the database, and stopping would strand every
            // job this worker was the only one looking at.
            error!("research worker pass failed: {error}");
        }
        if once {
            break;
        }
        tokio::time::sleep(Duration::from_millis(interval)).await;
    }
    Ok(())
}

async fn pass(state: &Arc<AppState>, worker_id: &str) -> Result<()> {
    let pool = state.db.worker_pool();
    let reconciled = reconcile_unresolved(pool).await?;
    let recovered = recover_lapsed_leases(pool).await?;
    let blocked = block_dead_branches(pool).await?;
    let dispatched = dispatch(state, worker_id).await?;
    let settled = settle_finished_jobs(pool).await?;
    if let Some(reference) = &state.config.treasury_connect_token_ref {
        if let crate::db::AgentConnectTokenLookup::Active(token) = state
            .db
            .lookup_agent_connect_token_by_ref(reference)
            .await?
        {
            if token
                .wallets
                .iter()
                .any(|wallet| state.is_treasury_wallet(&wallet.wallet_address))
            {
                sqlx::query("select public.reconcile_payment_holds($1)")
                    .bind(token.token_id)
                    .execute(pool)
                    .await?;
            }
        }
    }

    if reconciled + recovered + blocked + dispatched + settled > 0 {
        info!(
            reconciled,
            recovered, blocked, dispatched, settled, worker = %worker_id,
            "research worker pass"
        );
    }
    Ok(())
}

/// Bring undecided payments to a decision before anything new is spent.
///
/// An undecided node freezes its whole job: `claim_node` refuses every node of
/// a job that has one, so the siblings sit in `queued` and the job stays
/// `running` until someone decides. This is that someone, and it decides from
/// the one record that can: the gateway's own idempotency ledger. Every call
/// the worker sends carries an `idempotency-key`; the proxy CLAIMS that key
/// before it does anything else and FINALISES it with the response it sent,
/// receipt header included. So, for a node whose call is over:
///
///   - a finished ledger row carries the gateway's word on money in
///     `x-apiosk-buyer-total-atomic`: settled at that amount, or released when
///     the gateway answered without ever deciding a charge;
///   - no row at all, while the ledger still remembers everything of that age,
///     means the proxy never claimed the key or gave it back before any money
///     moved (`release_idempotency_claim`): the call never happened;
///   - a row still in flight, a receipt that cannot be read, or a node older
///     than the ledger's memory is left alone and ages into the visible
///     `payment_unresolved_aged` label as before. A clock is not evidence that
///     a call was free.
///
/// Nothing here re-runs the call or touches its facts: the node stays `failed`
/// with what it lost. It is the MONEY that is decided, and the money is what
/// the rest of the job was waiting on.
async fn reconcile_unresolved(pool: &sqlx::PgPool) -> Result<u64> {
    use sqlx::Row;

    let candidates = sqlx::query(
        r#"select n.job_id, n.node_key,
                  coalesce(n.started_at, n.created_at) as started_at,
                  l.response_status, l.response_headers
             from research_job_nodes n
             left join gateway_idempotency_keys l on l.key = n.idempotency_key
            where n.payment_status = 'unresolved'
              and n.status <> 'running'
              and n.idempotency_key is not null
              -- The proxy finalises its row after it has answered. Give it a
              -- moment, so a call that is merely slow is not read as absent.
              and coalesce(n.finished_at, n.started_at, n.created_at) < now() - interval '60 seconds'
            order by n.finished_at nulls first
            limit 50"#,
    )
    .fetch_all(pool)
    .await?;

    let now = chrono::Utc::now();
    let mut decided = 0u64;
    for row in candidates {
        let job_id: uuid::Uuid = row.try_get("job_id")?;
        let node_key: String = row.try_get("node_key")?;
        let started_at: chrono::DateTime<chrono::Utc> = row.try_get("started_at")?;
        let ledger = match row.try_get::<Option<i16>, _>("response_status")? {
            Some(response_status) => Some(LedgerRow {
                response_status,
                response_headers: row
                    .try_get::<Option<serde_json::Value>, _>("response_headers")?
                    .unwrap_or(serde_json::Value::Null),
            }),
            None => None,
        };

        let (payment, amount, code) = match reconcile_from_ledger(ledger.as_ref(), started_at, now)
        {
            Reconciled::Settled(amount) => ("settled", amount, "payment_reconciled_settled"),
            Reconciled::Released => ("released", 0, "payment_reconciled_released"),
            Reconciled::Undecided(reason) => {
                info!(job = %job_id, node = %node_key, reason, "undecided payment stays undecided");
                continue;
            }
        };

        // `payment_status = 'unresolved'` repeated in the predicate: another
        // worker may have decided it between the read and this write, and the
        // second decision must be a no-op, never an overwrite.
        let updated = sqlx::query(
            r#"update research_job_nodes
                  set payment_status = $3,
                      settled_atomic = $4,
                      error_code = $5
                where job_id = $1 and node_key = $2 and payment_status = 'unresolved'"#,
        )
        .bind(job_id)
        .bind(&node_key)
        .bind(payment)
        .bind(amount)
        .bind(code)
        .execute(pool)
        .await?
        .rows_affected();
        if updated == 1 {
            info!(job = %job_id, node = %node_key, payment, amount, "undecided payment decided from the gateway ledger");
            decided += 1;
        }
    }

    // Whatever could not be decided is labelled once it is old, so it is
    // visible and alertable rather than quietly waiting.
    let aged = sqlx::query(
        r#"update research_job_nodes
              set error_code = coalesce(error_code, 'payment_unresolved_aged')
            where payment_status = 'unresolved'
              and finished_at < now() - interval '15 minutes'
              and error_code is distinct from 'payment_unresolved_aged'"#,
    )
    .execute(pool)
    .await?
    .rows_affected();
    if aged > 0 {
        warn!(
            nodes = aged,
            "undecided payments older than 15 minutes that the ledger could not decide; they stay reserved"
        );
    }
    Ok(decided)
}

/// What the gateway's idempotency ledger holds for one call, as far as money
/// is concerned. The body is not read here: the facts are not being recovered,
/// only the charge.
struct LedgerRow {
    response_status: i16,
    response_headers: serde_json::Value,
}

/// Where an undecided payment ends up once the ledger has been read.
#[derive(Debug, PartialEq, Eq)]
enum Reconciled {
    /// The gateway charged this much. Zero is not this; zero is `Released`.
    Settled(i64),
    /// Nothing was charged: refused before payment, or never sent at all.
    Released,
    /// Still nobody knows, and the reason why.
    Undecided(&'static str),
}

/// The decision, kept pure so the rules can be tested without a database.
///
/// `started_at` is when the worker recorded its intent to call; the ledger
/// only remembers a key for `RECORD_TTL_HOURS`, so an absent row proves
/// "never claimed" only for a call younger than that.
fn reconcile_from_ledger(
    ledger: Option<&LedgerRow>,
    started_at: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> Reconciled {
    use crate::db::{IN_FLIGHT_STATUS, RECORD_TTL_HOURS};

    match ledger {
        Some(row) if row.response_status == IN_FLIGHT_STATUS => {
            Reconciled::Undecided("ledger_row_in_flight")
        }
        Some(row) => {
            let receipt = row.response_headers.as_object().and_then(|headers| {
                headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("x-apiosk-buyer-total-atomic"))
                    .map(|(_, value)| value)
            });
            match receipt {
                // Answered without ever deciding a charge: the gateway's own
                // refusal, before payment.
                None => Reconciled::Released,
                Some(value) => match value.as_str().and_then(crate::job::attempt::settled_atomic) {
                    Some(0) => Reconciled::Released,
                    Some(amount) => match i64::try_from(amount) {
                        Ok(amount) => Reconciled::Settled(amount),
                        Err(_) => Reconciled::Undecided("receipt_out_of_range"),
                    },
                    None => Reconciled::Undecided("receipt_unreadable"),
                },
            }
        }
        None if now - started_at < chrono::Duration::hours(RECORD_TTL_HOURS) => {
            Reconciled::Released
        }
        None => Reconciled::Undecided("ledger_window_passed"),
    }
}

/// Give up nodes whose lease has lapsed, so another worker may take them.
///
/// Only the lease is cleared, never the attempt count. A node that has already
/// spent its attempts stays spent: forgetting the attempt would let a crash
/// loop pay for the same call repeatedly.
async fn recover_lapsed_leases(pool: &sqlx::PgPool) -> Result<u64> {
    let rows = sqlx::query_as::<_, (uuid::Uuid, String, String, Option<String>)>(
        r#"update research_job_nodes
              set leased_by = null,
                  lease_expires_at = null,
                  status = case when payment_status = 'unresolved' or attempts >= max_attempts then 'failed' else 'queued' end,
                  finished_at = case when payment_status = 'unresolved' then coalesce(finished_at, now()) else finished_at end,
                  error_code = case when payment_status = 'unresolved' then 'lease_lost_payment_unresolved'
                                    when attempts >= max_attempts
                                    then coalesce(error_code, 'lease_lost_out_of_attempts')
                                    else error_code end
            where status = 'running'
              and lease_expires_at is not null
              and lease_expires_at < now()
              -- An expired uncertain call becomes terminal, never queued again.
            returning job_id, node_key, status, error_code"#,
    )
    .fetch_all(pool)
    .await?;
    for (job_id, node_key, status, error_code) in &rows {
        crate::db::jobs::append_event_on(
            pool,
            *job_id,
            Some(node_key),
            &crate::db::jobs::node_event_kind(status),
            error_code.as_deref().unwrap_or("lease lapsed"),
            serde_json::json!({ "status": status, "error_code": error_code }),
        )
        .await?;
    }
    Ok(rows.len() as u64)
}

/// Mark nodes whose branch has died.
///
/// Without this they sit in `queued` forever, and a job waits on something
/// that will never arrive while its status says it is still working.
async fn block_dead_branches(pool: &sqlx::PgPool) -> Result<u64> {
    let rows = sqlx::query_as::<_, (uuid::Uuid, String)>(
        r#"update research_job_nodes n
              set status = 'blocked',
                  finished_at = coalesce(n.finished_at, now()),
                  error_code = coalesce(n.error_code, 'dependency_failed')
            where n.status in ('queued', 'needs_input', 'paused')
              and exists (
                select 1 from research_job_nodes d
                 where d.job_id = n.job_id
                   and d.node_key = any(n.depends_on)
                   and d.status in ('failed', 'partial', 'expired', 'blocked', 'cancelled'))
            returning n.job_id, n.node_key"#,
    )
    .fetch_all(pool)
    .await?;
    for (job_id, node_key) in &rows {
        crate::db::jobs::append_event_on(
            pool,
            *job_id,
            Some(node_key),
            "node_blocked",
            "dependency_failed",
            serde_json::json!({ "error_code": "dependency_failed" }),
        )
        .await?;
    }
    Ok(rows.len() as u64)
}

/// Write each job's status from its nodes.
///
/// The rule this encodes is the one the release gate names: a job succeeds
/// only when EVERY required node succeeded. A required node that failed or was
/// blocked makes the job `partial`, and partial is a real terminal state — not
/// a nicer word for failed, and not a rounding of success.
///
/// Required nodes are the ones a required output depends on, recorded on the
/// job's first event when it was created.
async fn settle_finished_jobs(pool: &sqlx::PgPool) -> Result<u64> {
    Ok(sqlx::query(
        r#"with required as (
             select j.id as job_id,
                    coalesce(
                      (select array_agg(value::text)
                         from research_job_events e,
                              jsonb_array_elements_text(e.detail->'required_nodes') as value
                        where e.job_id = j.id and e.kind = 'job_queued'),
                      '{}') as keys
               from research_jobs j
              where j.status in ('queued', 'running')
                and j.cancelled_at is null
           ),
           progress as (
             select r.job_id,
                    bool_and(n.status in ('succeeded','partial','failed','expired','blocked','skipped','cancelled')) as all_done,
                    count(*) filter (where n.node_key = any(r.keys)) as required_total,
                    count(*) filter (where n.node_key = any(r.keys) and n.status = 'succeeded') as required_done
               from required r
               join research_job_nodes n on n.job_id = r.job_id
              group by r.job_id
           )
           update research_jobs j
              set status = case
                             when p.required_total = 0 then 'succeeded'
                             when p.required_done = p.required_total then 'succeeded'
                             when p.required_done = 0 then 'failed'
                             else 'partial'
                           end,
                  finished_at = coalesce(j.finished_at, now())
             from progress p
            where p.job_id = j.id
              and p.all_done
              and j.status in ('queued', 'running')"#,
    )
    .execute(pool)
    .await?
    .rows_affected())
}

/// Claim what is ready and run it.
///
/// The ready set runs CONCURRENTLY, and that is the point rather than an
/// optimisation. `max_concurrency` is part of what the buyer approved and what
/// the plan token signed: a plan whose two independent branches were priced to
/// run side by side is not honoured by running them one after the other, it is
/// only made slower. This loop used to `await` each `run_node` in turn, so a
/// single worker process executed exactly one provider call at a time across
/// every job it was advancing — at up to `SEND_TIMEOUT_SECONDS` each, one slow
/// provider stalled every other job in the pass.
///
/// Two gates keep the concurrency inside what was approved, and neither is
/// this loop:
///
///   * [`crate::job::schedule::dispatchable`] already returns at most
///     `max_concurrency - running` keys, so the set handed to us is the room
///     that is left, not the whole ready list;
///   * `claim_node` re-checks the same limit in SQL inside the claim, so two
///     workers racing produce one winner and one no-op.
///
/// Running the returned set at once therefore cannot exceed the shape. What
/// still needs bounding is the SUM across jobs — twenty jobs each entitled to
/// three calls is sixty sockets and sixty concurrent settlements — so a
/// process-wide semaphore caps total nodes in flight. It bounds this worker,
/// never the plan: a node that cannot get a permit is simply not claimed this
/// pass and is picked up by the next one, exactly as an unclaimed node always
/// was.
const MAX_NODES_IN_FLIGHT: usize = 12;

async fn dispatch(state: &Arc<AppState>, worker_id: &str) -> Result<u64> {
    let jobs = state.db.jobs_to_advance(20).await?;
    let permits = Arc::new(tokio::sync::Semaphore::new(MAX_NODES_IN_FLIGHT));
    let mut running = crate::in_flight::AdmittedTasks::new(state.in_flight.clone());

    for job in jobs {
        let nodes = state.db.job_nodes(job.job_id, &job.required_nodes).await?;
        let ready = crate::job::schedule::dispatchable(
            &nodes,
            chrono::Utc::now(),
            job.max_concurrency.max(1),
        );

        for node_key in ready {
            // Acquired BEFORE the claim, so a node is never claimed — and so
            // never leased, and never counted as running — by a worker that
            // has no capacity to actually send it.
            let Ok(permit) = Arc::clone(&permits).acquire_owned().await else {
                break;
            };
            let claimed = state
                .db
                .claim_node(
                    job.job_id,
                    &node_key,
                    worker_id,
                    chrono::Duration::seconds(LEASE_SECONDS),
                )
                .await?;
            if !claimed {
                // Another worker got there first. Not an error and not worth a
                // retry: the node is being run, just not by us.
                continue;
            }
            let state = Arc::clone(state);
            let job_id = job.job_id;
            running.spawn(async move {
                let _permit = permit;
                if let Err(error) = run_node(&state, job_id, &node_key).await {
                    error!(job = %job_id, node = %node_key, "node failed: {error}");
                }
            });
        }
    }

    // Awaited rather than detached: the pass must not return until the nodes it
    // claimed are in a terminal state, or the next pass would see leases it
    // itself holds and the loop would claim faster than it settles. A panicking
    // task is logged and its node recovers on lease expiry, the same path a
    // killed worker takes. If shutdown stops this producer, admitted nodes
    // remain independently tracked until their full receipt/finalization work
    // ends; dropping the producer must never cancel a paid node midway.
    let dispatched = running.len() as u64;
    for error in running.join().await {
        error!("research node task did not finish cleanly: {error}");
    }
    Ok(dispatched)
}

/// Run one claimed node, all the way to a terminal state.
///
/// The order is the crash-safety contract, and it is worth reading as an order:
/// bind, WRITE DOWN, send, interpret, settle. Nothing between the write and the
/// send holds a transaction open — a network call inside one would hold a row
/// lock for the length of a provider's worst day, and the lease that lock was
/// protecting would expire while it was held.
async fn run_node(state: &Arc<AppState>, job_id: uuid::Uuid, node_key: &str) -> Result<()> {
    use crate::job::attempt::{self, Outcome, PaymentStatus};
    use crate::job::bind;

    let Some(node) = state.db.runnable_node(job_id, node_key).await? else {
        // The endpoint was deactivated since the plan was approved. The
        // approval was for work that could be done, so this is refused rather
        // than attempted, and nothing is spent.
        state
            .db
            .finish_node(
                job_id,
                node_key,
                "failed",
                PaymentStatus::Released,
                Some(0),
                &[],
                &[],
                Some("source_no_longer_available"),
            )
            .await?;
        return Ok(());
    };

    // 1. Bind. A node that cannot be bound is never sent, and firing a lookup
    //    to go and find the missing value is exactly the unplanned cost the
    //    plan was supposed to have disclosed already.
    let bound = match bind::bind(&node.contract, &node.available_facts) {
        Ok(bound) => bound,
        Err(error) => {
            state
                .db
                .finish_node(
                    job_id,
                    node_key,
                    "failed",
                    PaymentStatus::Released,
                    Some(0),
                    &[],
                    &[],
                    Some("inputs_unavailable"),
                )
                .await?;
            warn!(job = %job_id, node = %node_key, "not sent: {error}");
            return Ok(());
        }
    };

    // 2. Write it down, and commit, before anything leaves.
    let payload = bound.as_json();
    let input_hash = attempt::input_hash(&payload);
    if state.db.reuse_node(job_id, node_key, &input_hash).await? {
        return Ok(());
    }
    let key = attempt::idempotency_key(&job_id.to_string(), node_key, &input_hash);
    if !state
        .db
        .record_intent(job_id, node_key, &payload, &input_hash, &key)
        .await?
    {
        // The same idempotency key already exists against different content.
        // Refused rather than sent: reusing a key across two different requests
        // is how one payment answers for another.
        state
            .db
            .finish_node(
                job_id,
                node_key,
                "failed",
                PaymentStatus::Released,
                Some(0),
                &[],
                &[],
                Some("idempotency_key_reused"),
            )
            .await?;
        return Ok(());
    }

    // 3. Send, through the gateway's own proxy — the same route, the same
    //    payment enforcement and the same settlement path any other paid call
    //    takes. A second way to pay would be a second set of rounding and a
    //    second thing to reconcile.
    let started = std::time::Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_secs(SEND_TIMEOUT_SECONDS),
        send(state, &node, &bound, &key),
    )
    .await
    .unwrap_or(Outcome::Undecided);
    info!(stage = "api_result", job = %job_id, node = node_key, api = %node.api_slug,
        latency_ms = started.elapsed().as_millis() as u64, "engine call completed");

    // 4. Interpret.
    let (payment, settled) = attempt::settle(&outcome);
    match outcome {
        Outcome::Answered { ref body, .. } => {
            finish_with_body(state, job_id, node_key, &node, body, payment, settled).await?
        }
        Outcome::NothingThere { .. } => {
            // The source answered, and the answer is that there is nothing.
            // NOT succeeded: nothing was proven, and a dependant binding
            // against nothing must not be released to run. NOT failed either:
            // it worked, and it was charged for.
            state
                .db
                .finish_node(
                    job_id,
                    node_key,
                    "partial",
                    payment,
                    settled,
                    &[],
                    &[],
                    Some("nothing_found"),
                )
                .await?;
        }
        Outcome::Refused { http_status } | Outcome::FailedResponse { http_status, .. } => {
            state
                .db
                .finish_node(
                    job_id,
                    node_key,
                    "failed",
                    payment,
                    settled,
                    &[],
                    &[],
                    Some(&format!("provider_refused_{http_status}")),
                )
                .await?;
        }
        Outcome::Undecided => {
            // The request left and we do not know what came back. It stays
            // reserved and unresolved; reconciliation decides, and no retry is
            // made here — retrying on a timeout is how one intent becomes two
            // payments.
            state
                .db
                .finish_node(
                    job_id,
                    node_key,
                    "failed",
                    PaymentStatus::Unresolved,
                    None,
                    &[],
                    &[],
                    Some("payment_undecided"),
                )
                .await?;
        }
    }
    Ok(())
}

/// Read a successful body through the node's contract and close the node.
async fn finish_with_body(
    state: &Arc<AppState>,
    job_id: uuid::Uuid,
    node_key: &str,
    node: &crate::db::jobs::RunnableNode,
    body: &serde_json::Value,
    payment: crate::job::attempt::PaymentStatus,
    settled: Option<i128>,
) -> Result<()> {
    let vocabulary = state.db.fact_vocabulary().await?;
    let extraction = match node.contract.extract(body, &vocabulary) {
        Ok(extraction) => extraction,
        Err(error) => {
            // The contract does not match what the provider sent. The call was
            // made and charged; it simply produced no fact, and saying so is
            // the honest outcome. A schema or contract error never yields a
            // valid fact.
            //
            // The reason goes on the NODE, not into a rejected-fact row.
            // `research_job_facts.fact_type` is a foreign key into
            // `fact_types`, and a contract mismatch is not about one fact type
            // — there is no slug to name. A rejection row is for "the provider
            // sent something this type does not accept"; this is "the contract
            // does not fit the response at all".
            warn!(job = %job_id, node = %node_key, "contract does not fit the response: {error}");
            state
                .db
                .finish_node(
                    job_id,
                    node_key,
                    "partial",
                    payment,
                    settled,
                    &[],
                    &[],
                    Some("contract_mismatch"),
                )
                .await?;
            return Ok(());
        }
    };

    let identity = node.identifies.clone().unwrap_or_default();
    let rejected: Vec<(String, String)> = extraction
        .rejected
        .iter()
        .map(|r| (r.fact_type.clone(), r.reason.clone()))
        .collect();

    match crate::job::ask::read(node_key, &identity, &extraction) {
        crate::job::ask::Reading::Resolved(subject) => {
            if node
                .identifies
                .as_ref()
                .and_then(|kind| node.available_facts.get(kind))
                .is_some_and(|known| subject.identity.as_ref() != Some(known))
            {
                state
                    .db
                    .finish_node(
                        job_id,
                        node_key,
                        "partial",
                        payment,
                        settled,
                        &[],
                        &[],
                        Some("source_identity_mismatch"),
                    )
                    .await?;
                return Ok(());
            }
            let complete = node
                .required_facts
                .iter()
                .all(|kind| subject.fact(kind).is_some());
            let subject_key = subject
                .identity
                .as_ref()
                .and_then(|v| v.as_str().map(str::to_string));
            let facts: Vec<(String, Option<String>, serde_json::Value, String)> = subject
                .facts
                .iter()
                .map(|fact| {
                    (
                        fact.fact_type.clone(),
                        subject_key.clone(),
                        fact.value.clone(),
                        fact.extraction_base.clone(),
                    )
                })
                .collect();
            let status = if facts.is_empty() || !complete {
                "partial"
            } else {
                "succeeded"
            };
            state
                .db
                .finish_node(
                    job_id,
                    node_key,
                    status,
                    payment,
                    settled,
                    &facts,
                    &rejected,
                    if facts.is_empty() || !complete {
                        Some("required_facts_missing")
                    } else {
                        None
                    },
                )
                .await?;
        }
        crate::job::ask::Reading::Ambiguous(question) => {
            // The lookup is paid for and its candidates are in hand. Parking
            // here is what makes resuming free.
            state
                .db
                .ask_question(job_id, node_key, &question, payment, settled)
                .await?;
            state
                .db
                .append_event(
                    job_id,
                    Some(node_key),
                    "question_asked",
                    &format!("{} possible companies", question.candidates.len()),
                    serde_json::to_value(&question)?,
                )
                .await?;
        }
        crate::job::ask::Reading::Empty => {
            state
                .db
                .finish_node(
                    job_id,
                    node_key,
                    "partial",
                    payment,
                    settled,
                    &[],
                    &rejected,
                    Some("nothing_found"),
                )
                .await?;
        }
    }
    Ok(())
}

/// Make the call, through the gateway's own proxy.
///
/// Payment calls use the native authenticated commerce router in this process.
/// There is no separate worker listener or configurable self-request origin.
/// The endpoint path with its path parameters put where they belong.
///
/// Two shapes exist in this catalogue and the difference is not cosmetic:
///
/// ```text
///   /api/v1/basisprofielen                  + 24330087
///     -> /api/v1/basisprofielen/24330087            APPEND
///
///   /api/v1/hvds/jaarrekeningen/kvknummer   + 24330087
///     -> /api/v1/hvds/jaarrekeningen/24330087       SUBSTITUTE
/// ```
///
/// The second path ends in the parameter's OWN NAME. `kvknummer` is a
/// placeholder standing where the value goes, not a literal segment to keep.
/// Appending to it built
/// `/api/v1/hvds/jaarrekeningen/kvknummer/24330087`, which KVK answers 404 —
/// and the job then reported `provider_refused_404`, which reads as "the
/// provider has no data on this company" when the truth was that we asked a
/// question no provider could answer. Three KVK endpoints are written this way
/// (`jaarrekeningen`, `naamgevingen`, `basisbedrijfsgegevens`), so a research
/// run that needed any of them could not succeed at all.
///
/// A placeholder is recognised by name rather than by decoration, because this
/// catalogue writes them bare. `{kvkNummer}` and `:kvkNummer` are honoured too,
/// so a listing that arrives written the conventional way also works.
fn request_path(endpoint_path: &str, values: &[crate::job::bind::PathValue]) -> String {
    let mut path = endpoint_path.to_string();

    for bound in values {
        let encoded = urlencoding_segment(&bound.value);
        match placeholder_at(&path, &bound.name) {
            Some(start) => path.replace_range(start.., &encoded),
            None => {
                path.push('/');
                path.push_str(&encoded);
            }
        }
    }
    path
}

/// Where the LAST segment of `path` is a placeholder for `name`, if it is.
///
/// Only the last segment: a value is appended to the end, so that is the only
/// position an unfilled placeholder can still occupy. Matching anywhere would
/// let `/company/{id}/filings` swallow a value meant for the end.
fn placeholder_at(path: &str, name: &str) -> Option<usize> {
    let start = path.rfind('/')? + 1;
    let last = &path[start..];
    let bare = last
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .or_else(|| last.strip_prefix(':'))
        .unwrap_or(last);
    // Case-insensitive: the catalogue writes `kvknummer` in the path and
    // `kvkNummer` in the schema, and they mean the same parameter.
    bare.eq_ignore_ascii_case(name).then_some(start)
}

async fn send(
    state: &Arc<AppState>,
    node: &crate::db::jobs::RunnableNode,
    bound: &crate::job::bind::WireRequest,
    idempotency_key: &str,
) -> crate::job::attempt::Outcome {
    use crate::job::attempt::Outcome;

    let base = "http://commerce.internal";
    let mut url = format!(
        "{base}/{}{}",
        node.api_slug,
        request_path(&node.endpoint_path, &bound.path_segments)
    );
    if !bound.query.is_empty() {
        let query: Vec<String> = bound
            .query
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding_segment(k), urlencoding_segment(v)))
            .collect();
        url.push('?');
        url.push_str(&query.join("&"));
    }

    let Some(service_key) = state.config.app_service_key.clone() else {
        // Without a service identity the worker cannot pay, and it must not
        // fall back to anything else. Nothing was sent.
        warn!("research worker has no service key; nothing was sent");
        return Outcome::Refused { http_status: 401 };
    };

    let method =
        reqwest::Method::from_bytes(node.method.as_bytes()).unwrap_or(reqwest::Method::GET);
    let mut call = state
        .http_client
        .request(method, &url)
        .header(crate::service_auth::HEADER_SERVICE_KEY, service_key)
        .header("idempotency-key", idempotency_key)
        .header(
            "x-apiosk-max-buyer-total-atomic",
            node.reserved_atomic.to_string(),
        )
        .header("accept", "application/json")
        .timeout(Duration::from_secs(SEND_TIMEOUT_SECONDS));
    if !bound.body.is_empty() {
        call = call.json(&serde_json::Value::Object(bound.body.clone()));
    }

    let mut attempt = 0;
    let response = loop {
        attempt += 1;
        let Some(request) = call.try_clone() else {
            return Outcome::Refused { http_status: 400 };
        };
        match crate::in_process::send(state, request).await {
            Ok(response) => break response,
            Err(error) if error.is_connect() && attempt < 3 && attempt < node.max_attempts => {
                tokio::time::sleep(Duration::from_millis(200 * attempt as u64)).await;
            }
            Err(error) if error.is_connect() => return Outcome::Refused { http_status: 503 },
            Err(_) => return Outcome::Undecided,
        }
    };

    let status = response.status().as_u16();
    // What was actually charged, from the proxy's own settlement header rather
    // than from the plan. The plan says what was approved; this says what
    // happened.
    let charged_atomic = response
        .headers()
        .get("x-apiosk-buyer-total-atomic")
        .and_then(|v| v.to_str().ok())
        .and_then(crate::job::attempt::settled_atomic);
    let body = response.json::<serde_json::Value>().await.ok();
    crate::job::attempt::http_outcome(status, charged_atomic, body)
}

/// Percent-encode everything that is not unreserved. A company name goes into
/// a query string, and `Van der Heijden & Zn.` must not become three
/// parameters.
fn urlencoding_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_company_name_survives_the_query_string() {
        assert_eq!(urlencoding_segment("Volve Capital"), "Volve%20Capital");
        assert_eq!(
            urlencoding_segment("Van der Heijden & Zn."),
            "Van%20der%20Heijden%20%26%20Zn."
        );
        assert_eq!(urlencoding_segment("30204462"), "30204462");
    }

    #[test]
    fn an_accented_name_is_encoded_as_utf8_bytes() {
        assert_eq!(urlencoding_segment("Café"), "Caf%C3%A9");
    }

    fn ledger(status: i16, headers: serde_json::Value) -> LedgerRow {
        LedgerRow {
            response_status: status,
            response_headers: headers,
        }
    }

    #[test]
    fn a_finished_ledger_row_decides_the_money_from_the_receipt_header() {
        let now = chrono::Utc::now();
        let started = now - chrono::Duration::minutes(5);
        let charged = ledger(
            200,
            serde_json::json!({"x-apiosk-buyer-total-atomic": "76086"}),
        );
        assert_eq!(
            reconcile_from_ledger(Some(&charged), started, now),
            Reconciled::Settled(76086)
        );
        // Stored header names are lowercased by the proxy, but the rule does
        // not depend on that.
        let mixed = ledger(200, serde_json::json!({"X-Apiosk-Buyer-Total-Atomic": "5"}));
        assert_eq!(
            reconcile_from_ledger(Some(&mixed), started, now),
            Reconciled::Settled(5)
        );
        // Deferred and never taken: the gateway itself says zero.
        let zero = ledger(504, serde_json::json!({"x-apiosk-buyer-total-atomic": "0"}));
        assert_eq!(
            reconcile_from_ledger(Some(&zero), started, now),
            Reconciled::Released
        );
        // Answered without deciding a charge at all: refused before payment.
        let refused = ledger(422, serde_json::json!({"content-type": "application/json"}));
        assert_eq!(
            reconcile_from_ledger(Some(&refused), started, now),
            Reconciled::Released
        );
    }

    #[test]
    fn what_the_ledger_cannot_prove_stays_undecided() {
        let now = chrono::Utc::now();
        let started = now - chrono::Duration::minutes(5);
        let in_flight = ledger(crate::db::IN_FLIGHT_STATUS, serde_json::json!({}));
        assert_eq!(
            reconcile_from_ledger(Some(&in_flight), started, now),
            Reconciled::Undecided("ledger_row_in_flight")
        );
        let garbled = ledger(
            200,
            serde_json::json!({"x-apiosk-buyer-total-atomic": "1.5"}),
        );
        assert_eq!(
            reconcile_from_ledger(Some(&garbled), started, now),
            Reconciled::Undecided("receipt_unreadable")
        );
        // Absent from a ledger that may already have forgotten it.
        let old = now - chrono::Duration::hours(crate::db::RECORD_TTL_HOURS + 1);
        assert_eq!(
            reconcile_from_ledger(None, old, now),
            Reconciled::Undecided("ledger_window_passed")
        );
    }

    #[test]
    fn a_call_the_proxy_never_claimed_was_never_made() {
        let now = chrono::Utc::now();
        let started = now - chrono::Duration::minutes(5);
        assert_eq!(
            reconcile_from_ledger(None, started, now),
            Reconciled::Released
        );
    }

    #[test]
    fn the_worker_waits_longer_than_the_proxy_and_the_lease_longer_than_the_worker() {
        // `GATEWAY_UPSTREAM_TIMEOUT_SECS` defaults to 60 in main.rs. If that
        // default moves, this must move with it, or the worker is back to
        // giving up before the proxy has spoken.
        assert!(SEND_TIMEOUT_SECONDS > 60);
        assert!(LEASE_SECONDS as u64 > SEND_TIMEOUT_SECONDS);
    }

    #[test]
    fn nothing_that_could_split_a_url_survives_unencoded() {
        for dangerous in ["?", "&", "=", "/", "#", " ", "%"] {
            let encoded = urlencoding_segment(dangerous);
            assert!(
                encoded.starts_with('%'),
                "{dangerous} must be encoded, got {encoded}"
            );
        }
    }
}

#[cfg(test)]
mod path_tests {
    use super::{placeholder_at, request_path};
    use crate::job::bind::PathValue;

    fn value(name: &str, value: &str) -> PathValue {
        PathValue {
            name: name.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn a_path_ending_in_the_parameter_name_is_substituted_not_extended() {
        // The bug that made every KVK research run fail. Appending produced
        // /api/v1/hvds/jaarrekeningen/kvknummer/24330087, KVK answered 404, and
        // the job reported `provider_refused_404` — which reads as "no data for
        // this company" when we had simply asked an impossible question.
        assert_eq!(
            request_path(
                "/api/v1/hvds/jaarrekeningen/kvknummer",
                &[value("kvkNummer", "24330087")]
            ),
            "/api/v1/hvds/jaarrekeningen/24330087"
        );
    }

    #[test]
    fn the_other_two_kvk_placeholder_paths_are_fixed_by_the_same_rule() {
        for path in [
            "/api/v1/naamgevingen/kvknummer",
            "/api/v1/hvds/basisbedrijfsgegevens/kvknummer",
        ] {
            let built = request_path(path, &[value("kvkNummer", "24330087")]);
            assert!(
                built.ends_with("/24330087") && !built.contains("kvknummer"),
                "{path} still carries its placeholder: {built}"
            );
        }
    }

    #[test]
    fn a_path_without_a_placeholder_still_appends() {
        // The shape that always worked must keep working — this is most of the
        // catalogue, and breaking it to fix the other one would be no fix.
        assert_eq!(
            request_path("/api/v1/basisprofielen", &[value("kvkNummer", "24330087")]),
            "/api/v1/basisprofielen/24330087"
        );
    }

    #[test]
    fn conventional_placeholder_spellings_are_honoured_too() {
        // This catalogue writes them bare, but a listing that arrives written
        // the usual way must not silently append.
        for path in [
            "/company/{kvkNummer}",
            "/company/:kvkNummer",
            "/company/KVKNUMMER",
        ] {
            assert_eq!(
                request_path(path, &[value("kvkNummer", "24330087")]),
                "/company/24330087",
                "{path}"
            );
        }
    }

    #[test]
    fn only_the_last_segment_can_be_a_placeholder() {
        // A value is appended at the end, so that is the only place an unfilled
        // placeholder can be. Matching mid-path would let `/company/{id}/filings`
        // swallow a value meant for the tail.
        assert_eq!(
            request_path(
                "/company/kvkNummer/filings",
                &[value("kvkNummer", "24330087")]
            ),
            "/company/kvkNummer/filings/24330087"
        );
        assert!(placeholder_at("/company/kvkNummer/filings", "kvkNummer").is_none());
    }

    #[test]
    fn a_value_is_url_encoded_wherever_it_lands() {
        assert_eq!(
            request_path("/search/term", &[value("term", "a b/c")]),
            "/search/a%20b%2Fc"
        );
        assert_eq!(
            request_path("/search", &[value("term", "a b/c")]),
            "/search/a%20b%2Fc"
        );
    }

    #[test]
    fn several_path_values_are_appended_in_order() {
        assert_eq!(
            request_path(
                "/company",
                &[value("kvkNummer", "24330087"), value("branch", "0001")]
            ),
            "/company/24330087/0001"
        );
    }

    #[test]
    fn a_placeholder_that_is_not_last_is_left_alone_and_the_value_appended() {
        // A KNOWN LIMIT, asserted so it is a decision rather than a surprise.
        //
        // Substituting any segment that matches a parameter name would handle
        // `/company/kvkNummer/branch`, but it would also rewrite a path whose
        // literal segment merely happens to share a name with a parameter —
        // turning `/search` + a parameter called `search` into `/<value>`.
        //
        // No path in this catalogue has a placeholder anywhere but the end, so
        // the narrow rule fixes every real case and invents no new failure. If
        // a listing ever needs the general form, it needs an explicit
        // `{placeholder}` in the stored path, which is honoured above.
        assert_eq!(
            request_path(
                "/company/kvkNummer/branch",
                &[value("kvkNummer", "24330087")]
            ),
            "/company/kvkNummer/branch/24330087"
        );
    }
}
