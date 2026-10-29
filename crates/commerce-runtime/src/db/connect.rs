//! Database access for the Apiosk Connect track (marketplaces, API keys,
//! merchants, skills, payments, audit). One responsibility per method; no
//! business logic here — callers in `crate::connect` own that.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

use super::Database;

/// Everything the 402 charge builder (04-02) needs about a skill in a single
/// query: its price + fee/wallet snapshot, plus the merchant's current readiness
/// (status + whether an active, verified wallet still exists) for the 03-03 guard
/// re-checked at charge time. Returned by [`Database::get_connect_skill_charge`].
#[allow(dead_code)]
pub struct ConnectSkillCharge {
    pub marketplace_id: Uuid,
    pub merchant_id: Uuid,
    pub price_amount: i64,
    pub price_currency: String,
    pub destination_wallet: String,
    pub marketplace_fee_bps: i32,
    pub apiosk_fee_bps: i32,
    pub active: bool,
    pub merchant_status: String,
    pub has_active_wallet: bool,
}

// Consumed by the connect `/v1/*` routes + middleware in package 01; until those
// land these methods are unused from non-test code (dead_code in this binary).
#[allow(dead_code)]
impl Database {
    /// Resolve a marketplace API key (already SHA-256 hashed by the caller) to
    /// its `marketplace_id`, or `None` if the key is unknown or revoked. The
    /// gateway connects with a role that bypasses RLS, so this is the trusted
    /// server-side lookup behind the `require_marketplace_api_key` middleware
    /// (00-08). Never accept or log the raw key — only the hash reaches here.
    pub async fn lookup_marketplace_by_api_key(&self, key_hash: &str) -> Result<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            r#"SELECT marketplace_id
                 FROM connect_api_keys
                WHERE key_hash = $1
                  AND kind = 'secret'
                  AND revoked_at IS NULL
                LIMIT 1"#,
        )
        .bind(key_hash)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.0))
    }

    /// Resolve a PUBLISHABLE marketplace key (SHA-256 hashed by the caller) to its
    /// `marketplace_id`, or `None` if unknown/revoked. The hard `kind = 'publishable'`
    /// filter is the boundary that keeps a public embed key from ever
    /// authenticating the secret management surface (045). Powers the public
    /// `/v1/connect/embed/start` endpoint.
    pub async fn lookup_marketplace_by_publishable_key(
        &self,
        key_hash: &str,
    ) -> Result<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            r#"SELECT marketplace_id
                 FROM connect_api_keys
                WHERE key_hash = $1
                  AND kind = 'publishable'
                  AND revoked_at IS NULL
                LIMIT 1"#,
        )
        .bind(key_hash)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.0))
    }

    /// A marketplace's display name, or `None` if it doesn't exist. Used by the
    /// embed-start endpoint so the seller (and the parent-page session container)
    /// can show which marketplace they're connecting under.
    pub async fn get_connect_marketplace_name(&self, id: Uuid) -> Result<Option<String>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT name FROM connect_marketplaces WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|r| r.0))
    }

    /// Marketplace display name + logo URL for the self-serve connect screen
    /// branding (embed flow). Logo may be NULL when the marketplace never set one.
    pub async fn get_connect_marketplace_brand(
        &self,
        id: Uuid,
    ) -> Result<Option<(String, Option<String>)>> {
        let row: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT name, logo_url FROM connect_marketplaces WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row)
    }

    /// Resolve a marketplace's `owner_user_id`, or `None` if the marketplace does
    /// not exist. Used by the owner-only key-mint endpoint (01-06) to authorize
    /// the caller before issuing a key.
    pub async fn get_connect_marketplace_owner(&self, id: Uuid) -> Result<Option<Uuid>> {
        let row: Option<(Uuid,)> =
            sqlx::query_as("SELECT owner_user_id FROM connect_marketplaces WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|r| r.0))
    }

    /// Insert a marketplace API key. For a SECRET key we store ONLY the SHA-256
    /// hash + last4 (`public_key` NULL, never the raw key). For a PUBLISHABLE key
    /// the raw value is public by design and is kept in `public_key` so the embed
    /// snippet can be re-rendered on demand (045). Returns the new key id +
    /// created_at for the one-time generate response (01-06).
    pub async fn insert_connect_api_key(
        &self,
        marketplace_id: Uuid,
        key_hash: &str,
        key_last4: &str,
        label: Option<&str>,
        kind: &str,
        public_key: Option<&str>,
    ) -> Result<(Uuid, DateTime<Utc>)> {
        let row: (Uuid, DateTime<Utc>) = sqlx::query_as(
            r#"INSERT INTO connect_api_keys
                   (marketplace_id, key_hash, key_last4, label, kind, public_key)
               VALUES ($1, $2, $3, $4, $5, $6)
               RETURNING id, created_at"#,
        )
        .bind(marketplace_id)
        .bind(key_hash)
        .bind(key_last4)
        .bind(label)
        .bind(kind)
        .bind(public_key)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    /// Revoke a marketplace's current live publishable key(s), if any. Called
    /// before minting a fresh one so the partial unique index
    /// `uq_connect_publishable_active` never conflicts (rotation). Idempotent.
    pub async fn revoke_connect_publishable_keys(&self, marketplace_id: Uuid) -> Result<()> {
        sqlx::query(
            r#"UPDATE connect_api_keys
                  SET revoked_at = now()
                WHERE marketplace_id = $1
                  AND kind = 'publishable'
                  AND revoked_at IS NULL"#,
        )
        .bind(marketplace_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Flip a marketplace to `active` (idempotent). Called after its first API key
    /// is minted (01-06).
    pub async fn activate_connect_marketplace(&self, id: Uuid) -> Result<()> {
        sqlx::query(
            "UPDATE connect_marketplaces SET status = 'active', updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Upsert a merchant on its unique `(marketplace_id, external_seller_id)` key
    /// and return its id. Idempotent: repeated onboarding-link verifies resolve to
    /// the same merchant (status stays `draft` until activation). Bumps
    /// `updated_at` so a re-verify is observable.
    pub async fn upsert_connect_merchant(
        &self,
        marketplace_id: Uuid,
        external_seller_id: &str,
    ) -> Result<Uuid> {
        let row: (Uuid,) = sqlx::query_as(
            r#"INSERT INTO connect_merchants (marketplace_id, external_seller_id)
               VALUES ($1, $2)
               ON CONFLICT (marketplace_id, external_seller_id)
               DO UPDATE SET updated_at = now()
               RETURNING id"#,
        )
        .bind(marketplace_id)
        .bind(external_seller_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.0)
    }

    /// Load a merchant's marketplace + status, or `None` if it doesn't exist.
    pub async fn get_connect_merchant(&self, merchant_id: Uuid) -> Result<Option<(Uuid, String)>> {
        let row: Option<(Uuid, String)> = sqlx::query_as(
            "SELECT marketplace_id, status::text FROM connect_merchants WHERE id = $1",
        )
        .bind(merchant_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Update a merchant's profile fields (02-03). Empty strings are stored as NULL.
    pub async fn update_connect_merchant_profile(
        &self,
        merchant_id: Uuid,
        display_name: Option<&str>,
        contact_email: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            r#"UPDATE connect_merchants
               SET display_name = $2, contact_email = $3, updated_at = now()
               WHERE id = $1"#,
        )
        .bind(merchant_id)
        .bind(display_name)
        .bind(contact_email)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// True when the merchant has exactly one active, SIWE-verified wallet (the
    /// activation gate, 02-07).
    pub async fn merchant_has_verified_active_wallet(&self, merchant_id: Uuid) -> Result<bool> {
        let row: Option<(i64,)> = sqlx::query_as(
            r#"SELECT count(*) FROM connect_wallets
               WHERE merchant_id = $1 AND is_active AND verified_at IS NOT NULL"#,
        )
        .bind(merchant_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.0 > 0).unwrap_or(false))
    }

    /// Replace the merchant's active wallet with a freshly SIWE-verified one
    /// (02-05). Deactivates any prior active wallet then inserts the new one as
    /// active+verified, in one transaction so the `uq_connect_wallet_active`
    /// partial index never conflicts. `address` must already be EIP-55 checksummed.
    pub async fn store_connect_verified_wallet(
        &self,
        merchant_id: Uuid,
        chain: &str,
        address: &str,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE connect_wallets SET is_active = false WHERE merchant_id = $1 AND is_active",
        )
        .bind(merchant_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            r#"INSERT INTO connect_wallets (merchant_id, chain, address, verified_at, is_active)
               VALUES ($1, $2, $3, now(), true)"#,
        )
        .bind(merchant_id)
        .bind(chain)
        .bind(address)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Flip a merchant to `active` (02-07). Idempotent.
    pub async fn activate_connect_merchant(&self, merchant_id: Uuid) -> Result<()> {
        sqlx::query(
            "UPDATE connect_merchants SET status = 'active', updated_at = now() WHERE id = $1",
        )
        .bind(merchant_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The marketplace's configured webhook URL (043), or `None`.
    pub async fn get_connect_marketplace_webhook_url(
        &self,
        marketplace_id: Uuid,
    ) -> Result<Option<String>> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT webhook_url FROM connect_marketplaces WHERE id = $1")
                .bind(marketplace_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.and_then(|r| r.0))
    }

    /// Append one row to the append-only `connect_audit_log` (00-06). Returns
    /// the new row id. This is the only write path for audit rows; the gateway
    /// role has INSERT but no UPDATE/DELETE on the table (migration 041), so the
    /// ledger cannot be tampered with even if calling code regresses.
    ///
    /// `metadata` MUST already be free of secrets/PII — callers go through
    /// `crate::connect::audit::audit_log`, which sanitizes before calling here.
    pub async fn insert_connect_audit(
        &self,
        actor: &str,
        action: &str,
        entity: &str,
        entity_id: Option<Uuid>,
        amount: Option<i64>,
        metadata: Option<Value>,
    ) -> Result<i64> {
        let row: (i64,) = sqlx::query_as(
            r#"INSERT INTO connect_audit_log
                   (actor, action, entity, entity_id, amount, metadata)
               VALUES ($1, $2, $3, $4, $5, $6)
               RETURNING id"#,
        )
        .bind(actor)
        .bind(action)
        .bind(entity)
        .bind(entity_id)
        .bind(amount)
        .bind(metadata)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.0)
    }

    /* ---------------------------------------------------------------------- */
    /* Skills (package 03)                                                    */
    /* ---------------------------------------------------------------------- */

    /// Resolve a merchant by its marketplace-scoped `external_seller_id` →
    /// `(id, status)`, or `None` if there's no such merchant in that marketplace.
    /// The marketplace scope is the authorization boundary for skill onboarding:
    /// an API key can only ever reach merchants of its own marketplace.
    pub async fn resolve_connect_merchant_by_external(
        &self,
        marketplace_id: Uuid,
        external_seller_id: &str,
    ) -> Result<Option<(Uuid, String)>> {
        let row: Option<(Uuid, String)> = sqlx::query_as(
            r#"SELECT id, status::text
                 FROM connect_merchants
                WHERE marketplace_id = $1 AND external_seller_id = $2"#,
        )
        .bind(marketplace_id)
        .bind(external_seller_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// The merchant's active, SIWE-verified payout wallet address, or `None` if it
    /// has no such wallet. This is the value snapshotted onto a skill as
    /// `destination_wallet` at onboarding time (03-01).
    pub async fn get_connect_active_wallet_address(
        &self,
        merchant_id: Uuid,
    ) -> Result<Option<String>> {
        let row: Option<(String,)> = sqlx::query_as(
            r#"SELECT address
                 FROM connect_wallets
                WHERE merchant_id = $1 AND is_active AND verified_at IS NOT NULL
                ORDER BY created_at DESC
                LIMIT 1"#,
        )
        .bind(merchant_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.0))
    }

    /// A marketplace's current fee config `(marketplace_fee_bps, apiosk_fee_bps)`,
    /// or `None` if the marketplace doesn't exist. Read once at skill onboarding to
    /// take the fee SNAPSHOT (039) — payments never re-read these live.
    pub async fn get_connect_marketplace_fees(
        &self,
        marketplace_id: Uuid,
    ) -> Result<Option<(i32, i32)>> {
        let row: Option<(i32, i32)> = sqlx::query_as(
            "SELECT marketplace_fee_bps, apiosk_fee_bps FROM connect_marketplaces WHERE id = $1",
        )
        .bind(marketplace_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Insert a billable skill with its fee + wallet SNAPSHOT (039). Returns
    /// `(id, created_at)`. A duplicate `(marketplace_id, external_skill_ref)` hits
    /// the `uq_connect_skill_ref` unique index and surfaces as a `sqlx` database
    /// error with SQLSTATE 23505 — the caller maps that to 409.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_connect_skill(
        &self,
        marketplace_id: Uuid,
        merchant_id: Uuid,
        external_skill_ref: &str,
        name: &str,
        price_amount: i64,
        price_currency: &str,
        destination_wallet: &str,
        marketplace_fee_bps: i32,
        apiosk_fee_bps: i32,
    ) -> Result<(Uuid, DateTime<Utc>)> {
        let row: (Uuid, DateTime<Utc>) = sqlx::query_as(
            r#"INSERT INTO connect_skills
                   (marketplace_id, merchant_id, external_skill_ref, name,
                    price_amount, price_currency, destination_wallet,
                    marketplace_fee_bps, apiosk_fee_bps)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
               RETURNING id, created_at"#,
        )
        .bind(marketplace_id)
        .bind(merchant_id)
        .bind(external_skill_ref)
        .bind(name)
        .bind(price_amount)
        .bind(price_currency)
        .bind(destination_wallet)
        .bind(marketplace_fee_bps)
        .bind(apiosk_fee_bps)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    /// Ownership row for a skill → `(marketplace_id, merchant_id)`, or `None` if it
    /// doesn't exist. Powers the PUT authorization check (03-02): a skill may only
    /// be updated through its own marketplace's key.
    pub async fn get_connect_skill_owner(&self, skill_id: Uuid) -> Result<Option<(Uuid, Uuid)>> {
        let row: Option<(Uuid, Uuid)> =
            sqlx::query_as("SELECT marketplace_id, merchant_id FROM connect_skills WHERE id = $1")
                .bind(skill_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row)
    }

    /// Load a skill's charge view — price/fee/wallet snapshot joined with the
    /// merchant's live status + active-wallet presence — or `None` if the skill
    /// doesn't exist. One query (no N+1); the merchant join + wallet EXISTS feed
    /// the 03-03 guard at charge time (04-02).
    #[allow(clippy::type_complexity)]
    pub async fn get_connect_skill_charge(
        &self,
        skill_id: Uuid,
    ) -> Result<Option<ConnectSkillCharge>> {
        let row: Option<(
            Uuid,
            Uuid,
            i64,
            String,
            String,
            i32,
            i32,
            bool,
            String,
            bool,
        )> = sqlx::query_as(
            r#"SELECT s.marketplace_id, s.merchant_id, s.price_amount, s.price_currency,
                          s.destination_wallet, s.marketplace_fee_bps, s.apiosk_fee_bps, s.active,
                          m.status::text AS merchant_status,
                          EXISTS(
                              SELECT 1 FROM connect_wallets w
                               WHERE w.merchant_id = s.merchant_id
                                 AND w.is_active AND w.verified_at IS NOT NULL
                          ) AS has_active_wallet
                     FROM connect_skills s
                     JOIN connect_merchants m ON m.id = s.merchant_id
                    WHERE s.id = $1"#,
        )
        .bind(skill_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| ConnectSkillCharge {
            marketplace_id: r.0,
            merchant_id: r.1,
            price_amount: r.2,
            price_currency: r.3,
            destination_wallet: r.4,
            marketplace_fee_bps: r.5,
            apiosk_fee_bps: r.6,
            active: r.7,
            merchant_status: r.8,
            has_active_wallet: r.9,
        }))
    }

    /* ---------------------------------------------------------------------- */
    /* Payments + payouts (package 04)                                        */
    /* ---------------------------------------------------------------------- */

    /// Replay pre-check: has this x402 authorization nonce already produced a
    /// connect payment? The `uq_connect_payment_nonce` partial-unique index is the
    /// hard backstop; this lets the charge flow reject a replay with a clean 409
    /// before it calls settle.
    pub async fn connect_payment_nonce_seen(&self, nonce: &str) -> Result<bool> {
        let row: (bool,) =
            sqlx::query_as("SELECT EXISTS(SELECT 1 FROM connect_payments WHERE nonce = $1)")
                .bind(nonce)
                .fetch_one(&self.pool)
                .await?;
        Ok(row.0)
    }

    /// Record a settled connect payment with its split components (04-04). Status
    /// is `settled` and `settled_at` is stamped here — the gross has already moved
    /// to the platform wallet via the facilitator. The `chk_connect_split` CHECK
    /// enforces `seller + marketplace + apiosk == gross`; a replayed nonce trips
    /// `uq_connect_payment_nonce` (surfaces as a 23505 the caller maps to 409).
    #[allow(clippy::too_many_arguments)]
    pub async fn record_settled_connect_payment(
        &self,
        marketplace_id: Uuid,
        merchant_id: Uuid,
        skill_id: Uuid,
        payer_address: Option<&str>,
        gross_amount: i64,
        seller_amount: i64,
        marketplace_fee: i64,
        apiosk_fee: i64,
        destination_wallet: &str,
        purpose_text: Option<&str>,
        nonce: Option<&str>,
        settle_tx_hash: Option<&str>,
    ) -> Result<Uuid> {
        let row: (Uuid,) = sqlx::query_as(
            r#"INSERT INTO connect_payments
                   (marketplace_id, merchant_id, skill_id, payer_address, gross_amount,
                    seller_amount, marketplace_fee, apiosk_fee, destination_wallet,
                    status, purpose_text, nonce, settle_tx_hash, settled_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9,
                       'settled', $10, $11, $12, now())
               RETURNING id"#,
        )
        .bind(marketplace_id)
        .bind(merchant_id)
        .bind(skill_id)
        .bind(payer_address)
        .bind(gross_amount)
        .bind(seller_amount)
        .bind(marketplace_fee)
        .bind(apiosk_fee)
        .bind(destination_wallet)
        .bind(purpose_text)
        .bind(nonce)
        .bind(settle_tx_hash)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.0)
    }

    /// Enqueue one leg of a settled payment's split into `connect_payouts` (046).
    /// Idempotent: `ON CONFLICT (connect_payment_id, kind) DO NOTHING`, so a retried
    /// settle can't double-enqueue. `kind` is `merchant` or `marketplace`; amount is
    /// minor units. The developer_payout_worker drains it (08).
    pub async fn enqueue_connect_payout(
        &self,
        connect_payment_id: Uuid,
        kind: &str,
        recipient_wallet: &str,
        amount: i64,
    ) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO connect_payouts
                   (connect_payment_id, kind, recipient_wallet, amount)
               VALUES ($1, $2, $3, $4)
               ON CONFLICT (connect_payment_id, kind) DO NOTHING"#,
        )
        .bind(connect_payment_id)
        .bind(kind)
        .bind(recipient_wallet)
        .bind(amount)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The marketplace owner's verified payout wallet (the address they connected
    /// in the portal), or `None` if they have none verified. This is where the
    /// marketplace's fee share is sent. Newest verified wallet wins.
    pub async fn get_connect_marketplace_payout_wallet(
        &self,
        marketplace_id: Uuid,
    ) -> Result<Option<String>> {
        let row: Option<(String,)> = sqlx::query_as(
            r#"SELECT w.address
                 FROM connect_marketplaces m
                 JOIN payout_wallets w
                   ON w.owner_id = m.owner_user_id
                  AND w.verification_status = 'verified'
                WHERE m.id = $1
                ORDER BY w.verified_at DESC NULLS LAST
                LIMIT 1"#,
        )
        .bind(marketplace_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.0))
    }

    /// Patch a skill (03-02). Only the provided fields change — `COALESCE` leaves a
    /// `NULL` argument as the existing value, so this never mass-overwrites. The fee
    /// snapshot is deliberately NOT touched here (never silently re-priced);
    /// `destination_wallet` changes only on a deliberate re-snapshot by the caller.
    pub async fn update_connect_skill(
        &self,
        skill_id: Uuid,
        price_amount: Option<i64>,
        active: Option<bool>,
        destination_wallet: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            r#"UPDATE connect_skills
                  SET price_amount       = COALESCE($2, price_amount),
                      active             = COALESCE($3, active),
                      destination_wallet = COALESCE($4, destination_wallet),
                      updated_at         = now()
                WHERE id = $1"#,
        )
        .bind(skill_id)
        .bind(price_amount)
        .bind(active)
        .bind(destination_wallet)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
