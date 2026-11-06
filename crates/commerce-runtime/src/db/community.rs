use super::*;

impl Database {
    #[allow(clippy::too_many_arguments)]
    pub async fn create_community_api(
        &self,
        id: Uuid,
        slug: &str,
        name: &str,
        description: &str,
        endpoint_url: &str,
        owner_wallet: &str,
        price: &BigDecimal,
        category: &str,
        listing_metadata: &serde_json::Value,
        verified: bool,
    ) -> Result<()> {
        let community_owner_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001")
            .unwrap_or_else(|_| Uuid::new_v4());
        let status = if verified { "active" } else { "inactive" };

        sqlx::query(
            r#"INSERT INTO apis
               (id, slug, name, description, origin_url, endpoint_url, owner_wallet,
                is_community, verified_at, active, status, category, listing_metadata, commission_rate, owner_id, wallet_address, is_verified)
               VALUES ($1, $2, $3, $4, $5, $6, $7, true,
                       CASE WHEN $8 THEN NOW() ELSE NULL END,
                       $8,
                       $9,
                       $10, $11, 0.03, $12, $7, $8)"#,
        )
        .bind(id)
        .bind(slug)
        .bind(name)
        .bind(description)
        .bind(endpoint_url)
        .bind(endpoint_url)
        .bind(owner_wallet)
        .bind(verified)
        .bind(status)
        .bind(category)
        .bind(listing_metadata)
        .bind(community_owner_id)
        .execute(&self.pool)
        .await?;

        for method in &["GET", "POST"] {
            sqlx::query(
                r#"INSERT INTO api_endpoints
                   (id, api_id, method, path, payment_required, price)
                   VALUES ($1, $2, $3, '/', true, $4)"#,
            )
            .bind(Uuid::new_v4())
            .bind(id)
            .bind(method)
            .bind(price)
            .execute(&self.pool)
            .await?;
        }

        Ok(())
    }

