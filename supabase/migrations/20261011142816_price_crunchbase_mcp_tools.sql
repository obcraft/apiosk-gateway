-- User-approved 2026-10-11: each official Crunchbase MCP tool displays EUR 0.05
-- including the standard 15% buyer fee at configured EUR/USD=0.92.
-- 47,259 + ceil(47,259 * 1500 / 10000) = 54,348 atomic USDC.
-- Settlement is USDC, not a permanent EUR exchange-rate lock.
-- Preserve endpoint/contract activation and the existing API.market tariffs.
BEGIN;
SELECT pg_advisory_xact_lock(hashtext('apiosk-crunchbase'));

DO $guard$
BEGIN
 IF (SELECT count(*) FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id
     WHERE a.slug='apimarket-magicapi-crunchbase-bbcca0a6'
       AND e.method='POST' AND e.origin_override='https://mcp.crunchbase.com'
       AND e.path IN ('/mcp/cb_entity_autocomplete','/mcp/cb_search_query','/mcp/cb_entity_get','/mcp/cb_reference')) <> 4
 THEN RAISE EXCEPTION 'Expected all four official Crunchbase MCP tools before setting their tariff';
 END IF;
END $guard$;

UPDATE providers.api_endpoints e SET price=0.047259, payment_required=true
FROM providers.apis a
WHERE a.id=e.api_id AND a.slug='apimarket-magicapi-crunchbase-bbcca0a6'
  AND e.method='POST' AND e.origin_override='https://mcp.crunchbase.com'
  AND e.path IN ('/mcp/cb_entity_autocomplete','/mcp/cb_search_query','/mcp/cb_entity_get','/mcp/cb_reference');

UPDATE providers.apis SET listing_metadata=jsonb_set(
  listing_metadata,'{official_mcp,pricing}',
  '{"listAtomic":47259,"buyerAtomic":54348,"buyerMarkupBps":1500,"displayCurrency":"EUR","displayAmount":0.05,"displayRate":0.92,"rateAsOf":"2026-08-21","approvedOn":"2026-10-11","unit":"tools/call","internalEnrichmentIncluded":true}'::jsonb
)
WHERE slug='apimarket-magicapi-crunchbase-bbcca0a6';

COMMIT;
