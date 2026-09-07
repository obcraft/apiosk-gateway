-- Onboard the requested overheid.io datasets as one managed source.
--
-- The credential is deliberately NOT part of this migration. Run
-- `node scripts/provision-overheid-io.mjs` after applying it; that script
-- probes every route with the local secret before it makes the source visible.

begin;

do $migration$
declare
  source_id constant uuid := '3ed22ff1-5dc3-42ee-909b-2771cd9361e5';
  platform_owner constant uuid := 'bf085fe8-41c4-4699-978f-e75e89518bf7';
  public_metadata jsonb := jsonb_build_object(
    'logo', 'https://overheid.io/apple-touch-icon.png',
    'billing', jsonb_build_object('mode', 'apiosk_balance'),
    'base_url', 'https://api.overheid.io',
    'auth_header', 'ovio-api-key',
    'operator_notice', 'Operated by overheid.io; this is not a Dutch government agency.',
    'tags', jsonb_build_array(
      'overheid.io', 'openkvk', 'kvk', 'company registry', 'dutch companies',
      'rdw', 'kenteken', 'vehicle data', 'bag', 'address', 'postcode',
      'netherlands', 'nederland', 'geojson'
    ),
    'sectors', jsonb_build_array('Government and public data', 'Company data', 'Mobility', 'Address data'),
    'service_groups', jsonb_build_array(
      jsonb_build_object(
        'name', 'Company Data',
        'description', 'OpenKvK V3 company search, company detail, autocomplete and GeoJSON proximity searches.',
        'endpoint_paths', jsonb_build_array(
          '/v3/openkvk',
          '/v3/openkvk/{id}',
          '/v3/suggest/openkvk/{query}',
          '/v3/geo/openkvk/radius/{lat}/{lon}/{radius}',
          '/v3/geo/openkvk/around/{id}/{radius}',
          '/v3/geo/openkvk/box/{tlLat}/{tlLon}/{brLat}/{brLon}'
        )
      ),
      jsonb_build_object(
        'name', 'Car License Plate API',
        'description', 'RDW vehicle search and full Dutch registration detail.',
        'endpoint_paths', jsonb_build_array('/voertuiggegevens', '/voertuiggegevens/{kenteken}')
      ),
      jsonb_build_object(
        'name', 'Address & Postal Codes API',
        'description', 'BAG address/postcode search and full address and building detail.',
        'endpoint_paths', jsonb_build_array('/bag', '/bag/{bag_id}')
      )
    )
  );
begin
  if not exists (select 1 from public.provider_profiles where id = platform_owner) then
    raise exception 'Apiosk platform owner % is missing', platform_owner;
  end if;

  if exists (select 1 from public.apis where slug = 'overheid-io' and id <> source_id) then
    raise exception 'The slug overheid-io already belongs to another source id';
  end if;

  insert into public.apis (
    id, owner_id, slug, name, status, description, long_description,
    category, docs_url, website_url, origin_url, endpoint_url,
    authentication_info, is_verified, active, discoverable, listing_type,
    listing_metadata, integration_mode, settlement_mode, is_external,
    is_platform_owned, is_community, source, wallet_address, payout_network,
    requires_payout_wallet
  ) values (
    source_id,
    platform_owner,
    'overheid-io',
    'Overheid.io — Dutch Public Data APIs',
    'active',
    'Search Dutch company, vehicle-registration and BAG address data through one managed source.',
    'Managed access to three overheid.io datasets: OpenKvK V3 company records, RDW vehicle data and BAG address/postcode data. The datasets originate from Dutch public registers; the API service itself is operated by overheid.io and is not a Dutch government agency.',
    'data',
    'https://overheid.io/documentatie/',
    'https://overheid.io/',
    'https://api.overheid.io',
    'https://api.overheid.io',
    jsonb_build_object(
      'type', 'apiKey',
      'caller', 'Pay per request through the Apiosk balance; callers do not provide an overheid.io key.',
      'upstream', jsonb_build_object(
        'header', 'ovio-api-key',
        'note', 'The private overheid.io API key is provisioned server-side and is never returned to callers.'
      )
    ),
    false,
    false,
    false,
    'api',
    public_metadata,
    'gateway',
    'provider_payout',
    false,
    true,
    true,
    'manual',
    '0x879739e5F9f83d4C9E2AE368466464fB5A743bb4',
    'base',
    false
  )
  on conflict (id) do update set
    owner_id = excluded.owner_id,
    slug = excluded.slug,
    name = excluded.name,
    description = excluded.description,
    long_description = excluded.long_description,
    category = excluded.category,
    docs_url = excluded.docs_url,
    website_url = excluded.website_url,
    origin_url = excluded.origin_url,
    endpoint_url = excluded.endpoint_url,
    authentication_info = excluded.authentication_info,
    listing_type = excluded.listing_type,
    listing_metadata = excluded.listing_metadata ||
      case
        when public.apis.listing_metadata ? 'auth_headers'
          then jsonb_build_object('auth_headers', public.apis.listing_metadata->'auth_headers')
        else '{}'::jsonb
      end,
    integration_mode = excluded.integration_mode,
    settlement_mode = excluded.settlement_mode,
    is_external = excluded.is_external,
    is_platform_owned = excluded.is_platform_owned,
    is_community = excluded.is_community,
    source = excluded.source,
    wallet_address = excluded.wallet_address,
    payout_network = excluded.payout_network,
    requires_payout_wallet = excluded.requires_payout_wallet;
end;
$migration$;

