-- Reviewed qualifier vocabulary for the exact ECB EUR reference-rate route.
-- Aliases reuse the canonical qualifier's values; no new dates or authorities
-- become supported and all existing schema validation remains unchanged.
DO $migration$
DECLARE
  endpoint_id text;
  actual jsonb;
  annotation jsonb;
  scope jsonb;
  qualifier jsonb;
  canonical_values jsonb;
  input_alias text;
BEGIN
  IF (SELECT count(*) FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id
      WHERE a.slug='frankfurter' AND e.method='GET'
        AND e.path='/v2/providers/ecb/rate/EUR/{quote}' AND e.active) <> 1
  THEN RAISE EXCEPTION 'Reviewed ECB endpoint identity drift'; END IF;
  SELECT e.id::text INTO STRICT endpoint_id
    FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id
    WHERE a.slug='frankfurter' AND e.method='GET'
      AND e.path='/v2/providers/ecb/rate/EUR/{quote}' AND e.active;

  SELECT value_schema INTO STRICT actual FROM catalog.fact_types
    WHERE slug='fx.ecb.reference_rate' FOR UPDATE;
  annotation := (actual->>'$comment')::jsonb;
  IF jsonb_typeof(annotation->'apiosk_fixed_scope_v1') IS DISTINCT FROM 'array'
     OR jsonb_array_length(annotation->'apiosk_fixed_scope_v1') <> 1
  THEN RAISE EXCEPTION 'Reviewed ECB fixed scope changed'; END IF;
  scope := annotation->'apiosk_fixed_scope_v1'->0;
  IF scope->>'endpoint_id' IS DISTINCT FROM endpoint_id
     OR scope->>'source_slug' IS DISTINCT FROM 'frankfurter'
     OR scope->>'method' IS DISTINCT FROM 'GET'
     OR scope->>'path' IS DISTINCT FROM '/v2/providers/ecb/rate/EUR/{quote}'
     OR scope->'input_facts' IS DISTINCT FROM '["fx.ecb.quote"]'::jsonb
  THEN RAISE EXCEPTION 'Reviewed ECB fixed scope identity changed'; END IF;

  FOR qualifier IN SELECT value FROM jsonb_array_elements('[
    {"canonical":"observation_date","values":["latest","most recent","laatste","meest recente"],"aliases":["recency","time_scope"]},
    {"canonical":"reference_source","values":["ECB"],"aliases":["authority","reference_authority"]}
  ]'::jsonb) LOOP
    canonical_values := scope->'inputs'->(qualifier->>'canonical');
    IF canonical_values IS DISTINCT FROM qualifier->'values'
    THEN RAISE EXCEPTION 'Reviewed ECB canonical qualifier values changed: %', qualifier->>'canonical'; END IF;
    FOR input_alias IN SELECT jsonb_array_elements_text(qualifier->'aliases') LOOP
      IF scope->'inputs' ? input_alias
         AND scope->'inputs'->input_alias IS DISTINCT FROM canonical_values
      THEN RAISE EXCEPTION 'Reviewed ECB qualifier already has different scope: %', input_alias; END IF;
      scope := jsonb_set(scope, ARRAY['inputs',input_alias], canonical_values, true);
    END LOOP;
  END LOOP;
  annotation := jsonb_set(annotation, '{apiosk_fixed_scope_v1,0}', scope);
  UPDATE catalog.fact_types
    SET value_schema=jsonb_set(actual, ARRAY['$comment'], to_jsonb(annotation::text))
    WHERE slug='fx.ecb.reference_rate';
END;
$migration$;
