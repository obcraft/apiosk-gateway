-- Buyer price for a standard Crunchbase call: exactly 0.10 USDC.
-- List 86,956 atomic + ceil(15% markup) 13,044 atomic = 100,000 atomic.
-- Workflow bundles retain their separately quoted workflow markup.
BEGIN;
SELECT pg_advisory_xact_lock(hashtext('apiosk-crunchbase'));

DO $guard$
BEGIN
  IF (SELECT count(*) FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id
      WHERE a.slug='apimarket-magicapi-crunchbase-bbcca0a6' AND e.active
        AND e.method='POST' AND e.path IN ('/autocompletes','/searches/organizations')) <> 2 THEN
    RAISE EXCEPTION 'Expected both verified Crunchbase operations';
  END IF;
END $guard$;

UPDATE providers.api_endpoints e
SET price=0.086956, payment_required=true
FROM providers.apis a
WHERE a.id=e.api_id AND a.slug='apimarket-magicapi-crunchbase-bbcca0a6'
  AND e.active AND e.method='POST'
  AND e.path IN ('/autocompletes','/searches/organizations');

UPDATE providers.apis
SET listing_metadata=listing_metadata || '{"buyer_pricing":{"currency":"USDC","standard_call_atomic":100000,"standard_call_usdc":"0.10","includes_standard_platform_fee":true}}'::jsonb
WHERE slug='apimarket-magicapi-crunchbase-bbcca0a6';

DO $verify$
BEGIN
  IF EXISTS(SELECT 1 FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id
      WHERE a.slug='apimarket-magicapi-crunchbase-bbcca0a6' AND e.active
        AND (NOT e.payment_required OR ceil(e.price*1000000)+ceil(ceil(e.price*1000000)*1500/10000) <> 100000)) THEN
    RAISE EXCEPTION 'Crunchbase buyer price must be exactly 100000 atomic USDC';
  END IF;
END $verify$;
COMMIT;