-- Extend the controlled subject taxonomy for the first vehicle facts. Geo is
-- already a subject kind in the catalogue and is reused below.
insert into public.fact_subject_kinds (slug, description)
values ('vehicle', 'Dutch RDW vehicle registration and profile facts.')
on conflict (slug) do nothing;

-- Fact vocabulary added only where the existing catalogue has no equivalent.
insert into public.fact_types
  (slug, name, description, jurisdiction, subject_kind, value_schema, status, freshness_seconds)
values
  ('company_registry.openkvk_slug', 'OpenKvK record slug', 'Stable overheid.io OpenKvK record id used by the detail and around routes.', 'NL', 'company', '{"type":"string","minLength":1,"maxLength":500}'::jsonb, 'active', 86400),
  ('vehicle.registration.nl.plate', 'Dutch vehicle registration', 'Dutch registration plate, accepted with or without display hyphens.', 'NL', 'vehicle', '{"type":"string","pattern":"^[A-Za-z0-9-]{6,10}$"}'::jsonb, 'active', 86400),
  ('vehicle.make', 'Vehicle make', 'Vehicle manufacturer or make.', 'NL', 'vehicle', '{"type":"string","minLength":1,"maxLength":200}'::jsonb, 'active', 86400),
  ('vehicle.model', 'Vehicle trade name', 'RDW handelsbenaming or model/trade name.', 'NL', 'vehicle', '{"type":"string","minLength":1,"maxLength":300}'::jsonb, 'active', 86400),
  ('vehicle.type', 'Vehicle type', 'RDW vehicle category, for example Personenauto.', 'NL', 'vehicle', '{"type":"string","minLength":1,"maxLength":200}'::jsonb, 'active', 86400),
  ('address.bag_slug', 'BAG address slug', 'overheid.io BAG record id used by the address detail route.', 'NL', 'address', '{"type":"string","minLength":1,"maxLength":500}'::jsonb, 'active', 86400),
  ('geo.location', 'WGS84 location', 'Latitude and longitude in WGS84 coordinates.', null, 'geo', '{"type":"object","required":["lat","lon"],"properties":{"lat":{"type":"number","minimum":-90,"maximum":90},"lon":{"type":"number","minimum":-180,"maximum":180}},"additionalProperties":false}'::jsonb, 'active', 86400),
  ('geo.latitude', 'Latitude', 'WGS84 latitude.', null, 'geo', '{"type":"number","minimum":-90,"maximum":90}'::jsonb, 'active', null),
  ('geo.longitude', 'Longitude', 'WGS84 longitude.', null, 'geo', '{"type":"number","minimum":-180,"maximum":180}'::jsonb, 'active', null),
  ('geo.radius_meters', 'Radius in metres', 'Positive search radius in metres.', null, 'geo', '{"type":"number","minimum":1,"maximum":100000}'::jsonb, 'active', null),
  ('geo.top_left.latitude', 'Bounding-box top-left latitude', 'Top-left WGS84 latitude.', null, 'geo', '{"type":"number","minimum":-90,"maximum":90}'::jsonb, 'active', null),
  ('geo.top_left.longitude', 'Bounding-box top-left longitude', 'Top-left WGS84 longitude.', null, 'geo', '{"type":"number","minimum":-180,"maximum":180}'::jsonb, 'active', null),
  ('geo.bottom_right.latitude', 'Bounding-box bottom-right latitude', 'Bottom-right WGS84 latitude.', null, 'geo', '{"type":"number","minimum":-90,"maximum":90}'::jsonb, 'active', null),
  ('geo.bottom_right.longitude', 'Bounding-box bottom-right longitude', 'Bottom-right WGS84 longitude.', null, 'geo', '{"type":"number","minimum":-180,"maximum":180}'::jsonb, 'active', null),
  ('geo.feature_collection', 'GeoJSON FeatureCollection', 'A GeoJSON FeatureCollection returned by a geographic search.', null, 'geo', '{"type":"object"}'::jsonb, 'active', null)
on conflict (slug) do update set
  name = excluded.name,
  description = excluded.description,
  jurisdiction = excluded.jurisdiction,
  subject_kind = excluded.subject_kind,
  value_schema = excluded.value_schema,
  status = excluded.status,
  freshness_seconds = excluded.freshness_seconds,
  updated_at = now();

