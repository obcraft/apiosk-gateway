-- KVK numbers identify entries in the Dutch Business Register. Preserve this
-- fact when a dependent service produces accounts without a new identity.
UPDATE public.fact_types SET jurisdiction='NL'
WHERE slug='company_registry.kvknummer' AND jurisdiction IS NULL;
