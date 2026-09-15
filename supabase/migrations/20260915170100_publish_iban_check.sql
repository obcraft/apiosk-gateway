-- Publish the IBAN check after the executor with iban_check.rs is deployed and a live check passes.

BEGIN;

SELECT pg_advisory_xact_lock(hashtext('apiosk-iban-check-v1'));

DO $publish$ DECLARE source_id uuid; BEGIN

SELECT id INTO STRICT source_id FROM providers.apis WHERE slug='iban-check' AND is_platform_owned AND settlement_mode='apiosk_revenue' AND listing_metadata->>'capability_adapter'='iban-check-v1' AND suspended_at IS NULL FOR UPDATE;

IF (SELECT count(*) FROM catalog.capability_execution_contracts c JOIN providers.api_endpoints e ON e.id=c.endpoint_id WHERE e.api_id=source_id AND e.price=0.054348 AND c.adapter_version='iban-check-v1' AND c.status IN ('validated','active') AND c.sample_payload IS NOT NULL)<>1 THEN RAISE EXCEPTION 'iban-check publication invariant failed'; END IF;

UPDATE providers.api_endpoints SET active=true WHERE api_id=source_id AND path='/iban/{iban}';

UPDATE catalog.capability_execution_contracts SET status='active',validated_note='Executor-computed; Rust adapter tests and production check' WHERE adapter_version='iban-check-v1' AND status='validated';

PERFORM set_config('apiosk.api_review_approved_id',source_id::text,true);

UPDATE providers.apis SET status='active',active=true,is_verified=true,discoverable=true,verified_at=coalesce(verified_at,now()),approved_at=coalesce(approved_at,now()),listing_metadata=coalesce(listing_metadata,'{}'::jsonb)||'{"catalog_state":"published"}'::jsonb WHERE id=source_id;

PERFORM set_config('apiosk.api_review_approved_id','',true);

IF NOT EXISTS(SELECT 1 FROM providers.apis WHERE id=source_id AND status='active' AND active AND is_verified AND discoverable) THEN RAISE EXCEPTION 'iban-check did not become publicly active'; END IF;

END $publish$;

COMMIT;

