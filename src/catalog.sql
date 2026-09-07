SELECT jsonb_build_object(
 'vocabulary',(SELECT coalesce(jsonb_agg(jsonb_build_object('slug',slug,'value_schema',value_schema) ORDER BY slug),'[]') FROM public.fact_types WHERE status='active'),
 'services',(SELECT coalesce(jsonb_agg(s.obj ORDER BY s.id),'[]') FROM (
 SELECT c.id,jsonb_build_object(
  'id',c.id,'endpoint_id',e.id,'api_slug',a.slug,'name',a.name,'source_url',coalesce(a.docs_url,a.website_url),
  'method',e.method,'path',e.path,'listing_type',coalesce(a.listing_type::text,'api'),
  'list_atomic',ceil(e.price*1000000)::bigint,
  'fixed_params',c.fixed_params,'preconditions',c.preconditions,'pagination',c.pagination,
  'freshness_seconds',c.freshness_seconds,'sample_payload',c.sample_payload,
  'jurisdiction',ft.jurisdiction,
  'contract',jsonb_build_object('capability_slug',c.capability_slug,'version',c.version,
    'cardinality',c.cardinality,'requires',c.requires,'input_map',c.input_map,
    'output_map',c.output_map,'identifies',c.identifies,'max_fan_out',c.max_fan_out)) obj
 FROM public.capability_execution_contracts c
 JOIN public.api_endpoints e ON e.id=c.endpoint_id
 JOIN public.apis a ON a.id=e.api_id
 JOIN public.capabilities cap ON cap.slug=c.capability_slug AND cap.status='active'
 LEFT JOIN public.fact_types ft ON ft.slug=c.identifies
 WHERE c.status='active' AND c.validated_at IS NOT NULL AND c.effect='read'
 AND c.price_model='per_call' AND e.active=true AND e.price IS NOT NULL AND e.price>=0
 AND a.status='active' AND coalesce(a.active,true) AND a.is_verified=true
 AND coalesce(a.discoverable,true) AND a.suspended_at IS NULL
 ) s)
);