-- Every path requested by the product owner is a real executable endpoint row.
insert into public.api_endpoints (
  id, api_id, method, path, price, active, description, query_params,
  response_body, request_example, response_example, tags, sectors, use_cases,
  endpoint_category, action_type, payment_required, external_docs_url,
  documentation_markdown, metadata_source, metadata_confidence
)
values
(
  '7e58d830-1e94-41ff-9809-66552e9bc431', '3ed22ff1-5dc3-42ee-909b-2771cd9361e5', 'GET', '/v3/openkvk', 0.018903, true,
  'Search and filter OpenKvK V3 company records. Returns a paginated JSON HAL list that is refreshed daily for new registrations and dissolutions.',
  '{"type":"object","properties":{"size":{"type":"integer","required":false,"description":"Results per page; default 100.","x-parameter-location":"query"},"page":{"type":"integer","required":false,"description":"Page number; default 1.","x-parameter-location":"query"},"query":{"type":"string","required":false,"description":"Company name, KvK number or wildcard search value.","x-parameter-location":"query"},"queryfields":{"type":"string","required":false,"description":"Fields searched by query, for example huidigeHandelsNamen or kvknummer.","x-parameter-location":"query"},"filters[bezoeklocatie.postcode]":{"type":"string","required":false,"description":"Exact Dutch postcode filter.","x-parameter-location":"query"},"fields[]":{"type":"string","required":false,"description":"Additional result field; may be repeated by raw callers.","x-parameter-location":"query"},"exists":{"type":"string","required":false,"description":"Require this field to exist.","x-parameter-location":"query"}}}'::jsonb,
  '{"type":"object","properties":{"totalItemCount":{"type":"integer"},"pageCount":{"type":"integer"},"size":{"type":"integer"},"_embedded":{"type":"object"}}}'::jsonb,
  '{"query":"Mollie","size":1}'::jsonb,
  '{"totalItemCount":58,"pageCount":58,"size":1,"_embedded":{"bedrijf":[{"kvknummer":"66076986","slug":"hoofdvestiging-gastouder-mollie-66076986-34733841","naam":"Gastouder Mollie"}]}}'::jsonb,
  array['openkvk','kvk','company search','business register','netherlands'], array['Company data'], array['Find a Dutch company by name or KvK number','Filter companies by postcode'],
  'company_data', 'search', true, 'https://overheid.io/documentatie/v3/openkvk',
  'GET /v3/openkvk — paginated OpenKvK V3 company search.', 'provider_documentation_and_live_probe', 1
),
(
  'e7b4647f-f2aa-481e-b613-9ffbf4cad6fd', '3ed22ff1-5dc3-42ee-909b-2771cd9361e5', 'GET', '/v3/openkvk/{id}', 0.066162, true,
  'Retrieve all available details for one OpenKvK company record by its slug, including names, registration, address, activities, legal form and location.',
  '{"type":"object","required":["id"],"properties":{"id":{"type":"string","required":true,"description":"OpenKvK slug returned by search or suggest.","x-parameter-location":"path"}}}'::jsonb,
  '{"type":"object","properties":{"kvknummer":{"type":"string"},"slug":{"type":"string"},"naam":{"type":"string"},"bezoeklocatie":{"type":"object"},"locatie":{"type":"object"},"sbi":{"type":"array"}}}'::jsonb,
  '{"id":"hoofdvestiging-gastouder-mollie-66076986-34733841"}'::jsonb,
  '{"kvknummer":"66076986","slug":"hoofdvestiging-gastouder-mollie-66076986-34733841","naam":"Gastouder Mollie","huidigeHandelsNamen":["Gastouder Mollie"],"sbi":["88911"],"locatie":{"lat":52.53592573,"lon":4.71146014}}'::jsonb,
  array['openkvk','kvk','company detail'], array['Company data'], array['Retrieve a complete Dutch company profile'],
  'company_data', 'lookup', true, 'https://overheid.io/documentatie/v3/openkvk',
  'GET /v3/openkvk/{id} — complete OpenKvK V3 record by slug.', 'provider_documentation_and_live_probe', 1
),
(
  '87098250-cb71-44af-9b12-cd4c5f68f1cb', '3ed22ff1-5dc3-42ee-909b-2771cd9361e5', 'GET', '/v3/suggest/openkvk/{query}', 0.018903, true,
  'Return up to 25 OpenKvK autocomplete suggestions for a partial company name or identifier.',
  '{"type":"object","required":["query"],"properties":{"query":{"type":"string","required":true,"description":"Partial company name or identifier.","x-parameter-location":"path"},"size":{"type":"integer","required":false,"description":"Suggestion count, maximum 25.","x-parameter-location":"query"}}}'::jsonb,
  '{"type":"array","items":{"type":"object"}}'::jsonb,
  '{"query":"Mollie","size":10}'::jsonb,
  '[{"naam":"Mollie B.V.","kvknummer":30204462,"link":"/v3/openkvk/besloten-vennootschap-mollie-bv-30204462"}]'::jsonb,
  array['openkvk','autocomplete','suggest'], array['Company data'], array['Autocomplete a Dutch company name'],
  'company_data', 'search', true, 'https://overheid.io/documentatie/v3/openkvk',
  'GET /v3/suggest/openkvk/{query} — OpenKvK autocomplete, maximum 25 suggestions.', 'provider_documentation_and_live_probe', 1
),
(
  '8f0e4c64-319b-42ee-9e28-26139aa50592', '3ed22ff1-5dc3-42ee-909b-2771cd9361e5', 'GET', '/v3/geo/openkvk/radius/{lat}/{lon}/{radius}', 0.066162, true,
  'Find OpenKvK companies within a radius in metres around a WGS84 coordinate. Returns GeoJSON.',
  '{"type":"object","required":["lat","lon","radius"],"properties":{"lat":{"type":"number","required":true,"description":"WGS84 latitude.","x-parameter-location":"path"},"lon":{"type":"number","required":true,"description":"WGS84 longitude.","x-parameter-location":"path"},"radius":{"type":"number","required":true,"description":"Radius in metres.","x-parameter-location":"path"},"size":{"type":"integer","required":false,"description":"Results per page.","x-parameter-location":"query"},"page":{"type":"integer","required":false,"description":"Page number.","x-parameter-location":"query"}}}'::jsonb,
  '{"type":"object","properties":{"type":{"type":"string"},"features":{"type":"array"}}}'::jsonb,
  '{"lat":52.3676,"lon":4.9041,"radius":1000,"size":1}'::jsonb,
  '{"type":"FeatureCollection","features":[]}'::jsonb,
  array['openkvk','geojson','radius'], array['Company data','Geospatial data'], array['Find companies around a coordinate'],
  'company_data', 'search', true, 'https://overheid.io/documentatie/v3/openkvk',
  'GET /v3/geo/openkvk/radius/{lat}/{lon}/{radius} — GeoJSON radius search.', 'provider_documentation_and_live_probe', 1
),
(
  '50800285-7fd1-417b-aeb7-b79790ae514a', '3ed22ff1-5dc3-42ee-909b-2771cd9361e5', 'GET', '/v3/geo/openkvk/around/{id}/{radius}', 0.066162, true,
  'Find OpenKvK companies around an existing company record slug within a radius in metres. Returns GeoJSON.',
  '{"type":"object","required":["id","radius"],"properties":{"id":{"type":"string","required":true,"description":"Existing OpenKvK record slug.","x-parameter-location":"path"},"radius":{"type":"number","required":true,"description":"Radius in metres.","x-parameter-location":"path"},"size":{"type":"integer","required":false,"description":"Results per page.","x-parameter-location":"query"},"page":{"type":"integer","required":false,"description":"Page number.","x-parameter-location":"query"}}}'::jsonb,
  '{"type":"object","properties":{"type":{"type":"string"},"features":{"type":"array"}}}'::jsonb,
  '{"id":"hoofdvestiging-gastouder-mollie-66076986-34733841","radius":1000,"size":1}'::jsonb,
  '{"type":"FeatureCollection","features":[]}'::jsonb,
  array['openkvk','geojson','around'], array['Company data','Geospatial data'], array['Find companies around an existing company'],
  'company_data', 'search', true, 'https://overheid.io/documentatie/v3/openkvk',
  'GET /v3/geo/openkvk/around/{id}/{radius} — GeoJSON proximity search around a company slug.', 'provider_documentation_and_live_probe', 1
),
(
  '5f12af9d-afa7-4be2-b2ad-772eb32ede47', '3ed22ff1-5dc3-42ee-909b-2771cd9361e5', 'GET', '/v3/geo/openkvk/box/{tlLat}/{tlLon}/{brLat}/{brLon}', 0.066162, true,
  'Find OpenKvK companies inside a WGS84 top-left/bottom-right bounding box. Returns GeoJSON.',
  '{"type":"object","required":["tlLat","tlLon","brLat","brLon"],"properties":{"tlLat":{"type":"number","required":true,"description":"Top-left latitude.","x-parameter-location":"path"},"tlLon":{"type":"number","required":true,"description":"Top-left longitude.","x-parameter-location":"path"},"brLat":{"type":"number","required":true,"description":"Bottom-right latitude.","x-parameter-location":"path"},"brLon":{"type":"number","required":true,"description":"Bottom-right longitude.","x-parameter-location":"path"},"size":{"type":"integer","required":false,"description":"Results per page.","x-parameter-location":"query"},"page":{"type":"integer","required":false,"description":"Page number.","x-parameter-location":"query"}}}'::jsonb,
  '{"type":"object","properties":{"type":{"type":"string"},"features":{"type":"array"}}}'::jsonb,
  '{"tlLat":52.38,"tlLon":4.88,"brLat":52.35,"brLon":4.93,"size":1}'::jsonb,
  '{"type":"FeatureCollection","features":[]}'::jsonb,
  array['openkvk','geojson','bounding box'], array['Company data','Geospatial data'], array['Find companies inside a map rectangle'],
  'company_data', 'search', true, 'https://overheid.io/documentatie/v3/openkvk',
  'GET /v3/geo/openkvk/box/{tlLat}/{tlLon}/{brLat}/{brLon} — GeoJSON bounding-box search.', 'provider_documentation_and_live_probe', 1
),
(
  '67bf3050-b73c-4a44-af60-e4382a94eb2b', '3ed22ff1-5dc3-42ee-909b-2771cd9361e5', 'GET', '/voertuiggegevens', 0.018903, true,
  'Search and filter Dutch RDW vehicle registrations by plate, make and other vehicle fields, with sorting and pagination.',
  '{"type":"object","properties":{"size":{"type":"integer","required":false,"description":"Results per page; default 100.","x-parameter-location":"query"},"page":{"type":"integer","required":false,"description":"Page number; default 1.","x-parameter-location":"query"},"query":{"type":"string","required":false,"description":"Plate, make or wildcard search value.","x-parameter-location":"query"},"queryfields":{"type":"string","required":false,"description":"Field searched by query, for example kenteken or merk.","x-parameter-location":"query"},"filters[merk]":{"type":"string","required":false,"description":"Exact vehicle make filter.","x-parameter-location":"query"},"filters[kenteken]":{"type":"string","required":false,"description":"Exact registration filter.","x-parameter-location":"query"},"sort":{"type":"string","required":false,"description":"Field to sort by.","x-parameter-location":"query"},"order":{"type":"string","required":false,"description":"Sort order, asc or desc.","x-parameter-location":"query"},"fields[]":{"type":"string","required":false,"description":"Additional result field; may be repeated by raw callers.","x-parameter-location":"query"}}}'::jsonb,
  '{"type":"object","properties":{"totalItemCount":{"type":"integer"},"pageCount":{"type":"integer"},"size":{"type":"integer"},"_embedded":{"type":"object"}}}'::jsonb,
  '{"query":"RZ874R","size":1}'::jsonb,
  '{"totalItemCount":1,"pageCount":1,"size":1,"_embedded":{"kenteken":[{"voertuigsoort":"Personenauto","handelsbenaming":"POLO","merk":"VOLKSWAGEN","kentekenplaat":"RZ-874-R"}]}}'::jsonb,
  array['rdw','kenteken','vehicle search'], array['Mobility'], array['Find a vehicle by Dutch registration','Filter vehicles by make'],
  'vehicle_data', 'search', true, 'https://overheid.io/documentatie/voertuiggegevens',
  'GET /voertuiggegevens — paginated RDW vehicle-data search.', 'provider_documentation_and_live_probe', 1
),
(
  '4265429e-0332-4397-a6a7-17147b6c9aa0', '3ed22ff1-5dc3-42ee-909b-2771cd9361e5', 'GET', '/voertuiggegevens/{kenteken}', 0.066162, true,
  'Retrieve complete RDW details for one Dutch registration without hyphens, including fuel, axles, body, APK and BPM data.',
  '{"type":"object","required":["kenteken"],"properties":{"kenteken":{"type":"string","pattern":"^[A-Za-z0-9]{6}$","required":true,"description":"Dutch registration without hyphens, for example RZ874R.","x-parameter-location":"path"}}}'::jsonb,
  '{"type":"object","properties":{"kenteken":{"type":"string"},"kentekenplaat":{"type":"string"},"merk":{"type":"string"},"handelsbenaming":{"type":"string"},"voertuigsoort":{"type":"string"},"brandstof":{"type":"array"},"as":{"type":"array"},"carrosserie":{"type":"array"},"vervaldatum_apk":{"type":"string"},"bruto_bpm":{"type":"number"}}}'::jsonb,
  '{"kenteken":"RZ874R"}'::jsonb,
  '{"kenteken":"RZ874R","kentekenplaat":"RZ-874-R","merk":"VOLKSWAGEN","handelsbenaming":"POLO","voertuigsoort":"Personenauto","vervaldatum_apk":"2026-10-29","bruto_bpm":3328,"brandstof":[{"brandstof_omschrijving":"Benzine"}],"as":[{"as_nummer":1}],"carrosserie":[{"type_carrosserie_europese_omschrijving":"Hatchback"}]}'::jsonb,
  array['rdw','kenteken','vehicle detail','apk','bpm'], array['Mobility'], array['Retrieve complete Dutch vehicle details'],
  'vehicle_data', 'lookup', true, 'https://overheid.io/documentatie/voertuiggegevens',
  'GET /voertuiggegevens/{kenteken} — complete registration detail; use a six-character plate without hyphens.', 'provider_documentation_and_live_probe', 1
),
(
  '5f80af63-c24e-46b0-aa0a-d31e1383c87a', '3ed22ff1-5dc3-42ee-909b-2771cd9361e5', 'GET', '/bag', 0.018903, true,
  'Search BAG addresses by postcode, house number or public-space name, with sorting and pagination.',
  '{"type":"object","properties":{"size":{"type":"integer","required":false,"description":"Results per page; default 100.","x-parameter-location":"query"},"page":{"type":"integer","required":false,"description":"Page number; default 1.","x-parameter-location":"query"},"query":{"type":"string","required":false,"description":"Address, street or postcode search value; wildcards are supported.","x-parameter-location":"query"},"queryfields":{"type":"string","required":false,"description":"Field searched by query, for example openbareruimtenaam or postcode.","x-parameter-location":"query"},"filters[postcode]":{"type":"string","required":false,"description":"Exact Dutch postcode filter.","x-parameter-location":"query"},"filters[huisnummer]":{"type":"string","required":false,"description":"Exact house-number filter.","x-parameter-location":"query"},"filters[openbareruimtenaam]":{"type":"string","required":false,"description":"Exact public-space or street-name filter.","x-parameter-location":"query"},"sort":{"type":"string","required":false,"description":"Field to sort by.","x-parameter-location":"query"},"order":{"type":"string","required":false,"description":"Sort order, asc or desc.","x-parameter-location":"query"},"fields[]":{"type":"string","required":false,"description":"Additional result field; may be repeated by raw callers.","x-parameter-location":"query"}}}'::jsonb,
  '{"type":"object","properties":{"totalItemCount":{"type":"integer"},"pageCount":{"type":"integer"},"size":{"type":"integer"},"_embedded":{"type":"object"}}}'::jsonb,
  '{"filters[postcode]":"3015BA","size":1}'::jsonb,
  '{"totalItemCount":110,"pageCount":110,"size":1,"_embedded":{"adres":[{"huisnummer":"10","huisletter":"C","postcode":"3015BA","openbareruimte":"Nieuwe Binnenweg","slug":"3015ba-nieuwe-binnenweg-10-c","woonplaats":"Rotterdam"}]}}'::jsonb,
  array['bag','address','postcode','building'], array['Address data'], array['Find an address by postcode and house number','Search Dutch streets'],
  'address_data', 'search', true, 'https://overheid.io/documentatie/bag',
  'GET /bag — paginated BAG address and postcode search.', 'provider_documentation_and_live_probe', 1
),
(
  '98af8578-98bb-4761-b43d-0c411401fdc1', '3ed22ff1-5dc3-42ee-909b-2771cd9361e5', 'GET', '/bag/{bag_id}', 0.066162, true,
  'Retrieve all details for one BAG address slug, including WGS84 and RD coordinates, building id/year, area and usage purpose.',
  '{"type":"object","required":["bag_id"],"properties":{"bag_id":{"type":"string","required":true,"description":"BAG slug returned by address search.","x-parameter-location":"path"}}}'::jsonb,
  '{"type":"object","properties":{"slug":{"type":"string"},"postcode":{"type":"string"},"huisnummer":{"type":"string"},"openbareruimte":{"type":"string"},"woonplaats":{"type":"string"},"locatie":{"type":"object"},"x":{"type":"number"},"y":{"type":"number"},"pand_id":{"type":"number"},"pandbouwjaar":{"type":"integer"},"oppervlakteverblijfsobject":{"type":"number"},"verblijfsobjectgebruiksdoel":{"type":"string"}}}'::jsonb,
  '{"bag_id":"3015ba-nieuwe-binnenweg-10-c"}'::jsonb,
  '{"slug":"3015ba-nieuwe-binnenweg-10-c","postcode":"3015BA","huisnummer":"10","huisletter":"C","openbareruimte":"Nieuwe Binnenweg","woonplaats":"Rotterdam","locatie":{"lat":51.91670953,"lon":4.4721661},"x":92047.05,"y":436865.24,"pand_id":599100000755382,"pandbouwjaar":1979,"oppervlakteverblijfsobject":63,"verblijfsobjectgebruiksdoel":"woonfunctie"}'::jsonb,
  array['bag','address detail','postcode','building'], array['Address data'], array['Retrieve complete BAG address and building details'],
  'address_data', 'lookup', true, 'https://overheid.io/documentatie/bag',
  'GET /bag/{bag_id} — complete BAG address and building detail by slug.', 'provider_documentation_and_live_probe', 1
)
on conflict (id) do update set
  api_id = excluded.api_id,
  method = excluded.method,
  path = excluded.path,
  price = excluded.price,
  active = excluded.active,
  description = excluded.description,
  query_params = excluded.query_params,
  response_body = excluded.response_body,
  request_example = excluded.request_example,
  response_example = excluded.response_example,
  tags = excluded.tags,
  sectors = excluded.sectors,
  use_cases = excluded.use_cases,
  endpoint_category = excluded.endpoint_category,
  action_type = excluded.action_type,
  payment_required = excluded.payment_required,
  external_docs_url = excluded.external_docs_url,
  documentation_markdown = excluded.documentation_markdown,
  metadata_source = excluded.metadata_source,
  metadata_confidence = excluded.metadata_confidence,
  metadata_at = now();

