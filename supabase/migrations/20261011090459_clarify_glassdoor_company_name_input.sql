-- The live company-search call accepts the supplied company name.
-- Name the input explicitly so Ask can bind a company subject without asking twice.
BEGIN;
UPDATE catalog.fact_types SET
 name='Glassdoor company name or ID',
 description='Company name or Glassdoor company ID to search.',
 value_schema=value_schema || '{"title":"Company name or Glassdoor ID","description":"Company name or Glassdoor company ID to search. Use the supplied company name when no ID is given."}'::jsonb
WHERE slug='source_input.e22c6c82933545999a55d64ac65128077.query';
UPDATE providers.api_endpoints e SET
 query_params=jsonb_set(e.query_params,'{properties,query,description}','"Company name or Glassdoor company ID to search. Use the supplied company name when no ID is given."'::jsonb)
FROM providers.apis a WHERE a.id=e.api_id AND a.slug='openwebninja' AND e.method='GET' AND e.path='/realtime-glassdoor-data/company-search';
COMMIT;
