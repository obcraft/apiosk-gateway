WITH recent_execution_evidence AS (
 SELECT u.api_slug, count(*) FILTER (WHERE x.status='succeeded') AS successes,
 count(*) FILTER (WHERE x.status='failed') AS failures,
 count(*) FILTER (WHERE x.status IN ('dispatching','unresolved')) AS unresolved
 FROM executions x JOIN user_selected_apis u ON u.id=x.selection_id
 WHERE x.created_at >= now() - interval '30 days' GROUP BY u.api_slug
)
SELECT jsonb_build_object(
 'known_sources',(SELECT coalesce(jsonb_agg(jsonb_build_object(
 'slug',a.slug,'name',a.name,'block_reason',a.listing_metadata->>'planner_block_reason',
 'metadata',jsonb_build_object(
   'provider_slug',p.username,'description',left(coalesce(a.description,''),600),'category',coalesce(a.category,''),
   'tags',coalesce((SELECT jsonb_agg(tag ORDER BY tag) FROM (SELECT DISTINCT left(t,80) AS tag FROM api_endpoints e CROSS JOIN LATERAL unnest(e.tags) t WHERE e.api_id=a.id AND e.active AND nullif(t,'') IS NOT NULL ORDER BY tag LIMIT 32) tags),'[]'),
   'capability_descriptions',coalesce((SELECT jsonb_object_agg(slug,description) FROM (SELECT DISTINCT cap.slug,left(coalesce(cap.description,''),240) AS description FROM capability_execution_contracts c JOIN api_endpoints e ON e.id=c.endpoint_id JOIN capabilities cap ON cap.slug=c.capability_slug WHERE e.api_id=a.id AND e.active AND (c.status='active' OR (c.status='validated' AND c.adapter_version='source-document-v1')) AND cap.status='active' ORDER BY cap.slug LIMIT 100) caps),'{}'),
   'identity_documented',nullif(trim(a.name),'') IS NOT NULL AND coalesce(a.docs_url,a.website_url,'') ~ '^https://',
   'endpoint_count',m.total,'described',m.described,'tagged',m.tagged,'categorized',m.categorized,'documented_inputs',m.inputs,'inferred',m.inferred,'unknown_provenance',m.unknown_provenance,
   'recent_successes',coalesce(r.successes,0),'recent_failures',coalesce(r.failures,0),'recent_unresolved',coalesce(r.unresolved,0),
   'coverage_notice',left(a.listing_metadata->>'coverage_notice',1000)
 )) ORDER BY a.slug),'[]') FROM apis a LEFT JOIN provider_profiles p ON p.id=a.owner_id
 LEFT JOIN recent_execution_evidence r ON r.api_slug=a.slug
 CROSS JOIN LATERAL (
  SELECT count(*) AS total,
   count(*) FILTER (WHERE nullif(trim(e.description),'') IS NOT NULL) AS described,
   count(*) FILTER (WHERE cardinality(e.tags)>0) AS tagged,
   count(*) FILTER (WHERE nullif(trim(e.endpoint_category),'') IS NOT NULL) AS categorized,
   -- An endpoint documents its inputs when it published something a form can be
   -- built from. Two ways this was too strict, and both hid real documentation:
   --
   --   1. An absent `required` means nothing is required, not that nobody wrote
   --      the schema — JSON Schema says so. Demanding the key made a source
   --      whose every parameter is optional impossible to count: KOOP scored
   --      0 of 1 while its /sru schema names q, authority and date, each with
   --      its own description and "required": false. It must still be an ARRAY
   --      where present, so a boolean put there is caught as the mistake it is.
   --   2. A flat parameter map is not JSON Schema, but it IS a schema this
   --      product reads: `readFields` in app/src/lib/domain/inputs.ts builds
   --      forms from it on purpose, flat or grouped by location. All five KVK
   --      endpoints publish that shape, every parameter described and flagged,
   --      and scored zero — the most-used source in the catalogue, reported as
   --      though nobody had written down what it takes.
   --
   -- `$.* ? (@.type() == "object")` is what separates those two real shapes from
   -- the empty publish: `{"type":"object"}` is what several listings send when
   -- they mean "no schema", every member of it is a string, and it still counts
   -- for nothing. The app draws the same line in `isFieldSpec`.
   count(*) FILTER (WHERE (jsonb_typeof(e.query_params->'properties')='object' AND jsonb_typeof(coalesce(e.query_params->'required','[]'::jsonb))='array') OR (jsonb_typeof(e.request_body->'properties')='object' AND jsonb_typeof(coalesce(e.request_body->'required','[]'::jsonb))='array') OR jsonb_path_exists(e.query_params,'$.* ? (@.type() == "object")') OR jsonb_path_exists(e.request_body,'$.* ? (@.type() == "object")')) AS inputs,
   count(*) FILTER (WHERE e.metadata_source='inferred') AS inferred,
   count(*) FILTER (WHERE nullif(trim(e.metadata_source),'') IS NULL) AS unknown_provenance
  FROM api_endpoints e WHERE e.api_id=a.id AND e.active
 ) m
 WHERE a.status='active' AND coalesce(a.active,true) AND a.is_verified=true AND coalesce(a.discoverable,true) AND a.suspended_at IS NULL),
 'vocabulary',(SELECT coalesce(jsonb_agg(jsonb_build_object('slug',slug,'value_schema',value_schema) ORDER BY slug),'[]') FROM fact_types WHERE status='active'),
 'services',(SELECT coalesce(jsonb_agg(s.obj ORDER BY s.id),'[]') FROM (
 SELECT c.id,jsonb_build_object(
  'id',c.id,'unverified',c.status<>'active','endpoint_id',e.id,'api_slug',a.slug,'name',a.name,'description',left(cap.description,400),'source_url',coalesce(a.docs_url,a.website_url),
  'usage_notice',a.listing_metadata->'usage_notice',
  'method',e.method,'path',e.path,'listing_type',coalesce(a.listing_type::text,'api'),
  'list_atomic',ceil(e.price*1000000)::bigint,
  'optional_outputs',(SELECT coalesce(jsonb_agg(p->>'fact_type' ORDER BY p->>'fact_type'),'[]') FROM jsonb_array_elements(c.produces) p WHERE p->>'optional'='true'),
  'fixed_params',c.fixed_params,'preconditions',c.preconditions,'pagination',c.pagination,
  'freshness_seconds',c.freshness_seconds,'sample_payload',c.sample_payload,
  'jurisdiction',coalesce(ft.jurisdiction,(
    SELECT CASE WHEN count(DISTINCT required_fact.jurisdiction)=1 THEN min(required_fact.jurisdiction) END
    FROM jsonb_array_elements(c.requires) requirement
    JOIN fact_types required_fact ON required_fact.slug=requirement->>'fact_type' AND required_fact.status='active'
    WHERE coalesce((requirement->>'optional')::boolean,false)=false
  )),
  'contract',jsonb_build_object('capability_slug',c.capability_slug,'version',c.version,
    'cardinality',c.cardinality,'requires',c.requires,'input_map',c.input_map,
    'output_map',c.output_map,'identifies',c.identifies,'max_fan_out',c.max_fan_out)) obj
 FROM capability_execution_contracts c
 JOIN api_endpoints e ON e.id=c.endpoint_id
 JOIN apis a ON a.id=e.api_id
 JOIN capabilities cap ON cap.slug=c.capability_slug AND cap.status='active'
 LEFT JOIN fact_types ft ON ft.slug=c.identifies
 WHERE (c.status='active' OR (c.status='validated' AND c.adapter_version='source-document-v1')) AND c.validated_at IS NOT NULL AND c.effect='read'
 AND c.price_model='per_call' AND e.active=true AND e.price IS NOT NULL AND e.price>=0
 AND a.status='active' AND coalesce(a.active,true) AND a.is_verified=true
 AND coalesce(a.discoverable,true) AND a.suspended_at IS NULL
 ) s)
);
