use super::*;

/// Lifecycle of a `settlements` row (migration 065).
///
/// Only the managed-wallet prepaid rail needs the full lifecycle: there the
/// gateway commits the ledger and *then* calls `ApioskSettlement.settle()`, so
/// between those two points the money has not moved yet. On the x402 rail the
/// facilitator has already settled before the gateway records anything, so
/// those rows are born [`Settled`](Self::Settled).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettlementStatus {
    /// Ledger committed, on-chain settle not yet confirmed. Nothing is credited
    /// to the provider until it resolves.
    Pending,
    /// Money moved. The provider's earning is booked.
    Settled,
    /// The settle attempt did not move money. Nothing is credited — booking an
    /// earning here is exactly the invisible shortfall audit K3 describes.
    Failed,
}

impl SettlementStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Settled => "settled",
            Self::Failed => "failed",
        }
    }

    /// Whether this status books provider revenue. Only a confirmed settle does.
    pub fn credits_developer(self) -> bool {
        matches!(self, Self::Settled)
    }
}

#[derive(Debug, sqlx::FromRow)]
struct ExistingCreditChargeRow {
    gateway_usage_id: i64,
    amount_eur: BigDecimal,
    credits_remaining: i32,
}

#[derive(sqlx::FromRow)]
struct ConnectTokenHeader {
    token_id: Uuid,
    user_id: Uuid,
    token_prefix: String,
    token_name: Option<String>,
    expires_at: chrono::DateTime<chrono::Utc>,
    revoked_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl Database {
    #[allow(clippy::too_many_arguments)]
    pub async fn log_usage(
        &self,
        api_id: Uuid,
        endpoint_id: Option<Uuid>,
        paid: bool,
        amount: Option<BigDecimal>,
        network_token_id: Option<Uuid>,
        wallet_address: Option<String>,
        request_id: Option<Uuid>,
        session_id: Option<Uuid>,
        managed_wallet_id: Option<Uuid>,
        client_id: Option<&str>,
        agent_connect_token_id: Option<Uuid>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO gateway_usage (api_id, endpoint_id, paid, amount, network_token_id, wallet_address, request_id, session_id, managed_wallet_id, client_id, agent_connect_token_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"
        )
        .bind(api_id)
        .bind(endpoint_id)
        .bind(paid)
        .bind(amount)
        .bind(network_token_id)
        .bind(wallet_address)
        .bind(request_id)
        .bind(session_id)
        .bind(managed_wallet_id)
        .bind(client_id)
        .bind(agent_connect_token_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn lookup_agent_connect_token_by_hash(
        &self,
        token_hash: &str,
    ) -> Result<AgentConnectTokenLookup> {
        // Note: we deliberately do NOT filter on revoked_at / expires_at here
        // so we can distinguish "unknown token", "revoked", and "expired" in
        // the gateway's 401 response. The lookup is by SHA-256 hash, so it is
        // not enumerable.
        let header = sqlx::query_as::<_, ConnectTokenHeader>(
            r#"SELECT
                 t.id AS token_id,
                 t.user_id,
                 t.token_prefix,
                 t.token_name,
                 t.expires_at,
                 t.revoked_at
               FROM agent_wallet_connect_tokens t
               WHERE t.token_hash = $1
               LIMIT 1"#,
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await?;

        self.finish_connect_token(header).await
    }

    /// The treasury's connect token, found by its id or its visible prefix —
    /// what `APIOSK_TREASURY_CONNECT_TOKEN_REF` holds. Service-key callers
    /// (`crate::service_auth`) spend through this token; nobody ever needs the
    /// raw token for it. Revoked and expired rows are reported as such, exactly
    /// like the hash lookup, so a treasury token that lapsed is a clear 401 and
    /// not a silent switch to some other token.
    pub async fn lookup_agent_connect_token_by_ref(
        &self,
        reference: &str,
    ) -> Result<AgentConnectTokenLookup> {
        let reference = reference.trim();
        let by_id = Uuid::parse_str(reference).ok();
        let header = sqlx::query_as::<_, ConnectTokenHeader>(
            r#"SELECT
                 t.id AS token_id,
                 t.user_id,
                 t.token_prefix,
                 t.token_name,
                 t.expires_at,
                 t.revoked_at
               FROM agent_wallet_connect_tokens t
               WHERE ($1::uuid IS NOT NULL AND t.id = $1::uuid)
                  OR ($1::uuid IS NULL AND t.token_prefix = $2)
               ORDER BY (t.revoked_at IS NULL) DESC, t.expires_at DESC
               LIMIT 1"#,
        )
        .bind(by_id)
        .bind(reference)
        .fetch_optional(&self.pool)
        .await?;
        self.finish_connect_token(header).await
    }

    /// The half of a connect-token lookup that does not depend on how the row
    /// was found: revoked/expired verdicts, then the wallets, domains and
    /// policy that make it an [`AgentConnectToken`].
    async fn finish_connect_token(
        &self,
        header: Option<ConnectTokenHeader>,
    ) -> Result<AgentConnectTokenLookup> {
        let Some(header) = header else {
            return Ok(AgentConnectTokenLookup::Unknown);
        };

        if header.revoked_at.is_some() {
            return Ok(AgentConnectTokenLookup::Revoked {
                token_prefix: header.token_prefix,
            });
        }

        if header.expires_at <= chrono::Utc::now() {
            return Ok(AgentConnectTokenLookup::Expired {
                token_prefix: header.token_prefix,
                expires_at: header.expires_at,
            });
        }

        let wallets = sqlx::query_as::<_, AgentConnectTokenWallet>(
            r#"SELECT
                 w.id AS wallet_id,
                 w.wallet_address,
                 w.status AS wallet_status,
                 COALESCE(ctw.daily_limit_usdc, w.daily_limit_usdc) AS daily_limit_usdc,
                 COALESCE(ctw.per_tx_limit_usdc, w.per_tx_limit_usdc) AS per_tx_limit_usdc
               FROM agent_wallet_connect_token_wallets ctw
               JOIN agent_wallets w ON w.id = ctw.wallet_id
               WHERE ctw.token_id = $1
               ORDER BY w.created_at ASC"#,
        )
        .bind(header.token_id)
        .fetch_all(&self.pool)
        .await?;

        let allowed_domains = self
            .get_connect_token_allowed_domains(header.token_id)
            .await;

        let policy_id = self.get_connect_token_policy_id(header.token_id).await;

        Ok(AgentConnectTokenLookup::Active(AgentConnectToken {
            token_id: header.token_id,
            user_id: header.user_id,
            wallets,
            allowed_domains,
            label: header.token_name,
            expires_at: Some(header.expires_at),
            policy_id,
        }))
    }

    /// Read a connect token's purchase policy id (migration 083).
    ///
    /// Same shape and same reasoning as `get_connect_token_allowed_domains`
    /// below: a separate best-effort query, so this binary shipping ahead of
    /// migration 083 costs the policy feature rather than every paid call. No
    /// column means no policy means today's behaviour.
    ///
    /// A connection's own policy wins; the wallet's is the fallback, resolved
    /// later by the caller, because a connection with no wallets has no wallet
    /// policy to fall back to and that is not an error.
    async fn get_connect_token_policy_id(&self, token_id: Uuid) -> Option<Uuid> {
        use std::sync::atomic::{AtomicU8, Ordering};
        const UNKNOWN: u8 = 0;
        const PRESENT: u8 = 1;
        const ABSENT: u8 = 2;
        static COLUMN_STATE: AtomicU8 = AtomicU8::new(UNKNOWN);

        if COLUMN_STATE.load(Ordering::Relaxed) == ABSENT {
            return None;
        }

        let result = sqlx::query_scalar::<_, Option<Uuid>>(
            "SELECT policy_id FROM agent_wallet_connect_tokens WHERE id = $1",
        )
        .bind(token_id)
        .fetch_optional(&self.pool)
        .await;

        match result {
            Ok(value) => {
                COLUMN_STATE.store(PRESENT, Ordering::Relaxed);
                value.flatten()
            }
            Err(err) if is_undefined_column(&err) => {
                if COLUMN_STATE.swap(ABSENT, Ordering::Relaxed) != ABSENT {
                    tracing::info!(
                        "agent_wallet_connect_tokens.policy_id does not exist; purchase \
                         policies are not enforced until migration 083 is applied"
                    );
                }
                None
            }
            Err(err) => {
                // Transient, so it must not latch — see the allowlist probe.
                tracing::warn!("could not read policy_id for connect token {token_id}: {err}");
                None
            }
        }
    }

    /// Read a connect token's destination allowlist (migration 069).
    ///
    /// **Deliberately a separate, best-effort query rather than a column on the
    /// main token lookup.** If `allowed_domains` were selected inline and the
    /// migration had not been applied, the whole lookup would fail with
    /// `undefined_column` — which means every paid call on the gateway would
    /// break the moment this binary shipped ahead of the migration. Splitting it
    /// out makes the feature strictly additive: no migration ⇒ empty list ⇒
    /// unrestricted ⇒ byte-identical behaviour to before it existed.
    ///
    /// Same resilience pattern as `get_wallet_paid_spend_today`'s handling of
    /// migration 055's `external_x402_fetches`.
    ///
    /// The presence of the column is probed once per process and then cached, so
    /// the steady-state cost is one cheap indexed lookup when the feature is
    /// live, and zero once it is known to be absent.
    async fn get_connect_token_allowed_domains(&self, token_id: Uuid) -> Vec<String> {
        use std::sync::atomic::{AtomicU8, Ordering};
        const UNKNOWN: u8 = 0;
        const PRESENT: u8 = 1;
        const ABSENT: u8 = 2;
        static COLUMN_STATE: AtomicU8 = AtomicU8::new(UNKNOWN);

        if COLUMN_STATE.load(Ordering::Relaxed) == ABSENT {
            return Vec::new();
        }

        let result = sqlx::query_scalar::<_, Option<Vec<String>>>(
            "SELECT allowed_domains FROM agent_wallet_connect_tokens WHERE id = $1",
        )
        .bind(token_id)
        .fetch_optional(&self.pool)
        .await;

        let raw = match result {
            Ok(value) => {
                COLUMN_STATE.store(PRESENT, Ordering::Relaxed);
                value.flatten().unwrap_or_default()
            }
            Err(err) if is_undefined_column(&err) => {
                // Migration 069 genuinely isn't applied. Latch it so we stop
                // asking, and say so once rather than on every call.
                if COLUMN_STATE.swap(ABSENT, Ordering::Relaxed) != ABSENT {
                    tracing::info!(
                        "agent_wallet_connect_tokens.allowed_domains does not exist; \
                         per-connection domain allowlists are disabled until migration 069 is applied"
                    );
                }
                return Vec::new();
            }
            Err(err) => {
                // Any OTHER error (connection blip, timeout, pool exhaustion) is
                // transient and must NOT latch: latching would disable every
                // buyer's allowlist for the lifetime of the process, silently,
                // because of one bad round-trip. Warn and retry on the next call.
                //
                // This call still degrades open, which is the deliberate trade:
                // failing closed here would turn a database hiccup into "no
                // agent can pay for anything", and the per-tx/daily caps still
                // bound the exposure.
                tracing::warn!(
                    "Could not read allowed_domains for connect token {} ({}); \
                     treating this call as unrestricted",
                    token_id,
                    err
                );
                return Vec::new();
            }
        };

        // Normalize on read so a hand-edited row (or one written before the
        // portal normalized) matches the same way `domain_policy` does.
        //
        // An entry that won't normalize is KEPT verbatim, not dropped: dropping
        // it could empty a non-empty list, and an empty list means unrestricted
        // — so one malformed row would silently remove the buyer's guardrail. A
        // retained junk entry simply never matches, which fails closed.
        raw.into_iter()
            .filter(|entry| !entry.trim().is_empty())
            .map(|entry| {
                crate::proxy::domain_policy::normalize_domain(&entry)
                    .unwrap_or_else(|| entry.trim().to_ascii_lowercase())
            })
            .collect()
    }

    /// Resolve a provider SDK key (`sk_live_…`) to a payer context so the same
    /// key that manages a provider's listings can also authorize paid `/execute`
    /// calls. Validates via the `verify_provider_api_key` SECURITY DEFINER RPC
    /// (hash match on an active key, stamps `last_used_at`), then loads the
    /// owner's active custodial wallets — the USDC payer(s) — into the same
    /// `AgentConnectToken` shape a connect token produces, so settlement is
    /// unchanged. `token_id` is the key's id (a plain, FK-free tracking column on
    /// `gateway_usage`), giving per-key usage attribution.
    ///
    /// Returns `Ok(None)` when the key is unknown/revoked. SEPA is never
    /// authorized via an SDK key (the euro rail stays human-gated).
    pub async fn lookup_provider_api_key_context(
        &self,
        secret: &str,
    ) -> Result<Option<AgentConnectToken>> {
        #[derive(sqlx::FromRow)]
        struct KeyOwner {
            owner_id: Uuid,
            key_id: Uuid,
        }

        let owner = sqlx::query_as::<_, KeyOwner>(
            "SELECT owner_id, key_id FROM verify_provider_api_key($1)",
        )
        .bind(secret)
        .fetch_optional(&self.pool)
        .await?;

        let Some(owner) = owner else {
            return Ok(None);
        };

        let wallets = sqlx::query_as::<_, AgentConnectTokenWallet>(
            r#"SELECT
                 w.id AS wallet_id,
                 w.wallet_address,
                 w.status AS wallet_status,
                 w.daily_limit_usdc,
                 w.per_tx_limit_usdc
               FROM agent_wallets w
               WHERE w.user_id = $1
                 AND w.status = 'active'
                 AND w.encrypted_private_key IS NOT NULL
               ORDER BY w.created_at ASC"#,
        )
        .bind(owner.owner_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(Some(AgentConnectToken {
            token_id: owner.key_id,
            user_id: owner.owner_id,
            wallets,
            // An SDK key is named and expired by its own table, not this one.
            label: None,
            expires_at: None,
            // A provider SDK key is the owner acting on their own account, not
            // a delegated agent credential. There is no buyer-set allowlist to
            // apply, so it stays unrestricted — and for the same reason, no
            // purchase policy: a policy governs what a buyer let an AGENT do.
            allowed_domains: Vec::new(),
            policy_id: None,
        }))
    }

    /// Today's wallet spend, using an explicit UTC day boundary.
    pub async fn get_wallet_paid_spend_today(&self, wallet_address: &str) -> Result<BigDecimal> {
        let spent = sqlx::query_scalar::<_, BigDecimal>(
            r#"SELECT COALESCE(SUM(amount), 0)
               FROM gateway_usage
               WHERE paid = true
                 AND lower(wallet_address) = lower($1)
                 AND occurred_at >= date_trunc('day', now() AT TIME ZONE 'utc') AT TIME ZONE 'utc'"#,
        )
        .bind(wallet_address)
        .fetch_one(&self.pool)
        .await?;

        // Also count today's external x402 fetches (the gateway-as-payer proxy),
        // which debit the same managed wallet outside gateway_usage, so a wallet's
        // daily cap covers catalog spend AND external-fetch spend uniformly. This
        // is resilient by design: if migration 055 has not been applied (the
        // table is absent) the query errors and we fall back to 0, so this can
        // never break the wallet-cap check on the hot proxy path.
        let external_spent = sqlx::query_scalar::<_, BigDecimal>(
            r#"SELECT COALESCE(SUM(quoted_price_usdc + fee_usdc), 0)
               FROM external_x402_fetches
               WHERE status IN ('paid', 'debited')
                 AND lower(wallet_address) = lower($1)
                 AND created_at >= date_trunc('day', now() AT TIME ZONE 'utc') AT TIME ZONE 'utc'"#,
        )
        .bind(wallet_address)
        .fetch_one(&self.pool)
        .await
        .unwrap_or_else(|_| BigDecimal::from(0));

        Ok(spent + external_spent)
    }

    /// The whole spend picture one purchase policy needs: today, this month,
    /// and how many purchases today.
    ///
    /// The daily figure comes from [`Self::get_wallet_paid_spend_today`] so the
    /// policy's daily budget and the wallet's own daily cap are counting the
    /// same money — two different sums for "spent today" would eventually
    /// disagree, and the buyer would have no way to tell which one refused
    /// their agent.
    ///
    /// Month and count are read here rather than derived, and both are
    /// best-effort: a failed read returns zero, which can only ALLOW a purchase
    /// the caps would otherwise have refused. That is the deliberate trade the
    /// rest of this file already makes — failing closed on a database hiccup
    /// turns a blip into "no agent can pay for anything", and the per-tx and
    /// daily caps still bound the exposure.
    pub async fn get_wallet_policy_spend(
        &self,
        wallet_address: &str,
    ) -> (BigDecimal, BigDecimal, i64) {
        let today = self
            .get_wallet_paid_spend_today(wallet_address)
            .await
            .unwrap_or_else(|err| {
                tracing::warn!("could not read today's spend for policy evaluation: {err}");
                BigDecimal::from(0)
            });

        let month = sqlx::query_scalar::<_, BigDecimal>(
            r#"SELECT COALESCE(SUM(amount), 0)
               FROM gateway_usage
               WHERE paid = true
                 AND lower(wallet_address) = lower($1)
                 AND occurred_at >= date_trunc('month', now() AT TIME ZONE 'utc') AT TIME ZONE 'utc'"#,
        )
        .bind(wallet_address)
        .fetch_one(&self.pool)
        .await
        .unwrap_or_else(|err| {
            tracing::warn!("could not read this month's spend for policy evaluation: {err}");
            BigDecimal::from(0)
        });

        let calls_today = sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*)
               FROM gateway_usage
               WHERE paid = true
                 AND lower(wallet_address) = lower($1)
                 AND occurred_at >= date_trunc('day', now() AT TIME ZONE 'utc') AT TIME ZONE 'utc'"#,
        )
        .bind(wallet_address)
        .fetch_one(&self.pool)
        .await
        .unwrap_or_else(|err| {
            tracing::warn!("could not count today's purchases for policy evaluation: {err}");
            0
        });

        (today, month, calls_today)
    }

    /// Has this buyer paid this seller before?
    ///
    /// Drives the new-seller hold. `None` means the question could not be
    /// answered, which is NOT the same as "no": the evaluator only holds on an
    /// explicit false, so a failed lookup lets the purchase through rather than
    /// wedging an agent over a database blip.
    pub async fn has_paid_seller_before(&self, user_id: Uuid, api_slug: &str) -> Option<bool> {
        sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS (
                 SELECT 1
                   FROM gateway_usage u
                   JOIN agent_wallets w ON lower(w.wallet_address) = lower(u.wallet_address)
                  WHERE u.paid = true
                    AND u.api_slug = $2
                    AND w.user_id = $1
                  LIMIT 1
               )"#,
        )
        .bind(user_id)
        .bind(api_slug)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| {
            tracing::warn!("could not read seller history for policy evaluation: {err}");
        })
        .ok()
    }

    pub async fn mark_agent_connect_token_used(
        &self,
        token_id: Uuid,
        wallet_id: Uuid,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            r#"UPDATE agent_wallet_connect_tokens
               SET last_used_at = now()
               WHERE id = $1"#,
        )
        .bind(token_id)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"UPDATE agent_wallets
               SET last_used_at = now()
               WHERE id = $1"#,
        )
        .bind(wallet_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    pub async fn is_managed_agent_wallet(&self, wallet_address: &str) -> Result<bool> {
        let found = sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*)
               FROM agent_wallets
               WHERE lower(wallet_address) = lower($1)
                 AND status != 'revoked'"#,
        )
        .bind(wallet_address)
        .fetch_one(&self.pool)
        .await?;

