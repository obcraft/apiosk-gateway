//! The single background writer. It drains the bounded queue, batches events by
//! flush-size or flush-interval, and writes them in ONE transaction per flush.
//! Everything here is best-effort: DB errors/timeouts are logged and the batch
//! is dropped — telemetry never back-pressures or fails a request.

use std::time::Duration;

use sqlx::PgPool;
use tokio::time::{interval, timeout, MissedTickBehavior};
use tracing::warn;

use crate::telemetry::event::{
    DiscoveryEvent, LifecycleEvent, PaymentAttempt, RequestRecord, TelemetryEvent, UpstreamAttempt,
};
use crate::telemetry::queue::Receiver;
use crate::telemetry::TelemetrySettings;

/// Drain the queue forever (until the sender side is dropped), flushing in
/// batches. Runs on its own spawned task.
pub async fn run_writer(mut rx: Receiver, pool: PgPool, settings: TelemetrySettings) {
    let batch_size = settings.batch_size.max(1);
    let flush_every = Duration::from_millis(settings.flush_interval_ms.max(1));
    let db_timeout = Duration::from_millis(settings.db_timeout_ms.max(1));

    let mut buf: Vec<TelemetryEvent> = Vec::with_capacity(batch_size);
    let mut ticker = interval(flush_every);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            maybe = rx.recv() => {
                match maybe {
                    Some(ev) => {
                        buf.push(ev);
                        if buf.len() >= batch_size {
                            flush_batch(&pool, &mut buf, db_timeout).await;
                        }
                    }
                    None => {
                        // All senders dropped (shutdown) — final drain then exit.
                        if !buf.is_empty() {
                            flush_batch(&pool, &mut buf, db_timeout).await;
                        }
                        break;
                    }
                }
            }
            _ = ticker.tick() => {
                if !buf.is_empty() {
                    flush_batch(&pool, &mut buf, db_timeout).await;
                }
            }
        }
    }
}

/// Partition a batch by table and write it in one transaction, with a hard
/// timeout and a small number of retries. On persistent failure the batch is
/// dropped (logged) so the writer never stalls.
async fn flush_batch(pool: &PgPool, buf: &mut Vec<TelemetryEvent>, db_timeout: Duration) {
    let events = std::mem::take(buf);
    let count = events.len();

    let mut requests: Vec<RequestRecord> = Vec::new();
    let mut lifecycle: Vec<LifecycleEvent> = Vec::new();
    let mut payments: Vec<PaymentAttempt> = Vec::new();
    let mut upstreams: Vec<UpstreamAttempt> = Vec::new();
    let mut discovery: Vec<DiscoveryEvent> = Vec::new();

    for ev in events {
        match ev {
            TelemetryEvent::Request(r) => requests.push(*r),
            TelemetryEvent::Lifecycle(e) => lifecycle.push(e),
            TelemetryEvent::Payment(p) => payments.push(*p),
            TelemetryEvent::Upstream(u) => upstreams.push(*u),
            TelemetryEvent::Discovery(d) => discovery.push(*d),
        }
    }

    // Up to 2 attempts; the whole flush is one transaction so a retry is safe
    // (request rows use ON CONFLICT DO NOTHING).
    const MAX_ATTEMPTS: u32 = 2;
    for attempt in 1..=MAX_ATTEMPTS {
        match timeout(
            db_timeout,
            write_all(
                pool, &requests, &lifecycle, &payments, &upstreams, &discovery,
            ),
        )
        .await
        {
            Ok(Ok(())) => return,
            Ok(Err(e)) => {
                if attempt == MAX_ATTEMPTS {
                    warn!("telemetry writer dropped a batch of {count} events after {attempt} attempts: {e}");
                }
            }
            Err(_) => {
                if attempt == MAX_ATTEMPTS {
                    warn!("telemetry writer timed out flushing {count} events (db_timeout={db_timeout:?}) — batch dropped");
                }
            }
        }
    }
}

async fn write_all(
    pool: &PgPool,
    requests: &[RequestRecord],
    lifecycle: &[LifecycleEvent],
    payments: &[PaymentAttempt],
    upstreams: &[UpstreamAttempt],
    discovery: &[DiscoveryEvent],
) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;

    for r in requests {
        insert_request(&mut tx, r).await?;
    }
    for e in lifecycle {
        insert_lifecycle(&mut tx, e).await?;
    }
    for p in payments {
        insert_payment(&mut tx, p).await?;
    }
    for u in upstreams {
        insert_upstream(&mut tx, u).await?;
    }
    for d in discovery {
        insert_discovery(&mut tx, d).await?;
    }

    tx.commit().await
}

