-- Publish only after the deployed adapter has completed the four exact public
-- probes below. No key, upstream snapshot identifier or raw response is copied
-- into the public catalog. Apply on the probe date, after checking deployment.
BEGIN;
SELECT pg_advisory_xact_lock(hashtext('apiosk-brightdata-linkedin-v1'));
DO $publish$
DECLARE
  source_id uuid;
  probe jsonb;
  job gateway_private.brightdata_linkedin_jobs%ROWTYPE;
  probe_date constant date := DATE '2026-10-08';
  probes constant jsonb := $probes$[
  {
    "kind": "profile",
    "url": "https://www.linkedin.com/in/satyanadella",
    "request_key": "74976ce2a766fe79c62634aff78b479d5ff0e3fc000817dd484344593d39e42f"
  },
  {
    "kind": "company",
    "url": "https://www.linkedin.com/company/microsoft",
    "request_key": "a6e6a00a84166d5fa10dd8df9ed91f4e5670a8e7fd5c4d1c921efbda24559297"
  },
  {
    "kind": "job",
    "url": "https://www.linkedin.com/jobs/view/4476198009",
    "request_key": "6844a832b3d1d24d963fb03e884f549084a3b03b01e0fb1a504b05790d40becd"
  },
  {
    "kind": "post",
    "url": "https://www.linkedin.com/posts/microsoft_september-2026-lessons-from-our-ai-transformation-activity-7508888854716628992-JRUU",
    "request_key": "0bc6cf5f5a4fd964c367c0a3a74052402198c6494d732b74701cfb07a74e369b"
  }
]$probes$::jsonb;
BEGIN
  IF (now() AT TIME ZONE 'Europe/Lisbon')::date <> probe_date THEN
    RAISE EXCEPTION 'Bright Data activation requires live probes from today';
  END IF;
  SELECT id INTO STRICT source_id FROM providers.apis
    WHERE slug='brightdata'
      AND owner_id=(SELECT owner_id FROM providers.apis WHERE slug='koop')
      AND is_platform_owned AND NOT is_external AND NOT requires_payout_wallet
      AND integration_mode='gateway' AND settlement_mode='apiosk_revenue'
      AND listing_type='api' AND origin_url='https://api.brightdata.com'
      AND endpoint_url='https://api.brightdata.com' AND suspended_at IS NULL
      AND status IN ('inactive','active')
      AND listing_metadata->>'adapter'='brightdata-linkedin-v1'
      AND listing_metadata->>'capability_adapter'='brightdata-linkedin-v1'
      AND listing_metadata->>'catalog_state' IN ('staged','published')
      AND listing_metadata->>'planner_block_reason' IS NULL
      AND listing_metadata #>> '{credential,location}'='server_environment'
      AND listing_metadata #>> '{credential,env}'='BRIGHTDATA_API_KEY'
      AND NOT (listing_metadata ?| ARRAY['auth_headers','auth_query'])
    FOR UPDATE;
  PERFORM 1 FROM providers.api_endpoints WHERE api_id=source_id FOR UPDATE;
  IF (SELECT count(*) FROM providers.api_endpoints WHERE api_id=source_id)<>4
     OR (SELECT count(*) FROM providers.api_endpoints WHERE api_id=source_id
       AND method='POST' AND path IN ('/linkedin/profile','/linkedin/company','/linkedin/job','/linkedin/post')
       AND price=0.0015 AND payment_required AND contract_documented
       AND endpoint_category='data' AND action_type='lookup'
       AND request_body->'required'='["url"]'::jsonb
       AND request_body->'additionalProperties'='false'::jsonb
       AND request_body->'properties' ? 'url'
       AND (SELECT count(*) FROM jsonb_object_keys(request_body->'properties'))=1
       AND request_body #>> '{properties,url,type}'='string'
       AND request_body #>> '{properties,url,maxLength}'='2048')<>4 THEN
    RAISE EXCEPTION 'Bright Data source, endpoint tariff or request contract drift';
  END IF;

  FOR probe IN SELECT value FROM jsonb_array_elements(probes) LOOP
    SELECT * INTO job FROM gateway_private.brightdata_linkedin_jobs
      WHERE request_key=probe->>'request_key'
        AND kind=probe->>'kind' AND source_url=probe->>'url'
      FOR SHARE;
    IF NOT FOUND THEN
      RAISE EXCEPTION 'Bright Data live probe is missing: %',probe->>'kind';
    END IF;
    IF job.state<>'ready' OR job.error_code IS NOT NULL
       OR coalesce(job.snapshot_id,'') !~ '^(s|sd)_[A-Za-z0-9_]+$' OR length(job.snapshot_id)>128
       OR (job.created_at AT TIME ZONE 'Europe/Lisbon')::date<>probe_date
       OR (job.updated_at AT TIME ZONE 'Europe/Lisbon')::date<>probe_date
       OR job.updated_at<job.created_at OR job.expires_at<=now()
       OR (CASE WHEN jsonb_typeof(job.result)='array' THEN jsonb_array_length(job.result) ELSE 0 END)<>1
       OR jsonb_typeof(job.result->0) IS DISTINCT FROM 'object' OR job.result->0='{}'::jsonb THEN
      RAISE EXCEPTION 'Bright Data live probe is not a fresh successful single record: %',probe->>'kind';
    END IF;
    IF EXISTS (SELECT 1 FROM jsonb_each(job.result->0) record
      WHERE record.key IN ('error','error_code','error_message','errors')
        AND record.value NOT IN ('null'::jsonb,'false'::jsonb,'""'::jsonb,'[]'::jsonb,'{}'::jsonb)) THEN
      RAISE EXCEPTION 'Bright Data live probe contains a provider error: %',probe->>'kind';
    END IF;
  END LOOP;

  FOR probe IN SELECT value FROM jsonb_array_elements(probes) LOOP
    UPDATE providers.api_endpoints SET active=true,
      request_example=jsonb_build_object('url',probe->>'url'),
      request_body=jsonb_set(request_body,'{properties,url,example}',probe->'url'),
      documentation_markdown=replace(documentation_markdown,
        'This listing is staged: no live provider response or successful purchase is claimed. The request example is illustrative and may no longer be available. Activation requires the server-side credential, deployed adapter and a successful live check.',
        'The deployed adapter completed a live one-record provider probe for this operation on 2026-10-08. The public example URL was checked then and may later change. This probe is not a customer-purchase claim.')
      WHERE api_id=source_id AND method='POST' AND path='/linkedin/'||(probe->>'kind');
  END LOOP;

  -- Existing publish-review trigger authorizes only this source in this transaction.
  PERFORM set_config('apiosk.api_review_approved_id',source_id::text,true);
  UPDATE providers.apis SET status='active',active=true,is_verified=true,discoverable=true,
    verified_at=now(),approved_at=coalesce(approved_at,now()),
    long_description=replace(long_description,
      'This source is staged until its credential, adapter deployment and live requests are verified.',
      'The deployed adapter was verified against the four supported public page types on 2026-10-08.'),
    listing_metadata=jsonb_set(listing_metadata-'staging_reason'-'activation_requirements',
      '{credential,configured}','true'::jsonb)
      || jsonb_build_object('catalog_state','published','production_probe',
        jsonb_build_object('probed_on',probe_date,'adapter','brightdata-linkedin-v1','verified_kinds',4))
    WHERE id=source_id;
  PERFORM set_config('apiosk.api_review_approved_id','',true);

  IF NOT EXISTS (SELECT 1 FROM providers.apis WHERE id=source_id AND status='active'
       AND active AND is_verified AND discoverable AND verified_at IS NOT NULL AND approved_at IS NOT NULL
       AND listing_metadata->>'catalog_state'='published'
       AND listing_metadata #>> '{credential,configured}'='true'
       AND NOT (listing_metadata ? 'staging_reason'))
     OR (SELECT count(*) FROM providers.api_endpoints WHERE api_id=source_id AND active)<>4 THEN
    RAISE EXCEPTION 'Bright Data publication invariant failed';
  END IF;
END $publish$;
COMMIT;
