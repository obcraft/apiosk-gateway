-- Dedicated URL-reading contract; existing Linkup operations and endpoint prices are unchanged.

-- Apply only after deploying the matching Gateway input adapter.

BEGIN;

SELECT pg_advisory_xact_lock(hashtext('ask-website-read-v1'));

DO $check$ BEGIN IF NOT EXISTS (
 SELECT 1 FROM catalog.capability_execution_contracts c JOIN providers.api_endpoints e ON e.id=c.endpoint_id
 JOIN providers.apis a ON a.id=e.api_id WHERE c.id='8d328d7a-6706-4d8e-a68e-b941aa4077af'
 AND c.status='active' AND a.slug='linkup' AND a.status='active' AND e.path='/v1/search' AND e.active AND e.price=0.01
) THEN RAISE EXCEPTION 'Reviewed Linkup operation changed'; END IF; END $check$;

INSERT INTO catalog.fact_types(slug,name,description,subject_kind,value_schema,status,freshness_seconds) VALUES('web.website_query','query','User-supplied literal for bounded website reading.','web','{"type":"string","minLength":1,"maxLength":300,"title":"query"}'::jsonb,'active',300) ON CONFLICT(slug) DO NOTHING;

INSERT INTO catalog.fact_types(slug,name,description,subject_kind,value_schema,status,freshness_seconds) VALUES('web.website_url','url','User-supplied literal for bounded website reading.','web','{"type":"string","minLength":1,"maxLength":500,"pattern":"^https?://[^\\s]+$","title":"url"}'::jsonb,'active',300) ON CONFLICT(slug) DO NOTHING;

INSERT INTO catalog.capabilities(slug,name,description,category,input_contract,output_contract,status) SELECT 'research.website.read','Website reading','Read a supplied website URL and related pages to answer the requested website questions with cited sources.','research','{"type":"object","properties":{"url":{"type":"string","minLength":1,"maxLength":500,"pattern":"^https?://[^\\s]+$","title":"url"},"query":{"type":"string","minLength":1,"maxLength":300,"title":"query"}},"required":["url","query"]}'::jsonb,output_contract,'active' FROM catalog.capabilities WHERE slug='web.linkup-answer' ON CONFLICT(slug) DO NOTHING;

INSERT INTO catalog.capability_execution_contracts(id,capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,output_map,cardinality,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,sample_payload,validated_at,validated_note,adapter_version,notes)
SELECT 'a2cb8ea3-e5ca-42d8-8195-17abdabdd819','research.website.read',endpoint_id,1,'active','[{"fact_type":"web.website_query","subject_role":"subject","optional":false},{"fact_type":"web.website_url","subject_role":"subject","optional":false}]'::jsonb,produces,'{"web.website_query":{"location":"body","name":"q"},"web.website_url":{"location":"body","name":"url"}}'::jsonb,'{"depth":"standard","outputType":"sourcedAnswer","maxResults":3,"includeInlineCitations":true}'::jsonb,output_map,'one',1,'{}','{}','read','per_call',300,sample_payload,validated_at,
'Existing validated Linkup response shape. URL-scoped input adapter validated separately; no claim of a new provider response in this sample.', 'website-read-v1',
'One standard call per website; three returned sources; domain restricted. Gateway owns URL validation and query scoping.'
FROM catalog.capability_execution_contracts WHERE id='8d328d7a-6706-4d8e-a68e-b941aa4077af' ON CONFLICT(id) DO NOTHING;

COMMIT;

