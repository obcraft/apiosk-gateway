use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

use super::Database;

/// What we cached on the first call so a duplicate Idempotency-Key can be
/// replayed without re-charging. Mirrors the response we sent (status,
/// filtered headers, body) plus a request fingerprint so a leaked key can't
/// be used to swap the payload.
///
/// A row with `response_status == 0` ([`IN_FLIGHT_STATUS`]) is a **claim**: the
/// first attempt has started and not yet finished. It exists so a concurrent
/// duplicate — the same key arriving while the first attempt is still paying
/// the provider — is answered `409 in_progress` instead of run a second time.
#[derive(Debug, Clone, FromRow)]
pub struct IdempotencyRecord {
    pub request_fingerprint: String,
    pub response_status: i16,
    pub response_headers: serde_json::Value,
    pub response_body: Vec<u8>,
    pub created_at: DateTime<Utc>,
}

impl IdempotencyRecord {
    /// True while the first attempt under this key is still running.
    pub fn is_in_flight(&self) -> bool {
        self.response_status == IN_FLIGHT_STATUS
    }
}

/// The status a claimed-but-unfinished row carries. No HTTP response has
/// status 0, so a finished record can never be mistaken for a claim.
pub const IN_FLIGHT_STATUS: i16 = 0;

// An unfinished claim never expires automatically: elapsed time cannot prove
// that an interrupted request was free. Reconciliation must resolve it.

/// How long a finished record replays for. Past it the key is free again, so a
/// retry gets a fresh charge rather than a stale receipt.
pub const RECORD_TTL_HOURS: i64 = 24;

/// What a claim attempt found under `(token_id, key)`.
#[derive(Debug)]
pub enum IdempotencyClaim {
    /// This request now owns the key; run the handler, then finalise or release.
    Claimed,
    /// A finished record with the same fingerprint — replay it, run nothing.
    Finished(IdempotencyRecord),
    /// A first attempt is still running — answer `409 in_progress`.
    InFlight,
    /// The key was already used for a different method/path.
    Conflict,
}

impl Database {
    /// Claim `(token_id, key)` for a request that is about to run.
    ///
    /// Inserts an in-flight row; if one already exists the outcome is decided
    /// from it — replay, `in_progress`, or conflict. Only a finished record
    /// older than [`RECORD_TTL_HOURS`] may be reused. An unfinished request
    /// remains held until its payment outcome is reconciled.
    pub async fn claim_idempotency_key(
        &self,
        token_id: Uuid,
        key: &str,
        request_fingerprint: &str,
    ) -> Result<IdempotencyClaim> {
        let inserted = sqlx::query(
            r#"
            INSERT INTO gateway_idempotency_keys
                (token_id, key, request_fingerprint, response_status,
                 response_headers, response_body)
            VALUES ($1, $2, $3, $4, '{}'::jsonb, ''::bytea)
            ON CONFLICT (token_id, key) DO NOTHING
            "#,
        )
        .bind(token_id)
        .bind(key)
        .bind(request_fingerprint)
        .bind(IN_FLIGHT_STATUS)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if inserted > 0 {
            return Ok(IdempotencyClaim::Claimed);
        }

        let existing: Option<IdempotencyRecord> = sqlx::query_as(
            r#"
            SELECT request_fingerprint,
                   response_status,
                   response_headers,
                   response_body,
                   created_at
              FROM gateway_idempotency_keys
             WHERE token_id = $1
               AND key = $2
            "#,
        )
        .bind(token_id)
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;
        let Some(existing) = existing else {
            // Pruned between the insert and the read; the retry will claim.
            return Ok(IdempotencyClaim::InFlight);
        };
        if existing.request_fingerprint != request_fingerprint {
            return Ok(IdempotencyClaim::Conflict);
        }
        let age = Utc::now() - existing.created_at;
        let reusable = !existing.is_in_flight() && age.num_hours() >= RECORD_TTL_HOURS;
        if !reusable {
            return Ok(if existing.is_in_flight() {
                IdempotencyClaim::InFlight
            } else {
                IdempotencyClaim::Finished(existing)
            });
        }

        // Reuse only an expired finished record. Repeat both predicates so a
        // concurrent retry cannot take a claim this one just won.
        let taken = sqlx::query(
            r#"
            UPDATE gateway_idempotency_keys
               SET request_fingerprint = $3,
                   response_status = $4,
                   response_headers = '{}'::jsonb,
                   response_body = ''::bytea,
                   created_at = now()
             WHERE token_id = $1
               AND key = $2
               AND response_status <> $4
               AND created_at < now() - make_interval(hours => $5)
            "#,
        )
        .bind(token_id)
        .bind(key)
        .bind(request_fingerprint)
        .bind(IN_FLIGHT_STATUS)
        .bind(RECORD_TTL_HOURS as i32)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(if taken > 0 {
            IdempotencyClaim::Claimed
        } else {
            IdempotencyClaim::InFlight
        })
    }

