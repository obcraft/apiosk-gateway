//! Storage for the connect-token handoff (migration 077).
//!
//! Two reads and two writes, all of them boring on purpose: the interesting
//! decisions — who may be redirected where, whether a PKCE verifier matches —
//! live in `crate::connect::oauth`, and none of them belong in SQL.
//!
//! The one thing this module does own is the single-use guarantee. Redemption is
//! a `SELECT … FOR UPDATE` followed by a wipe inside one transaction, so two
//! platforms racing on the same code produce exactly one winner and one
//! `invalid_grant` — not two live credentials.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::Database;

/// A registered platform, as the authorize step needs to see it.
#[derive(Debug, Clone)]
pub struct ConnectOauthClient {
    pub client_id: String,
    pub display_name: String,
    pub redirect_uri_patterns: Vec<String>,
    pub requires_pkce: bool,
    pub homepage_url: Option<String>,
}

/// Everything parked under one authorization code.
#[derive(Debug, Clone)]
pub struct ParkedConnectToken {
    pub client_id: String,
    pub user_id: Uuid,
    pub redirect_uri: String,
    pub connect_token: String,
    pub gateway_url: String,
    pub connection_id: Option<Uuid>,
    pub connection_label: Option<String>,
    pub token_expires_at: Option<DateTime<Utc>>,
    pub code_challenge: Option<String>,
    /// What the connection is bound to, as the buyer was shown it at consent.
    /// Nullable throughout: every client that parked before migration 082 has
    /// none of it, and the exchange simply omits the fields.
    pub wallet_id: Option<Uuid>,
    pub policy_id: Option<Uuid>,
    pub limits_snapshot: Option<serde_json::Value>,
}

/// What to park. Separate from [`ParkedConnectToken`] because the caller does
/// not choose the code — it hands over the hash and keeps the raw value.
pub struct ParkConnectToken<'a> {
    pub code_hash: &'a str,
    pub client_id: &'a str,
    pub user_id: Uuid,
    pub redirect_uri: &'a str,
    pub connect_token: &'a str,
    pub gateway_url: &'a str,
    pub connection_id: Option<Uuid>,
    pub connection_label: Option<&'a str>,
    pub token_expires_at: Option<DateTime<Utc>>,
    pub code_challenge: Option<&'a str>,
    pub expires_at: DateTime<Utc>,
    pub wallet_id: Option<Uuid>,
    pub policy_id: Option<Uuid>,
    pub limits_snapshot: Option<&'a serde_json::Value>,
}

/// Postgres `undefined_column` (42703): the schema genuinely lacks the column,
/// as opposed to the statement failing for a transient reason.
fn is_undefined_column(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|db_err| db_err.code())
        .map(|code| code == "42703")
        .unwrap_or(false)
}

impl Database {
    /// Look up a registered client. `None` for unknown or disabled — the caller
    /// must not distinguish the two in its error response.
    #[allow(clippy::type_complexity)]
    pub async fn get_connect_oauth_client(
        &self,
        client_id: &str,
    ) -> Result<Option<ConnectOauthClient>> {
        let row: Option<(String, String, Vec<String>, bool, Option<String>)> = sqlx::query_as(
            r#"SELECT client_id, display_name, redirect_uri_patterns, requires_pkce, homepage_url
                 FROM connect_oauth_clients
                WHERE client_id = $1
                  AND disabled_at IS NULL
                LIMIT 1"#,
        )
        .bind(client_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(
            |(client_id, display_name, redirect_uri_patterns, requires_pkce, homepage_url)| {
                ConnectOauthClient {
                    client_id,
                    display_name,
                    redirect_uri_patterns,
                    requires_pkce,
                    homepage_url,
                }
            },
        ))
    }