-- Grounded Gateway v2 contracts. Every contract is validated against the
-- endpoint-specific sample stored above; live upstream probes are a separate
-- activation gate in provision-overheid-io.mjs.
insert into public.capability_execution_contracts (
  id, capability_slug, endpoint_id, version, status, requires, produces,
  input_map, fixed_params, output_map, cardinality, preconditions,
  freshness_seconds, max_fan_out, pagination, effect, price_model,
  adapter_version, sample_payload, validated_at, validated_note, notes,
  identifies
)
values
(
  '81748e89-9e52-42fc-95db-44fea0f48ea9', 'company_registry.search', '7e58d830-1e94-41ff-9809-66552e9bc431', 1, 'active',
  '[{"fact_type":"company.name","subject_role":"subject","optional":false}]'::jsonb, '[]'::jsonb,
  '{"company.name":{"in":"query","name":"query"}}'::jsonb, '{}'::jsonb,
  '{"base":"$._embedded.bedrijf[*]","produces":{"company.name":{"path":"$.naam"},"company_registry.kvknummer":{"path":"$.kvknummer"},"company_registry.openkvk_slug":{"path":"$.slug"}}}'::jsonb,
  'many', '{}'::jsonb, 86400, 1, '{"kind":"hal","next":"$._links.next.href"}'::jsonb, 'read', 'per_call', 'v1',
  '{"totalItemCount":58,"pageCount":58,"size":1,"_embedded":{"bedrijf":[{"kvknummer":"66076986","slug":"hoofdvestiging-gastouder-mollie-66076986-34733841","naam":"Gastouder Mollie"}]}}'::jsonb,
  now(), 'Contract extraction matches a live overheid.io response sampled on 2026-09-07.', 'OpenKvK name search bound to query.', 'company_registry.kvknummer'
),
(
  'c7e42d13-b3cd-4230-b6db-fac4585cb2b9', 'company_registry.lookup', 'e7b4647f-f2aa-481e-b613-9ffbf4cad6fd', 1, 'active',
  '[{"fact_type":"company_registry.openkvk_slug","subject_role":"subject","optional":false}]'::jsonb, '[]'::jsonb,
  '{"company_registry.openkvk_slug":{"in":"path","name":"id"}}'::jsonb, '{}'::jsonb,
  '{"base":"$","produces":{"company.name":{"path":"$.naam"},"company_registry.kvknummer":{"path":"$.kvknummer"},"company_registry.openkvk_slug":{"path":"$.slug"},"company.profile":{"fields":{"legal_name":{"path":"$.naam"},"trade_names":{"path":"$.huidigeHandelsNamen[*]","collect":true},"legal_form":{"path":"$.rechtsvormOmschrijving"},"sbi_codes":{"path":"$.sbi[*]","collect":true}}}}}'::jsonb,
  'one', '{}'::jsonb, 86400, 1, '{}'::jsonb, 'read', 'per_call', 'v1',
  '{"kvknummer":"66076986","slug":"hoofdvestiging-gastouder-mollie-66076986-34733841","naam":"Gastouder Mollie","huidigeHandelsNamen":["Gastouder Mollie"],"rechtsvormOmschrijving":"Eenmanszaak","sbi":["88911"]}'::jsonb,
  now(), 'Contract extraction matches a live overheid.io response sampled on 2026-09-07.', 'OpenKvK detail bound to the exact record slug.', 'company_registry.kvknummer'
),
(
  'a5f870c8-2d30-4593-8eee-1387ba4ffb3c', 'company_registry.search', '87098250-cb71-44af-9b12-cd4c5f68f1cb', 1, 'active',
  '[{"fact_type":"company.name","subject_role":"subject","optional":false}]'::jsonb, '[]'::jsonb,
  '{"company.name":{"in":"path","name":"query"}}'::jsonb, '{}'::jsonb,
  '{"base":"$[*]","produces":{"company.name":{"path":"$.naam"},"company_registry.kvknummer":{"path":"$.kvknummer","cast":"string"}}}'::jsonb,
  'many', '{}'::jsonb, 86400, 1, '{}'::jsonb, 'read', 'per_call', 'v1',
  '[{"naam":"Mollie B.V.","kvknummer":30204462}]'::jsonb,
  now(), 'Contract extraction matches a live overheid.io response sampled on 2026-09-07.', 'OpenKvK autocomplete bound to a partial company name.', 'company_registry.kvknummer'
),
(
  '66a89fe4-7eba-4375-a4ff-6c688778d93f', 'geo_environment.search', '8f0e4c64-319b-42ee-9e28-26139aa50592', 1, 'active',
  '[{"fact_type":"geo.latitude","subject_role":"subject","optional":false},{"fact_type":"geo.longitude","subject_role":"subject","optional":false},{"fact_type":"geo.radius_meters","subject_role":"subject","optional":false}]'::jsonb, '[]'::jsonb,
  '{"geo.latitude":{"in":"path","name":"lat"},"geo.longitude":{"in":"path","name":"lon"},"geo.radius_meters":{"in":"path","name":"radius"}}'::jsonb, '{}'::jsonb,
  '{"base":"$","produces":{"geo.feature_collection":{"path":"$"}}}'::jsonb,
  'one', '{}'::jsonb, 86400, 1, '{}'::jsonb, 'read', 'per_call', 'v1',
  '{"type":"FeatureCollection","features":[]}'::jsonb,
  now(), 'Contract extraction matches the live GeoJSON response envelope on 2026-09-07.', 'OpenKvK radius GeoJSON search.', null
),
(
  '53426cf7-b8ba-4281-84b5-6b04c1dcdfb0', 'geo_environment.search', '50800285-7fd1-417b-aeb7-b79790ae514a', 1, 'active',
  '[{"fact_type":"company_registry.openkvk_slug","subject_role":"subject","optional":false},{"fact_type":"geo.radius_meters","subject_role":"subject","optional":false}]'::jsonb, '[]'::jsonb,
  '{"company_registry.openkvk_slug":{"in":"path","name":"id"},"geo.radius_meters":{"in":"path","name":"radius"}}'::jsonb, '{}'::jsonb,
  '{"base":"$","produces":{"geo.feature_collection":{"path":"$"}}}'::jsonb,
  'one', '{}'::jsonb, 86400, 1, '{}'::jsonb, 'read', 'per_call', 'v1',
  '{"type":"FeatureCollection","features":[]}'::jsonb,
  now(), 'Contract extraction matches the live GeoJSON response envelope on 2026-09-07.', 'OpenKvK proximity search around an existing record.', null
),
(
  '576fc821-16c6-4044-9801-3dafbaa05e9b', 'geo_environment.search', '5f12af9d-afa7-4be2-b2ad-772eb32ede47', 1, 'active',
  '[{"fact_type":"geo.top_left.latitude","subject_role":"subject","optional":false},{"fact_type":"geo.top_left.longitude","subject_role":"subject","optional":false},{"fact_type":"geo.bottom_right.latitude","subject_role":"subject","optional":false},{"fact_type":"geo.bottom_right.longitude","subject_role":"subject","optional":false}]'::jsonb, '[]'::jsonb,
  '{"geo.top_left.latitude":{"in":"path","name":"tlLat"},"geo.top_left.longitude":{"in":"path","name":"tlLon"},"geo.bottom_right.latitude":{"in":"path","name":"brLat"},"geo.bottom_right.longitude":{"in":"path","name":"brLon"}}'::jsonb, '{}'::jsonb,
  '{"base":"$","produces":{"geo.feature_collection":{"path":"$"}}}'::jsonb,
  'one', '{}'::jsonb, 86400, 1, '{}'::jsonb, 'read', 'per_call', 'v1',
  '{"type":"FeatureCollection","features":[]}'::jsonb,
  now(), 'Contract extraction matches the live GeoJSON response envelope on 2026-09-07.', 'OpenKvK bounding-box GeoJSON search.', null
),
(
  'e8450e16-8582-4f4d-8011-37d983a86498', 'consumer_commerce.search', '67bf3050-b73c-4a44-af60-e4382a94eb2b', 1, 'active',
  '[{"fact_type":"vehicle.registration.nl.plate","subject_role":"subject","optional":false}]'::jsonb, '[]'::jsonb,
  '{"vehicle.registration.nl.plate":{"in":"query","name":"query"}}'::jsonb, '{}'::jsonb,
  '{"base":"$._embedded.kenteken[*]","produces":{"vehicle.registration.nl.plate":{"path":"$.kentekenplaat"},"vehicle.make":{"path":"$.merk"},"vehicle.model":{"path":"$.handelsbenaming"},"vehicle.type":{"path":"$.voertuigsoort"}}}'::jsonb,
  'many', '{}'::jsonb, 86400, 1, '{"kind":"hal","next":"$._links.next.href"}'::jsonb, 'read', 'per_call', 'v1',
  '{"totalItemCount":1,"pageCount":1,"size":1,"_embedded":{"kenteken":[{"voertuigsoort":"Personenauto","handelsbenaming":"POLO","merk":"VOLKSWAGEN","kentekenplaat":"RZ-874-R"}]}}'::jsonb,
  now(), 'Contract extraction matches a live overheid.io response sampled on 2026-09-07.', 'Vehicle search bound to a Dutch registration value.', 'vehicle.registration.nl.plate'
),
(
  'ecd6f1d8-24a5-424e-8d74-d27356b2ec55', 'consumer_commerce.lookup', '4265429e-0332-4397-a6a7-17147b6c9aa0', 1, 'active',
  '[{"fact_type":"vehicle.registration.nl.plate","subject_role":"subject","optional":false}]'::jsonb, '[]'::jsonb,
  '{"vehicle.registration.nl.plate":{"in":"path","name":"kenteken"}}'::jsonb, '{}'::jsonb,
  '{"base":"$","produces":{"vehicle.registration.nl.plate":{"path":"$.kenteken"},"vehicle.make":{"path":"$.merk"},"vehicle.model":{"path":"$.handelsbenaming"},"vehicle.type":{"path":"$.voertuigsoort"}}}'::jsonb,
  'one', '{}'::jsonb, 86400, 1, '{}'::jsonb, 'read', 'per_call', 'v1',
  '{"kenteken":"RZ874R","merk":"VOLKSWAGEN","handelsbenaming":"POLO","voertuigsoort":"Personenauto"}'::jsonb,
  now(), 'Contract extraction matches a live overheid.io response sampled on 2026-09-07.', 'Vehicle detail bound to the six-character registration path.', 'vehicle.registration.nl.plate'
),
(
  '0ff9f5ad-7d22-462a-babe-29d43e7d3fd3', 'geo_environment.search', '5f80af63-c24e-46b0-aa0a-d31e1383c87a', 1, 'active',
  '[{"fact_type":"building.postcode","subject_role":"subject","optional":false}]'::jsonb, '[]'::jsonb,
  '{"building.postcode":{"in":"query","name":"filters[postcode]"}}'::jsonb, '{}'::jsonb,
  '{"base":"$._embedded.adres[*]","produces":{"address.bag_slug":{"path":"$.slug"},"building.postcode":{"path":"$.postcode"},"address.postal":{"fields":{"postcode":{"path":"$.postcode"},"street":{"path":"$.openbareruimte"},"house_number":{"path":"$.huisnummer"},"city":{"path":"$.woonplaats"}}}}}'::jsonb,
  'many', '{}'::jsonb, 86400, 1, '{"kind":"hal","next":"$._links.next.href"}'::jsonb, 'read', 'per_call', 'v1',
  '{"totalItemCount":110,"pageCount":110,"size":1,"_embedded":{"adres":[{"huisnummer":"10","postcode":"3015BA","openbareruimte":"Nieuwe Binnenweg","slug":"3015ba-nieuwe-binnenweg-10-c","woonplaats":"Rotterdam"}]}}'::jsonb,
  now(), 'Contract extraction matches a live overheid.io response sampled on 2026-09-07.', 'BAG address search bound to an exact Dutch postcode.', 'address.bag_slug'
),
(
  '15312d58-6cd6-4e7c-9df8-e480aa403d77', 'building.registry-lookup', '98af8578-98bb-4761-b43d-0c411401fdc1', 1, 'active',
  '[{"fact_type":"address.bag_slug","subject_role":"subject","optional":false}]'::jsonb, '[]'::jsonb,
  '{"address.bag_slug":{"in":"path","name":"bag_id"}}'::jsonb, '{}'::jsonb,
  '{"base":"$","produces":{"address.bag_slug":{"path":"$.slug"},"building.postcode":{"path":"$.postcode"},"address.postal":{"fields":{"postcode":{"path":"$.postcode"},"street":{"path":"$.openbareruimte"},"house_number":{"path":"$.huisnummer"},"city":{"path":"$.woonplaats"}}},"geo.location":{"fields":{"lat":{"path":"$.locatie.lat"},"lon":{"path":"$.locatie.lon"}}},"building.bagpnd":{"path":"$.pand_id","cast":"string"},"building.gebruiksdoel":{"path":"$.verblijfsobjectgebruiksdoel"}}}'::jsonb,
  'one', '{}'::jsonb, 86400, 1, '{}'::jsonb, 'read', 'per_call', 'v1',
  '{"slug":"3015ba-nieuwe-binnenweg-10-c","postcode":"3015BA","huisnummer":"10","openbareruimte":"Nieuwe Binnenweg","woonplaats":"Rotterdam","locatie":{"lat":51.91670953,"lon":4.4721661},"pand_id":599100000755382,"verblijfsobjectgebruiksdoel":"woonfunctie"}'::jsonb,
  now(), 'Contract extraction matches a live overheid.io response sampled on 2026-09-07.', 'BAG detail bound to the exact address slug.', 'address.bag_slug'
)
on conflict (id) do update set
  capability_slug = excluded.capability_slug,
  endpoint_id = excluded.endpoint_id,
  version = excluded.version,
  status = excluded.status,
  requires = excluded.requires,
  produces = excluded.produces,
  input_map = excluded.input_map,
  fixed_params = excluded.fixed_params,
  output_map = excluded.output_map,
  cardinality = excluded.cardinality,
  preconditions = excluded.preconditions,
  freshness_seconds = excluded.freshness_seconds,
  max_fan_out = excluded.max_fan_out,
  pagination = excluded.pagination,
  effect = excluded.effect,
  price_model = excluded.price_model,
  adapter_version = excluded.adapter_version,
  sample_payload = excluded.sample_payload,
  validated_at = excluded.validated_at,
  validated_note = excluded.validated_note,
  notes = excluded.notes,
  identifies = excluded.identifies,
  updated_at = now();

commit;