        Ok(found > 0)
    }

    pub async fn get_presence_session_context(
        &self,
        session_id: Uuid,
    ) -> Result<Option<PresenceSessionContext>> {
        let row = sqlx::query_as::<_, PresenceSessionContext>(
            r#"SELECT session_id, client_id, managed_wallet_id
               FROM agent_presence_sessions
               WHERE session_id = $1
               LIMIT 1"#,
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row)
    }

    pub async fn upsert_presence_session(
        &self,
        input: PresenceSessionUpsert,
    ) -> Result<PresenceSessionContext> {
        let row = sqlx::query_as::<_, PresenceSessionContext>(
            r#"INSERT INTO agent_presence_sessions
               (session_id, client_id, client_kind, managed_wallet_id, wallet_address, connected_at, last_heartbeat_at, disconnected_at, status, metadata)
               VALUES ($1, $2, $3, $4, $5, now(), now(), NULL, 'online', $6::jsonb)
               ON CONFLICT (session_id) DO UPDATE SET
                 client_id = EXCLUDED.client_id,
                 client_kind = EXCLUDED.client_kind,
                 managed_wallet_id = EXCLUDED.managed_wallet_id,
                 wallet_address = EXCLUDED.wallet_address,
                 connected_at = now(),
                 last_heartbeat_at = now(),
                 disconnected_at = NULL,
                 status = 'online',
                 metadata = EXCLUDED.metadata
               RETURNING session_id, client_id, managed_wallet_id"#,
        )
        .bind(input.session_id)
        .bind(input.client_id)
        .bind(input.client_kind)
        .bind(input.managed_wallet_id)
        .bind(input.wallet_address)
        .bind(input.metadata)
        .fetch_one(&self.pool)
        .await?;

        Ok(row)
    }

    pub async fn heartbeat_presence_session(
        &self,
        session_id: Uuid,
        last_api_slug: Option<&str>,
    ) -> Result<Option<PresenceSessionContext>> {
        let row = sqlx::query_as::<_, PresenceSessionContext>(
            r#"UPDATE agent_presence_sessions
               SET last_heartbeat_at = now(),
                   disconnected_at = NULL,
                   status = 'online',
                   last_api_slug = COALESCE($2, last_api_slug)
               WHERE session_id = $1
               RETURNING session_id, client_id, managed_wallet_id"#,
        )
        .bind(session_id)
        .bind(last_api_slug)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row)
    }

    pub async fn disconnect_presence_session(&self, session_id: Uuid) -> Result<bool> {
        let updated = sqlx::query_scalar::<_, i64>(
            r#"WITH updated AS (
                 UPDATE agent_presence_sessions
                 SET disconnected_at = now(),
                     status = 'offline'
                 WHERE session_id = $1
                 RETURNING 1
               )
               SELECT COUNT(*) FROM updated"#,
        )
        .bind(session_id)
        .fetch_one(&self.pool)
        .await?;

        Ok(updated > 0)
    }

    pub async fn touch_presence_session(
        &self,
        session_id: Uuid,
        last_api_slug: Option<&str>,
    ) -> Result<Option<PresenceSessionContext>> {
        let row = sqlx::query_as::<_, PresenceSessionContext>(
            r#"UPDATE agent_presence_sessions
               SET last_api_slug = COALESCE($2, last_api_slug)
               WHERE session_id = $1
               RETURNING session_id, client_id, managed_wallet_id"#,
        )
        .bind(session_id)
        .bind(last_api_slug)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row)
    }

    pub fn log_request_event(&self, event: GatewayRequestLogInsert<'_>) {
        let query = sqlx::query(
            r#"INSERT INTO gateway_request_logs
               (request_id, session_id, managed_wallet_id, client_id, method, path,
                query_string, status_code, latency_ms, api_slug, is_api_route,
                user_agent, ip_hash, origin, referrer, request_headers,
                response_bytes, error_code)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                       $13, $14, $15, $16, $17, $18)"#,
        )
        .bind(event.request_id)
        .bind(event.session_id)
        .bind(event.managed_wallet_id)
        .bind(event.client_id.map(str::to_owned))
        .bind(event.method.to_owned())
        .bind(event.path.to_owned())
        .bind(event.query_string.map(str::to_owned))
        .bind(event.status_code)
        .bind(event.latency_ms)
        .bind(event.api_slug.map(str::to_owned))
        .bind(event.is_api_route)
        .bind(event.user_agent.map(str::to_owned))
        .bind(event.ip_hash.map(str::to_owned))
        .bind(event.origin.map(str::to_owned))
        .bind(event.referrer.map(str::to_owned))
        .bind(event.request_headers.clone())
        .bind(event.response_bytes)
        .bind(event.error_code.map(str::to_owned));
        let pool = self.log_pool.clone();
        if !self.request_log_queue.submit(async move {
            query.execute(&pool).await?;
            Ok(())
        }) {
            tracing::warn!("Request log queue full; dropping telemetry event");
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn log_settlement(
        &self,
        api_id: Uuid,
        endpoint_id: Option<Uuid>,
        tx_hash: &str,
        gross_amount: BigDecimal,
        commission_rate: BigDecimal,
        commission_amount: BigDecimal,
        developer_amount: BigDecimal,
        network_token_id: Option<Uuid>,
        wallet_address: Option<String>,
        request_id: Option<Uuid>,
        session_id: Option<Uuid>,
        managed_wallet_id: Option<Uuid>,
        client_id: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO settlements (api_id, endpoint_id, tx_hash, amount, gross_amount, commission_rate, commission_amount, developer_amount, network_token_id, wallet_address, request_id, session_id, managed_wallet_id, client_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)"
        )
        .bind(api_id)
        .bind(endpoint_id)
        .bind(tx_hash)
        .bind(&gross_amount)
        .bind(&gross_amount)
        .bind(&commission_rate)
        .bind(&commission_amount)
        .bind(&developer_amount)
        .bind(network_token_id)
        .bind(wallet_address)
        .bind(request_id)
        .bind(session_id)
        .bind(managed_wallet_id)
        .bind(client_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn get_effective_commission_rate(
        &self,
        api_id: Uuid,
        base_rate: BigDecimal,
    ) -> Result<BigDecimal> {
        let tier_rate: Option<(BigDecimal,)> = sqlx::query_as(
            r#"SELECT ct.commission_rate
               FROM commission_tiers ct
               WHERE ct.min_monthly_volume <= COALESCE(
                 (SELECT COALESCE(SUM(amount), 0) FROM gateway_usage
                  WHERE api_id = $1 AND paid = true
                  AND occurred_at >= date_trunc('month', now())),
                 0)
               ORDER BY ct.min_monthly_volume DESC
               LIMIT 1"#,
        )
        .bind(api_id)
        .fetch_optional(&self.pool)
        .await?;

        match tier_rate {
            Some((rate,)) if rate < base_rate => Ok(rate),
            _ => Ok(base_rate),
        }
    }

    pub async fn credit_developer_balance(
        &self,
        owner_id: Uuid,
        api_id: Uuid,
        developer_amount: BigDecimal,
        tx_hash: &str,
        network_token_id: Option<Uuid>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO developer_balances (owner_id, api_id, amount, tx_hash, entry_type, description, network_token_id) VALUES ($1, $2, $3, $4, 'earning', $5, $6)"
        )
        .bind(owner_id)
        .bind(api_id)
        .bind(&developer_amount)
        .bind(tx_hash)
        .bind(format!("Revenue share from settlement {}", tx_hash))
        .bind(network_token_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn charge_credits_for_api_call(
        &self,
        user_id: Uuid,
        api: &Api,
        endpoint: &Endpoint,
        request_id: Uuid,
        session_id: Option<Uuid>,
        managed_wallet_id: Option<Uuid>,
        client_id: Option<&str>,
        agent_connect_token_id: Option<Uuid>,
    ) -> std::result::Result<CreditChargeResult, CreditChargeError> {
        let (credits_used, amount_eur) = endpoint_price_to_credit_amounts(&endpoint.price)
            .ok_or(CreditChargeError::UnsupportedPrice)?;

        let commission_rate = self
            .get_effective_commission_rate(api.id, api.commission_rate.clone())
            .await
            .map_err(CreditChargeError::Database)?;
        let commission_amount = &amount_eur * &commission_rate;
        let developer_amount = &amount_eur - &commission_amount;
        let synthetic_tx_hash = format!("credits:{}", request_id.simple());
        let ledger_metadata = serde_json::json!({
            "api_id": api.id,
            "api_slug": api.slug,
            "endpoint_id": endpoint.id,
            "endpoint_path": endpoint.path,
            "endpoint_method": endpoint.method,
            "payment_rail": "credits",
        });

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|err| CreditChargeError::Database(err.into()))?;

        let existing = sqlx::query_as::<_, ExistingCreditChargeRow>(
            r#"SELECT
                 gu.id AS gateway_usage_id,
                 gu.amount AS amount_eur,
                 COALESCE(ucb.credits, 0) AS credits_remaining
               FROM credit_ledger cl
               JOIN gateway_usage gu
                 ON gu.request_id = cl.reference_id
               LEFT JOIN user_credit_balances ucb
                 ON ucb.user_id = $1
               WHERE cl.user_id = $1
                 AND cl.type = 'spend'
                 AND cl.reference_id = $2
                 AND gu.api_id = $3
                 AND gu.paid = true
               LIMIT 1"#,
        )
        .bind(user_id)
        .bind(request_id)
        .bind(api.id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|err| CreditChargeError::Database(err.into()))?;

        if let Some(existing) = existing {
            tx.commit()
                .await
                .map_err(|err| CreditChargeError::Database(err.into()))?;
            return Ok(CreditChargeResult {
                gateway_usage_id: existing.gateway_usage_id,
                request_id,
                credits_used,
                credits_remaining: existing.credits_remaining,
                amount_eur: existing.amount_eur,
                already_processed: true,
            });
        }

        let credits_remaining = sqlx::query_scalar::<_, i32>(
            r#"UPDATE user_credit_balances
               SET credits = credits - $2
               WHERE user_id = $1
                 AND credits >= $2
               RETURNING credits"#,
        )
        .bind(user_id)
        .bind(credits_used)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|err| CreditChargeError::Database(err.into()))?
        .ok_or(CreditChargeError::InsufficientCredits)?;

        let gateway_usage_id = sqlx::query_scalar::<_, i64>(
            r#"INSERT INTO gateway_usage
                 (api_id, endpoint_id, paid, amount, network_token_id, wallet_address, request_id, session_id, managed_wallet_id, client_id, agent_connect_token_id)
               VALUES ($1, $2, true, $3, NULL, NULL, $4, $5, $6, $7, $8)
               RETURNING id"#,
        )
        .bind(api.id)
        .bind(endpoint.id)
        .bind(&amount_eur)
        .bind(request_id)
        .bind(session_id)
        .bind(managed_wallet_id)
        .bind(client_id)
        .bind(agent_connect_token_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|err| CreditChargeError::Database(err.into()))?;

        let mut ledger_metadata = ledger_metadata;
        ledger_metadata["gateway_usage_id"] = serde_json::json!(gateway_usage_id);

        let ledger_insert = sqlx::query(
            r#"INSERT INTO credit_ledger
                 (user_id, type, credits, amount_eur, reference_id, metadata)
               VALUES ($1, 'spend', $2, $3, $4, $5::jsonb)"#,
        )
        .bind(user_id)
        .bind(-credits_used)
        .bind(-amount_eur.clone())
        .bind(request_id)
        .bind(&ledger_metadata)
        .execute(&mut *tx)
        .await;

        if let Err(err) = ledger_insert {
            if is_unique_violation(&err) {
                tx.rollback()
                    .await
                    .map_err(|rollback_err| CreditChargeError::Database(rollback_err.into()))?;
                let existing =
                    fetch_existing_credit_charge(&self.pool, user_id, api.id, request_id)
                        .await
                        .map_err(CreditChargeError::Database)?
                        .ok_or_else(|| {
                            CreditChargeError::Database(anyhow::anyhow!(
                            "credit spend request {} conflicted but no existing charge was found",
                            request_id
                        ))
                        })?;

                return Ok(CreditChargeResult {
                    gateway_usage_id: existing.gateway_usage_id,
                    request_id,
                    credits_used,
                    credits_remaining: existing.credits_remaining,
                    amount_eur: existing.amount_eur,
                    already_processed: true,
                });
            }
            return Err(CreditChargeError::Database(err.into()));
        }

        sqlx::query(
            r#"INSERT INTO developer_balances
                 (owner_id, api_id, amount, tx_hash, entry_type, description, network_token_id)
               VALUES ($1, $2, $3, $4, 'earning', $5, NULL)"#,
        )
        .bind(api.owner_id)
        .bind(api.id)
        .bind(&developer_amount)
        .bind(&synthetic_tx_hash)
        .bind(format!(
            "Revenue share from credit usage {} {} {}",
            endpoint.method, api.slug, endpoint.path
        ))
        .execute(&mut *tx)
        .await
        .map_err(|err| CreditChargeError::Database(err.into()))?;

        if api.is_community.unwrap_or(false) {
            let revenue_wallet = api.wallet_address.trim();
            if !revenue_wallet.is_empty() {
                sqlx::query(
                    r#"INSERT INTO developer_revenue
                         (api_id, owner_wallet, settlement_tx, gross_amount, commission_rate, commission_amount, developer_amount)
                       VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
                )
                .bind(api.id)
                .bind(revenue_wallet)
                .bind(&synthetic_tx_hash)
                .bind(&amount_eur)
                .bind(&commission_rate)
                .bind(&commission_amount)
                .bind(&developer_amount)
                .execute(&mut *tx)
                .await
                .map_err(|err| CreditChargeError::Database(err.into()))?;
            }
        }

        tx.commit()
            .await
            .map_err(|err| CreditChargeError::Database(err.into()))?;

        Ok(CreditChargeResult {
            gateway_usage_id,
            request_id,
            credits_used,
            credits_remaining,
            amount_eur,
            already_processed: false,
        })
    }

    pub async fn ensure_user_can_afford_api_call_with_credits(
        &self,
        user_id: Uuid,
        endpoint: &Endpoint,
    ) -> std::result::Result<(), CreditChargeError> {
        let (credits_required, _) = endpoint_price_to_credit_amounts(&endpoint.price)
            .ok_or(CreditChargeError::UnsupportedPrice)?;

        let available = sqlx::query_scalar::<_, i32>(
            r#"SELECT COALESCE(credits, 0)
               FROM user_credit_balances
               WHERE user_id = $1"#,
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| CreditChargeError::Database(err.into()))?
        .unwrap_or(0);

        if available < credits_required {
            return Err(CreditChargeError::InsufficientCredits);
        }

        Ok(())
    }

    /// Atomic settlement record for the x402 / agent-wallet rail.
    ///
    /// Combines settlement insert, developer ledger credit, paid usage row, and
    /// optional community revenue row into one Postgres transaction. Without
    /// this, a crash between calls leaves the books inconsistent. Idempotent on
    /// `settlements.tx_hash`: a duplicate tx_hash short-circuits the rest of
    /// the writes and returns `Ok(false)`.
    ///
    /// `status` decides whether provider revenue is booked now. Callers that
    /// still have to move the money (the managed-wallet prepaid rail, which
    /// settles on-chain *after* this call) pass
    /// [`SettlementStatus::Pending`] and settle up via
    /// [`Self::finalize_direct_settlement`]. Anything already funded passes
    /// [`SettlementStatus::Settled`].
    #[allow(clippy::too_many_arguments)]
    pub async fn record_paid_settlement_atomically(
        &self,
        api_id: Uuid,
        endpoint_id: Uuid,
        owner_id: Uuid,
        tx_hash: &str,
        gross_amount: BigDecimal,
        commission_rate: BigDecimal,
        commission_amount: BigDecimal,
        developer_amount: BigDecimal,
        network_token_id: Option<Uuid>,
        wallet_address: Option<String>,
        request_id: Option<Uuid>,
        session_id: Option<Uuid>,
        managed_wallet_id: Option<Uuid>,
        client_id: Option<&str>,
        agent_connect_token_id: Option<Uuid>,
        community_revenue_wallet: Option<&str>,
        status: SettlementStatus,
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await?;

        let settlement_inserted = sqlx::query(
            r#"INSERT INTO settlements
                 (api_id, endpoint_id, tx_hash, amount, gross_amount, commission_rate,
                  commission_amount, developer_amount, network_token_id, wallet_address,
                  request_id, session_id, managed_wallet_id, client_id, settlement_status)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
               ON CONFLICT (tx_hash) DO NOTHING"#,
        )
        .bind(api_id)
        .bind(endpoint_id)
        .bind(tx_hash)
        .bind(&gross_amount)
        .bind(&gross_amount)
        .bind(&commission_rate)
        .bind(&commission_amount)
        .bind(&developer_amount)
        .bind(network_token_id)
        .bind(&wallet_address)
        .bind(request_id)
        .bind(session_id)
        .bind(managed_wallet_id)
        .bind(client_id)
        .bind(status.as_str())
        .execute(&mut *tx)
        .await?
        .rows_affected();

        if settlement_inserted == 0 {
            tx.commit().await?;
            return Ok(false);
        }

        // Provider revenue is booked only once the money is known to have
        // moved. A pending row gets its credit in finalize_direct_settlement;
        // a failed one never gets one at all.
        if status.credits_developer() {
            sqlx::query(
                r#"INSERT INTO developer_balances
                     (owner_id, api_id, amount, tx_hash, entry_type, description, network_token_id)
                   VALUES ($1, $2, $3, $4, 'earning', $5, $6)"#,
            )
            .bind(owner_id)
            .bind(api_id)
            .bind(&developer_amount)
            .bind(tx_hash)
            .bind(format!("Revenue share from settlement {}", tx_hash))
            .bind(network_token_id)
            .execute(&mut *tx)
            .await?;
        }

        sqlx::query(
            r#"INSERT INTO gateway_usage
                 (api_id, endpoint_id, paid, amount, network_token_id, wallet_address,
                  request_id, session_id, managed_wallet_id, client_id, agent_connect_token_id)
               VALUES ($1, $2, true, $3, $4, $5, $6, $7, $8, $9, $10)
               ON CONFLICT DO NOTHING"#,
        )
        .bind(api_id)
        .bind(endpoint_id)
        .bind(&gross_amount)
        .bind(network_token_id)
        .bind(wallet_address)
        .bind(request_id)
        .bind(session_id)
        .bind(managed_wallet_id)
        .bind(client_id)
        .bind(agent_connect_token_id)
        .execute(&mut *tx)
        .await?;

        // Community revenue tracks the same money as the developer credit, so
        // it follows the same rule: only booked once the settle is confirmed.
        if let Some(wallet) = community_revenue_wallet.filter(|_| status.credits_developer()) {
            if !wallet.is_empty() {
                sqlx::query(
                    r#"INSERT INTO developer_revenue
                         (api_id, owner_wallet, settlement_tx, gross_amount,
                          commission_rate, commission_amount, developer_amount)
                       VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
                )
                .bind(api_id)
                .bind(wallet)
                .bind(tx_hash)
                .bind(&gross_amount)
                .bind(&commission_rate)
                .bind(&commission_amount)
                .bind(&developer_amount)
                .execute(&mut *tx)
                .await?;
            }
        }

        tx.commit().await?;
        Ok(true)
    }

    /// Resolve a [`SettlementStatus::Pending`] row once the on-chain settle has
    /// returned, in one transaction (audit K3).
    ///
    /// On success this books the provider's earning — deliberately keyed on the
    /// synthetic `aw_internal_…` `tx_hash`, NOT on `onchain_tx_hash`. That
    /// prefix is what makes `developer_payout_worker` score the row as 0
    /// (`support.rs`): on this rail `settle()` already paid the provider their
    /// 98% directly on-chain, so a payable ledger entry would pay them twice.
    /// The real Base hash is recorded alongside, for tracing only.
    ///
    /// On failure nothing is credited and the row is left as evidence for
    /// `reconciliation_stuck_settlements`.
    #[allow(clippy::too_many_arguments)]
    pub async fn finalize_direct_settlement(
        &self,
        tx_hash: &str,
        outcome: Result<&str, String>,
        owner_id: Uuid,
        api_id: Uuid,
        gross_amount: &BigDecimal,
        commission_rate: &BigDecimal,
        commission_amount: &BigDecimal,
        developer_amount: &BigDecimal,
        network_token_id: Option<Uuid>,
        community_revenue_wallet: Option<&str>,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;

        let (status, onchain_tx_hash, error) = match outcome {
            Ok(hash) => (SettlementStatus::Settled, Some(hash), None),
            Err(ref message) => (SettlementStatus::Failed, None, Some(message.as_str())),
        };

        // Guard on the current status so a retry (or a duplicate finalize) can
        // never credit the provider twice.
        let updated = sqlx::query(
            r#"UPDATE settlements
                  SET settlement_status = $2,
                      onchain_tx_hash = $3,
                      settlement_error = $4,
                      settlement_finalized_at = now()
                WHERE tx_hash = $1
                  AND settlement_status = 'pending'"#,
        )
        .bind(tx_hash)
        .bind(status.as_str())
        .bind(onchain_tx_hash)
        .bind(error)
        .execute(&mut *tx)
        .await?
        .rows_affected();

        if updated == 0 {
            tx.commit().await?;
            return Ok(());
        }

        if status.credits_developer() {
            sqlx::query(
                r#"INSERT INTO developer_balances
                     (owner_id, api_id, amount, tx_hash, entry_type, description, network_token_id)
                   VALUES ($1, $2, $3, $4, 'earning', $5, $6)"#,
            )
            .bind(owner_id)
            .bind(api_id)
            .bind(developer_amount)
            .bind(tx_hash)
            .bind(format!("Revenue share from settlement {}", tx_hash))
            .bind(network_token_id)
            .execute(&mut *tx)
            .await?;

            if let Some(wallet) = community_revenue_wallet.filter(|w| !w.is_empty()) {
                sqlx::query(
                    r#"INSERT INTO developer_revenue
                         (api_id, owner_wallet, settlement_tx, gross_amount,
                          commission_rate, commission_amount, developer_amount)
                       VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
                )
                .bind(api_id)
                .bind(wallet)
                .bind(tx_hash)
                .bind(gross_amount)
                .bind(commission_rate)
                .bind(commission_amount)
                .bind(developer_amount)
                .execute(&mut *tx)
                .await?;
            }
        }

        tx.commit().await?;
        Ok(())
    }

    /// Defense-in-depth replay guard for x402 payment proofs.
    ///
    /// Returns `Ok(true)` if this is the first time we have settled this
    /// proof_hash, `Ok(false)` if it was already claimed. The facilitator
    /// should enforce nonces, but a gateway-side check stops a leaked proof
    /// from being replayed across slugs/paths within its validity window.
    pub async fn claim_x402_proof_nonce(
        &self,
        proof_hash: &str,
        request_id: Uuid,
        api_id: Uuid,
    ) -> Result<bool> {
        let inserted = sqlx::query(
            r#"INSERT INTO x402_proof_nonces (proof_hash, request_id, api_id)
               VALUES ($1, $2, $3)
               ON CONFLICT (proof_hash) DO NOTHING"#,
        )
        .bind(proof_hash)
        .bind(request_id)
        .bind(api_id)
        .execute(&self.pool)
        .await?
        .rows_affected();

        Ok(inserted > 0)
    }
}

