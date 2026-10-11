-- Reviewed Glassdoor search ratings describe employees' employer ratings.
-- Add one output synonym on rating only; customer ratings remain unsupported.
DO $migration$
DECLARE
  actual jsonb;
  expected jsonb := $before${"properties":{"document":{"properties":{"data":{"items":{"properties":{"company_description":{"$comment":"{\"apiosk_output_aliases_v1\":[\"description\"]}","type":"string"},"company_id":{"type":"integer"},"company_link":{"$comment":"{\"apiosk_output_aliases_v1\":[\"profile_url\"]}","type":"string"},"name":{"type":"string"},"rating":{"$comment":"{\"apiosk_output_aliases_v1\":[\"average_rating\",\"employer_rating\"]}","type":"number"},"review_count":{"type":"integer"}},"type":"object"},"type":"array"},"status":{"enum":["OK"],"type":"string"}},"required":["status","data"],"type":"object"}},"required":["document"],"type":"object"}$before$::jsonb;
  reviewed jsonb;
BEGIN
  IF (SELECT count(*) FROM catalog.capability_execution_contracts c
      JOIN providers.api_endpoints e ON e.id=c.endpoint_id
      JOIN providers.apis a ON a.id=e.api_id
      WHERE c.status='active' AND e.active AND e.method='GET'
        AND e.path='/realtime-glassdoor-data/company-search'
        AND a.slug='openwebninja'
        AND c.output_map->'produces'->'company.glassdoor_search_results'=
          '{"adapter":"document.response","key":"document"}'::jsonb) <> 1
  THEN RAISE EXCEPTION 'Reviewed Glassdoor rating contract identity drift'; END IF;
  SELECT value_schema INTO STRICT actual FROM catalog.fact_types
    WHERE slug='company.glassdoor_search_results' AND status='active' FOR UPDATE;
  reviewed := jsonb_set(expected,
    '{properties,document,properties,data,items,properties,rating,$comment}',
    to_jsonb('{"apiosk_output_aliases_v1":["average_rating","employer_rating","employee_rating"]}'::text));
  IF actual IS DISTINCT FROM expected AND actual IS DISTINCT FROM reviewed
  THEN RAISE EXCEPTION 'Reviewed Glassdoor rating schema drift'; END IF;
  UPDATE catalog.fact_types SET value_schema=reviewed
    WHERE slug='company.glassdoor_search_results';
END;
$migration$;
