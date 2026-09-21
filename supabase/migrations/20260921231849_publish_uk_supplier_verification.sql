-- Apply only after deploying the reviewed UK VAT adapter and bounded workflow.
BEGIN;
SELECT pg_advisory_xact_lock(hashtext('uk-supplier-verification-v1'));
DO $publish$
DECLARE count_contracts integer;
BEGIN
 SELECT count(*) INTO count_contracts
 FROM catalog.capability_execution_contracts c
 JOIN providers.api_endpoints e ON e.id=c.endpoint_id
 JOIN providers.apis a ON a.id=e.api_id
 WHERE c.notes='uk-supplier-v1' AND c.adapter_version='uk-supplier-v1'
   AND c.status='validated' AND c.validated_at IS NOT NULL
   AND c.version=1 AND c.effect='read' AND c.price_model='per_call'
   AND ((a.slug='opensanctions' AND e.path='/match' AND ceil(e.price*1000000)=94518
         AND c.capability_slug IN ('company.uk-director-screening-1','company.uk-director-screening-2','company.uk-director-screening-3','company.uk-director-screening-4','company.uk-director-screening-5'))
     OR (a.slug='sirenic' AND e.path='/v1/eu/entreprise/GB/{company_number}/dirigeants' AND ceil(e.price*1000000)=10000 AND c.capability_slug='company.uk-director-roster')
     OR (a.slug='tavily' AND e.path='/search' AND ceil(e.price*1000000)=10000 AND c.capability_slug='company.uk-vat-candidate-search')
     OR (a.slug='ask-merlin' AND e.id='056c56b0-777e-4692-8bd4-ea3ec5f54560' AND e.path='/api/lookup' AND ceil(e.price*1000000)=85066 AND c.capability_slug='vat.uk.validate'));
 IF count_contracts<>8 THEN RAISE EXCEPTION 'UK supplier publish requires exactly eight reviewed contracts'; END IF;
 UPDATE providers.apis SET active=true,is_verified=true,discoverable=true,status='active',
   listing_metadata=listing_metadata || '{"catalog_state":"active","verification":"free live valid/invalid UK VAT probes; contract and mocked approval/PDF tests; no paid customer E2E assertion"}'::jsonb
 WHERE slug='ask-merlin' AND origin_url='https://askmerlin.com' AND is_platform_owned AND settlement_mode='apiosk_revenue';
 IF NOT FOUND THEN RAISE EXCEPTION 'Missing reviewed UK VAT source'; END IF;
 UPDATE providers.api_endpoints SET active=true WHERE id='056c56b0-777e-4692-8bd4-ea3ec5f54560' AND api_id=(SELECT id FROM providers.apis WHERE slug='ask-merlin');
 UPDATE catalog.capability_execution_contracts SET status='active',validated_note='Reviewed contract and local one-approval backend-worker/PDF tests passed. VAT egress live positive and invalid source checks passed. Live paid customer execution still requires approval.' WHERE notes='uk-supplier-v1' AND adapter_version='uk-supplier-v1' AND status='validated';
END $publish$;
COMMIT;
