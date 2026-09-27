WITH matched AS (
 SELECT a.slug, a.name, e.id, e.method, e.path, e.description,
        e.endpoint_category, e.tags, e.use_cases,
        ARRAY(SELECT DISTINCT c.capability_slug FROM catalog.capability_execution_contracts c
          JOIN catalog.capabilities cap ON cap.slug=c.capability_slug AND cap.status='active'
          WHERE c.endpoint_id=e.id AND (c.status='active' OR (c.status='validated' AND c.adapter_version='source-document-v1'))
          ORDER BY c.capability_slug) AS capabilities
 FROM providers.apis a
 LEFT JOIN providers.provider_profiles p ON p.id=a.owner_id
 JOIN providers.api_endpoints e ON e.api_id=a.id AND e.active=true
 WHERE a.status='active' AND coalesce(a.active,true) AND a.is_verified=true
   AND coalesce(a.discoverable,true) AND a.suspended_at IS NULL
   AND (($1 = ANY($7::text[]) AND p.username = $1) OR (NOT ($1 = ANY($7::text[])) AND a.slug = $1))
   AND ($6='' OR a.slug=$6)
   AND ($2='' OR concat_ws(' ',a.name,a.slug,e.method,e.path,e.description,e.endpoint_category,
        array_to_string(e.tags,' '),array_to_string(e.use_cases,' ')) ILIKE '%' || $2 || '%')
), filtered AS (
 SELECT * FROM matched WHERE cardinality($5::text[])=0 OR capabilities && $5::text[]
), page AS (
 SELECT * FROM filtered ORDER BY lower(name),path,id LIMIT $3 OFFSET $4
)
SELECT jsonb_build_object(
 'total',(SELECT count(*) FROM filtered),
 'items',coalesce((SELECT jsonb_agg(jsonb_build_object(
   'slug',slug,'listing',name,'endpoint',jsonb_build_object(
     'candidate_id',id,'method',method,'path',path,'description',description,
     'category',endpoint_category,'capabilities',capabilities,'tags',coalesce(tags,'{}'::text[]),
     'use_cases',coalesce(use_cases,'{}'::text[]))) ORDER BY lower(name),path,id)
   FROM page),'[]'::jsonb))
