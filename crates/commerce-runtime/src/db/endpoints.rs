use super::*;

impl Database {
    pub async fn list_endpoint_pricing_rules(
        &self,
        endpoint_id: Uuid,
    ) -> Result<Vec<EndpointPricingRule>> {
        Ok(sqlx::query_as::<_, EndpointPricingRule>(
            r#"SELECT parameter_name, operator, comparison_value, price, priority
               FROM endpoint_pricing_rules
               WHERE endpoint_id = $1 AND active = true
               ORDER BY priority ASC, created_at ASC"#,
        )
        .bind(endpoint_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn get_endpoint(
        &self,
        api_id: Uuid,
        method: &str,
        path: &str,
    ) -> Result<Option<Endpoint>> {
        let endpoints = sqlx::query_as::<_, Endpoint>(
            r#"SELECT id, api_id, method, path, payment_required, price, network_token_id,
                      origin_override, auth_override, cache_ttl_seconds
               FROM api_endpoints
               WHERE api_id = $1 AND method = $2 AND COALESCE(active, true)
               ORDER BY path, id"#,
        )
        .bind(api_id)
        .bind(method)
        .fetch_all(&self.pool)
        .await?;
        Ok(endpoints
            .into_iter()
            .filter_map(|ep| endpoint_match_score(&ep.path, path).map(|score| (score, ep)))
            .max_by_key(|(score, _)| *score)
            .map(|(_, ep)| ep))
    }

    /// The parameter schemas of specific endpoints, by id.
    ///
    /// `CapabilityOffer` carries what an endpoint costs and where it lives but
    /// not what it takes, because until now nothing downstream of `/v1/decide`
    /// needed to fill anything in. Request binding does: it can only bind a
    /// parameter the endpoint actually documents, so these two columns are the
    /// whole basis for what it is allowed to emit (see `crate::request_binding`).
    ///
    /// One query for a set of ids rather than one per candidate — binding runs
    /// after a decision, so it is one id today, but the shape costs nothing and
    /// removes the N+1 before it exists.
    pub async fn endpoint_schemas(
        &self,
        endpoint_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, EndpointSchemas>> {
        if endpoint_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = sqlx::query_as::<_, EndpointSchemas>(
            r#"SELECT id, query_params, request_body
               FROM api_endpoints
               WHERE id = ANY($1) AND COALESCE(active, true)"#,
        )
        .bind(endpoint_ids)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|r| (r.id, r)).collect())
    }

    /// Fetch the documentation columns for a single resolved endpoint joined
    /// with its parent API's description. Uses the same path-prefix match as
    /// `get_endpoint` so the docs row always matches the endpoint that the
    /// payment flow just resolved. Read-only, no side effects.
    pub async fn get_endpoint_docs_bundle(
        &self,
        api_id: Uuid,
        method: &str,
        path: &str,
    ) -> Result<Option<EndpointDocsBundle>> {
        let Some(endpoint) = self.get_endpoint(api_id, method, path).await? else {
            return Ok(None);
        };
        let bundle = sqlx::query_as::<_, EndpointDocsBundle>(
            r#"SELECT e.description, e.request_body, e.response_body,
                      e.response_example, e.query_params,
                      COALESCE(a.description, '') AS api_description,
                      COALESCE(a.name, a.slug) AS api_name,
                      COALESCE(a.category, '') AS api_category
               FROM api_endpoints e JOIN apis a ON a.id = e.api_id
               WHERE e.id = $1 AND COALESCE(e.active, true)"#,
        )
        .bind(endpoint.id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(bundle)
    }

    pub async fn get_endpoint_methods_for_path(
        &self,
        api_id: Uuid,
        path: &str,
    ) -> Result<Vec<String>> {
        let rows = sqlx::query_as::<_, (String, String)>(
            r#"SELECT method, path FROM api_endpoints
               WHERE api_id = $1 AND COALESCE(active, true)
               ORDER BY method, path"#,
        )
        .bind(api_id)
        .fetch_all(&self.pool)
        .await?;
        let mut methods: Vec<String> = rows
            .into_iter()
            .filter(|(_, candidate)| endpoint_match_score(candidate, path).is_some())
            .map(|(method, _)| method)
            .collect();
        methods.dedup();
        Ok(methods)
    }

    pub async fn list_endpoints_for_api(&self, api_id: Uuid) -> Result<Vec<Endpoint>> {
        let endpoints = sqlx::query_as::<_, Endpoint>(
            r#"SELECT id, api_id, method, path, payment_required, price, network_token_id,
                      origin_override, auth_override, cache_ttl_seconds
               FROM api_endpoints
               WHERE api_id = $1 AND COALESCE(active, true)
               ORDER BY path ASC, method ASC"#,
        )
        .bind(api_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(endpoints)
    }

    pub async fn get_api_detail(&self, slug: &str) -> Result<Option<ApiDetail>> {
        let api = sqlx::query_as::<_, ApiDetail>(
            r#"SELECT
                id, slug,
                COALESCE(name, slug) as name,
                COALESCE(description, '') as description,
                COALESCE(endpoint_url, origin_url) as origin_url,
                docs_url,
                category,
                listing_type::text as listing_type,
                COALESCE(listing_metadata, '{}'::jsonb) as listing_metadata,
                long_description,
                example_usage,
                authentication_info,
                active,
                is_verified,
                COALESCE(is_external, FALSE) as is_external,
                COALESCE(external_x402, '[]'::jsonb) as external_x402,
                created_at
               FROM apis
               -- No connected-payout-wallet gate: payouts run through the
               -- treasury, so a payout_wallets row is not a precondition for a
               -- listing being resolvable. See CATALOG_VISIBILITY in
               -- db/discovery.rs — the two must agree.
               WHERE slug = $1 AND status = 'active'"#,
        )
        .bind(slug)
        .fetch_optional(&self.pool)
        .await?;

        Ok(api)
    }

    pub async fn list_endpoints_with_docs(&self, api_id: Uuid) -> Result<Vec<EndpointWithDocs>> {
        let endpoints = sqlx::query_as::<_, EndpointWithDocs>(
            r#"SELECT
                id, api_id, method, path, payment_required, price, network_token_id,
                description, request_body, response_body,
                request_example, response_example, headers, query_params,
                documentation_markdown, external_docs_url, openapi_url,
                authentication_notes,
                COALESCE((SELECT jsonb_agg(jsonb_build_object(
                  'parameter_name', r.parameter_name,
                  'operator', r.operator,
                  'value', r.comparison_value,
                  'price', r.price,
                  'priority', r.priority
                ) ORDER BY r.priority, r.created_at)
                FROM endpoint_pricing_rules r
                WHERE r.endpoint_id = api_endpoints.id AND r.active = true), '[]'::jsonb) AS pricing_rules
               FROM api_endpoints
               WHERE api_id = $1 AND COALESCE(active, true)
               ORDER BY path ASC, method ASC"#,
        )
        .bind(api_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(endpoints)
    }

    pub async fn update_endpoint_docs(
        &self,
        endpoint_id: Uuid,
        description: Option<&str>,
        request_body: Option<&serde_json::Value>,
        response_body: Option<&serde_json::Value>,
        request_example: Option<&serde_json::Value>,
        response_example: Option<&serde_json::Value>,
    ) -> Result<()> {
        sqlx::query(
            r#"UPDATE api_endpoints SET
                description = COALESCE($2, description),
                request_body = COALESCE($3, request_body),
                response_body = COALESCE($4, response_body),
                request_example = COALESCE($5, request_example),
                response_example = COALESCE($6, response_example)
               WHERE id = $1"#,
        )
        .bind(endpoint_id)
        .bind(description)
        .bind(request_body)
        .bind(response_body)
        .bind(request_example)
        .bind(response_example)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn get_network_token(&self, id: Uuid) -> Result<Option<NetworkToken>> {
        let nt = sqlx::query_as::<_, NetworkToken>(
            r#"SELECT id, network_name, network_display_name, chain_id, explorer_url,
                      is_testnet, token_symbol, token_name, token_decimals, contract_address
               FROM network_tokens_view WHERE id = $1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(nt)
    }

    /// Per-network payout destinations configured for an API (max 4 rows —
    /// one per allowed network, enforced by the table's UNIQUE + CHECK).
    /// Returns (network_name, wallet_address) pairs.
    pub async fn get_api_payout_wallets(&self, api_id: Uuid) -> Result<Vec<(String, String)>> {
        let rows = sqlx::query_as::<_, (String, String)>(
            r#"SELECT network, wallet_address
               FROM api_payout_wallets
               WHERE api_id = $1"#,
        )
        .bind(api_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }

    /// All active mainnet USDC network tokens on the x402 v1 allowlist —
    /// used by the discovery document to resolve each endpoint's advertised
    /// network without a per-endpoint query.
    pub async fn list_x402_usdc_network_tokens(&self) -> Result<Vec<NetworkToken>> {
        let tokens = sqlx::query_as::<_, NetworkToken>(
            r#"SELECT id, network_name, network_display_name, chain_id, explorer_url,
                      is_testnet, token_symbol, token_name, token_decimals, contract_address
               FROM network_tokens_view
               WHERE token_symbol = 'USDC'
                 AND is_testnet = false
                 AND network_name IN ('base', 'polygon', 'arbitrum')"#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(tokens)
    }

    pub async fn get_default_network_token(&self) -> Result<Option<NetworkToken>> {
        let nt = sqlx::query_as::<_, NetworkToken>(
            r#"SELECT id, network_name, network_display_name, chain_id, explorer_url,
                      is_testnet, token_symbol, token_name, token_decimals, contract_address
               FROM network_tokens_view
               WHERE network_name = 'base'
                 AND is_testnet = false
                 AND token_symbol = 'USDC'"#,
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(nt)
    }
}

/// More-specific routes win. A template consumes exactly one nonempty path
/// segment per parameter and must match the complete path. Literal prefix
/// routes retain their existing subtree behavior, without SQL LIKE wildcards.
fn endpoint_match_score(pattern: &str, path: &str) -> Option<(usize, usize, bool)> {
    let expected: Vec<_> = pattern.split('/').collect();
    let actual: Vec<_> = path.split('/').collect();
    let parameter = |s: &str| {
        s.starts_with('{')
            && s.ends_with('}')
            && s.len() > 2
            && !s[1..s.len() - 1].contains(['{', '}'])
    };
    let template = expected.iter().any(|s| parameter(s));
    if actual.len() < expected.len() || (template && actual.len() != expected.len()) {
        return None;
    }
    if !expected.iter().zip(&actual).all(|(want, got)| {
        if parameter(want) {
            !got.is_empty() && *got != "." && *got != ".."
        } else {
            want == got
        }
    }) {
        return None;
    }
    Some((
        expected.len(),
        expected.iter().filter(|s| !parameter(s)).count(),
        pattern == path,
    ))
}

#[cfg(test)]
mod route_tests {
    use super::endpoint_match_score as score;
    #[tokio::test]
    #[ignore = "requires a disposable local Postgres; scripts/test-sirenic-migration.py runs it"]
    async fn database_resolves_active_templates_and_ignores_retired_literal_routes() {
        let url = std::env::var("SIRENIC_ROUTE_TEST_DATABASE_URL").unwrap();
        let parsed = url::Url::parse(&url).unwrap();
        assert!(matches!(parsed.host_str(), Some("127.0.0.1" | "localhost")));
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        let db = super::Database {
            pool: pool.clone(),
            log_pool: pool.clone(),
            request_log_queue: crate::db::request_log::RequestLogQueue::new(),
        };
        let api = uuid::Uuid::parse_str("804c77ce-bac1-4880-9e83-04f8b701b839").unwrap();
        let profile = db
            .get_endpoint(api, "GET", "/v1/entreprise/552032534")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(profile.path, "/v1/entreprise/{siren}");
        assert_eq!(
            profile.price,
            "0.005".parse::<bigdecimal::BigDecimal>().unwrap()
        );
        let financials = db
            .get_endpoint(api, "GET", "/v1/entreprise/552032534/finances")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(financials.path, "/v1/entreprise/{siren}/finances");
        assert_ne!(profile.id, financials.id);
        assert!(db
            .get_endpoint(api, "POST", "/v1/entreprise/552032534")
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            db.get_endpoint_methods_for_path(api, "/v1/entreprise/552032534")
                .await
                .unwrap(),
            vec!["GET"]
        );
        assert_eq!(db.list_endpoints_for_api(api).await.unwrap().len(), 6);
        assert!(db
            .get_endpoint_docs_bundle(api, "GET", "/v1/entreprise/552032534/finances")
            .await
            .unwrap()
            .is_some());
        assert!(db
            .get_endpoint(api, "GET", "/v1/entreprise/552032534/unreviewed")
            .await
            .unwrap()
            .is_none());
        pool.close().await;
    }
    #[test]
    fn sirenic_templates_match_real_ids_without_crossing_operation_boundaries() {
        assert!(score("/v1/entreprise/{siren}", "/v1/entreprise/552032534").is_some());
        assert!(score(
            "/v1/entreprise/{siren}/finances",
            "/v1/entreprise/552032534/finances"
        )
        .is_some());
        assert!(score(
            "/v1/eu/entreprise/{pays}/{id}",
            "/v1/eu/entreprise/NO/923609016"
        )
        .is_some());
        for path in [
            "/v1/entreprise/",
            "/v1/entreprise/..",
            "/v1/entreprise/552032534/finances",
        ] {
            assert!(score("/v1/entreprise/{siren}", path).is_none());
        }
    }
    #[test]
    fn specific_routes_win_and_literal_prefixes_remain_literal() {
        let path = "/v1/entreprise/552032534";
        assert!(score(path, path) > score("/v1/entreprise/{siren}", path));
        assert!(score("/v1/entreprise/{siren}", path) > score("/v1/entreprise", path));
        assert!(score("/v1", path).is_some());
        assert!(score("/v1/thing_name", "/v1/thingXname").is_none());
        assert!(score("/v1/thing%", "/v1/thing123").is_none());
        assert!(score("/v1/thing", "/v1/things").is_none());
    }
}
