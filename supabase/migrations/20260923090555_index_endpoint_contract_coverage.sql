-- One auditable row for every published endpoint, including Pulse services.
-- Contract state is metadata. Only Gateway's accepted runtime catalog may quote.
CREATE OR REPLACE VIEW catalog.endpoint_contract_coverage
WITH (security_invoker = true) AS
SELECT
  CASE WHEN p.username = 'pulsenetwork' THEN 'pulsenetwork' ELSE a.slug END AS source_slug,
  a.slug AS service_slug,
  a.name AS service_name,
  e.id AS endpoint_id,
  e.method,
  e.path,
  e.endpoint_category AS category,
  e.tags,
  e.use_cases,
  e.action_type,
  e.contract_documented,
  e.response_example IS NOT NULL AS has_response_example,
  a.listing_metadata->>'planner_block_reason' AS planner_block_reason,
  coalesce(contract_index.active_contracts, 0) AS active_contracts,
  coalesce(contract_index.staged_contracts, 0) AS staged_contracts,
  coalesce(offer_index.capability_slugs, ARRAY[]::text[]) AS offered_capabilities,
  coalesce(contract_index.capability_slugs, ARRAY[]::text[]) AS contracted_capabilities,
  CASE
    WHEN nullif(a.listing_metadata->>'planner_block_reason', '') IS NOT NULL THEN 'blocked'
    WHEN coalesce(contract_index.active_contracts, 0) > 0 THEN 'active_contract'
    WHEN coalesce(contract_index.staged_contracts, 0) > 0 THEN 'staged_contract'
    ELSE 'uncontracted'
  END AS contract_state
FROM providers.apis a
LEFT JOIN providers.provider_profiles p ON p.id = a.owner_id
JOIN providers.api_endpoints e ON e.api_id = a.id AND e.active = true
LEFT JOIN LATERAL (
  SELECT count(*) FILTER (WHERE c.status = 'active' AND c.validated_at IS NOT NULL
                            AND c.effect = 'read' AND c.price_model = 'per_call') AS active_contracts,
         count(*) FILTER (WHERE c.status = 'validated') AS staged_contracts,
         array_agg(DISTINCT c.capability_slug ORDER BY c.capability_slug) AS capability_slugs
  FROM catalog.capability_execution_contracts c WHERE c.endpoint_id = e.id
) contract_index ON true
LEFT JOIN LATERAL (
  SELECT array_agg(DISTINCT o.capability_slug ORDER BY o.capability_slug) AS capability_slugs
  FROM catalog.capability_offers o WHERE o.endpoint_id = e.id
) offer_index ON true
WHERE a.status = 'active' AND coalesce(a.active, true) AND a.is_verified = true
  AND coalesce(a.discoverable, true) AND a.suspended_at IS NULL;

COMMENT ON VIEW catalog.endpoint_contract_coverage IS
  'Published endpoint classification and capability coverage. active_contract is not proof of Gateway runtime admission or a payable quote.';
REVOKE ALL ON catalog.endpoint_contract_coverage FROM PUBLIC, anon, authenticated;
GRANT SELECT ON catalog.endpoint_contract_coverage TO service_role;