    /// Park a freshly-minted connect token under an authorization code.
    ///
    /// The plaintext token is passed straight into `encrypt_provider_secret` in
    /// the statement itself, so it is never written to a row literal, never
    /// appears in a query log, and is at rest under the same key as every other
    /// provider secret.
    /// Park a freshly-minted token under a single-use code.
    ///
    /// Migration 083's three columns (`wallet_id`, `policy_id`,
    /// `limits_snapshot`) are OPTIONAL here, and that is load-bearing rather
    /// than tidy. This binary can ship ahead of the migration — it did, once —
    /// and a handoff that fails because a nice-to-have column is missing would
    /// break n8n, Make and Zapier for a field none of them send. So the write is
    /// tried with them, and on `undefined_column` it is retried without, latched
    /// so the fallback costs one failed statement per process rather than one
    /// per handoff.
    ///
    /// Same resilience contract as `usage::get_connect_token_allowed_domains`,
    /// applied to a write instead of a read.
    pub async fn park_connect_oauth_token(&self, input: ParkConnectToken<'_>) -> Result<()> {
        use std::sync::atomic::{AtomicBool, Ordering};
        static COLUMNS_ABSENT: AtomicBool = AtomicBool::new(false);

        const HEAD: &str = "INSERT INTO connect_oauth_codes
                 (code_hash, client_id, user_id, redirect_uri, connect_token_enc,
                  gateway_url, connection_id, connection_label, token_expires_at,
                  code_challenge, code_challenge_method, expires_at";
        const VALUES: &str = ") VALUES ($1, $2, $3, $4, public.encrypt_provider_secret($5),
                       $6, $7, $8, $9,
                       $10, CASE WHEN $10 IS NULL THEN NULL ELSE 'S256' END, $11";

        let with_context = !COLUMNS_ABSENT.load(Ordering::Relaxed);
        let sql = if with_context {
            format!("{HEAD}, wallet_id, policy_id, limits_snapshot{VALUES}, $12, $13, $14)")
        } else {
            format!("{HEAD}{VALUES})")
        };

        let mut query = sqlx::query(&sql)
            .bind(input.code_hash)
            .bind(input.client_id)
            .bind(input.user_id)
            .bind(input.redirect_uri)
            .bind(input.connect_token)
            .bind(input.gateway_url)
            .bind(input.connection_id)
            .bind(input.connection_label)
            .bind(input.token_expires_at)
            .bind(input.code_challenge)
            .bind(input.expires_at);
        if with_context {
            query = query
                .bind(input.wallet_id)
                .bind(input.policy_id)
                .bind(input.limits_snapshot);
        }

        match query.execute(&self.pool).await {
            Ok(_) => Ok(()),
            Err(err) if with_context && is_undefined_column(&err) => {
                if !COLUMNS_ABSENT.swap(true, Ordering::Relaxed) {
                    tracing::info!(
                        "connect_oauth_codes is missing wallet_id/policy_id/limits_snapshot; \
                         parking without them until migration 083 is applied. Handoffs work; \
                         the collecting client just has to ask what it is connected to."
                    );
                }
                // Retry once, without them. A handoff must not fail over a field
                // nobody asked for.
                sqlx::query(&format!("{HEAD}{VALUES})"))
                    .bind(input.code_hash)
                    .bind(input.client_id)
                    .bind(input.user_id)
                    .bind(input.redirect_uri)
                    .bind(input.connect_token)
                    .bind(input.gateway_url)
                    .bind(input.connection_id)
                    .bind(input.connection_label)
                    .bind(input.token_expires_at)
                    .bind(input.code_challenge)
                    .bind(input.expires_at)
                    .execute(&self.pool)
                    .await?;
                Ok(())
            }
            Err(err) => Err(err.into()),
        }
    }

    /// Claim a code exactly once.
    ///
    /// `FOR UPDATE` on the live row is what makes this safe under concurrency:
    /// a second exchange arriving mid-flight blocks, then re-reads a row whose
    /// `consumed_at` is set and fails the filter. Returns `None` for unknown,
    /// expired, or already-redeemed — all three are `invalid_grant` to the
    /// caller, and telling them apart would be a probing oracle.
    pub async fn redeem_connect_oauth_code(
        &self,
        code_hash: &str,
    ) -> Result<Option<ParkedConnectToken>> {
        let mut tx = self.pool.begin().await?;

        #[allow(clippy::type_complexity)]
        let row: Option<(
            String,
            Uuid,
            String,
            Option<String>,
            String,
            Option<Uuid>,
            Option<String>,
            Option<DateTime<Utc>>,
            Option<String>,
            // Read as text through to_jsonb so the statement works whether or
            // not migration 083 added the columns; parsed back below.
            Option<String>,
            Option<String>,
            Option<serde_json::Value>,
        )> = sqlx::query_as(
            r#"SELECT client_id,
                      user_id,
                      redirect_uri,
                      public.decrypt_provider_secret(connect_token_enc),
                      gateway_url,
                      connection_id,
                      connection_label,
                      token_expires_at,
                      code_challenge,
                      -- Same optionality as the INSERT above: a gateway running
                      -- ahead of migration 083 still redeems codes, it just has
                      -- nothing extra to hand back.
                      to_jsonb(connect_oauth_codes) ->> 'wallet_id'     AS wallet_id_text,
                      to_jsonb(connect_oauth_codes) ->> 'policy_id'     AS policy_id_text,
                      to_jsonb(connect_oauth_codes) ->  'limits_snapshot' AS limits_snapshot
                 FROM connect_oauth_codes
                WHERE code_hash = $1
                  AND consumed_at IS NULL
                  AND expires_at > now()
                  FOR UPDATE"#,
        )
        .bind(code_hash)
        .fetch_optional(&mut *tx)
        .await?;

        let Some((
            client_id,
            user_id,
            redirect_uri,
            connect_token,
            gateway_url,
            connection_id,
            connection_label,
            token_expires_at,
            code_challenge,
            wallet_id_text,
            policy_id_text,
            limits_snapshot,
        )) = row
        else {
            // Nothing claimed — release the lock without writing.
            tx.rollback().await.ok();
            return Ok(None);
        };

        // A row that survived the filter but won't decrypt means the encryption
        // key rotated out from under it. Burn the code rather than leaving a
        // row that fails forever while still holding ciphertext.
        let Some(connect_token) = connect_token else {
            sqlx::query(
                "UPDATE connect_oauth_codes
                    SET consumed_at = now(), connect_token_enc = NULL
                  WHERE code_hash = $1",
            )
            .bind(code_hash)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Err(anyhow!("parked connect token could not be decrypted"));
        };

        // Consume + destroy in one write. The 077 check constraint requires both
        // to move together, which is what stops a redeemed code from lingering
        // with a live credential inside it.
        sqlx::query(
            "UPDATE connect_oauth_codes
                SET consumed_at = now(), connect_token_enc = NULL
              WHERE code_hash = $1",
        )
        .bind(code_hash)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok(Some(ParkedConnectToken {
            client_id,
            user_id,
            redirect_uri,
            connect_token,
            gateway_url,
            connection_id,
            connection_label,
            token_expires_at,
            code_challenge,
            wallet_id: wallet_id_text.as_deref().and_then(|v| v.parse().ok()),
            policy_id: policy_id_text.as_deref().and_then(|v| v.parse().ok()),
            limits_snapshot,
        }))
    }
}
