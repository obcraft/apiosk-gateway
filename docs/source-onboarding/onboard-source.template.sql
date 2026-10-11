-- Apiosk source bundle v1. Schema inspected in production on 2026-10-11.
-- TEMPLATE: render with render.mjs and a reviewed manifest. Not an auto-applied migration.
-- One transaction registers ONE NEW source, all endpoints, vocabulary and contracts.
-- The source remains inactive. See README.txt for the separate publication contract.
-- Never embed real credentials in this file, its manifest, examples or AI input.
-- Optional backend credential input on the SAME connection, BEFORE this script:
--   SELECT set_config('apiosk.onboarding_secrets', $1, false);
-- Bind $1 to a JSON object {"VAULT_NAME":"secret"}; do not interpolate SQL.
-- Use a dedicated connection and clear the setting/discard connection in finally.
-- Vault storage does NOT implement a generic gateway credential resolver.

BEGIN;
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '30s';
SET LOCAL standard_conforming_strings = on;

DO $onboard$
DECLARE
  bundle jsonb := $manifest$__SOURCE_MANIFEST_JSON__$manifest$::jsonb;
  src jsonb;
  item jsonb;
  requirement jsonb;
  source_id uuid;
  endpoint_id uuid;
  endpoint_ids jsonb := '{}'::jsonb;
  endpoint_row providers.api_endpoints%ROWTYPE;
  contract_row catalog.capability_execution_contracts%ROWTYPE;
  credential_values jsonb := coalesce(nullif(current_setting('apiosk.onboarding_secrets', true), ''), '{}')::jsonb;
  secret_name text;
  fact_slug text;
  reused jsonb;