    /// Insert a FEDERATED listing from the public self-serve intake
    /// (`src/federated_intake.rs`) as a `status='pending'` row so it lands in the
    /// existing admin review queue. Mirrors the provider portal's
    /// `federated-import` commit shape, with two differences for the no-account
    /// path: the owner is the synthetic community sentinel (whose rows are NOT
    /// auto-verified — see migration 023 `trg_apis_auto_verify_owner`, so we set
    /// `is_verified=true` explicitly), and the submitter's logo + contact email
    /// live in `listing_metadata` (there is no `provider_profiles` row to join).
    ///
    /// NOT proxied: `listing_type='federated'` diverts every request path; the
    /// provider payTo in `wallet_address`/`external_x402` is informational and is
    /// never rewritten to the platform wallet. Invisible until approved — the
    /// discovery gate requires `status='active'` (this row is `'pending'`).
    ///
    /// Re-submission: like `federated-import` commit() this upserts on `slug`,
    /// so a provider who fixes a typo and submits again updates their pending
    /// row instead of hitting "slug taken" (the confirmation page tells them to
    /// do exactly that). The DO UPDATE is fenced to a row that is still
    /// `pending`, still self-serve, and carries the SAME contact email — the
    /// only ownership claim a no-account submitter has — so a re-submit can
    /// never touch an approved listing or a logged-in provider's row. Doing the
    /// ownership test inside the upsert (rather than a SELECT first) also closes
    /// the check-then-insert race on this globally unique slug.
    ///
    /// Returns the row's id: equal to `id` when it was inserted, the existing
    /// row's id when it updated, and `None` when the slug belongs to a listing
    /// the submitter may not touch.
    ///
    /// Writing `status='pending'` is what arms the review pipeline: the BEFORE
    /// trigger stamps `submitted_for_review_at`, and the AFTER trigger
    /// (`trg_apis_review_email`) enqueues an outbox row + dispatches the
    /// `api-review-mailer` Edge Function — the same path the logged-in provider
    /// flow uses. An UPDATE leaves status alone, so a re-submit does not
    /// re-notify.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_federated_submission(
        &self,
        id: Uuid,
        slug: &str,
        name: &str,
        description: &str,
        discovery_url: &str,
        primary_pay_to: &str,
        category: &str,
        external_x402: &serde_json::Value,
        listing_metadata: &serde_json::Value,
        website_url: &str,
        github_url: Option<&str>,
        endpoints: &[(String, String, Option<String>, f64)],
    ) -> Result<Option<Uuid>> {
        let community_owner_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001")
            .unwrap_or_else(|_| Uuid::new_v4());

        let mut tx = self.pool.begin().await?;

        // `active` is derived from status by the BEFORE-INSERT sync trigger
        // (023), so it is intentionally not set here. listing_type/source use
        // bare 'federated' literals (assignment-coerced to the enum), matching
        // the onboard-federated-*.sql inserts. `docs_url` carries the discovery
        // URL: it is what the admin review queue shows (admin_pending_apis
        // selects apis.docs_url) and what the public detail page links as docs,
        // so storing it keeps the approved page identical to the preview.
        let row_id: Option<Uuid> = sqlx::query_scalar(
            r#"INSERT INTO apis
               (id, owner_id, slug, name, description, origin_url, docs_url, wallet_address,
                status, category, listing_type, source, commission_rate,
                is_verified, is_external, external_discovery_url, external_x402,
                listing_metadata, website_url, github_url)
               VALUES ($1, $2, $3, $4, $5, $6, $6, $7,
                       'pending', $8, 'federated', 'federated', 0,
                       true, true, $6, $9,
                       $10, $11, $12)
               ON CONFLICT (slug) DO UPDATE SET
                 name                   = excluded.name,
                 description            = excluded.description,
                 origin_url             = excluded.origin_url,
                 docs_url               = excluded.docs_url,
                 wallet_address         = excluded.wallet_address,
                 category               = excluded.category,
                 external_discovery_url = excluded.external_discovery_url,
                 external_x402          = excluded.external_x402,
                 listing_metadata       = excluded.listing_metadata,
                 website_url            = excluded.website_url,
                 github_url             = excluded.github_url
               WHERE apis.status = 'pending'
                 AND apis.listing_metadata->>'source' = 'self-serve-federated'
                 AND lower(apis.listing_metadata->>'contact_email')
                     = lower(excluded.listing_metadata->>'contact_email')
               RETURNING id"#,
        )
        .bind(id)
        .bind(community_owner_id)
        .bind(slug)
        .bind(name)
        .bind(description)
        .bind(discovery_url)
        .bind(primary_pay_to)
        .bind(category)
        .bind(external_x402)
        .bind(listing_metadata)
        .bind(website_url)
        .bind(github_url)
        .fetch_optional(&mut *tx)
        .await?;

        let Some(row_id) = row_id else {
            // Slug taken by a listing this submitter may not touch.
            tx.rollback().await?;
            return Ok(None);
        };

        // Replace the endpoint set wholesale so a re-submit re-syncs cleanly
        // (same as federated-import commit()). A no-op on the insert path.
        sqlx::query("DELETE FROM api_endpoints WHERE api_id = $1")
            .bind(row_id)
            .execute(&mut *tx)
            .await?;

        for (method, path, endpoint_description, price) in endpoints {
            let price_bd = BigDecimal::try_from(*price).unwrap_or_else(|_| BigDecimal::from(0));
            sqlx::query(
                r#"INSERT INTO api_endpoints
                   (id, api_id, method, path, description, price, payment_required)
                   VALUES ($1, $2, $3, $4, $5, $6, true)"#,
            )
            .bind(Uuid::new_v4())
            .bind(row_id)
            .bind(method)
            .bind(path)
            .bind(endpoint_description.as_deref())
            .bind(&price_bd)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(Some(row_id))
    }

    pub async fn update_community_api(
        &self,
        api_id: Uuid,
        endpoint_url: Option<&str>,
        price: Option<&BigDecimal>,
        description: Option<&str>,
        active: Option<bool>,
        listing_metadata: Option<&serde_json::Value>,
    ) -> Result<()> {
        let mut updates = vec![];
        let mut param_count = 1;

        if endpoint_url.is_some() {
            updates.push(format!(
                "endpoint_url = ${}, origin_url = ${}",
                param_count, param_count
            ));
            param_count += 1;
        }
        if description.is_some() {
            updates.push(format!("description = ${}", param_count));
            param_count += 1;
        }
        if active.is_some() {
            updates.push(format!(
                "active = ${}, status = ${}",
                param_count,
                param_count + 1
            ));
            param_count += 2;
        }
        if listing_metadata.is_some() {
            // MERGE, never replace.
            //
            // This is a whole-column write on a jsonb that holds keys the
            // caller does not own and cannot see. The one that bites is
            // `resell.enabled`: without it `handle_flow.rs` leaves
            // `resolved.resell` as None, `forward.rs`'s gate stays shut, and
            // the upstream's own 402 passes STRAIGHT BACK to the buyer as a
            // payment failure. Migration 091_restore_resell_after_sync_wipe
            // exists because the sync scripts did exactly this and silently
            // unpaid every federated listing until someone noticed.
            //
            // `req.listing_metadata` comes off an HTTP body, so an owner
            // updating one field through the portal used to drop every key
            // they happened not to send. `||` is a shallow merge: keys the
            // caller sends still win, keys they never mentioned survive.
            //
            // The cost is that a key can no longer be deleted by omitting it.
            // That is the right trade here — silent data loss is worse than
            // needing an explicit route to remove a key, and nothing today
            // removes one this way.
            updates.push(format!(
                "listing_metadata = COALESCE(listing_metadata, '{{}}'::jsonb) || ${}",
                param_count
            ));
            param_count += 1;
        }

        if updates.is_empty() && price.is_none() {
            return Ok(());
        }

        if !updates.is_empty() {
            let query_str = format!(
                "UPDATE apis SET {} WHERE id = ${}",
                updates.join(", "),
                param_count
            );

            let mut query = sqlx::query(&query_str);

            if let Some(url) = endpoint_url {
                query = query.bind(url);
            }
            if let Some(desc) = description {
                query = query.bind(desc);
            }
            if let Some(act) = active {
                query = query.bind(act);
                query = query.bind(if act { "active" } else { "inactive" });
            }
            if let Some(metadata) = listing_metadata {
                query = query.bind(metadata);
            }
            query = query.bind(api_id);

            query.execute(&self.pool).await?;
        }

        if let Some(new_price) = price {
            sqlx::query("UPDATE api_endpoints SET price = $1 WHERE api_id = $2")
                .bind(new_price)
                .bind(api_id)
                .execute(&self.pool)
                .await?;
        }

        Ok(())
    }

    pub async fn get_apis_by_wallet(
        &self,
        wallet: &str,
    ) -> Result<Vec<crate::api_management::CommunityApiInfo>> {
        let apis = sqlx::query_as::<_, (
            Uuid,
            String,
            String,
            String,
            String,
            BigDecimal,
            bool,
            Option<chrono::DateTime<Utc>>,
            chrono::DateTime<Utc>,
        )>(
            r#"SELECT
                 a.id, a.slug, COALESCE(a.name, a.slug) as name,
                 COALESCE(a.description, '') as description,
                 COALESCE(a.endpoint_url, a.origin_url) as endpoint_url,
                 COALESCE((SELECT MIN(e.price) FROM api_endpoints e WHERE e.api_id = a.id), 0) as price_usd,
                 COALESCE(a.active, true) as active,
                 a.verified_at,
                 a.created_at
               FROM apis a
               WHERE a.owner_wallet = $1 AND a.is_community = true
               ORDER BY a.created_at DESC"#,
        )
        .bind(wallet)
        .fetch_all(&self.pool)
        .await?;

        let mut result = vec![];
        for (id, slug, name, description, endpoint_url, price, active, verified_at, created_at) in
            apis
        {
            let usage: (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM gateway_usage WHERE api_id = $1")
                    .bind(id)
                    .fetch_one(&self.pool)
                    .await
                    .unwrap_or((0,));

            let earnings: (BigDecimal,) = sqlx::query_as(
                "SELECT COALESCE(SUM(developer_amount), 0) FROM developer_revenue WHERE api_id = $1"
            )
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .unwrap_or((BigDecimal::from(0),));

            let pending: (BigDecimal,) = sqlx::query_as(
                "SELECT COALESCE(SUM(developer_amount), 0) FROM developer_revenue WHERE api_id = $1 AND withdrawn = false"
            )
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .unwrap_or((BigDecimal::from(0),));

            result.push(crate::api_management::CommunityApiInfo {
                id: id.to_string(),
                slug,
                name,
                description,
                endpoint_url,
                price_usd: price.to_string().parse().unwrap_or(0.0),
                active,
                verified: verified_at.is_some(),
                created_at: created_at.to_rfc3339(),
                total_requests: usage.0,
                total_earned_usd: earnings.0.to_string().parse().unwrap_or(0.0),
                pending_withdrawal_usd: pending.0.to_string().parse().unwrap_or(0.0),
            });
        }

        Ok(result)
    }

    pub async fn get_total_earnings(&self, wallet: &str) -> Result<BigDecimal> {
        let total: (BigDecimal,) = sqlx::query_as(
            "SELECT COALESCE(SUM(developer_amount), 0) FROM developer_revenue WHERE owner_wallet = $1"
        )
        .bind(wallet)
        .fetch_one(&self.pool)
        .await?;

        Ok(total.0)
    }

    pub async fn deactivate_api(&self, api_id: Uuid) -> Result<()> {
        sqlx::query("UPDATE apis SET active = false, status = 'inactive' WHERE id = $1")
            .bind(api_id)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

}
