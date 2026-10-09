BEGIN;
-- Keep the imported metadata, but expose only the listing requested by the owner.
-- discoverable gates both the Sources directory and the planner catalog.
UPDATE providers.apis a
SET discoverable = (a.slug = 'apimarket-magicapi-crunchbase-bbcca0a6')
FROM providers.provider_profiles p
WHERE a.owner_id=p.id AND p.username='api-market'
  AND a.listing_metadata #>> '{apimarket,main_category}' = 'business-apis';

UPDATE providers.apis a
SET name='Crunchbase',
    listing_metadata=jsonb_set(
      jsonb_set(a.listing_metadata, '{logo}', '"https://www.crunchbase.com/favicon.ico"'::jsonb),
      '{apimarket,original_title}',
      coalesce(a.listing_metadata #> '{apimarket,original_title}', to_jsonb(a.name)))
FROM providers.provider_profiles p
WHERE a.owner_id=p.id AND p.username='api-market'
  AND a.slug='apimarket-magicapi-crunchbase-bbcca0a6'
  AND a.listing_metadata #>> '{apimarket,id}'='magicapi/crunchbase';

-- The requested key was supplied in chat and saved directly to Vault.
-- Remove the abandoned admin-form RPCs; the encrypted secret is untouched.
DROP FUNCTION IF EXISTS public.admin_save_apimarket_key(text);
DROP FUNCTION IF EXISTS public.admin_apimarket_key_status();
DROP FUNCTION IF EXISTS apiosk_private.admin_apimarket_credential(boolean,text);
REVOKE USAGE ON SCHEMA apiosk_private FROM authenticated;
NOTIFY pgrst, 'reload schema';
COMMIT;
