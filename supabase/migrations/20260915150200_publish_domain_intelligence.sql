-- Publish domain intelligence only after the gateway with the domain adapter is
-- deployed and both production-egress probes (RDAP registration, DNS MX) pass.
-- Mirrors the reviewed EU public source publication.
BEGIN;
SELECT pg_advisory_xact_lock(hashtext('apiosk-domain-intelligence-v1'));
DO $publish$
DECLARE source_id uuid;
BEGIN
 SELECT id INTO STRICT source_id FROM providers.apis WHERE slug='domain-intelligence'
  AND owner_id=(SELECT owner_id FROM providers.apis WHERE slug='koop')
  AND is_platform_owned AND settlement_mode='apiosk_revenue' AND NOT requires_payout_wallet
  AND listing_metadata->>'capability_adapter'='domain-intelligence-v1' AND suspended_at IS NULL FOR UPDATE;
 IF (SELECT count(*) FROM catalog.capability_execution_contracts c JOIN providers.api_endpoints e ON e.id=c.endpoint_id
  WHERE e.api_id=source_id AND e.price=0.085066 AND c.adapter_version='domain-intelligence-v1'
  AND c.status IN ('validated','active') AND c.validated_at IS NOT NULL AND c.sample_payload IS NOT NULL)<>2
 THEN RAISE EXCEPTION 'domain-intelligence publication invariant failed'; END IF;
 UPDATE providers.api_endpoints SET active=true WHERE api_id=source_id AND path IN ('/domain/{domain}','/dns/{domain}');
 UPDATE catalog.capability_execution_contracts SET status='active',validated_note='Real registry/DNS fixtures, Rust adapter tests and production-egress probes'
  WHERE adapter_version='domain-intelligence-v1' AND status='validated';
 PERFORM set_config('apiosk.api_review_approved_id',source_id::text,true);
 UPDATE providers.apis SET status='active',active=true,is_verified=true,discoverable=true,
  verified_at=coalesce(verified_at,now()),approved_at=coalesce(approved_at,now()),
  listing_metadata=coalesce(listing_metadata,'{}'::jsonb)||'{"catalog_state":"published","production_probe":{"probed_on":"2026-09-15","region":"ams","expected_capabilities":2}}'::jsonb
  WHERE id=source_id;
 PERFORM set_config('apiosk.api_review_approved_id','',true);
 IF NOT EXISTS(SELECT 1 FROM providers.apis WHERE id=source_id AND status='active' AND active AND is_verified AND discoverable)
 THEN RAISE EXCEPTION 'domain-intelligence did not become publicly active'; END IF;
END $publish$;
COMMIT;
