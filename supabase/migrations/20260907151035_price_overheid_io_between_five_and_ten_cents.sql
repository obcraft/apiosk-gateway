-- Keep every visible overheid.io buyer price between five and ten USD cents.
-- Apiosk adds 15% to the provider list price:
--   $0.050000 -> $0.057500 buyer total
--   $0.075000 -> $0.086250 buyer total

begin;

update public.api_endpoints
set price = case
  when path in (
    '/v3/openkvk',
    '/v3/suggest/openkvk/{query}',
    '/voertuiggegevens',
    '/bag'
  ) then 0.050000
  else 0.075000
end,
metadata_at = now()
where api_id = '3ed22ff1-5dc3-42ee-909b-2771cd9361e5'
  and path in (
    '/v3/openkvk',
    '/v3/openkvk/{id}',
    '/v3/suggest/openkvk/{query}',
    '/v3/geo/openkvk/radius/{lat}/{lon}/{radius}',
    '/v3/geo/openkvk/around/{id}/{radius}',
    '/v3/geo/openkvk/box/{tlLat}/{tlLon}/{brLat}/{brLon}',
    '/voertuiggegevens',
    '/voertuiggegevens/{kenteken}',
    '/bag',
    '/bag/{bag_id}'
  );

do $verify$
declare
  endpoint_total integer;
  search_total integer;
  detail_total integer;
begin
  select
    count(*),
    count(*) filter (where price = 0.050000),
    count(*) filter (where price = 0.075000)
  into endpoint_total, search_total, detail_total
  from public.api_endpoints
  where api_id = '3ed22ff1-5dc3-42ee-909b-2771cd9361e5'
    and active = true;

  if endpoint_total <> 10 or search_total <> 4 or detail_total <> 6 then
    raise exception 'overheid.io pricing incomplete: endpoints=%, search=%, detail=%',
      endpoint_total, search_total, detail_total;
  end if;

  if exists (
    select 1
    from public.api_endpoints
    where api_id = '3ed22ff1-5dc3-42ee-909b-2771cd9361e5'
      and (ceil(price * 1000000 * 1.15) / 1000000) not between 0.05 and 0.10
  ) then
    raise exception 'an overheid.io buyer price is outside the requested five-to-ten-cent range';
  end if;
end
$verify$;

commit;
