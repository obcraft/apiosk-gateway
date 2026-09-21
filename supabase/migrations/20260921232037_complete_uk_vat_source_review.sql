-- Complete the existing per-source catalog review gate after runtime/egress checks.
-- This is publication of the new platform source, never a buyer payment approval.
BEGIN;
SELECT pg_advisory_xact_lock(hashtext('uk-supplier-verification-v1'));
DO $review$
DECLARE source_id uuid;
BEGIN
 SELECT a.id INTO STRICT source_id FROM providers.apis a
 WHERE a.slug='ask-merlin' AND a.origin_url='https://askmerlin.com'
   AND a.is_platform_owned AND a.settlement_mode='apiosk_revenue'
   AND a.listing_metadata->>'adapter'='uk-vat-v1'
   AND EXISTS(SELECT 1 FROM providers.api_endpoints e JOIN catalog.capability_execution_contracts c ON c.endpoint_id=e.id
     WHERE e.api_id=a.id AND e.id='056c56b0-777e-4692-8bd4-ea3ec5f54560' AND e.active AND c.status='active' AND c.capability_slug='vat.uk.validate');
 PERFORM set_config('apiosk.api_review_approved_id',source_id::text,true);
 UPDATE providers.apis SET status='active',active=true,is_verified=true,discoverable=true,
   verified_at=coalesce(verified_at,now()),approved_at=coalesce(approved_at,now())
 WHERE id=source_id;
 IF NOT EXISTS(SELECT 1 FROM providers.apis WHERE id=source_id AND status='active' AND active AND is_verified AND discoverable AND suspended_at IS NULL)
 THEN RAISE EXCEPTION 'Reviewed UK VAT source did not become available'; END IF;
END $review$;
COMMIT;
