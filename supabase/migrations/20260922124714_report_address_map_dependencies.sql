-- Add source-derived RD map extents to an exact KVK-address/PDOK match.
-- No new provider, price, authority, table or privilege is introduced.
BEGIN;
UPDATE catalog.capability_execution_contracts c SET
 output_map=jsonb_set(c.output_map,'{produces}',c.output_map->'produces' || '{"source.pdok_ruimtelijke_plannen.b_b_o_x":{"adapter":"point.rd-extent","key":"/source_record/centroide_rd"},"source.bodemloket.b_b_o_x":{"adapter":"point.rd-extent","key":"/source_record/centroide_rd"}}'::jsonb),
 produces=c.produces || '[{"fact_type":"source.pdok_ruimtelijke_plannen.b_b_o_x","subject_role":"subject","cardinality":"many","optional":true},{"fact_type":"source.bodemloket.b_b_o_x","subject_role":"subject","cardinality":"many","optional":true}]'::jsonb,
 version=c.version+1,validated_at=now()
FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id
WHERE c.endpoint_id=e.id AND a.slug='pdok' AND c.capability_slug='company.registered-address-context'
 AND c.status='active' AND NOT (c.output_map->'produces' ? 'source.bodemloket.b_b_o_x');
-- Published default map layers; explicit caller-provided layer still overrides them.
UPDATE catalog.capability_execution_contracts c SET
 requires=(SELECT jsonb_agg(CASE WHEN r->>'fact_type' LIKE '%.l_a_y_e_r_s' THEN r || '{"optional":true}'::jsonb ELSE r END ORDER BY n) FROM jsonb_array_elements(c.requires) WITH ORDINALITY AS x(r,n)),
 input_map=(SELECT jsonb_object_agg(k,CASE WHEN k LIKE '%.l_a_y_e_r_s' THEN v || jsonb_build_object('default',CASE c.capability_slug WHEN 'soil.bodemloket.feature-info' THEN 'Beschikbaarheid_gegevens' ELSE 'enkelbestemming' END) ELSE v END) FROM jsonb_each(c.input_map) AS x(k,v)),
 version=c.version+1,validated_at=now()
WHERE c.status='active' AND c.capability_slug IN ('soil.bodemloket.feature-info','planning.pdok.feature-info')
 AND NOT jsonb_path_exists(c.input_map,'$.*.default');
COMMIT;