BEGIN
  IF bundle->>'format' IS DISTINCT FROM 'apiosk.source-bundle.v1' THEN
    RAISE EXCEPTION 'Unsupported source bundle format';
  END IF;
  src := bundle->'source';
  IF jsonb_typeof(src) IS DISTINCT FROM 'object'
     OR coalesce(src->>'slug','') !~ '^[a-z0-9]+(-[a-z0-9]+)*$'
     OR nullif(btrim(src->>'name'),'') IS NULL
     OR nullif(btrim(src->>'description'),'') IS NULL
     OR coalesce(src->>'origin_url','') !~ '^https://[^/@?#]+(:[0-9]+)?(/[^?#]*)?$'
     OR coalesce(src->>'docs_url','') !~ '^https://'
     OR nullif(src->>'category','') IS NULL
     OR src->>'owner_id' IS NULL THEN
    RAISE EXCEPTION 'Source identity, existing owner UUID, HTTPS origin/docs and category are required';
  END IF;
  IF NOT EXISTS (SELECT 1 FROM auth.users WHERE id=(src->>'owner_id')::uuid) THEN
    RAISE EXCEPTION 'Source owner must be an existing auth.users ID';
  END IF;
  IF jsonb_typeof(bundle->'endpoints') IS DISTINCT FROM 'array'
     OR jsonb_array_length(bundle->'endpoints')=0 THEN
    RAISE EXCEPTION 'At least one endpoint is required';
  END IF;
  IF jsonb_typeof(credential_values) IS DISTINCT FROM 'object' THEN
    RAISE EXCEPTION 'Credential input must be a server-bound JSON object';
  END IF;
  IF jsonb_typeof(src->'listing_metadata') IS DISTINCT FROM 'object'
     OR jsonb_typeof(src->'authentication_info') IS DISTINCT FROM 'object'
     OR jsonb_typeof(src->'static_headers') IS DISTINCT FROM 'array' THEN
    RAISE EXCEPTION 'Source metadata/authentication must be objects and static_headers an array';
  END IF;
  -- Serializes this importer for the same slug; the unique constraint also
  -- protects against other writers. Existing sources require a separate update.
  PERFORM pg_advisory_xact_lock(hashtextextended('source-bundle:'||(src->>'slug'), 0));
  IF EXISTS (SELECT 1 FROM providers.apis WHERE slug=src->>'slug') THEN
    RAISE EXCEPTION 'Source slug already exists; inspect prior import instead of overwriting';
  END IF;
  IF coalesce(src->'listing_metadata','{}') ?| ARRAY['auth_headers','auth_query','planner_block_reason','onboarding'] THEN
    RAISE EXCEPTION 'Use static_headers/credential_requirements; reserved metadata cannot be supplied';
  END IF;
  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(src->'static_headers','[]')) LOOP
    -- These are public protocol settings. Credentials have a separate channel.
    IF lower(coalesce(item->>'name','')) NOT IN ('accept','content-type','accept-language')
       OR nullif(item->>'value','') IS NULL OR (item->>'value') ~ '[\r\n]' THEN
      RAISE EXCEPTION 'Only public Accept, Content-Type and Accept-Language static headers are supported by this template';
    END IF;
  END LOOP;

  INSERT INTO providers.apis (
    owner_id,slug,name,origin_url,endpoint_url,description,long_description,
    category,docs_url,website_url,github_url,example_usage,authentication_info,
    status,active,discoverable,is_verified,listing_type,is_platform_owned,
    is_external,is_community,source,integration_mode,settlement_mode,
    requires_payout_wallet,listing_metadata
  ) VALUES (
    (src->>'owner_id')::uuid,src->>'slug',src->>'name',src->>'origin_url',src->>'origin_url',
    src->>'description',src->>'long_description',src->>'category',src->>'docs_url',
    src->>'website_url',src->>'github_url',src->>'example_usage',src->'authentication_info',
    'inactive',false,false,false,'api',true,false,false,'manual','gateway','apiosk_revenue',false,
    coalesce(src->'listing_metadata','{}') || jsonb_build_object(
      'auth_headers',coalesce(src->'static_headers','[]'),
      'planner_block_reason','Onboarding: credentials, runtime contract and live acceptance await review',
      'onboarding',jsonb_build_object('format',bundle->>'format','bundle_id',bundle->>'bundle_id',
        'provenance',bundle->'provenance','credential_requirements',coalesce(bundle->'credential_requirements','[]'),
        'state','staged'))
  ) RETURNING id INTO source_id;
  -- Production's owner trigger auto-verifies on INSERT. Explicitly reset this
  -- metadata so staging does not imply that an upstream test was successful.
  UPDATE providers.apis SET is_verified=false,verified_at=NULL WHERE id=source_id;

  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'subject_kinds','[]')) LOOP
    INSERT INTO catalog.fact_subject_kinds(slug,description)
    VALUES(item->>'slug',item->>'description') ON CONFLICT(slug) DO NOTHING;
  END LOOP;

  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'fact_types','[]')) LOOP
    INSERT INTO catalog.fact_types(slug,name,description,subject_kind,jurisdiction,
      value_schema,freshness_seconds,match_hints,status)
    VALUES(item->>'slug',item->>'name',item->>'description',item->>'subject_kind',item->>'jurisdiction',
      item->'value_schema',(item->>'freshness_seconds')::integer,coalesce(item->'match_hints','{}'),'draft')
    ON CONFLICT(slug) DO NOTHING;
    -- Global vocabulary is shared by existing sources. Never redefine it.
    SELECT jsonb_build_object('subject_kind',f.subject_kind,'jurisdiction',f.jurisdiction,
      'value_schema',f.value_schema,'freshness_seconds',f.freshness_seconds,'match_hints',f.match_hints)
      INTO reused FROM catalog.fact_types f WHERE f.slug=item->>'slug';
    IF reused IS DISTINCT FROM jsonb_build_object('subject_kind',item->>'subject_kind',
      'jurisdiction',item->>'jurisdiction','value_schema',item->'value_schema',
      'freshness_seconds',(item->>'freshness_seconds')::integer,'match_hints',coalesce(item->'match_hints','{}')) THEN
      RAISE EXCEPTION 'Shared fact definition differs: %',item->>'slug';
    END IF;
  END LOOP;

  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'capabilities','[]')) LOOP
    INSERT INTO catalog.capabilities(slug,name,description,category,input_contract,output_contract,status)
    VALUES(item->>'slug',item->>'name',item->>'description',item->>'category',
      coalesce(item->'input_contract','{}'),coalesce(item->'output_contract','{}'),'draft')
    ON CONFLICT(slug) DO NOTHING;
    SELECT jsonb_build_object('name',c.name,'description',c.description,'category',c.category,
      'input_contract',c.input_contract,'output_contract',c.output_contract)
      INTO reused FROM catalog.capabilities c WHERE c.slug=item->>'slug';
    IF reused IS DISTINCT FROM jsonb_build_object('name',item->>'name','description',item->>'description',
      'category',item->>'category','input_contract',coalesce(item->'input_contract','{}'),
      'output_contract',coalesce(item->'output_contract','{}')) THEN
      RAISE EXCEPTION 'Shared capability definition differs: %',item->>'slug';
    END IF;
  END LOOP;

  FOR item IN SELECT value FROM jsonb_array_elements(bundle->'endpoints') LOOP
    IF nullif(item->>'key','') IS NULL OR endpoint_ids ? (item->>'key') THEN
      RAISE EXCEPTION 'Each endpoint needs a unique bundle-local key';
    END IF;
    endpoint_row := jsonb_populate_record(NULL::providers.api_endpoints,item-'key');
    IF endpoint_row.method NOT IN ('GET','POST') OR endpoint_row.method IS NULL
       OR coalesce(endpoint_row.path,'') !~ '^/[^?#]*$' OR endpoint_row.path LIKE '//%'
       OR endpoint_row.price IS NULL OR endpoint_row.price<0
       OR endpoint_row.price<>round(endpoint_row.price,6)
       OR nullif(btrim(endpoint_row.description),'') IS NULL
       OR endpoint_row.auth_override IS NOT NULL THEN
      RAISE EXCEPTION 'Invalid endpoint: require read GET/POST, relative path, micro-USD precision, description; secret auth_override is unsupported';
    END IF;
    IF endpoint_row.origin_override IS NOT NULL AND endpoint_row.origin_override !~ '^https://[^/@?#]+(:[0-9]+)?(/[^?#]*)?$' THEN
      RAISE EXCEPTION 'Origin override must be a reviewed HTTPS origin without credentials/query/fragment';
    END IF;
    IF jsonb_typeof(endpoint_row.query_params->'properties') IS DISTINCT FROM 'object'
       AND jsonb_typeof(endpoint_row.request_body->'properties') IS DISTINCT FROM 'object' THEN
      RAISE EXCEPTION 'Endpoint must document its inputs, including properties={} for no inputs';
    END IF;
    INSERT INTO providers.api_endpoints (
      api_id,method,path,payment_required,price,description,request_body,response_body,
      request_example,response_example,headers,query_params,active,documentation_markdown,
      external_docs_url,openapi_url,authentication_notes,origin_override,cache_ttl_seconds,
      tags,search_terms,use_cases,sectors,input_entities,output_entities,endpoint_category,
      action_type,task_summary,metadata_source,metadata_model,metadata_confidence,metadata_at,contract_documented
    ) VALUES (
      source_id,endpoint_row.method,endpoint_row.path,endpoint_row.price>0,endpoint_row.price,
      endpoint_row.description,endpoint_row.request_body,endpoint_row.response_body,
      endpoint_row.request_example,endpoint_row.response_example,endpoint_row.headers,
      endpoint_row.query_params,false,endpoint_row.documentation_markdown,
      endpoint_row.external_docs_url,endpoint_row.openapi_url,endpoint_row.authentication_notes,
      endpoint_row.origin_override,endpoint_row.cache_ttl_seconds,endpoint_row.tags,
      endpoint_row.search_terms,endpoint_row.use_cases,endpoint_row.sectors,
      endpoint_row.input_entities,endpoint_row.output_entities,endpoint_row.endpoint_category,
      endpoint_row.action_type,endpoint_row.task_summary,endpoint_row.metadata_source,
      endpoint_row.metadata_model,endpoint_row.metadata_confidence,endpoint_row.metadata_at,
      coalesce(endpoint_row.contract_documented,false)
    ) RETURNING id INTO endpoint_id;
    endpoint_ids := endpoint_ids || jsonb_build_object(item->>'key',endpoint_id);
  END LOOP;

  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'contracts','[]')) LOOP
    endpoint_id := (endpoint_ids->>(item->>'endpoint_key'))::uuid;
    IF endpoint_id IS NULL THEN RAISE EXCEPTION 'Unknown contract endpoint_key'; END IF;
    contract_row := jsonb_populate_record(NULL::catalog.capability_execution_contracts,item-'endpoint_key');
    IF coalesce(item->>'effect','read')<>'read' OR coalesce(item->>'price_model','per_call')<>'per_call'
       OR coalesce(item->'preconditions','{}')<>'{}'::jsonb THEN
      RAISE EXCEPTION 'Current Ask runtime requires read/per_call and empty preconditions';
    END IF;
    IF jsonb_typeof(item->'requires') IS DISTINCT FROM 'array'
       OR jsonb_typeof(item->'produces') IS DISTINCT FROM 'array'
       OR jsonb_typeof(item->'input_map') IS DISTINCT FROM 'object'
       OR jsonb_typeof(item->'output_map'->'produces') IS DISTINCT FROM 'object'
       OR jsonb_array_length(item->'produces')=0 THEN
      RAISE EXCEPTION 'Contract requires typed requires/produces and input/output maps';
    END IF;
    FOR requirement IN SELECT value FROM jsonb_array_elements((item->'requires')||(item->'produces')) LOOP
      fact_slug := requirement->>'fact_type';
      IF requirement->>'subject_role' IS DISTINCT FROM 'subject'
         OR NOT EXISTS(SELECT 1 FROM catalog.fact_types WHERE slug=fact_slug) THEN
        RAISE EXCEPTION 'Unknown fact or unsupported subject role: %',fact_slug;
      END IF;
    END LOOP;
    IF EXISTS(SELECT 1 FROM jsonb_array_elements(item->'requires') r
      WHERE NOT (item->'input_map' ? (r->>'fact_type')))
      OR EXISTS(SELECT 1 FROM jsonb_array_elements(item->'produces') r
      WHERE NOT (item->'output_map'->'produces' ? (r->>'fact_type'))) THEN
      RAISE EXCEPTION 'Every declared input/output requires a mapping';
    END IF;
    INSERT INTO catalog.capability_execution_contracts (
      capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,
      output_map,cardinality,identifies,max_fan_out,preconditions,pagination,effect,price_model,
      freshness_seconds,adapter_version,sample_payload,notes
    ) VALUES (
      contract_row.capability_slug,endpoint_id,coalesce(contract_row.version,1),'draft',
      contract_row.requires,contract_row.produces,contract_row.input_map,
      coalesce(contract_row.fixed_params,'{}'),contract_row.output_map,
      coalesce(contract_row.cardinality,'one'),contract_row.identifies,
      coalesce(contract_row.max_fan_out,1),'{}',coalesce(contract_row.pagination,'{}'),
      'read','per_call',contract_row.freshness_seconds,coalesce(contract_row.adapter_version,'v1'),
      contract_row.sample_payload,contract_row.notes
    );
  END LOOP;

  -- Optional legacy discovery links. These do NOT authorize execution.
  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'offers','[]')) LOOP
    endpoint_id := (endpoint_ids->>(item->>'endpoint_key'))::uuid;
    IF endpoint_id IS NULL THEN RAISE EXCEPTION 'Unknown offer endpoint_key'; END IF;
    INSERT INTO catalog.capability_offers(capability_slug,endpoint_id,api_id,source,confidence,
      input_map,fixed_params,output_map,notes)
    VALUES(item->>'capability_slug',endpoint_id,source_id,coalesce(item->>'source','inferred'),
      coalesce((item->>'confidence')::numeric,0),coalesce(item->'input_map','{}'),
      coalesce(item->'fixed_params','{}'),item->'output_map',item->>'notes');
  END LOOP;

  -- Optional atomic Vault provisioning: values arrive only through a bound
  -- server connection, not the AI manifest. No existing key is overwritten.
  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'credential_requirements','[]')) LOOP
    secret_name := item->>'vault_name';
    IF coalesce(secret_name,'') !~ '^[A-Z][A-Z0-9_]+$' THEN RAISE EXCEPTION 'Invalid Vault name'; END IF;
    IF item->>'endpoint_key' IS NOT NULL AND NOT endpoint_ids ? (item->>'endpoint_key') THEN
      RAISE EXCEPTION 'Unknown credential endpoint_key';
    END IF;
    IF credential_values ? secret_name THEN
      IF jsonb_typeof(credential_values->secret_name)<>'string'
         OR nullif(btrim(credential_values->>secret_name),'') IS NULL THEN
        RAISE EXCEPTION 'Credential must be a nonempty string';
      END IF;
      IF EXISTS(SELECT 1 FROM vault.secrets WHERE name=secret_name) THEN
        RAISE EXCEPTION 'Vault name already exists; key rotation requires a separate explicit operation';
      END IF;
      PERFORM vault.create_secret(credential_values->>secret_name,secret_name,
        'Apiosk source onboarding: '||(src->>'slug')||'; runtime binding must be implemented and tested');
    END IF;
  END LOOP;
  IF EXISTS(SELECT 1 FROM jsonb_object_keys(credential_values) k WHERE NOT EXISTS(
    SELECT 1 FROM jsonb_array_elements(coalesce(bundle->'credential_requirements','[]')) r WHERE r->>'vault_name'=k)) THEN
    RAISE EXCEPTION 'Credential input contains an undeclared Vault name';
  END IF;
  PERFORM set_config('apiosk.onboarding_secrets','',true);
  RAISE NOTICE 'Source % registered inactive with % endpoints; contracts remain draft. No live execution has been certified.',src->>'slug',jsonb_array_length(bundle->'endpoints');
END;
$onboard$;

COMMIT;
