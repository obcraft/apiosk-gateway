//! Resolving an external x402 host to a listing the books can name.
//!
//! `settlements.api_id` and `gateway_usage.api_id` are both `not null
//! references apis(id)`. That is not an obstacle to work around — it is the
//! books insisting that every recorded payment names who was paid, and it is
//! the reason `/v1/x402/fetch` has never appeared in a revenue chart: it pays a
//! host, and a host is not an `apis` row.
//!
//! So give it one. The shape already exists and is already in use — 187 rows
//! carry `is_external = true` with `listing_type = 'federated'`, which is
//! exactly "external x402 supply Apiosk discovers and executes but does not
//! operate". A host the sweep found and a buyer paid is that, precisely.
//!
//! Resolution before creation, always. Most paid hosts ALREADY have a federated
//! listing — that is what `external_x402_fetches.ref_marker_sent` means — and
//! minting a second row for one provider would split it across two lines in
//! every per-provider breakdown while leaving the totals right, which is the
//! kind of wrong that survives review.

use super::*;

/// The owner every listing nobody signed up for is filed under. Same id
/// `create_community_api` uses; there is one system owner, not two.
const SYSTEM_OWNER_ID: &str = "00000000-0000-0000-0000-000000000001";

impl Database {
    /// The listing and endpoint to bill an external x402 call against,
    /// creating them only if this host has never been seen.
    ///
    /// `Ok(None)` is a real answer and not an error: it means the call cannot
    /// be attributed, and the caller must go on to answer the buyer anyway. A
    /// payment that already happened is never unwound because the bookkeeping
    /// for it failed.
    pub async fn ensure_external_listing(
        &self,
        host: &str,
        pay_to: &str,
        method: &str,
        path: &str,
        price: &BigDecimal,
    ) -> Result<Option<(Api, Endpoint)>> {
        let host = host.trim().to_ascii_lowercase();
        if host.is_empty() {
            return Ok(None);
        }

        let api = match self.find_api_by_host(&host).await? {
            Some(api) => api,
            None => self.create_external_listing(&host, pay_to).await?,
        };
        let endpoint = self
            .ensure_external_endpoint(api.id, method, path, price)
            .await?;
        Ok(Some((api, endpoint)))
    }

