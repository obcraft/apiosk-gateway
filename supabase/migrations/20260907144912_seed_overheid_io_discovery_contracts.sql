-- Complete the machine-readable discovery surface for overheid.io. The first
-- onboarding migration made every route executable; this migration makes the
-- capability taxonomy and declared inputs/outputs equally explicit.

begin;

update public.apis
set listing_metadata = jsonb_set(
  jsonb_set(
    coalesce(listing_metadata, '{}'::jsonb),
    '{categories}',
    '["company_registry","vehicle_registry","address_registry","geospatial"]'::jsonb,
    true
  ),
  '{capability_slugs}',
  '["company_registry.search","company_registry.lookup","company_registry.radius-search","company_registry.nearby-search","company_registry.box-search","vehicle.registry-search","vehicle.registry-lookup","building.registry-search","building.registry-lookup"]'::jsonb,
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

insert into public.capabilities
  (slug, name, description, category, input_contract, output_contract, status)
values
(
  'company_registry.search',
  'Company register search (Handelsregister)',
  'Search an official business register by company name, trade name, registration number, postcode or address and resolve the registered company identifiers.',
  'company_registry',
  '{"type":"object","properties":{"query":{"type":"string","description":"Company name, trade name or registration number."},"company_name":{"type":"string"},"kvk_number":{"type":"string","pattern":"^[0-9]{8}$"},"postcode":{"type":"string","pattern":"^[0-9]{4}\\s?[A-Za-z]{2}$"}},"anyOf":[{"required":["query"]},{"required":["company_name"]},{"required":["kvk_number"]},{"required":["postcode"]}]}'::jsonb,
  '{"type":"array","items":{"type":"object","properties":{"company_name":{"type":"string"},"kvk_number":{"type":"string"},"record_id":{"type":"string"},"postcode":{"type":"string"}}}}'::jsonb,
  'active'
),
(
  'company_registry.lookup',
  'Company register lookup (Handelsregister)',
  'Retrieve the complete official registration record for one company, including names, activities, legal form, addresses and register identifiers.',
  'company_registry',
  '{"type":"object","required":["record_id"],"properties":{"record_id":{"type":"string","minLength":1},"kvk_number":{"type":"string","pattern":"^[0-9]{8}$"}}}'::jsonb,
  '{"type":"object","properties":{"company_name":{"type":"string"},"kvk_number":{"type":"string"},"branch_number":{"type":"string"},"trade_names":{"type":"array","items":{"type":"string"}},"activities":{"type":"array"},"legal_form":{"type":"string"},"active":{"type":"boolean"},"address":{"type":"object"},"location":{"type":"object"}}}'::jsonb,
  'active'
),
(
  'company_registry.radius-search',
  'Find registered companies around coordinates',
  'Find official company-register records within a radius in metres around a WGS84 latitude and longitude.',
  'company_registry',
  '{"type":"object","required":["latitude","longitude","radius_meters"],"properties":{"latitude":{"type":"number","minimum":-90,"maximum":90},"longitude":{"type":"number","minimum":-180,"maximum":180},"radius_meters":{"type":"number","minimum":1,"maximum":100000}}}'::jsonb,
  '{"type":"object","required":["type","features"],"properties":{"type":{"const":"FeatureCollection"},"features":{"type":"array","items":{"type":"object"}}}}'::jsonb,
  'active'
),
(
  'company_registry.nearby-search',
  'Find registered companies near another company',
  'Find official company-register records within a radius in metres around an existing company record.',
  'company_registry',
  '{"type":"object","required":["record_id","radius_meters"],"properties":{"record_id":{"type":"string","minLength":1},"radius_meters":{"type":"number","minimum":1,"maximum":100000}}}'::jsonb,
  '{"type":"object","required":["type","features"],"properties":{"type":{"const":"FeatureCollection"},"features":{"type":"array","items":{"type":"object"}}}}'::jsonb,
  'active'
),
(
  'company_registry.box-search',
  'Find registered companies inside a map box',
  'Find official company-register records inside an explicit WGS84 top-left and bottom-right bounding box.',
  'company_registry',
  '{"type":"object","required":["top_left_latitude","top_left_longitude","bottom_right_latitude","bottom_right_longitude"],"properties":{"top_left_latitude":{"type":"number","minimum":-90,"maximum":90},"top_left_longitude":{"type":"number","minimum":-180,"maximum":180},"bottom_right_latitude":{"type":"number","minimum":-90,"maximum":90},"bottom_right_longitude":{"type":"number","minimum":-180,"maximum":180}}}'::jsonb,
  '{"type":"object","required":["type","features"],"properties":{"type":{"const":"FeatureCollection"},"features":{"type":"array","items":{"type":"object"}}}}'::jsonb,
  'active'
),
(
  'vehicle.registry-search',
  'Search Dutch vehicle registrations',
  'Search RDW vehicle-registration data by Dutch licence plate, make or wildcard query and return matching vehicle summaries.',
  'vehicle_registry',
  '{"type":"object","properties":{"query":{"type":"string"},"licence_plate":{"type":"string","pattern":"^[A-Za-z0-9-]{6,10}$"},"make":{"type":"string"}},"anyOf":[{"required":["query"]},{"required":["licence_plate"]},{"required":["make"]}]}'::jsonb,
  '{"type":"array","items":{"type":"object","properties":{"licence_plate":{"type":"string"},"make":{"type":"string"},"model":{"type":"string"},"vehicle_type":{"type":"string"},"version":{"type":"string"}}}}'::jsonb,
  'active'
),
(
  'vehicle.registry-lookup',
  'Look up a Dutch vehicle registration',
  'Retrieve the complete RDW record for one Dutch licence plate, including fuel, emissions, axles, body, APK, BPM, mass and dimensions.',
  'vehicle_registry',
  '{"type":"object","required":["licence_plate"],"properties":{"licence_plate":{"type":"string","pattern":"^[A-Za-z0-9]{6}$"}}}'::jsonb,
  '{"type":"object","properties":{"licence_plate":{"type":"string"},"make":{"type":"string"},"model":{"type":"string"},"vehicle_type":{"type":"string"},"fuel":{"type":"array"},"axles":{"type":"array"},"body":{"type":"array"},"apk_expiry":{"type":"string"},"bpm":{"type":"number"},"catalogue_price":{"type":"number"}}}'::jsonb,
  'active'
),
(
  'building.registry-search',
  'Search the Dutch addresses and buildings register',
  'Search BAG address records by postcode, house number, street or place and resolve the stable BAG address identifier.',
  'address_registry',
  '{"type":"object","properties":{"postcode":{"type":"string","pattern":"^[0-9]{4}\\s?[A-Za-z]{2}$"},"house_number":{"type":"string"},"street":{"type":"string"},"place":{"type":"string"}},"anyOf":[{"required":["postcode"]},{"required":["street"]},{"required":["place"]}]}'::jsonb,
  '{"type":"array","items":{"type":"object","properties":{"bag_id":{"type":"string"},"postcode":{"type":"string"},"house_number":{"type":"string"},"house_letter":{"type":"string"},"street":{"type":"string"},"place":{"type":"string"}}}}'::jsonb,
  'active'
),
(
  'building.registry-lookup',
  'Look up a building or address in the national register',
  'Retrieve an official BAG address and building record including postcode, street, municipality, province, WGS84 and RD coordinates, building identifiers, year, area, status and use purpose.',
  'address_registry',
  '{"type":"object","required":["bag_id"],"properties":{"bag_id":{"type":"string","minLength":1}}}'::jsonb,
  '{"type":"object","properties":{"bag_id":{"type":"string"},"address":{"type":"object"},"location":{"type":"object"},"rd_coordinates":{"type":"object"},"building_id":{"type":"string"},"construction_year":{"type":"integer"},"area_square_metres":{"type":"number"},"status":{"type":"string"},"use_purpose":{"type":"string"}}}'::jsonb,
  'active'
)
on conflict (slug) do update set
  name = excluded.name,
  description = excluded.description,
  category = excluded.category,
  input_contract = excluded.input_contract,
  output_contract = excluded.output_contract,
  status = excluded.status,
  updated_at = now();

insert into public.fact_types
  (slug, name, description, jurisdiction, subject_kind, value_schema, status, freshness_seconds)
values
  ('company_registry.branch_number', 'Company branch number', 'Dutch Handelsregister vestigingsnummer.', 'NL', 'company', '{"type":"string","pattern":"^[0-9]{12}$"}'::jsonb, 'active', 86400),
  ('company.active', 'Company active status', 'Whether the company registration is currently active.', 'NL', 'company', '{"type":"boolean"}'::jsonb, 'active', 86400),
  ('company.activities', 'Registered company activities', 'Official registered activities with codes, descriptions and main-activity flags.', 'NL', 'company', '{"type":"array","items":{"type":"object","properties":{"code":{"type":"string"},"omschrijving":{"type":"string"},"hoofdactiviteit":{"type":"boolean"}}}}'::jsonb, 'active', 86400),
  ('vehicle.fuel', 'Vehicle fuel and emissions', 'RDW fuel, consumption, power, emissions and noise records.', 'NL', 'vehicle', '{"type":"array","items":{"type":"object"}}'::jsonb, 'active', 86400),
  ('vehicle.axles', 'Vehicle axles', 'RDW axle counts, positions, widths and permitted loads.', 'NL', 'vehicle', '{"type":"array","items":{"type":"object"}}'::jsonb, 'active', 86400),
  ('vehicle.body', 'Vehicle body', 'RDW body/carrosserie classification records.', 'NL', 'vehicle', '{"type":"array","items":{"type":"object"}}'::jsonb, 'active', 86400),
  ('vehicle.apk_expiry', 'APK expiry date', 'RDW expiry date of the Dutch periodic vehicle inspection.', 'NL', 'vehicle', '{"type":"string","pattern":"^[0-9]{8}$"}'::jsonb, 'active', 86400),
  ('vehicle.bpm', 'Gross BPM', 'Gross Dutch private motor vehicle and motorcycle tax in euros.', 'NL', 'vehicle', '{"type":"number","minimum":0}'::jsonb, 'active', 86400),
  ('vehicle.catalogue_price', 'Vehicle catalogue price', 'RDW catalogue price in euros.', 'NL', 'vehicle', '{"type":"number","minimum":0}'::jsonb, 'active', 86400),
  ('vehicle.color', 'Vehicle primary colour', 'RDW primary vehicle colour.', 'NL', 'vehicle', '{"type":"string","minLength":1,"maxLength":100}'::jsonb, 'active', 86400),
  ('geo.rd_coordinates', 'Dutch RD coordinates', 'Rijksdriehoekscoordinaten x and y.', 'NL', 'geo', '{"type":"object","required":["x","y"],"properties":{"x":{"type":"number"},"y":{"type":"number"}},"additionalProperties":false}'::jsonb, 'active', 86400),
  ('building.year', 'Building construction year', 'BAG construction year of the associated building.', 'NL', 'building', '{"type":"integer","minimum":1000,"maximum":3000}'::jsonb, 'active', 86400),
  ('building.area_square_metres', 'Addressable-object area', 'BAG floor area of the addressable object in square metres.', 'NL', 'building', '{"type":"number","minimum":0}'::jsonb, 'active', 86400),
  ('building.status', 'Building status', 'Official BAG status of the associated building.', 'NL', 'building', '{"type":"string","minLength":1}'::jsonb, 'active', 86400),
  ('address.municipality', 'Municipality', 'Dutch municipality name for an address.', 'NL', 'address', '{"type":"string","minLength":1}'::jsonb, 'active', 86400),
  ('address.province', 'Province', 'Dutch province name for an address.', 'NL', 'address', '{"type":"string","minLength":1}'::jsonb, 'active', 86400)
on conflict (slug) do update set
  name = excluded.name,
  description = excluded.description,
  jurisdiction = excluded.jurisdiction,
  subject_kind = excluded.subject_kind,
  value_schema = excluded.value_schema,
  status = excluded.status,
  freshness_seconds = excluded.freshness_seconds,
  updated_at = now();

-- Full documented/live-observed output envelopes for all ten routes.
update public.api_endpoints
set response_body = case path
  when '/v3/openkvk' then '{"type":"object","required":["totalItemCount","pageCount","size","_embedded"],"properties":{"totalItemCount":{"type":"integer"},"pageCount":{"type":"integer"},"size":{"type":"integer"},"_links":{"type":"object"},"_embedded":{"type":"object","properties":{"bedrijf":{"type":"array","items":{"type":"object","properties":{"kvknummer":{"type":"string"},"slug":{"type":"string"},"naam":{"type":"string"},"_links":{"type":"object"}}}}}}}}'::jsonb
  when '/v3/openkvk/{id}' then '{"type":"object","properties":{"activiteitomschrijving":{"type":"string"},"verblijfsobjectgebruiksdoel":{"type":"string"},"vbo_id":{"type":"string"},"locatie":{"type":"object","properties":{"lat":{"type":"number"},"lon":{"type":"number"}}},"rechtsvormCode":{"type":"string"},"rechtsvormOmschrijving":{"type":"string"},"vestigingsnummer":{"type":"string"},"huidigeHandelsNamen":{"type":"array","items":{"type":"string"}},"activiteiten":{"type":"array","items":{"type":"object","properties":{"omschrijving":{"type":"string"},"code":{"type":"string"},"hoofdactiviteit":{"type":"boolean"}}}},"vestiging":{"type":"boolean"},"naam":{"type":"string"},"bezoeklocatie":{"type":"object","properties":{"plaats":{"type":"string"},"straat":{"type":"string"},"huisnummer":{"type":"string"},"postcode":{"type":"string"}}},"postlocatie":{"type":"array"},"kvknummer":{"type":"string"},"pand_id":{"type":"string"},"last_checked_at":{"type":"string"},"updated_at":{"type":"string"},"actief":{"type":"boolean"},"sbi":{"type":"array","items":{"type":"string"}},"inschrijvingstype":{"type":"string"},"slug":{"type":"string"},"_links":{"type":"object"}}}'::jsonb
  when '/v3/suggest/openkvk/{query}' then '{"type":"array","maxItems":25,"items":{"type":"object","properties":{"handelsnamen":{"type":"array","items":{"type":"string"}},"kvknummer":{"type":"integer"},"link":{"type":"string"},"naam":{"type":"string"},"postcode":{"type":"string"},"vestigingsnummer":{"type":"integer"}}}}'::jsonb
  when '/v3/geo/openkvk/radius/{lat}/{lon}/{radius}' then '{"type":"object","required":["type","features"],"properties":{"type":{"const":"FeatureCollection"},"features":{"type":"array","items":{"type":"object","properties":{"type":{"const":"Feature"},"geometry":{"type":"object","properties":{"type":{"type":"string"},"coordinates":{"type":"array"}}},"properties":{"type":"object","properties":{"kvknummer":{"type":"string"},"slug":{"type":"string"},"huidigeNamen":{"type":"array"}}}}}}}}'::jsonb
  when '/v3/geo/openkvk/around/{id}/{radius}' then '{"type":"object","required":["type","features"],"properties":{"type":{"const":"FeatureCollection"},"features":{"type":"array","items":{"type":"object","properties":{"type":{"const":"Feature"},"geometry":{"type":"object","properties":{"type":{"type":"string"},"coordinates":{"type":"array"}}},"properties":{"type":"object","properties":{"kvknummer":{"type":"string"},"slug":{"type":"string"},"huidigeNamen":{"type":"array"}}}}}}}}'::jsonb
  when '/v3/geo/openkvk/box/{tlLat}/{tlLon}/{brLat}/{brLon}' then '{"type":"object","required":["type","features"],"properties":{"type":{"const":"FeatureCollection"},"features":{"type":"array","items":{"type":"object","properties":{"type":{"const":"Feature"},"geometry":{"type":"object","properties":{"type":{"type":"string"},"coordinates":{"type":"array"}}},"properties":{"type":"object","properties":{"kvknummer":{"type":"string"},"slug":{"type":"string"},"huidigeNamen":{"type":"array"}}}}}}}}'::jsonb
  when '/voertuiggegevens' then '{"type":"object","required":["totalItemCount","pageCount","size","_embedded"],"properties":{"totalItemCount":{"type":"integer"},"pageCount":{"type":"integer"},"size":{"type":"integer"},"_links":{"type":"object"},"_embedded":{"type":"object","properties":{"kenteken":{"type":"array","items":{"type":"object","properties":{"voertuigsoort":{"type":"string"},"handelsbenaming":{"type":"string"},"merk":{"type":"string"},"kentekenplaat":{"type":"string"},"uitvoering":{"type":"string"},"_links":{"type":"object"}}}}}}}}'::jsonb
  when '/voertuiggegevens/{kenteken}' then '{"type":"object","properties":{"kenteken":{"type":"string"},"kentekenplaat":{"type":"string"},"kenteken_formatted":{"type":"string"},"merk":{"type":"string"},"handelsbenaming":{"type":"string"},"voertuigsoort":{"type":"string"},"type":{"type":"string"},"variant":{"type":"string"},"uitvoering":{"type":"string"},"eerste_kleur":{"type":"string"},"inrichting":{"type":"string"},"brandstof":{"type":"array","items":{"type":"object"}},"as":{"type":"array","items":{"type":"object"}},"carrosserie":{"type":"array","items":{"type":"object"}},"vervaldatum_apk":{"type":"string"},"bruto_bpm":{"type":"number"},"catalogusprijs":{"type":"number"},"datum_eerste_toelating":{"type":"string"},"datum_tenaamstelling":{"type":"string"},"aantal_cilinders":{"type":"integer"},"cilinderinhoud":{"type":"number"},"aantal_zitplaatsen":{"type":"integer"},"aantal_deuren":{"type":"integer"},"aantal_wielen":{"type":"integer"},"massa_rijklaar":{"type":"number"},"massa_ledig_voertuig":{"type":"number"},"lengte":{"type":"number"},"breedte":{"type":"number"},"hoogte_voertuig":{"type":"number"},"wielbasis":{"type":"number"},"wam_verzekerd":{"type":"boolean"},"export_indicator":{"type":"boolean"},"taxi_indicator":{"type":"boolean"},"tenaamstellen_mogelijk":{"type":"boolean"},"openstaande_terugroepactie_indicator":{"type":"boolean"},"_links":{"type":"object"}}}'::jsonb
  when '/bag' then '{"type":"object","required":["totalItemCount","pageCount","size","_embedded"],"properties":{"totalItemCount":{"type":"integer"},"pageCount":{"type":"integer"},"size":{"type":"integer"},"_links":{"type":"object"},"_embedded":{"type":"object","properties":{"adres":{"type":"array","items":{"type":"object","properties":{"huisnummer":{"type":"string"},"huisletter":{"type":"string"},"postcode":{"type":"string"},"openbareruimte":{"type":"string"},"slug":{"type":"string"},"woonplaats":{"type":"string"},"_links":{"type":"object"}}}}}}}}'::jsonb
  when '/bag/{bag_id}' then '{"type":"object","properties":{"verblijfsobjectgebruiksdoel":{"type":"string"},"locatie":{"type":"object","properties":{"lat":{"type":"number"},"lon":{"type":"number"}}},"object_type":{"type":"string"},"huisletter":{"type":"string"},"postcode":{"type":"string"},"pandbouwjaar":{"type":"integer"},"object_id":{"type":"number"},"pand_id":{"type":"number"},"woonplaats":{"type":"string"},"provincie":{"type":"string"},"pandstatus":{"type":"string"},"huisnummer":{"type":"string"},"x":{"type":"number"},"verblijfsobjectstatus":{"type":"string"},"y":{"type":"number"},"openbareruimte":{"type":"string"},"gemeente":{"type":"string"},"slug":{"type":"string"},"oppervlakteverblijfsobject":{"type":"number"},"_links":{"type":"object"}}}'::jsonb
  else response_body
end,
tags = case path
  when '/v3/openkvk' then array['overheid.io','openkvk','kvk','handelsregister','company','bedrijf','business search','company search','postcode','netherlands','nederland']
  when '/v3/openkvk/{id}' then array['overheid.io','openkvk','kvk','handelsregister','company profile','bedrijfsgegevens','sbi','rechtsvorm','vestiging','netherlands']
  when '/v3/suggest/openkvk/{query}' then array['overheid.io','openkvk','kvk','autocomplete','suggest','company name','bedrijfsnaam','handelsnaam']
  when '/v3/geo/openkvk/radius/{lat}/{lon}/{radius}' then array['overheid.io','openkvk','kvk','geojson','radius','nearby companies','bedrijven in de buurt','latitude','longitude']
  when '/v3/geo/openkvk/around/{id}/{radius}' then array['overheid.io','openkvk','kvk','geojson','nearby','around company','bedrijven rond bedrijf','radius']
  when '/v3/geo/openkvk/box/{tlLat}/{tlLon}/{brLat}/{brLon}' then array['overheid.io','openkvk','kvk','geojson','bounding box','map rectangle','bedrijven in gebied']
  when '/voertuiggegevens' then array['overheid.io','rdw','kenteken','licence plate','license plate','vehicle','voertuig','auto','car','make','merk','netherlands']
  when '/voertuiggegevens/{kenteken}' then array['overheid.io','rdw','kenteken','licence plate','license plate','vehicle details','voertuiggegevens','brandstof','assen','carrosserie','apk','bpm']
  when '/bag' then array['overheid.io','bag','address','adres','postcode','postal code','huisnummer','street','straat','building','gebouw','netherlands']
  when '/bag/{bag_id}' then array['overheid.io','bag','address details','adresgegevens','postcode','coordinates','rd coordinates','pand','bouwjaar','oppervlakte','gebruiksdoel']
  else tags
end,
sectors = case
  when path like '/v3/%' then array['Company data','Government and public data','Geospatial data']
  when path like '/voertuiggegevens%' then array['Mobility','Government and public data','Vehicle data']
  when path like '/bag%' then array['Address data','Government and public data','Real estate']
  else sectors
end,
metadata_at = now()
where api_id = '3ed22ff1-5dc3-42ee-909b-2771cd9361e5';

-- Add all meaningful live-observed fields to the extraction maps.
update public.capability_execution_contracts c
set output_map = case e.path
  when '/v3/openkvk/{id}' then '{"base":"$","produces":{"company.name":{"path":"$.naam"},"company_registry.kvknummer":{"path":"$.kvknummer"},"company_registry.openkvk_slug":{"path":"$.slug"},"company_registry.branch_number":{"path":"$.vestigingsnummer"},"company.active":{"path":"$.actief"},"company.activities":{"path":"$.activiteiten[*]","collect":true},"company.profile":{"fields":{"legal_name":{"path":"$.naam"},"trade_names":{"path":"$.huidigeHandelsNamen[*]","collect":true},"legal_form":{"path":"$.rechtsvormOmschrijving"},"sbi_codes":{"path":"$.sbi[*]","collect":true}}},"address.postal":{"fields":{"postcode":{"path":"$.bezoeklocatie.postcode"},"street":{"path":"$.bezoeklocatie.straat"},"house_number":{"path":"$.bezoeklocatie.huisnummer"},"city":{"path":"$.bezoeklocatie.plaats"}}},"geo.location":{"fields":{"lat":{"path":"$.locatie.lat"},"lon":{"path":"$.locatie.lon"}}}}}'::jsonb
  when '/voertuiggegevens/{kenteken}' then '{"base":"$","produces":{"vehicle.registration.nl.plate":{"path":"$.kenteken"},"vehicle.make":{"path":"$.merk"},"vehicle.model":{"path":"$.handelsbenaming"},"vehicle.type":{"path":"$.voertuigsoort"},"vehicle.fuel":{"path":"$.brandstof[*]","collect":true},"vehicle.axles":{"path":"$.as[*]","collect":true},"vehicle.body":{"path":"$.carrosserie[*]","collect":true},"vehicle.apk_expiry":{"path":"$.vervaldatum_apk"},"vehicle.bpm":{"path":"$.bruto_bpm"},"vehicle.catalogue_price":{"path":"$.catalogusprijs"},"vehicle.color":{"path":"$.eerste_kleur"}}}'::jsonb
  when '/bag/{bag_id}' then '{"base":"$","produces":{"address.bag_slug":{"path":"$.slug"},"building.postcode":{"path":"$.postcode"},"address.postal":{"fields":{"postcode":{"path":"$.postcode"},"street":{"path":"$.openbareruimte"},"house_number":{"path":"$.huisnummer"},"city":{"path":"$.woonplaats"}}},"address.municipality":{"path":"$.gemeente"},"address.province":{"path":"$.provincie"},"geo.location":{"fields":{"lat":{"path":"$.locatie.lat"},"lon":{"path":"$.locatie.lon"}}},"geo.rd_coordinates":{"fields":{"x":{"path":"$.x"},"y":{"path":"$.y"}}},"building.bagpnd":{"path":"$.pand_id","cast":"string"},"building.year":{"path":"$.pandbouwjaar"},"building.area_square_metres":{"path":"$.oppervlakteverblijfsobject"},"building.status":{"path":"$.pandstatus"},"building.gebruiksdoel":{"path":"$.verblijfsobjectgebruiksdoel"}}}'::jsonb
  else c.output_map
end,
capability_slug = case e.path
  when '/v3/geo/openkvk/radius/{lat}/{lon}/{radius}' then 'company_registry.radius-search'
  when '/v3/geo/openkvk/around/{id}/{radius}' then 'company_registry.nearby-search'
  when '/v3/geo/openkvk/box/{tlLat}/{tlLon}/{brLat}/{brLon}' then 'company_registry.box-search'
  when '/voertuiggegevens' then 'vehicle.registry-search'
  when '/voertuiggegevens/{kenteken}' then 'vehicle.registry-lookup'
  when '/bag' then 'building.registry-search'
  when '/bag/{bag_id}' then 'building.registry-lookup'
  else c.capability_slug
end,
validated_at = now(),
validated_note = 'Schema, taxonomy and extraction coverage verified against live overheid.io response shapes on 2026-09-07.',
updated_at = now()
from public.api_endpoints e
where e.id = c.endpoint_id
  and e.api_id = '3ed22ff1-5dc3-42ee-909b-2771cd9361e5';

-- The legacy `produces` projection is retained for discovery clients while
-- Gateway v2 continues to execute the richer output_map directly.
update public.capability_execution_contracts c
set produces = (
  select coalesce(
    jsonb_agg(jsonb_build_object('fact_type', fact_type, 'optional', false) order by fact_type),
    '[]'::jsonb
  )
  from jsonb_object_keys(c.output_map->'produces') as fact_type
), updated_at = now()
from public.api_endpoints e
where e.id = c.endpoint_id
  and e.api_id = '3ed22ff1-5dc3-42ee-909b-2771cd9361e5';

commit;
