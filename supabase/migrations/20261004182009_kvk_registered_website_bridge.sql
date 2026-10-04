-- Bind company website research to the selected registry response, never the
-- initial search name. Deploy the record.website-query adapter before this data migration.
-- Existing prices, generic Linkup search, saved contracts and paid results stay intact.
BEGIN;
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '20s';
SELECT pg_advisory_xact_lock(hashtext('kvk-registered-website-bridge-v1'));
DO $migration$
DECLARE
  previous catalog.capability_execution_contracts%ROWTYPE;
  current_contract catalog.capability_execution_contracts%ROWTYPE;
  capability text;
  repaired_produces jsonb;
  repaired_output jsonb;
  expected_output jsonb := $expected_output${"base": "$", "produces": {"company.name": {"path": "$.naam"}, "company.profile": {"fields": {"sbi_codes": {"path": "$.sbiActiviteiten[*].sbiCode", "collect": true}, "legal_name": {"path": "$.statutaireNaam"}, "trade_names": {"path": "$.handelsnamen[*].naam", "collect": true}}}, "company_registry.kvknummer": {"path": "$.kvkNummer"}, "company.nl.registered_name": {"path": "$.statutaireNaam"}, "company.nl.rsin": {"path": "$._embedded.eigenaar.rsin"}, "company.nl.main_establishment_addresses": {"path": "$._embedded.hoofdvestiging.adressen"}, "company.nl.main_establishment_id": {"path": "$._embedded.hoofdvestiging.vestigingsnummer"}, "company.nl.sbi_activities": {"path": "$.sbiActiviteiten"}}}$expected_output$::jsonb;
  query_output jsonb := $query_output${"adapter":"record.website-query","key":"{\"name\":\"/naam\",\"identifier\":\"/kvkNummer\",\"registry\":\"KVK\",\"pattern\":\"^[0-9]{8}$\"}"}$query_output$::jsonb;
  query_requires jsonb := '[{"optional":false,"fact_type":"web.company_website_query","subject_role":"subject"}]'::jsonb;
  query_input jsonb := '{"web.company_website_query":{"name":"q","location":"body"}}'::jsonb;
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id
    WHERE e.id='b050c654-284a-49e8-a7ca-0bfca565816e' AND a.slug='kvk-dutch-business-register'
      AND e.method='GET' AND e.path='/api/v1/basisprofielen' AND e.active
  ) THEN RAISE EXCEPTION 'KVK profile endpoint drift'; END IF;
  IF NOT EXISTS (
    SELECT 1 FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id
    WHERE e.id='083326ff-e9f9-410f-bea4-0e2aaca9e56a' AND a.slug='linkup'
      AND e.method='POST' AND e.path='/v1/search' AND e.active
      AND e.price=0.01000000
  ) THEN RAISE EXCEPTION 'Linkup search endpoint drift'; END IF;

  INSERT INTO catalog.fact_types(slug,name,description,subject_kind,value_schema,status)
  VALUES('web.company_website_query','Registered company website query',
    'Website research query derived from the name and identifier in one selected registry response. Search results still require source-backed identity checks.',
    'company','{"type":"string","minLength":1,"maxLength":1000}'::jsonb,'active')
  ON CONFLICT(slug) DO NOTHING;
  IF NOT EXISTS (SELECT 1 FROM catalog.fact_types WHERE slug='web.company_website_query'
    AND subject_kind='company' AND status='active'
    AND value_schema='{"type":"string","minLength":1,"maxLength":1000}'::jsonb)
  THEN RAISE EXCEPTION 'Company website query vocabulary drift'; END IF;

  repaired_output := jsonb_set(expected_output,'{produces,web.company_website_query}',query_output);
  FOREACH capability IN ARRAY ARRAY['company.registry-profile','company_registry.lookup'] LOOP
    SELECT * INTO STRICT previous FROM catalog.capability_execution_contracts c
      WHERE c.endpoint_id='b050c654-284a-49e8-a7ca-0bfca565816e'
        AND c.capability_slug=capability AND c.version=8 FOR UPDATE;
    IF previous.status NOT IN ('active','retired') OR previous.output_map IS DISTINCT FROM expected_output
      OR previous.cardinality IS DISTINCT FROM 'one'
      OR previous.identifies IS DISTINCT FROM 'company_registry.kvknummer'
      OR previous.max_fan_out IS DISTINCT FROM 1
      OR previous.requires IS DISTINCT FROM '[{"optional":false,"fact_type":"company_registry.kvknummer","subject_role":"subject"}]'::jsonb
      OR previous.input_map IS DISTINCT FROM '{"company_registry.kvknummer":{"in":"path_append","name":"kvkNummer"}}'::jsonb
      OR previous.fixed_params IS DISTINCT FROM '{}'::jsonb
      OR (
        SELECT jsonb_agg(v ORDER BY v->>'fact_type') FROM jsonb_array_elements(previous.produces) v
      ) IS DISTINCT FROM (
        SELECT jsonb_agg(jsonb_build_object('fact_type',k,'subject_role','subject','cardinality','one','optional',k LIKE 'company.nl.%' OR k='company.profile') ORDER BY k)
          FROM jsonb_object_keys(expected_output->'produces') k
      ) THEN RAISE EXCEPTION 'KVK v8 contract drift: %',capability; END IF;

    repaired_produces := previous.produces || '[{"fact_type":"web.company_website_query","subject_role":"subject","cardinality":"one","optional":true}]'::jsonb;
    SELECT * INTO current_contract FROM catalog.capability_execution_contracts c
      WHERE c.endpoint_id=previous.endpoint_id AND c.capability_slug=capability AND c.version=9;
    IF FOUND THEN
      IF previous.status IS DISTINCT FROM 'retired' OR current_contract.status IS DISTINCT FROM 'active'
        OR current_contract.output_map IS DISTINCT FROM repaired_output
        OR current_contract.produces IS DISTINCT FROM repaired_produces
        OR (to_jsonb(current_contract)-ARRAY['id','version','status','output_map','produces','validated_at','validated_note','notes','created_at','updated_at'])
          IS DISTINCT FROM (to_jsonb(previous)-ARRAY['id','version','status','output_map','produces','validated_at','validated_note','notes','created_at','updated_at'])
      THEN RAISE EXCEPTION 'KVK v9 contract drift: %',capability; END IF;
      CONTINUE;
    END IF;
    IF previous.status IS DISTINCT FROM 'active' THEN RAISE EXCEPTION 'KVK v8 contract already retired: %',capability; END IF;
    UPDATE catalog.capability_execution_contracts SET status='retired',updated_at=now() WHERE id=previous.id;
    INSERT INTO catalog.capability_execution_contracts(
      capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,output_map,
      cardinality,identifies,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,
      sample_payload,validated_at,validated_note,notes,adapter_version)
    SELECT capability_slug,endpoint_id,9,'active',requires,repaired_produces,input_map,fixed_params,repaired_output,
      cardinality,identifies,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,
      sample_payload,now(),'Website search query uses exact source name and validated registry identity; no inferred website or initial-name shortcut.',
      'kvk-registered-website-bridge-v1',adapter_version
    FROM catalog.capability_execution_contracts WHERE id=previous.id;
  END LOOP;

  SELECT * INTO STRICT previous FROM catalog.capability_execution_contracts c
    WHERE c.endpoint_id='083326ff-e9f9-410f-bea4-0e2aaca9e56a'
      AND c.capability_slug='web.linkup-answer' AND c.version=1 FOR UPDATE;
  IF previous.status IS DISTINCT FROM 'active'
    OR previous.requires IS DISTINCT FROM '[{"optional":false,"fact_type":"web.source_search_query","subject_role":"subject"}]'::jsonb
    OR previous.input_map IS DISTINCT FROM '{"web.source_search_query":{"name":"q","location":"body"}}'::jsonb
    OR previous.output_map IS DISTINCT FROM '{"base":"$","produces":{"web.linkup_answer_page":{"path":"$"}}}'::jsonb
    OR previous.fixed_params IS DISTINCT FROM '{"depth":"standard","outputType":"sourcedAnswer"}'::jsonb
    OR previous.cardinality IS DISTINCT FROM 'one' OR previous.identifies IS NOT NULL
    OR previous.max_fan_out IS DISTINCT FROM 1 OR previous.effect IS DISTINCT FROM 'read'
    OR previous.price_model IS DISTINCT FROM 'per_call'
  THEN RAISE EXCEPTION 'Linkup sourced-answer v1 drift'; END IF;

  INSERT INTO catalog.capabilities(slug,name,description,category,input_contract,output_contract,status)
  VALUES('web.company-website-search','Registered company website research',
    'Search public website evidence using the registered company name and identifier from the selected registry record. Cite returned pages, products and services; state uncertainty when website identity is not supported.',
    'web','{}','{}','active') ON CONFLICT(slug) DO NOTHING;
  IF NOT EXISTS (SELECT 1 FROM catalog.capabilities WHERE slug='web.company-website-search'
    AND status='active' AND input_contract='{}'::jsonb AND output_contract='{}'::jsonb)
  THEN RAISE EXCEPTION 'Company website capability drift'; END IF;

  SELECT * INTO current_contract FROM catalog.capability_execution_contracts c
    WHERE c.endpoint_id=previous.endpoint_id AND c.capability_slug='web.company-website-search' AND c.version=1;
  IF FOUND THEN
    IF current_contract.status IS DISTINCT FROM 'active'
      OR current_contract.requires IS DISTINCT FROM query_requires
      OR current_contract.input_map IS DISTINCT FROM query_input
      OR (to_jsonb(current_contract)-ARRAY['id','capability_slug','requires','input_map','validated_at','validated_note','notes','created_at','updated_at'])
        IS DISTINCT FROM (to_jsonb(previous)-ARRAY['id','capability_slug','requires','input_map','validated_at','validated_note','notes','created_at','updated_at'])
    THEN RAISE EXCEPTION 'Company website search v1 drift'; END IF;
  ELSE
    INSERT INTO catalog.capability_execution_contracts(
      capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,output_map,
      cardinality,identifies,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,
      sample_payload,validated_at,validated_note,notes,adapter_version)
    SELECT 'web.company-website-search',endpoint_id,version,status,query_requires,produces,query_input,fixed_params,output_map,
      cardinality,identifies,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,
      sample_payload,now(),'Existing sourced-answer endpoint with selected-registry-derived query only; same price and response shape.',
      'kvk-registered-website-bridge-v1',adapter_version
    FROM catalog.capability_execution_contracts WHERE id=previous.id;
  END IF;
  -- Directory metadata mirrors the existing offer when present; execution
  -- remains governed by the active, versioned contract above.
  INSERT INTO catalog.capability_offers(
    capability_slug,endpoint_id,api_id,source,confidence,input_map,fixed_params,notes,output_map)
  SELECT 'web.company-website-search',endpoint_id,api_id,source,confidence,query_input,fixed_params,
    'kvk-registered-website-bridge-v1',output_map
  FROM catalog.capability_offers
  WHERE capability_slug='web.linkup-answer' AND endpoint_id=previous.endpoint_id
  ON CONFLICT(capability_slug,endpoint_id) DO NOTHING;
END $migration$;
COMMIT;