    /// Turn a claim into a finished record. Returns `Ok(false)` when no claim
    /// was waiting (released, taken over, or never made) — the response is
    /// still served live; it just will not replay.
    pub async fn finalize_idempotency_record(
        &self,
        token_id: Uuid,
        key: &str,
        response_status: u16,
        response_headers: serde_json::Value,
        response_body: &[u8],
    ) -> Result<bool> {
        let updated = sqlx::query(
            r#"
            UPDATE gateway_idempotency_keys
               SET response_status = $3,
                   response_headers = $4,
                   response_body = $5
             WHERE token_id = $1
               AND key = $2
               AND response_status = $6
            "#,
        )
        .bind(token_id)
        .bind(key)
        .bind(i16::try_from(response_status).unwrap_or(0))
        .bind(response_headers)
        .bind(response_body)
        .bind(IN_FLIGHT_STATUS)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(updated > 0)
    }

    /// Give a claim back without recording an outcome — for refusals made
    /// before any money moved (a cap, a rate limit, an unknown endpoint), which
    /// the caller should be free to retry under the same key once fixed.
    pub async fn release_idempotency_claim(&self, token_id: Uuid, key: &str) -> Result<bool> {
        let deleted = sqlx::query(
            r#"
            DELETE FROM gateway_idempotency_keys
             WHERE token_id = $1
               AND key = $2
               AND response_status = $3
            "#,
        )
        .bind(token_id)
        .bind(key)
        .bind(IN_FLIGHT_STATUS)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(deleted > 0)
    }

    /// Lookup a previously stored idempotency record. Rows older than the TTL
    /// (24h) are treated as misses so a key gets a fresh charge — never a
    /// stale replay — once the window has elapsed.
    pub async fn get_idempotency_record(
        &self,
        token_id: Uuid,
        key: &str,
    ) -> Result<Option<IdempotencyRecord>> {
        let row: Option<IdempotencyRecord> = sqlx::query_as(
            r#"
            SELECT request_fingerprint,
                   response_status,
                   response_headers,
                   response_body,
                   created_at
              FROM gateway_idempotency_keys
             WHERE token_id = $1
               AND key = $2
               AND created_at > now() - interval '24 hours'
            "#,
        )
        .bind(token_id)
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Persist the response under (token_id, key). Returns `Ok(true)` when
    /// the row was newly inserted, `Ok(false)` when the key already existed
    /// (a concurrent first request beat us to it — caller should treat that
    /// like a successful save and trust the existing record on the next
    /// retry).
    pub async fn store_idempotency_record(
        &self,
        token_id: Uuid,
        key: &str,
        request_fingerprint: &str,
        response_status: u16,
        response_headers: serde_json::Value,
        response_body: &[u8],
    ) -> Result<bool> {
        let inserted = sqlx::query(
            r#"
            INSERT INTO gateway_idempotency_keys
                (token_id, key, request_fingerprint, response_status,
                 response_headers, response_body)
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (token_id, key) DO NOTHING
            "#,
        )
        .bind(token_id)
        .bind(key)
        .bind(request_fingerprint)
        .bind(i16::try_from(response_status).unwrap_or(0))
        .bind(response_headers)
        .bind(response_body)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(inserted > 0)
    }

    /// Delete rows older than the TTL. Intended to be called from the
    /// reconciliation worker on its existing cron. Returns the number of
    /// rows pruned.
    pub async fn prune_idempotency_keys(&self) -> Result<u64> {
        let pruned = sqlx::query(
            r#"
            DELETE FROM gateway_idempotency_keys
             WHERE response_status <> 0 AND created_at < now() - interval '24 hours'
            "#,
        )
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(pruned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(response_status: i16, age_seconds: i64) -> IdempotencyRecord {
        IdempotencyRecord {
            request_fingerprint: "fp".to_string(),
            response_status,
            response_headers: serde_json::json!({}),
            response_body: Vec::new(),
            created_at: Utc::now() - chrono::Duration::seconds(age_seconds),
        }
    }

    /// A claim is a row with status 0 and nothing else is: no HTTP response
    /// has that status, so a finished record can never read as in flight and
    /// be answered `in_progress` forever.
    #[test]
    fn a_claim_is_status_zero_and_every_real_status_is_finished() {
        assert!(record(IN_FLIGHT_STATUS, 0).is_in_flight());
        for finished in [200i16, 201, 400, 402, 409, 429, 500, 502, 504] {
            assert!(!record(finished, 0).is_in_flight(), "{finished}");
        }
    }

    #[test]
    fn unresolved_claims_remain_in_flight_beyond_the_finished_record_ttl() {
        assert!(record(IN_FLIGHT_STATUS, 7 * 24 * 3600).is_in_flight());
    }
}
