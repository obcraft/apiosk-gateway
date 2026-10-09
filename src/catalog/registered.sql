-- Runtime SELECT fragment, NOT a migration. Embedded by execution_catalog_sql().
-- The gateway binds $1 as uuid[] containing selected service/endpoint IDs.
-- For an SQL-editor check, supply that parameter with PREPARE/EXECUTE.
-- Re-read inside the reservation transaction; generated IDs are endpoint IDs.
SELECT coalesce(jsonb_agg(jsonb_build_object(
 'slug',a.slug,'name',a.name,'block_reason',a.listing_metadata->>'planner_block_reason',
 'metadata',jsonb_build_object('endpoints',coalesce((
   SELECT jsonb_agg(jsonb_build_object(
     'endpoint_id',e.id,'description',left(e.description,800),'method',coalesce(e.method,''),'path',coalesce(e.path,''),
     'source_url',coalesce(a.docs_url,a.website_url),
     'registration',jsonb_build_object('list_atomic',ceil(e.price*1000000)::bigint,
       'query_params',e.query_params,'request_body',e.request_body,
       'listing_type',coalesce(a.listing_type::text,'api'),'usage_notice',a.listing_metadata->'usage_notice')
   ) ORDER BY e.id) FROM providers.api_endpoints e
   WHERE e.api_id=a.id AND e.id=ANY($1) AND e.active AND nullif(trim(e.description),'') IS NOT NULL
 ),'[]'))
) ORDER BY a.slug),'[]') FROM providers.apis a
WHERE a.status='active' AND coalesce(a.active,true) AND a.is_verified=true
AND coalesce(a.discoverable,true) AND a.suspended_at IS NULL
