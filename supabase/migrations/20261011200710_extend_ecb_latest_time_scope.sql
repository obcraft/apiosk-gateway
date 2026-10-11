-- This exact ECB route always returns the most recent EUR reference rate.
-- Preserve all existing metadata and give synonymous time inputs the same
-- reviewed latest-only values. Historical dates are deliberately not accepted.
DO $migration$
DECLARE
  endpoint_id text;
  actual jsonb;
  annotation jsonb;
  scope jsonb;
  latest_values jsonb;
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

  latest_values := scope->'inputs'->'observation_date';
  IF latest_values IS DISTINCT FROM '["latest","most recent","laatste","meest recente"]'::jsonb
  THEN RAISE EXCEPTION 'Reviewed ECB latest-only values changed'; END IF;
  FOREACH input_alias IN ARRAY ARRAY['period','as_of','date','time_period'] LOOP
    IF scope->'inputs' ? input_alias
       AND scope->'inputs'->input_alias IS DISTINCT FROM latest_values
    THEN RAISE EXCEPTION 'Reviewed ECB time input already has different scope: %', input_alias; END IF;
    scope := jsonb_set(scope, ARRAY['inputs',input_alias], latest_values, true);
  END LOOP;
  annotation := jsonb_set(annotation, '{apiosk_fixed_scope_v1,0}', scope);
  UPDATE catalog.fact_types
    SET value_schema=jsonb_set(actual, ARRAY['$comment'], to_jsonb(annotation::text))
    WHERE slug='fx.ecb.reference_rate';
END;
$migration$;
