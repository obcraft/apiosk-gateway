-- Complete published endpoint classification from the source's own metadata.
-- This does not change execution status or claim that an endpoint is buyable.
BEGIN;

WITH classified AS (
  SELECT e.id,
    coalesce(nullif(trim(e.endpoint_category), ''), nullif(trim(a.category), '')) AS category,
    CASE WHEN coalesce(cardinality(e.tags), 0) > 0 THEN e.tags ELSE
      coalesce((
        SELECT array_agg(tag ORDER BY tag) FROM (
          SELECT DISTINCT left(trim(value), 80) AS tag
          FROM jsonb_array_elements_text(CASE WHEN jsonb_typeof(a.listing_metadata->'tags')='array'
            THEN a.listing_metadata->'tags' ELSE '[]'::jsonb END) AS value
          WHERE nullif(trim(value), '') IS NOT NULL
        ) labels
      ), ARRAY[nullif(trim(a.category), '')])
    END AS tags,
    CASE WHEN coalesce(cardinality(e.use_cases), 0) > 0 THEN e.use_cases ELSE
      ARRAY[left(coalesce(nullif(trim(e.task_summary), ''), nullif(trim(e.description), '')), 160)]
    END AS use_cases
  FROM providers.api_endpoints e
  JOIN providers.apis a ON a.id=e.api_id
  WHERE a.status='active' AND coalesce(a.active,true) AND a.is_verified=true
    AND coalesce(a.discoverable,true) AND a.suspended_at IS NULL
)
UPDATE providers.api_endpoints e SET
  endpoint_category=classified.category,
  tags=classified.tags,
  use_cases=classified.use_cases
FROM classified
WHERE e.id=classified.id AND (
  e.endpoint_category IS DISTINCT FROM classified.category OR
  e.tags IS DISTINCT FROM classified.tags OR
  e.use_cases IS DISTINCT FROM classified.use_cases
);

COMMIT;
