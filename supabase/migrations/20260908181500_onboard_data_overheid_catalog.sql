-- Onboard data.overheid.nl — the Dutch national open-data register — as a
-- DISCOVERY source. Three endpoints over its CKAN API.
--
-- WHAT THIS IS, AND WHAT IT IS NOT
-- ---------------------------------------------------------------------------
-- data.overheid.nl (DONL) indexes 20,276 datasets from 33 Dutch government
-- publishers. It is a CATALOGUE, not a data source, and this migration says so
-- in every description rather than letting a buyer discover it after paying.
--
-- Verified on 2026-09-08, not assumed: the CKAN DataStore extension is not
-- merely disabled, it is ABSENT from the action registry.
--     GET  /data/api/3/action/datastore_search       -> 400 "Action name not known"
--     POST /data/api/3/action/datastore_search       -> 400 "Action name not known"
--     GET  /data/api/3/action/help_show?name=datastore_search -> 404 "Action function not found"
-- while help_show?name=package_search returns a full docstring, so the 404 is a
-- real absence and not a permissions answer. status_show lists the installed
-- extensions as donl-auth, donl-ui, donl-schema, donl-rdf, donl-taxonomies —
-- no datastore, no datapusher, no xloader. Resource dictionaries carry no
-- datastore_active field at all.
--
-- So every row of actual data lives on a third-party host, and the catalogue's
-- job is to tell you which one. That is a real job: across a 1,000-dataset
-- sample the resource URLs pointed at geo.rijkswaterstaat.nl, data.eindhoven.nl,
-- opendata.cbs.nl, onderwijsdata.duo.nl, opendata.rdw.nl and 100+ other hosts.
--
-- WHY IT IS STILL WORTH SELLING
-- ---------------------------------------------------------------------------
-- Three things, all verified:
--   1. NO CORS HEADERS. Sending an Origin header returns no
--      access-control-allow-origin, so a browser client cannot call DONL
--      directly at all. A server-side proxy is the only way in.
--   2. THE VOCABULARIES ARE URIs, NOT WORDS. `format` is an EU Publications
--      Office URI ("http://publications.europa.eu/resource/authority/file-type/CSV")
--      and `distribution_type` is a DONL URI. Every naive client mis-parses
--      both; you must split on "/" and take the last segment.
--   3. THE CATALOGUE BURIES THE CALLABLE RESOURCE. 77% of resources declare
--      no distribution_type at all, and the ones that do are not ordered
--      usefully. Verified on kvk-handelsregister-open-dataset-jaarrekeningen,
--      the most commercially relevant entry in the register: of its four
--      resources the first three are an order page, the same order page again
--      and a developer-portal page, and only the FOURTH — the one marked
--      distribution_type webservice — is a callable API
--      (https://opendata.kvk.nl/api/v1/hvds/jaarrekeningen/kvknummer). A
--      consumer that reads resources[0] and stops gets a marketing page.
-- The endpoint descriptions below carry all three warnings, because in this
-- catalogue the description is the interface.
--
-- COST AND PRICE
-- ---------------------------------------------------------------------------
-- Free upstream, no key, no quota. Verified: reads returned 200 with no
-- credential and with a deliberately bogus Authorization header; 15 concurrent
-- and 20 rapid sequential requests all returned 200; ~100 requests over ten
-- minutes produced no 429, no Retry-After and no rate-limit header. Writes are
-- closed to anonymous callers (403 on package_create).
--
-- Price is EUR 0.05 displayed, a buyer total of USD 0.054348 at the published
-- EUR/USD rate of 0.92 — the same arithmetic and rate as
-- 20260908173901_kvk_annual_accounts_ten_euro_cents.sql:
--   list 47,259 + ceil(47,259 x 1500 / 10000) = 47,259 + 7,089 = 54,348 micro-USD
--   54,348 x 0.92 = EUR 0.05000016, displayed as EUR 0.05
--
-- WORTH SAYING PLAINLY: this puts a metadata lookup at the same price as a CBS
-- statistics call, and above the identifier-resolution steps elsewhere in the
-- catalogue (KVK Search sits on the cheapest tier). A discovery call returns no
-- data rows — package_show hands back URLs, not figures — so a buyer can spend
-- EUR 0.05 and still be one call away from an answer. That is a deliberate
-- pricing decision by the operator, not an accident of the arithmetic, and the
-- endpoint descriptions below are written so nobody buys it expecting rows.
-- Check the live price of the comparison endpoints before quoting this
-- paragraph as evidence: prices in this catalogue move by migration.
--
-- cache_ttl_seconds is 3600 rather than the day used for CBS: RDW records in
-- this catalogue were modified the same day they were probed, so the register
-- does move, and an hour is enough to absorb repeated discovery without
-- serving stale publisher metadata.
--
-- NO EXECUTION CONTRACTS, for the same reason as the CBS onboarding: a CKAN
-- search is addressed by a Solr query string the planner cannot compose from a
-- fact, and a catalogue row is not a fact about a company. Discovery here is a
-- caller-driven or model-driven call, which is what it should be.
--
-- NO PAYOUT, NO WALLET, NO SETTLEMENT ROUTE
-- ---------------------------------------------------------------------------
-- DONL is a free public register with no provider to pay, so this listing sets
-- `settlement_mode = 'apiosk_revenue'` and `requires_payout_wallet = false`
-- and writes NO wallet_address and NO payout_network. The row is inserted as
-- 'inactive' — the staged state EODHD uses — which keeps it out of both the
-- review queue and `require_payout_wallet_to_publish()` (provider repo,
-- 20260728), which raises 23514 for any row entering 'pending' or 'active'
-- with an empty, malformed or all-zero burn wallet. apis.wallet_address
-- defaults to that burn address, so the only correct move for a source with no
-- payee is to leave the payout columns alone entirely.
--
-- APPLY : via the Supabase CLI migration history, then
--         `node scripts/sync-migration.mjs` to mirror it into app/.
-- AFTER : inserted as a DRAFT. Go live in a separate migration.

begin;

do $migration$
declare
  source_id constant uuid := '803bcf10-ae0f-4875-9872-b25585d9b92a';
  platform_owner constant uuid := 'bf085fe8-41c4-4699-978f-e75e89518bf7';
  public_metadata jsonb := jsonb_build_object(
    'logo', 'https://data.overheid.nl/sites/default/files/favicon.ico',
    'billing', jsonb_build_object('mode', 'apiosk_balance'),
    'base_url', 'https://data.overheid.nl/data/api/3/action',
    'operator_notice', 'Operated by KOOP for the Dutch government. Catalogue metadata only — the data itself lives on each publisher''s own host.',
    'license', jsonb_build_object(
      'name', 'Per dataset',
      'attribution', 'Bron: data.overheid.nl',
      'note', 'Each catalogue entry carries its own license_id; most are CC BY 4.0 or CC0, but this is per dataset and must be read from the record.',
      'cost_upstream', 'EUR 0.00'
    ),
    'tags', jsonb_build_array(
      'data.overheid.nl', 'donl', 'ckan', 'dataregister', 'open data',
      'nederlandse overheid', 'dutch government', 'netherlands', 'nederland',
      'dataset search', 'data catalog', 'datacatalogus', 'metadata',
      'bedrijven', 'ondernemingen', 'handelsregister', 'kvk', 'rdw',
      'faillissement', 'vergunning', 'subsidie', 'aanbesteding',
      'basisregistratie', 'high value dataset', 'dcat'
    ),
    'sectors', jsonb_build_array('Government and public data', 'Company data'),
    'coverage', jsonb_build_object(
      'datasets', 20276,
      'organizations', 33,
      'probed_on', '2026-09-08',
      'data_rows', 'none — DataStore is absent from this CKAN install; every row lives on a third-party host'
    ),
    'service_groups', jsonb_build_array(
      jsonb_build_object(
        'name', 'Dataset discovery',
        'description', 'Search the register, then read one entry in full to reach its publisher''s actual endpoints.',
        'endpoint_paths', jsonb_build_array(
          '/data/api/3/action/package_search',
          '/data/api/3/action/package_show'
        )
      ),
      jsonb_build_object(
        'name', 'Publishers',
        'description', 'The 33 organizations that publish into the register.',
        'endpoint_paths', jsonb_build_array('/data/api/3/action/organization_list')
      )
    )
  );
begin
  if not exists (select 1 from public.provider_profiles where id = platform_owner) then
    raise exception 'Apiosk platform owner % is missing', platform_owner;
  end if;

  if exists (select 1 from public.apis where slug = 'data-overheid-nl' and id <> source_id) then
    raise exception 'The slug data-overheid-nl already belongs to another source id';
  end if;

  insert into public.apis (
    id, owner_id, slug, name, status, description, long_description,
    category, docs_url, website_url, origin_url, endpoint_url,
    authentication_info, is_verified, active, discoverable, listing_type,
    listing_metadata, integration_mode, settlement_mode, is_external,
    is_platform_owned, is_community, source, requires_payout_wallet
  ) values (
    source_id,
    platform_owner,
    'data-overheid-nl',
    'data.overheid.nl — Dutch National Open Data Register',
    'inactive',
    'Search the Dutch government''s open-data register: 20,276 datasets from 33 publishers, with the publisher endpoints behind each one. Catalogue metadata, not data rows.',
    'Het Dataregister van de Nederlandse Overheid (DONL), the national index of Dutch government open data, published as a CKAN 2.8 API. Use it to find which official dataset answers a question and where its data actually lives: search 20,276 entries by keyword, publisher, theme or license, then read one entry in full to get its resources — the URLs, formats and access methods on the publisher''s own host. Company-relevant clusters include the KVK Handelsregister open datasets (basis bedrijfsgegevens and jaarrekeningen), RDW''s daily-updated register of accredited businesses and the vehicle base register, CBS insolvency and firm statistics, and thousands of municipal permit, subsidy and business-park datasets. READ THIS BEFORE BUYING: this is a catalogue and it returns NO data rows. The CKAN DataStore extension is absent from this install, verified — datastore_search returns HTTP 400 "Action name not known" — so every actual row lives on a third-party host that you call separately. Two parsing traps: the `format` field is an EU Publications Office URI, not a plain string, and `distribution_type` is a DONL URI, so split both on "/" and take the last segment; and 77% of resources declare no distribution type at all, while some resources labelled as downloads are in fact marketing pages. What this source buys you is server-side access (DONL sends no CORS headers, so browsers cannot call it directly), one described interface over the vocabularies, and a single place to find which of a hundred publisher hosts holds the data.',
    'data',
    'https://data.overheid.nl/ondersteuning/data-publiceren/api',
    'https://data.overheid.nl/',
    'https://data.overheid.nl',
    'https://data.overheid.nl',
    jsonb_build_object(
      'type', 'none',
      'caller', 'Pay per request through the Apiosk balance. data.overheid.nl requires no API key for reads.',
      'upstream', jsonb_build_object(
        'note', 'Reads are unauthenticated. Writes are refused to anonymous callers upstream and no write endpoint is listed here.'
      )
    ),
    false,
    false,
    false,
    'api',
    public_metadata,
    'gateway',
    'apiosk_revenue',
    false,
    true,
    false,
    'manual',
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
    source = excluded.source;
end;
$migration$;

-- Endpoints. Every response_example is a real trimmed body observed 2026-09-08.
insert into public.api_endpoints (
  id, api_id, method, path, price, active, cache_ttl_seconds, description,
  query_params, response_body, request_example, response_example,
  tags, sectors, use_cases, endpoint_category, action_type, payment_required,
  external_docs_url, documentation_markdown, metadata_source, metadata_confidence
)
values
(
  'f635dc84-9e80-451b-b714-e9facc2d079a', '803bcf10-ae0f-4875-9872-b25585d9b92a', 'GET', '/data/api/3/action/package_search', 0.047259, true, 3600,
  'Search the Dutch national open-data register — 20,276 datasets from 33 government publishers. Zoek in het Dataregister van de Nederlandse Overheid op trefwoord, publicerende organisatie, thema of licentie. Free-text search goes in `q` (Solr syntax, so field terms like organization:centraal-bureau-voor-de-statistiek work — call organization_list first, because a slug that does not exist returns success:true with count 0 rather than an error), structured filters go in `fq`. Returns the full catalogue record for each hit: title, notes, publisher, license_id, metadata_modified and the resources that say where the data actually lives. Real result counts on 2026-09-08: bedrijven 1332, werkgelegenheid 441, subsidie 249, ondernemingen 225, omzet 220, vergunning 141, jaarrekening 111, faillissement 42, KVK 23, handelsregister 19. TWO CAPS TO KNOW: `rows` is silently truncated to 1000 — asking for 2000 still returns success:true with 1000 rows — and this endpoint returns METADATA ONLY. No dataset rows are ever returned by this API; follow a hit through package_show to its publisher''s own host.',
  '{"type":"object","properties":{"q":{"type":"string","required":false,"description":"Free-text or Solr query, e.g. jaarrekening, or organization:kamer_van_koophandel. Defaults to everything.","x-parameter-location":"query"},"fq":{"type":"string","required":false,"description":"Solr filter query for structured narrowing, e.g. res_format:JSON or license_id:cc-zero.","x-parameter-location":"query"},"rows":{"type":"integer","required":false,"description":"Results per page. Default 20. Values above 1000 are SILENTLY truncated to 1000, with success:true.","x-parameter-location":"query"},"start":{"type":"integer","required":false,"description":"Offset into the result set, for paging.","x-parameter-location":"query"},"sort":{"type":"string","required":false,"description":"Sort expression, e.g. metadata_modified desc or score desc.","x-parameter-location":"query"},"facet.field":{"type":"string","required":false,"description":"JSON array of fields to facet on, e.g. [\"organization\",\"res_format\"].","x-parameter-location":"query"}}}'::jsonb,
  '{"type":"object","properties":{"success":{"type":"boolean"},"result":{"type":"object","properties":{"count":{"type":"integer","description":"Total matching datasets."},"results":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"name":{"type":"string","description":"The slug to pass to package_show."},"title":{"type":"string"},"notes":{"type":"string"},"organization":{"type":"object"},"license_id":{"type":"string","description":"Often a full URI rather than a short code."},"metadata_modified":{"type":"string"},"num_resources":{"type":"integer"},"resources":{"type":"array"}}}},"search_facets":{"type":"object"}}}}}'::jsonb,
  '{"q":"jaarrekening","rows":1}'::jsonb,
  '{"success":true,"result":{"count":111,"results":[{"id":"975c8f7d-84fe-4f3c-bd59-4f703752b30a","name":"kvk-handelsregister-open-dataset-jaarrekeningen","title":"KVK Handelsregister Open Dataset Jaarrekeningen","license_id":"http://creativecommons.org/licenses/by/4.0/deed.nl","num_resources":4}]}}'::jsonb,
  array['data.overheid.nl','donl','ckan','dataset search','open data','nederlandse overheid','datacatalogus'], array['Government and public data','Company data'], array['Find which official Dutch dataset answers a question','List every open dataset a ministry publishes','Discover company or business datasets across government'],
  'discovery', 'search', true, 'https://data.overheid.nl/ondersteuning/data-publiceren/api',
  'GET /data/api/3/action/package_search — national open-data register search. rows is silently capped at 1000. Metadata only; no data rows.', 'provider_documentation_and_live_probe', 1
),
(
  '56171f82-d820-40a0-8d44-a55377bbf893', '803bcf10-ae0f-4875-9872-b25585d9b92a', 'GET', '/data/api/3/action/package_show', 0.047259, true, 3600,
  'Read one dataset record from the Dutch open-data register in full, by its `name` slug or id — and, most importantly, its `resources`: the list of places the actual data lives. This is the handoff endpoint, and it is where the catalogue''s two traps bite. First, `format` is an EU Publications Office URI ("http://publications.europa.eu/resource/authority/file-type/CSV") and `distribution_type` is a DONL URI ("https://data.overheid.nl/distributiontype/download") — split each on "/" and take the last segment, because naive string matching fails on both. Second, the callable resource is rarely the first one: verified on 2026-09-08, kvk-handelsregister-open-dataset-jaarrekeningen has four resources of which the first three are an order page, the same order page again and a developer-portal page, and only the fourth — distribution_type webservice — is a real API at https://opendata.kvk.nl/api/v1/hvds/jaarrekeningen/kvknummer. Scan the whole array for distribution_type ending in webservice before concluding a dataset has no endpoint. Resources that ARE directly callable in this register include RDW''s Socrata views (rewrite the export URL to /resource/<id>.json) and CBS''s OData tables (the machine endpoint is often in `identifier` rather than in the resource url). Verify a resource before promising a buyer it returns data.',
  '{"type":"object","required":["id"],"properties":{"id":{"type":"string","required":true,"description":"Dataset name slug (e.g. kvk-handelsregister-open-dataset-jaarrekeningen) or uuid, from package_search.","x-parameter-location":"query"}}}'::jsonb,
  '{"type":"object","properties":{"success":{"type":"boolean"},"result":{"type":"object","properties":{"id":{"type":"string"},"name":{"type":"string"},"title":{"type":"string"},"notes":{"type":"string"},"license_id":{"type":"string"},"num_resources":{"type":"integer"},"organization":{"type":"object"},"metadata_modified":{"type":"string"},"resources":{"type":"array","items":{"type":"object","properties":{"name":{"type":"string"},"url":{"type":"string","description":"Frequently a landing or order page rather than a callable endpoint. Check before relying on it."},"format":{"type":"string","description":"An EU Publications Office URI; take the last path segment for CSV/JSON/XML/WFS."},"distribution_type":{"type":"string","description":"A DONL URI ending in download, webservice, documentation, visualization or dataschema. Empty on roughly 77% of resources."},"description":{"type":"string"},"mimetype":{"type":"string"},"last_modified":{"type":"string"}}}}}}}}'::jsonb,
  '{"id":"kvk-handelsregister-open-dataset-jaarrekeningen"}'::jsonb,
  '{"success":true,"result":{"id":"975c8f7d-84fe-4f3c-bd59-4f703752b30a","name":"kvk-handelsregister-open-dataset-jaarrekeningen","title":"KVK Handelsregister Open Dataset Jaarrekeningen","num_resources":4,"license_id":"http://creativecommons.org/licenses/by/4.0/deed.nl","resources":[{"name":"KVK Handelsregister Open Dataset Jaarrekeningen","format":"http://publications.europa.eu/resource/authority/file-type/XML","url":"https://www.kvk.nl/producten-bestellen/kvk-jaarrekeningen-open-data-set/","distribution_type":"https://data.overheid.nl/distributiontype/download"},{"name":"KVK Handelsregister Open Dataset Jaarrekeningen- API","format":"http://publications.europa.eu/resource/authority/file-type/JSON","url":"https://developers.kvk.nl/nl/documentation/open-dataset-jaarrekeningen-api","distribution_type":"https://data.overheid.nl/distributiontype/documentation"},{"name":"KVK Handelsregister Open Dataset Jaarrekeningen- API","format":"http://publications.europa.eu/resource/authority/file-type/JSON","url":"https://opendata.kvk.nl/api/v1/hvds/jaarrekeningen/kvknummer","distribution_type":"https://data.overheid.nl/distributiontype/webservice"}]}}'::jsonb,
  array['data.overheid.nl','donl','ckan','dataset detail','resources','open data','access methods'], array['Government and public data','Company data'], array['Find where a government dataset''s data actually lives','Check which formats a dataset is published in','Follow a catalogue entry to a callable publisher API'],
  'discovery', 'lookup', true, 'https://data.overheid.nl/ondersteuning/data-publiceren/api',
  'GET /data/api/3/action/package_show — one dataset record with its resources. format and distribution_type are URIs; resource urls are often landing pages.', 'provider_documentation_and_live_probe', 1
),
(
  'b90f85d7-4f9a-409b-b817-64282acf43e8', '803bcf10-ae0f-4875-9872-b25585d9b92a', 'GET', '/data/api/3/action/organization_list', 0.047259, true, 3600,
  'List every organization publishing into the Dutch open-data register — 33 of them on 2026-09-08, returned as slugs. Use a slug to scope a package_search with q=organization:<slug>. The list is short and stable, so cache it. Publishers include centraal-bureau-voor-de-statistiek, cbs-microdata, de-nederlandsche-bank, dienst-uitvoering-onderwijs, data-overheid-nl and a long tail of municipalities. There is NO kamer_van_koophandel organization: the KVK datasets are carried under data-overheid-nl, so q=organization:kamer_van_koophandel returns count 0 and finds nothing. Pass all_fields=true for titles and descriptions instead of bare slugs; note that CKAN''s organization objects can carry internal account metadata, so treat any extra fields as internal and do not pass them on.',
  '{"type":"object","properties":{"all_fields":{"type":"boolean","required":false,"description":"Return full organization objects (title, description, package_count) instead of bare slugs.","x-parameter-location":"query"},"limit":{"type":"integer","required":false,"description":"Maximum organizations to return.","x-parameter-location":"query"}}}'::jsonb,
  '{"type":"object","properties":{"success":{"type":"boolean"},"result":{"type":"array","items":{"type":"string","description":"Organization slug, or a full object when all_fields=true."}}}}'::jsonb,
  '{}'::jsonb,
  '{"success":true,"result":["centraal-bureau-voor-de-statistiek-derden","cbs-microdata","centraal-bureau-voor-de-statistiek","de-nederlandsche-bank","data-overheid-nl","dienst-uitvoering-onderwijs","dataplatform","gemeente-amsterdam","gemeente-arnhem","gemeente-assen"]}'::jsonb,
  array['data.overheid.nl','donl','ckan','publishers','organizations','open data'], array['Government and public data'], array['List every Dutch government open-data publisher','Scope a dataset search to one ministry or agency'],
  'discovery', 'lookup', true, 'https://data.overheid.nl/ondersteuning/data-publiceren/api',
  'GET /data/api/3/action/organization_list — the 33 publishing organizations. Use a slug to scope package_search.', 'provider_documentation_and_live_probe', 1
)
on conflict (id) do update set
  api_id = excluded.api_id,
  method = excluded.method,
  path = excluded.path,
  price = excluded.price,
  active = excluded.active,
  cache_ttl_seconds = excluded.cache_ttl_seconds,
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
  metadata_confidence = excluded.metadata_confidence;

-- Guard. A slug that does not match production selects zero rows in silence;
-- kvk-resolvers-live-slug.sql records what that cost the last time.
do $verify$
declare
  source_id constant uuid := '803bcf10-ae0f-4875-9872-b25585d9b92a';
  endpoint_total integer;
  priced_total integer;
  contract_total integer;
  visible_total integer;
begin
  select count(*) into endpoint_total
  from public.api_endpoints where api_id = source_id;
  if endpoint_total <> 3 then
    raise exception 'data.overheid.nl onboarding expects exactly 3 endpoints, found %', endpoint_total;
  end if;

  select count(*) into priced_total
  from public.api_endpoints
  where api_id = source_id and price = 0.047259 and active and cache_ttl_seconds = 3600;
  if priced_total <> 3 then
    raise exception 'data.overheid.nl expects 3 endpoints at list 0.047259, active, cached an hour; found %', priced_total;
  end if;

  select count(*) into contract_total
  from public.capability_execution_contracts contract
  join public.api_endpoints endpoint on endpoint.id = contract.endpoint_id
  where endpoint.api_id = source_id;
  if contract_total <> 0 then
    raise exception 'data.overheid.nl onboarding refuses % execution contracts; a catalogue row is not a fact about a company', contract_total;
  end if;

  select count(*) into visible_total
  from public.apis
  where id = source_id
    and status = 'active' and coalesce(active, true) and is_verified
    and coalesce(discoverable, true) and suspended_at is null;
  if visible_total <> 0 then
    raise exception 'data.overheid.nl source must stay a draft in this migration; go live in a separate one';
  end if;
end;
$verify$;

commit;

-- ---------------------------------------------------------------------------
-- AFTER APPLYING
-- ---------------------------------------------------------------------------
-- 1. node scripts/sync-migration.mjs
-- 2. Probe the three endpoints from the production egress IP.
-- 3. Flip live in a separate migration, which must clear both gates:
--    a. set_config('apiosk.api_review_approved_id', <source_id>, true) before
--       the UPDATE, or the review trigger rewrites status to 'pending';
--    b. require_payout_wallet_to_publish() raises 23514 on the transition into
--       'pending'/'active' with a burn or empty wallet_address. This source has
--       no payee, so that is a platform-settlement decision, not one this
--       onboarding can make.
--
-- Deliberately NOT onboarded: datastore_search and datastore_search_sql (absent
-- from this CKAN install — HTTP 400 "Action name not known"), every write
-- action (403 to anonymous callers, and this listing sells reads), and
-- user_list (blocked upstream at nginx). The downstream publisher APIs this
-- register points at — RDW's Socrata service in particular, which is the best
-- genuinely-callable company dataset in the catalogue — deserve their own
-- listings rather than being proxied through a discovery endpoint.