    /// An existing listing serving this host, whatever it is called.
    ///
    /// The host is compared against the parsed authority of each URL rather
    /// than by `like 'https://host%'`: a prefix match on the raw column reads
    /// `https://api.example.com.attacker.test/` as a match for
    /// `api.example.com`, and this function decides who gets credited with
    /// revenue.
    async fn find_api_by_host(&self, host: &str) -> Result<Option<Api>> {
        let row = sqlx::query_as::<_, Api>(
            r#"SELECT id, slug, origin_url, wallet_address, status, owner_id,
                      commission_rate, owner_wallet, endpoint_url, is_community,
                      active, is_verified, listing_type::text AS listing_type,
                      hmac_signing_algorithm, hmac_signing_header,
                      hmac_signing_secret_enc, hmac_signing_payload_format
                 FROM apis
                WHERE status <> 'inactive'
                  AND lower(split_part(split_part(
                        coalesce(origin_url, endpoint_url, external_discovery_url, ''),
                        '://', 2), '/', 1)) = $1
                -- A live listing outranks one still in review, and the oldest
                -- outranks a newer duplicate, so the host resolves to the same
                -- row on every call rather than drifting between two.
                ORDER BY (status = 'active') DESC, created_at ASC
                LIMIT 1"#,
        )
        .bind(host)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Mint the listing for a host nobody has listed.
    ///
    /// `status = 'active'` because this is not a submission awaiting review —
    /// it is a record of a provider who has already been paid, and a row the
    /// revenue charts must join to. It is kept out of the marketplace by
    /// `is_external`/`listing_type`, which is what the other 187 rows are kept
    /// out by, not by a status nobody queries on.
    async fn create_external_listing(&self, host: &str, pay_to: &str) -> Result<Api> {
        let owner_id = Uuid::parse_str(SYSTEM_OWNER_ID)?;
        let slug = external_slug(host);

        let api = sqlx::query_as::<_, Api>(
            r#"INSERT INTO apis
                 (owner_id, slug, name, origin_url, wallet_address, status,
                  listing_type, integration_mode, is_external, source, category,
                  description, listing_metadata)
               VALUES ($1, $2, $3, $4, $5, 'active',
                       'federated'::listing_type, 'federated', true, 'x402-fetch', 'other',
                       $6, jsonb_build_object('source', 'x402-fetch', 'host', $3))
               ON CONFLICT (slug) DO UPDATE
                 -- Touch a column so RETURNING yields the existing row rather
                 -- than nothing, which is what DO NOTHING would do and would
                 -- turn a harmless race into a lost settlement.
                 SET wallet_address = COALESCE(NULLIF(apis.wallet_address, ''), EXCLUDED.wallet_address)
               RETURNING id, slug, origin_url, wallet_address, status, owner_id,
                         commission_rate, owner_wallet, endpoint_url, is_community,
                         active, is_verified, listing_type::text AS listing_type,
                         hmac_signing_algorithm, hmac_signing_header,
                         hmac_signing_secret_enc, hmac_signing_payload_format"#,
        )
        .bind(owner_id)
        .bind(&slug)
        .bind(host)
        .bind(format!("https://{host}"))
        .bind(pay_to)
        .bind(format!("External x402 endpoint at {host}, paid through Apiosk."))
        .fetch_one(&self.pool)
        .await?;

        tracing::info!(host = %host, slug = %slug, api_id = %api.id, "minted external listing for x402 fetch");
        Ok(api)
    }

    /// The endpoint row for one external target, at the price actually quoted.
    ///
    /// The price is refreshed on every call because an external provider's 402
    /// is the live price and this row is only ever a mirror of it. Nothing
    /// reads it to decide what to charge — the buyer was already charged what
    /// the provider asked — so a stale figure here would misreport, never
    /// misbill.
    async fn ensure_external_endpoint(
        &self,
        api_id: Uuid,
        method: &str,
        path: &str,
        price: &BigDecimal,
    ) -> Result<Endpoint> {
        let path = if path.is_empty() { "/" } else { path };
        let endpoint = sqlx::query_as::<_, Endpoint>(
            r#"INSERT INTO api_endpoints (api_id, method, path, payment_required, price)
               VALUES ($1, $2, $3, true, $4)
               ON CONFLICT (api_id, method, path) DO UPDATE SET price = EXCLUDED.price
               RETURNING id, api_id, method, path, payment_required, price, network_token_id,
                         origin_override, auth_override, cache_ttl_seconds"#,
        )
        .bind(api_id)
        .bind(method.to_ascii_uppercase())
        .bind(path)
        .bind(price)
        .fetch_one(&self.pool)
        .await?;
        Ok(endpoint)
    }
}

/// A host as a slug that cannot collide with a listing somebody registered.
///
/// Prefixed rather than merely sanitised: `api.stripe.com` and a real listing
/// called `api-stripe-com` are different things, and the prefix is what keeps
/// an auto-minted row from ever being mistaken for — or colliding with — one a
/// provider chose.
fn external_slug(host: &str) -> String {
    let cleaned: String = host
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    format!("ext-{}", &trimmed[..trimmed.len().min(58)])
}

#[cfg(test)]
mod tests {
    use super::external_slug;

    #[test]
    fn a_host_becomes_a_prefixed_slug() {
        assert_eq!(external_slug("x402.forgemesh.io"), "ext-x402-forgemesh-io");
        assert_eq!(
            external_slug("bazaar-gateway.vercel.app"),
            "ext-bazaar-gateway-vercel-app"
        );
    }

    #[test]
    fn the_prefix_is_what_stops_a_collision_with_a_real_listing() {
        // A provider who registered the slug `x402-forgemesh-io` keeps it.
        assert_ne!(external_slug("x402.forgemesh.io"), "x402-forgemesh-io");
        assert!(external_slug("anything.example.com").starts_with("ext-"));
    }

    #[test]
    fn a_very_long_host_stays_inside_the_column() {
        let host = format!("{}.example.com", "a".repeat(120));
        assert!(external_slug(&host).len() <= 62);
    }
}
