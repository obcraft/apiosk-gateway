SELECT coalesce(jsonb_agg(jsonb_build_object(
 'slug',a.slug,'provider_slug',p.username,'logo_url',coalesce(nullif(a.listing_metadata->>'logo',''),nullif(p.avatar_url,'')),'name',left(a.name,160),'description',left(coalesce(a.description,''),600),
 'provider_logo_url',nullif(p.avatar_url,''),'category',coalesce(a.category,''),
 'tags',CASE WHEN jsonb_typeof(a.listing_metadata->'tags')='array' THEN a.listing_metadata->'tags' ELSE '[]'::jsonb END,
 'sectors',CASE WHEN jsonb_typeof(a.listing_metadata->'sectors')='array' THEN a.listing_metadata->'sectors' ELSE '[]'::jsonb END,
 'endpoint_count',(SELECT count(*) FROM api_endpoints e WHERE e.api_id=a.id AND e.active=true)
) ORDER BY lower(a.name),a.slug),'[]'::jsonb)
FROM apis a
LEFT JOIN provider_profiles p ON p.id=a.owner_id
WHERE a.status='active' AND coalesce(a.active,true) AND a.is_verified=true
AND coalesce(a.discoverable,true) AND a.suspended_at IS NULL;
