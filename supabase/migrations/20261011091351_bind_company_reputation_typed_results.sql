-- Keep authored response schemas separate from the automatic opaque endpoint vocabulary.
-- The raw provider document and optional fields remain unchanged.
BEGIN;

INSERT INTO catalog.fact_types(slug,name,description,subject_kind,value_schema,status,freshness_seconds)
SELECT 'company.glassdoor_search_results','Glassdoor employer search results','Provider document with documented optional search-result fields. Empty results are allowed; missing ratings remain unknown.',subject_kind,value_schema,'active',3600
FROM catalog.fact_types WHERE slug='source_document.e22c6c82933545999a55d64ac65128077.response'
ON CONFLICT(slug) DO NOTHING;
UPDATE catalog.capability_execution_contracts SET
 version=2,
 produces='[{"fact_type":"company.glassdoor_search_results","subject_role":"subject","cardinality":"one"}]'::jsonb,
 output_map='{"base":"$","produces":{"company.glassdoor_search_results":{"adapter":"document.response","key":"document"}}}'::jsonb,
 updated_at=now()
WHERE id='077bec6e-1593-469b-8afc-293cb30b7f6b' AND version=1 AND status='active';

INSERT INTO catalog.fact_types(slug,name,description,subject_kind,value_schema,status,freshness_seconds)
SELECT 'location.yelp_search_results','Yelp business search results','Provider document with documented optional search-result fields. Empty results are allowed; missing ratings remain unknown.',subject_kind,value_schema,'active',3600
FROM catalog.fact_types WHERE slug='source_document.eeddb418e721e589b9ea21083dee98afd.response'
ON CONFLICT(slug) DO NOTHING;
UPDATE catalog.capability_execution_contracts SET
 version=2,
 produces='[{"fact_type":"location.yelp_search_results","subject_role":"subject","cardinality":"one"}]'::jsonb,
 output_map='{"base":"$","produces":{"location.yelp_search_results":{"adapter":"document.response","key":"document"}}}'::jsonb,
 updated_at=now()
WHERE id='b030b0eb-d618-448d-8583-849063e0d620' AND version=1 AND status='active';
DO $verify$ BEGIN IF (SELECT count(*) FROM catalog.capability_execution_contracts WHERE id IN ('077bec6e-1593-469b-8afc-293cb30b7f6b','b030b0eb-d618-448d-8583-849063e0d620') AND status='active' AND version=2) <> 2 THEN RAISE EXCEPTION 'Expected two typed result mappings'; END IF; END $verify$;
COMMIT;
