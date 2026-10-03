-- A KVK sole trader has naam but need not have statutaireNaam or an RSIN.
-- Keep the legal-name-dependent normalized profile optional; never call a trade
-- name a legal name. Required source identity and company name remain enforced.
-- Historical contracts/results, tariffs, approvals and executions are preserved.
BEGIN;
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '20s';
SELECT pg_advisory_xact_lock(hashtext('kvk-sole-trader-profiles-v1'));
DO $migration$
DECLARE
  previous catalog.capability_execution_contracts%ROWTYPE;
  current_contract catalog.capability_execution_contracts%ROWTYPE;
  capability text;
  repaired_produces jsonb;
  expected_output jsonb := $expected_output${"base": "$", "produces": {"company.name": {"path": "$.statutaireNaam"}, "company.profile": {"fields": {"sbi_codes": {"path": "$.sbiActiviteiten[*].sbiCode", "collect": true}, "legal_name": {"path": "$.statutaireNaam"}, "trade_names": {"path": "$.handelsnamen[*].naam", "collect": true}}}, "company_registry.kvknummer": {"path": "$.kvkNummer"}, "company.nl.registered_name": {"path": "$.statutaireNaam"}, "company.nl.rsin": {"path": "$._embedded.eigenaar.rsin"}, "company.nl.main_establishment_addresses": {"path": "$._embedded.hoofdvestiging.adressen"}, "company.nl.main_establishment_id": {"path": "$._embedded.hoofdvestiging.vestigingsnummer"}, "company.nl.sbi_activities": {"path": "$.sbiActiviteiten"}}}$expected_output$::jsonb;
  repaired_output jsonb := $repaired_output${"base": "$", "produces": {"company.name": {"path": "$.naam"}, "company.profile": {"fields": {"sbi_codes": {"path": "$.sbiActiviteiten[*].sbiCode", "collect": true}, "legal_name": {"path": "$.statutaireNaam"}, "trade_names": {"path": "$.handelsnamen[*].naam", "collect": true}}}, "company_registry.kvknummer": {"path": "$.kvkNummer"}, "company.nl.registered_name": {"path": "$.statutaireNaam"}, "company.nl.rsin": {"path": "$._embedded.eigenaar.rsin"}, "company.nl.main_establishment_addresses": {"path": "$._embedded.hoofdvestiging.adressen"}, "company.nl.main_establishment_id": {"path": "$._embedded.hoofdvestiging.vestigingsnummer"}, "company.nl.sbi_activities": {"path": "$.sbiActiviteiten"}}}$repaired_output$::jsonb;
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id
    WHERE e.id='b050c654-284a-49e8-a7ca-0bfca565816e' AND a.slug='kvk-dutch-business-register'
      AND e.method='GET' AND e.path='/api/v1/basisprofielen' AND e.active
  ) THEN RAISE EXCEPTION 'KVK profile endpoint drift'; END IF;

  FOREACH capability IN ARRAY ARRAY['company.registry-profile','company_registry.lookup'] LOOP
    SELECT * INTO STRICT previous FROM catalog.capability_execution_contracts c
      WHERE c.endpoint_id='b050c654-284a-49e8-a7ca-0bfca565816e'
        AND c.capability_slug=capability AND c.version=7 FOR UPDATE;
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
        SELECT jsonb_agg(jsonb_build_object('fact_type',k,'subject_role','subject','cardinality','one','optional',k LIKE 'company.nl.%') ORDER BY k)
          FROM jsonb_object_keys(expected_output->'produces') k
      ) THEN RAISE EXCEPTION 'KVK v7 contract drift: %',capability; END IF;

    SELECT jsonb_agg(CASE WHEN v->>'fact_type'='company.profile' THEN jsonb_set(v,'{optional}','true') ELSE v END ORDER BY ord)
      INTO repaired_produces FROM jsonb_array_elements(previous.produces) WITH ORDINALITY AS p(v,ord);
    SELECT * INTO current_contract FROM catalog.capability_execution_contracts c
      WHERE c.endpoint_id=previous.endpoint_id AND c.capability_slug=capability AND c.version=8;
    IF FOUND THEN
      IF previous.status IS DISTINCT FROM 'retired' OR current_contract.status IS DISTINCT FROM 'active'
        OR current_contract.output_map IS DISTINCT FROM repaired_output
        OR current_contract.produces IS DISTINCT FROM repaired_produces
        OR (to_jsonb(current_contract)-ARRAY['id','version','status','output_map','produces','validated_at','validated_note','notes','created_at','updated_at'])
          IS DISTINCT FROM (to_jsonb(previous)-ARRAY['id','version','status','output_map','produces','validated_at','validated_note','notes','created_at','updated_at'])
      THEN RAISE EXCEPTION 'KVK v8 contract drift: %',capability; END IF;
      CONTINUE;
    END IF;
    IF previous.status IS DISTINCT FROM 'active' THEN RAISE EXCEPTION 'KVK v7 contract already retired: %',capability; END IF;
    UPDATE catalog.capability_execution_contracts SET status='retired',updated_at=now() WHERE id=previous.id;
    INSERT INTO catalog.capability_execution_contracts(
      capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,output_map,
      cardinality,identifies,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,
      sample_payload,validated_at,validated_note,notes,adapter_version)
    SELECT capability_slug,endpoint_id,8,'active',requires,repaired_produces,input_map,fixed_params,repaired_output,
      cardinality,identifies,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,
      sample_payload,now(),'KVK company name and identity required; legal-name-dependent profile optional for sole traders. BV and sole-trader regression fixtures.',
      'kvk-sole-trader-profiles-v1',adapter_version
    FROM catalog.capability_execution_contracts WHERE id=previous.id;
  END LOOP;
END $migration$;
COMMIT;
