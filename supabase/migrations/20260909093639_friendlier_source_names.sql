-- Keep Sources card titles short and scannable. The descriptions directly
-- below them retain the country, provenance and coverage details.

begin;

do $names$
declare
  expected integer;
  changed integer;
begin
  with desired(id, slug, old_name, new_name) as (values
    ('c5939334-ac41-40d0-80d5-ced0101a7ad6'::uuid, 'cbs-statline', 'CBS — Statistics Netherlands (StatLine open data)', 'CBS Statistics'),
    ('2c934a36-5d90-4690-8e5a-e5779d272df8'::uuid, 'companies-house', 'Companies House — UK Company Register', 'Companies House'),
    ('803bcf10-ae0f-4875-9872-b25585d9b92a'::uuid, 'data-overheid-nl', 'data.overheid.nl — Dutch National Open Data Register', 'Dutch Open Data Register'),
    ('b268fe55-41d2-5b7a-b147-31af1f1c1d0f'::uuid, 'eodhd', 'EODHD Financial APIs', 'EODHD Market Data'),
    ('a282854e-2117-4597-bbca-d27715396113'::uuid, 'kvk-dutch-business-register', 'KVK — Dutch Business Register', 'KVK Business Register'),
    ('3ed22ff1-5dc3-42ee-909b-2771cd9361e5'::uuid, 'overheid-io', 'Overheid.io — Dutch Public Data APIs', 'Overheid.io Public Data'),
    ('5f8c78db-0579-4de8-bd1f-eed9bb8b7df5'::uuid, 'pdok', 'PDOK — Dutch Geospatial & Government Data', 'PDOK Maps & Addresses')
  )
  select count(*) into expected
  from desired
  join public.apis source on source.id = desired.id and source.slug = desired.slug
  where source.name in (desired.old_name, desired.new_name)
    and source.status = 'active'
    and source.active
    and source.is_verified
    and source.discoverable;

  if expected <> 7 then
    raise exception 'friendly source names require all 7 expected visible listings; found %', expected;
  end if;

  with desired(id, slug, new_name) as (values
    ('c5939334-ac41-40d0-80d5-ced0101a7ad6'::uuid, 'cbs-statline', 'CBS Statistics'),
    ('2c934a36-5d90-4690-8e5a-e5779d272df8'::uuid, 'companies-house', 'Companies House'),
    ('803bcf10-ae0f-4875-9872-b25585d9b92a'::uuid, 'data-overheid-nl', 'Dutch Open Data Register'),
    ('b268fe55-41d2-5b7a-b147-31af1f1c1d0f'::uuid, 'eodhd', 'EODHD Market Data'),
    ('a282854e-2117-4597-bbca-d27715396113'::uuid, 'kvk-dutch-business-register', 'KVK Business Register'),
    ('3ed22ff1-5dc3-42ee-909b-2771cd9361e5'::uuid, 'overheid-io', 'Overheid.io Public Data'),
    ('5f8c78db-0579-4de8-bd1f-eed9bb8b7df5'::uuid, 'pdok', 'PDOK Maps & Addresses')
  )
  update public.apis source
  set name = desired.new_name
  from desired
  where source.id = desired.id and source.slug = desired.slug;

  get diagnostics changed = row_count;
  if changed <> 7 then
    raise exception 'friendly source name update expected 7 rows; changed %', changed;
  end if;
end;
$names$;

do $postconditions$
declare
  correct integer;
begin
  with desired(id, slug, new_name) as (values
    ('c5939334-ac41-40d0-80d5-ced0101a7ad6'::uuid, 'cbs-statline', 'CBS Statistics'),
    ('2c934a36-5d90-4690-8e5a-e5779d272df8'::uuid, 'companies-house', 'Companies House'),
    ('803bcf10-ae0f-4875-9872-b25585d9b92a'::uuid, 'data-overheid-nl', 'Dutch Open Data Register'),
    ('b268fe55-41d2-5b7a-b147-31af1f1c1d0f'::uuid, 'eodhd', 'EODHD Market Data'),
    ('a282854e-2117-4597-bbca-d27715396113'::uuid, 'kvk-dutch-business-register', 'KVK Business Register'),
    ('3ed22ff1-5dc3-42ee-909b-2771cd9361e5'::uuid, 'overheid-io', 'Overheid.io Public Data'),
    ('5f8c78db-0579-4de8-bd1f-eed9bb8b7df5'::uuid, 'pdok', 'PDOK Maps & Addresses')
  )
  select count(*) into correct
  from desired
  join public.apis source
    on source.id = desired.id
   and source.slug = desired.slug
   and source.name = desired.new_name;

  if correct <> 7 then
    raise exception 'friendly source name postcondition failed: % of 7 correct', correct;
  end if;
end;
$postconditions$;

commit;
