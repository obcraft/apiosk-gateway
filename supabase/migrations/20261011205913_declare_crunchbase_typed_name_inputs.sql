-- Reviewed official MCP autocomplete input: one literal company or person name.
-- Only semantic aliases change; type, validation, transport and price stay intact.
-- Scope remains the operation's declared company/person collection.
DO $migration$
DECLARE
  actual jsonb;
  expected jsonb := '{"type":"string","minLength":1,"maxLength":200,"description":"Literal name, not the entire task"}'::jsonb;
  reviewed jsonb;
  operations text[];
BEGIN
  SELECT array_agg(c.capability_slug ORDER BY c.capability_slug) INTO operations
  FROM catalog.capability_execution_contracts c
  JOIN providers.api_endpoints e ON e.id=c.endpoint_id
  JOIN providers.apis a ON a.id=e.api_id
  WHERE c.status='active' AND e.active AND e.method='POST'
    AND e.path='/mcp/cb_entity_autocomplete'
    AND a.slug='apimarket-magicapi-crunchbase-bbcca0a6'
    AND c.input_map->'crunchbase.entity_autocomplete_query'='{"in":"body","name":"query"}'::jsonb;
  IF operations IS DISTINCT FROM ARRAY['company.crunchbase-official-search','person.crunchbase-search','research.crunchbase-identifiers']::text[]
  THEN RAISE EXCEPTION 'Reviewed Crunchbase autocomplete contract identity drift'; END IF;
  SELECT value_schema INTO STRICT actual FROM catalog.fact_types
    WHERE slug='crunchbase.entity_autocomplete_query' AND status='active' FOR UPDATE;
  reviewed := expected || jsonb_build_object('$comment',
    '{"apiosk_input_aliases_v1":["company_name","business_name","organization_name","organisation_name","person_name","individual_name"]}');
  IF actual IS DISTINCT FROM expected AND actual IS DISTINCT FROM reviewed
  THEN RAISE EXCEPTION 'Reviewed Crunchbase autocomplete input schema drift'; END IF;
  UPDATE catalog.fact_types SET value_schema=reviewed
    WHERE slug='crunchbase.entity_autocomplete_query';
END;
$migration$;
