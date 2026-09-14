-- Finish the existing, API-specific backend publication workflow after live
-- production validation. Mirrors publish_koop_rdw_rvo and Reuters/Bloomberg.
BEGIN;
SELECT pg_advisory_xact_lock(hashtext('apiosk-eu-public-sources-v1'));
DO $publish$
DECLARE source_id uuid; source_slug text; expected integer;
BEGIN
 FOR source_slug,expected IN SELECT * FROM (VALUES
  ('vies',3),('koop',3),('eurlex-cellar',2),('eu-funding-tenders',1),('pdok-ruimtelijke-plannen',1),('bodemloket',1)
 ) s(slug,n) LOOP
  SELECT id INTO STRICT source_id FROM providers.apis WHERE slug=source_slug
   AND owner_id=(SELECT owner_id FROM providers.apis WHERE slug='koop')
   AND is_platform_owned AND settlement_mode='apiosk_revenue' AND NOT requires_payout_wallet
   AND listing_metadata->>'capability_adapter'='eu-public-sources-v1'
   AND listing_metadata#>>'{production_probe,probed_on}'='2026-09-14'
   AND suspended_at IS NULL FOR UPDATE;
  IF (SELECT count(*) FROM catalog.capability_execution_contracts c JOIN providers.api_endpoints e ON e.id=c.endpoint_id
   WHERE e.api_id=source_id AND e.active AND e.price=0.085066 AND c.adapter_version='eu-public-sources-v1'
   AND c.status='active' AND c.validated_at IS NOT NULL AND c.sample_payload IS NOT NULL)<>expected
  THEN RAISE EXCEPTION '% source publication invariant failed',source_slug; END IF;
  PERFORM set_config('apiosk.api_review_approved_id',source_id::text,true);
  UPDATE providers.apis SET status='active',active=true,is_verified=true,discoverable=true,
   verified_at=coalesce(verified_at,now()),approved_at=coalesce(approved_at,now()) WHERE id=source_id;
  PERFORM set_config('apiosk.api_review_approved_id','',true);
  IF NOT EXISTS(SELECT 1 FROM providers.apis WHERE id=source_id AND status='active' AND active AND is_verified AND discoverable)
  THEN RAISE EXCEPTION '% source did not become publicly active',source_slug; END IF;
 END LOOP;
END $publish$;
COMMIT;
