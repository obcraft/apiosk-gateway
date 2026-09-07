-- Source-level terms are what Gateway v2's free source browser searches.
-- Keep both Dutch and common English spellings so the three service families
-- are discoverable without callers knowing overheid.io's product vocabulary.

begin;

update public.apis
set listing_metadata = jsonb_set(
  coalesce(listing_metadata, '{}'::jsonb),
  '{tags}',
  '["overheid.io","dutch government data","public data","open data","openkvk","kvk","handelsregister","company registry","company data","business register","business search","bedrijf","bedrijfsgegevens","dutch companies","rdw","kenteken","licence plate","license plate","car registration","vehicle registration","vehicle data","voertuig","auto","car","bag","address","adres","postcode","postal code","building","pand","netherlands","nederland","geojson","geospatial"]'::jsonb,
  true
)
where id = '3ed22ff1-5dc3-42ee-909b-2771cd9361e5'
  and slug = 'overheid-io';

do $guard$
begin
  if not exists (
    select 1 from public.apis
    where id = '3ed22ff1-5dc3-42ee-909b-2771cd9361e5'
      and slug = 'overheid-io'
  ) then
    raise exception 'overheid.io source is missing';
  end if;
end
$guard$;

commit;
