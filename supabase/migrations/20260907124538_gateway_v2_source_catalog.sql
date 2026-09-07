-- Published catalog metadata only; existing RLS and server-only role remain.
-- The HTTP response projects descriptions, category, tags and sectors explicitly.
GRANT SELECT(description,category,listing_metadata) ON public.apis TO gateway_v2_runtime;