fn meta_str(v: &serde_json::Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string())
}

async fn insert_request(tx: &mut sqlx::PgConnection, r: &RequestRecord) -> sqlx::Result<()> {
    sqlx::query(
        r#"INSERT INTO gateway_requests_v2
           (request_id, trace_id, parent_request_id, occurred_at, completed_at,
            method, host, path, route_template, query_keys, http_version, scheme,
            api_id, endpoint_id, api_slug, request_kind,
            status_code, outcome, failure_stage, error_code, retryable,
            latency_ms, request_bytes, response_bytes, request_content_type, response_content_type,
            time_to_first_byte_ms, client_disconnected,
            ip_address, country_code, region, asn, network_name, network_type,
            user_agent, agent_name, agent_version, agent_framework, sdk_name, sdk_version, client_kind,
            user_id, session_id, client_id, managed_wallet_id, agent_connect_token_id, wallet_address,
            referrer, origin, metadata)
           VALUES
           ($1,$2,$3, coalesce($4::timestamptz, now()), $5,
            $6,$7,$8,$9,$10,$11,$12,
            $13,$14,$15,$16,
            $17,$18,$19,$20,$21,
            $22,$23,$24,$25,$26,
            $27,$28,
            $29::inet,$30,$31,$32,$33,$34,
            $35,$36,$37,$38,$39,$40,$41,
            $42,$43,$44,$45,$46,$47,
            $48,$49,$50::jsonb)
           ON CONFLICT (request_id) DO NOTHING"#,
    )
    .bind(r.request_id)
    .bind(r.trace_id)
    .bind(r.parent_request_id)
    .bind(r.occurred_at)
    .bind(r.completed_at)
    .bind(&r.method)
    .bind(&r.host)
    .bind(&r.path)
    .bind(&r.route_template)
    .bind(&r.query_keys)
    .bind(&r.http_version)
    .bind(&r.scheme)
    .bind(r.api_id)
    .bind(r.endpoint_id)
    .bind(&r.api_slug)
    .bind(r.request_kind.map(|k| k.as_str()))
    .bind(r.status_code)
    .bind(r.outcome.map(|o| o.as_str()))
    .bind(r.failure_stage.map(|s| s.as_str()))
    .bind(&r.error_code)
    .bind(r.retryable)
    .bind(r.latency_ms)
    .bind(r.request_bytes)
    .bind(r.response_bytes)
    .bind(&r.request_content_type)
    .bind(&r.response_content_type)
    .bind(r.time_to_first_byte_ms)
    .bind(r.client_disconnected)
    .bind(&r.ip_address)
    .bind(&r.country_code)
    .bind(&r.region)
    .bind(r.asn)
    .bind(&r.network_name)
    .bind(&r.network_type)
    .bind(&r.user_agent)
    .bind(&r.agent_name)
    .bind(&r.agent_version)
    .bind(&r.agent_framework)
    .bind(&r.sdk_name)
    .bind(&r.sdk_version)
    .bind(&r.client_kind)
    .bind(r.user_id)
    .bind(r.session_id)
    .bind(&r.client_id)
    .bind(r.managed_wallet_id)
    .bind(r.agent_connect_token_id)
    .bind(&r.wallet_address)
    .bind(&r.referrer)
    .bind(&r.origin)
    .bind(meta_str(&r.metadata))
    .execute(&mut *tx)
    .await?;
    Ok(())
}

async fn insert_lifecycle(tx: &mut sqlx::PgConnection, e: &LifecycleEvent) -> sqlx::Result<()> {
    sqlx::query(
        r#"INSERT INTO gateway_request_events
           (request_id, occurred_at, event_type, duration_ms, status, error_code, metadata)
           VALUES ($1, coalesce($2::timestamptz, now()), $3, $4, $5, $6, $7::jsonb)"#,
    )
    .bind(e.request_id)
    .bind(e.occurred_at)
    .bind(&e.event_type)
    .bind(e.duration_ms)
    .bind(&e.status)
    .bind(&e.error_code)
    .bind(meta_str(&e.metadata))
    .execute(&mut *tx)
    .await?;
    Ok(())
}