async fn fetch_existing_credit_charge(
    pool: &PgPool,
    user_id: Uuid,
    api_id: Uuid,
    request_id: Uuid,
) -> Result<Option<ExistingCreditChargeRow>> {
    sqlx::query_as::<_, ExistingCreditChargeRow>(
        r#"SELECT
             gu.id AS gateway_usage_id,
             gu.amount AS amount_eur,
             COALESCE(ucb.credits, 0) AS credits_remaining
           FROM credit_ledger cl
           JOIN gateway_usage gu
             ON gu.request_id = cl.reference_id
           LEFT JOIN user_credit_balances ucb
             ON ucb.user_id = $1
           WHERE cl.user_id = $1
             AND cl.type = 'spend'
             AND cl.reference_id = $2
             AND gu.api_id = $3
             AND gu.paid = true
           LIMIT 1"#,
    )
    .bind(user_id)
    .bind(request_id)
    .bind(api_id)
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

pub(crate) fn endpoint_price_supports_credits(price: &BigDecimal) -> bool {
    endpoint_price_to_credit_amounts(price).is_some()
}

fn endpoint_price_to_credit_amounts(price: &BigDecimal) -> Option<(i32, BigDecimal)> {
    let normalized = price.normalized().to_string();
    let trimmed = normalized.trim();
    if trimmed.is_empty() || trimmed.starts_with('-') {
        return None;
    }

    let mut parts = trimmed.split('.');
    let whole = parts.next()?;
    let fractional = parts.next().unwrap_or("");
    if parts.next().is_some() || fractional.len() > 2 {
        return None;
    }

    let whole_units = whole.parse::<i64>().ok()?;
    let fractional_value = match fractional.len() {
        0 => 0_i64,
        1 => fractional.parse::<i64>().ok()? * 10,
        2 => fractional.parse::<i64>().ok()?,
        _ => return None,
    };
    let cents = whole_units
        .checked_mul(100)?
        .checked_add(fractional_value)?;
    let credits = i32::try_from(cents).ok()?;
    let amount = BigDecimal::from_str(&format!("{}.{:02}", cents / 100, cents % 100)).ok()?;

    Some((credits, amount))
}

/// Postgres `undefined_column` (42703) — i.e. the schema genuinely lacks the
/// column, as opposed to the query failing for a transient reason.
fn is_undefined_column(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|db_err| db_err.code())
        .map(|code| code == "42703")
        .unwrap_or(false)
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|db_err| db_err.code())
        .map(|code| code == "23505")
        .unwrap_or(false)
}
