-- Declare response fields needed for output coverage; no extra API calls or broadened request scope.

-- Crunchbase and Glassdoor: saved successful provider responses. Yelp: official business-search OpenAPI schema.

-- Optional row fields and empty arrays remain valid. Review text and full company profiles are not promised.

BEGIN;

UPDATE catalog.fact_types SET value_schema=$crunchbase_output${"type":"object","required":["count","entities"],"properties":{"count":{"type":"integer"},"entities":{"type":"array","items":{"type":"object","required":["identifier"],"properties":{"short_description":{"type":"string","$comment":"{\"apiosk_output_aliases_v1\":[\"description\",\"company_description\"]}"},"identifier":{"type":"object","properties":{"uuid":{"type":"string"},"value":{"type":"string"},"permalink":{"type":"string"},"url":{"type":"string","$comment":"{\"apiosk_output_aliases_v1\":[\"profile_url\",\"crunchbase_url\",\"link\"]}"}}}},"$comment":"{\"apiosk_output_aliases_v1\":[\"profile\"]}"}}}}$crunchbase_output$::jsonb WHERE slug='company.crunchbase_search_results';

UPDATE catalog.fact_types SET value_schema=$glassdoor_output${"type":"object","required":["document"],"properties":{"document":{"type":"object","required":["status","data"],"properties":{"status":{"type":"string","enum":["OK"]},"data":{"type":"array","items":{"type":"object","properties":{"name":{"type":"string"},"company_id":{"type":"integer"},"company_link":{"type":"string","$comment":"{\"apiosk_output_aliases_v1\":[\"profile_url\"]}"},"rating":{"type":"number","$comment":"{\"apiosk_output_aliases_v1\":[\"average_rating\",\"employer_rating\"]}"},"review_count":{"type":"integer"},"company_description":{"type":"string","$comment":"{\"apiosk_output_aliases_v1\":[\"description\"]}"}}}}}}}}$glassdoor_output$::jsonb WHERE slug='source_document.e22c6c82933545999a55d64ac65128077.response';

UPDATE catalog.fact_types SET value_schema=$yelp_output${"type":"object","required":["document"],"properties":{"document":{"type":"object","required":["status","data"],"properties":{"status":{"type":"string","enum":["OK"]},"data":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"alias":{"type":"string"},"name":{"type":"string"},"business_page_link":{"type":"string","$comment":"{\"apiosk_output_aliases_v1\":[\"profile_url\"]}"},"rating":{"type":"number","$comment":"{\"apiosk_output_aliases_v1\":[\"average_rating\",\"customer_rating\"]}"},"review_count":{"type":"integer"}}}}}}}}$yelp_output$::jsonb WHERE slug='source_document.eeddb418e721e589b9ea21083dee98afd.response';

COMMIT;
