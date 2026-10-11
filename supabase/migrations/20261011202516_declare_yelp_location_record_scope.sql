-- Reviewed Yelp business-search record alias. A returned business record is a
-- location/business_location; this does not promise a street address.
-- Evidence: https://openwebninja.s3.us-east-1.amazonaws.com/portal/openapi/yelp_business_data.yaml
-- Only the item object's semantic annotation changes; validation and extraction
-- (including valid empty result arrays) remain identical.
DO $migration$
DECLARE
  actual jsonb;
  expected jsonb := $before${"properties":{"document":{"properties":{"data":{"items":{"properties":{"alias":{"type":"string"},"business_page_link":{"$comment":"{\"apiosk_output_aliases_v1\":[\"profile_url\"]}","type":"string"},"id":{"type":"string"},"name":{"type":"string"},"rating":{"$comment":"{\"apiosk_output_aliases_v1\":[\"average_rating\",\"customer_rating\"]}","type":"number"},"review_count":{"type":"integer"}},"type":"object"},"type":"array"},"status":{"enum":["OK"],"type":"string"}},"required":["status","data"],"type":"object"}},"required":["document"],"type":"object"}$before$::jsonb;
  reviewed jsonb;
BEGIN
  IF (SELECT count(*) FROM providers.api_endpoints e
      JOIN providers.apis a ON a.id=e.api_id
      JOIN catalog.capability_execution_contracts c ON c.endpoint_id=e.id
      WHERE a.slug='openwebninja' AND e.method='GET'
        AND e.path='/yelp-business-data/business-search' AND e.active
        AND c.status='active' AND c.output_map->'produces' ? 'location.yelp_search_results') <> 1
  THEN RAISE EXCEPTION 'Reviewed Yelp business-search contract identity drift'; END IF;
  SELECT value_schema INTO STRICT actual FROM catalog.fact_types
    WHERE slug='location.yelp_search_results' AND status='active' FOR UPDATE;
  reviewed := jsonb_set(expected,
    '{properties,document,properties,data,items,$comment}',
    to_jsonb('{"apiosk_output_aliases_v1":["location","business_location"]}'::text), true);
  IF actual IS DISTINCT FROM expected AND actual IS DISTINCT FROM reviewed
  THEN RAISE EXCEPTION 'Reviewed Yelp business-search schema drift'; END IF;
  UPDATE catalog.fact_types SET value_schema=reviewed
    WHERE slug='location.yelp_search_results';
END;
$migration$;
