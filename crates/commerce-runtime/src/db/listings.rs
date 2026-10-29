use super::*;

impl Database {
    pub async fn get_api_by_slug(&self, slug: &str) -> Result<Option<Api>> {
        let api = sqlx::query_as::<_, Api>(
            "SELECT id, slug, origin_url, coalesce(wallet_address, '') AS wallet_address, coalesce(is_platform_owned AND settlement_mode = 'apiosk_revenue' AND NOT requires_payout_wallet AND listing_type::text = 'api' AND coalesce(listing_metadata->'resell'->>'enabled', 'false') <> 'true', false) AS apiosk_revenue, status, owner_id, commission_rate, owner_wallet, endpoint_url, is_community, active, is_verified, listing_type::text AS listing_type, hmac_signing_algorithm, hmac_signing_header, hmac_signing_secret_enc, hmac_signing_payload_format FROM apis WHERE slug = $1",
        )
        .bind(slug)
        .fetch_optional(&self.pool)
        .await?;

        Ok(api)
    }

    /// Public profile (display name / handle / avatar / bio / sector) of the
    /// provider that owns `slug`, joined from `provider_profiles` on
    /// `apis.owner_id`. Non-sensitive fields only — never auth/wallet columns.
    /// Returns None when the listing or its owner profile is absent.
    #[allow(clippy::type_complexity)]
    pub async fn get_provider_public(
        &self,
        slug: &str,
    ) -> Result<
        Option<(
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        )>,
    > {
        let row = sqlx::query_as::<
            _,
            (
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
            ),
        >(
            "SELECT p.display_name, p.username, p.avatar_url, p.bio, p.sector \
             FROM apis a JOIN provider_profiles p ON p.id = a.owner_id \
             WHERE a.slug = $1",
        )
        .bind(slug)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row)
    }

    /// Decrypt an HMAC signing secret stored as bytea ciphertext. The
    /// `decrypt_provider_secret` RPC is service_role only via PostgREST,
    /// but the gateway connects with the postgres superuser so it has
    /// EXECUTE rights regardless.
    pub async fn decrypt_provider_secret(&self, ciphertext: &[u8]) -> Result<Option<String>> {
        let plaintext: Option<String> =
            sqlx::query_scalar("SELECT decrypt_provider_secret($1::bytea)")
                .bind(ciphertext)
                .fetch_one(&self.pool)
                .await?;
        Ok(plaintext)
    }

    pub async fn get_listing_by_slug(&self, slug: &str) -> Result<Option<Listing>> {
        let listing = sqlx::query_as::<_, Listing>(
            r#"SELECT id, slug, origin_url, coalesce(wallet_address, '') AS wallet_address, status, owner_id,
                      commission_rate, listing_type, is_platform_owned, listing_metadata
               FROM apis WHERE slug = $1"#,
        )
        .bind(slug)
        .fetch_optional(&self.pool)
        .await?;

        Ok(listing)
    }

    pub async fn get_skill_listing(&self, api_id: Uuid) -> Result<Option<SkillListing>> {
        let s = sqlx::query_as::<_, SkillListing>(
            "SELECT id, api_id, endpoint_url, input_schema, tool_manifest, auth_type, auth_secret
             FROM skill_listings WHERE api_id = $1",
        )
        .bind(api_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(s)
    }

    pub async fn get_product_listing(&self, api_id: Uuid) -> Result<Option<ProductListing>> {
        let p = sqlx::query_as::<_, ProductListing>(
            r#"SELECT id, api_id, storage_type, storage_path, file_name, mime_type,
                      file_size_bytes, license_type, license_key,
                      download_expiry_seconds, max_downloads
               FROM product_listings WHERE api_id = $1"#,
        )
        .bind(api_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(p)
    }

    pub async fn get_dataset_listing(&self, api_id: Uuid) -> Result<Option<DatasetListing>> {
        let d = sqlx::query_as::<_, DatasetListing>(
            r#"SELECT id, api_id, delivery_type, bulk_url, query_endpoint,
                      data_schema, sample_data, record_count, data_format
               FROM dataset_listings WHERE api_id = $1"#,
        )
        .bind(api_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(d)
    }

    pub async fn get_service_listing(&self, api_id: Uuid) -> Result<Option<ServiceListing>> {
        let s = sqlx::query_as::<_, ServiceListing>(
            r#"SELECT id, api_id, webhook_url, relay_method, trigger_schema,
                      auth_type, auth_secret, timeout_ms
               FROM service_listings WHERE api_id = $1"#,
        )
        .bind(api_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(s)
    }

    pub async fn get_connector_listing(&self, api_id: Uuid) -> Result<Option<ConnectorListing>> {
        let c = sqlx::query_as::<_, ConnectorListing>(
            r#"SELECT id, api_id, platform, auth_type, relay_url, supported_actions,
                      NULL::text as auth_secret
               FROM connector_listings WHERE api_id = $1"#,
        )
        .bind(api_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(c)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_product_purchase(
        &self,
        product_id: Uuid,
        api_id: Uuid,
        buyer_wallet: &str,
        tx_hash: &str,
        amount_paid: BigDecimal,
        network_token_id: Option<Uuid>,
        access_token: &str,
        download_expiry_seconds: i32,
    ) -> Result<ProductPurchase> {
        let expires_at = if download_expiry_seconds > 0 {
            Some(Utc::now() + chrono::Duration::seconds(download_expiry_seconds as i64))
        } else {
            None
        };

        let purchase = sqlx::query_as::<_, ProductPurchase>(
            r#"INSERT INTO product_purchases
               (product_id, api_id, buyer_wallet, tx_hash, amount_paid, network_token_id, access_token, expires_at, max_downloads)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
               RETURNING id, product_id, api_id, buyer_wallet, tx_hash, access_token, download_count, max_downloads, expires_at"#,
        )
        .bind(product_id)
        .bind(api_id)
        .bind(buyer_wallet)
        .bind(tx_hash)
        .bind(&amount_paid)
        .bind(network_token_id)
        .bind(access_token)
        .bind(expires_at)
        .bind(0_i32)
        .fetch_one(&self.pool)
        .await?;

        Ok(purchase)
    }

    pub async fn get_purchase_by_token(
        &self,
        access_token: &str,
    ) -> Result<Option<ProductPurchase>> {
        let p = sqlx::query_as::<_, ProductPurchase>(
            r#"SELECT id, product_id, api_id, buyer_wallet, tx_hash, access_token,
                      download_count, max_downloads, expires_at
               FROM product_purchases WHERE access_token = $1"#,
        )
        .bind(access_token)
        .fetch_optional(&self.pool)
        .await?;
        Ok(p)
    }

    pub async fn increment_download_count(&self, purchase_id: Uuid) -> Result<()> {
        sqlx::query(
            "UPDATE product_purchases SET download_count = download_count + 1 WHERE id = $1",
        )
        .bind(purchase_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