async fn insert_payment(tx: &mut sqlx::PgConnection, p: &PaymentAttempt) -> sqlx::Result<()> {
    sqlx::query(
        r#"INSERT INTO gateway_payment_attempts
           (request_id, attempt_number, rail_requested, rail_selected, x402_version,
            network, chain, token, offered_amount, authorized_amount, wallet_address,
            proof_fingerprint, verification_outcome, settlement_outcome, facilitator_latency_ms,
            facilitator_status, tx_hash, failure_code, fallback_rail, started_at, completed_at, metadata)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22::jsonb)"#,
    )
    .bind(p.request_id)
    .bind(p.attempt_number)
    .bind(&p.rail_requested)
    .bind(&p.rail_selected)
    .bind(p.x402_version)
    .bind(&p.network)
    .bind(&p.chain)
    .bind(&p.token)
    .bind(&p.offered_amount)
    .bind(&p.authorized_amount)
    .bind(&p.wallet_address)
    .bind(&p.proof_fingerprint)
    .bind(&p.verification_outcome)
    .bind(&p.settlement_outcome)
    .bind(p.facilitator_latency_ms)
    .bind(&p.facilitator_status)
    .bind(&p.tx_hash)
    .bind(&p.failure_code)
    .bind(&p.fallback_rail)
    .bind(p.started_at)
    .bind(p.completed_at)
    .bind(meta_str(&p.metadata))
    .execute(&mut *tx)
    .await?;
    Ok(())
}

async fn insert_upstream(tx: &mut sqlx::PgConnection, u: &UpstreamAttempt) -> sqlx::Result<()> {
    sqlx::query(
        r#"INSERT INTO gateway_upstream_attempts
           (request_id, api_id, endpoint_id, attempt_number, upstream_host, upstream_status,
            connect_ms, tls_ms, time_to_first_byte_ms, total_ms, request_bytes, response_bytes,
            error_category, is_resell, upstream_x402_amount, retry_reason, started_at, completed_at, metadata)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19::jsonb)"#,
    )
    .bind(u.request_id)
    .bind(u.api_id)
    .bind(u.endpoint_id)
    .bind(u.attempt_number)
    .bind(&u.upstream_host)
    .bind(u.upstream_status)
    .bind(u.connect_ms)
    .bind(u.tls_ms)
    .bind(u.time_to_first_byte_ms)
    .bind(u.total_ms)
    .bind(u.request_bytes)
    .bind(u.response_bytes)
    .bind(&u.error_category)
    .bind(u.is_resell)
    .bind(&u.upstream_x402_amount)
    .bind(&u.retry_reason)
    .bind(u.started_at)
    .bind(u.completed_at)
    .bind(meta_str(&u.metadata))
    .execute(&mut *tx)
    .await?;
    Ok(())
}

async fn insert_discovery(tx: &mut sqlx::PgConnection, d: &DiscoveryEvent) -> sqlx::Result<()> {
    sqlx::query(
        r#"INSERT INTO gateway_discovery_events
           (request_id, occurred_at, resource, method, status_code, ip_address,
            client_fingerprint, user_agent, crawler_class, cache_result, latency_ms, response_bytes, metadata)
           VALUES ($1, coalesce($2::timestamptz, now()), $3, $4, $5, $6::inet, $7, $8, $9, $10, $11, $12, $13::jsonb)"#,
    )
    .bind(d.request_id)
    .bind(d.occurred_at)
    .bind(&d.resource)
    .bind(&d.method)
    .bind(d.status_code)
    .bind(&d.ip_address)
    .bind(&d.client_fingerprint)
    .bind(&d.user_agent)
    .bind(&d.crawler_class)
    .bind(&d.cache_result)
    .bind(d.latency_ms)
    .bind(d.response_bytes)
    .bind(meta_str(&d.metadata))
    .execute(&mut *tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::event::LifecycleEvent;
    use sqlx::postgres::PgPoolOptions;
    use uuid::Uuid;

    #[tokio::test]
    async fn db_unavailable_drops_batch_without_panicking() {
        // Lazy pool to a refused port → every flush attempt errors fast. The
        // writer must swallow it, empty the buffer, and return — never panic or
        // hang (this is what guarantees a DB outage can't affect a response).
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_millis(100))
            .connect_lazy("postgresql://nouser:nopass@127.0.0.1:1/nodb")
            .expect("lazy pool builds");

        let mut buf = vec![TelemetryEvent::Lifecycle(LifecycleEvent {
            request_id: Uuid::nil(),
            occurred_at: None,
            event_type: "request_received".into(),
            duration_ms: None,
            status: None,
            error_code: None,
            metadata: serde_json::json!({}),
        })];

        flush_batch(&pool, &mut buf, Duration::from_millis(50)).await;
        // Batch was taken and dropped; no panic, no hang.
        assert!(buf.is_empty());
    }
}
