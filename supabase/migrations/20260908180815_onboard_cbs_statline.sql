-- Onboard CBS (Centraal Bureau voor de Statistiek) StatLine OData v4 as one
-- managed source: the sector context layer under a filed KVK jaarrekening.
--
-- WHY THIS SOURCE
-- ---------------------------------------------------------------------------
-- Apiosk already sells one company's filed annual accounts. Those figures are
-- authoritative and, on their own, uninterpretable: a EUR 682,987 balance
-- sheet with negative equity is only alarming relative to the branch it sits
-- in. Every KVK jaarrekening response carries an `SbiBusinessCode` and a
-- `FinancialYear`, and CBS publishes both dimensions. The six curated tables
-- below are the ones that answer "what does this company's sector look like"
-- with official statistics rather than a model's guess.
--
-- CBS already appears in this catalogue, but only as the three geographic
-- slices PDOK republishes (postcode-6, postcode-4, buurten — onboard-pdok.sql
-- sections 2.12-2.14). Those are demographics for an address. This is the
-- statistics service itself, keyed on economic activity.
--
-- COST AND PRICE
-- ---------------------------------------------------------------------------
-- Free upstream. No API key, no registration, no quota: verified on
-- 2026-09-08 with ~350 requests (15 rapid sequential, 40 concurrent, 10 heavy
-- 2.9 MB responses) without a single 429, 503 or Retry-After, and with no
-- rate-limit header of any kind in the response.
--
-- Price is EUR 0.05 displayed, which at the published EUR/USD rate of 0.92 is
-- a buyer total of USD 0.054348. Same arithmetic and same rate as
-- 20260908173901_kvk_annual_accounts_ten_euro_cents.sql:
--   list 47,259 + ceil(47,259 x 1500 / 10000) = 47,259 + 7,089 = 54,348 micro-USD
--   54,348 x 0.92 = EUR 0.05000016, displayed as EUR 0.05
-- (gateway/crates/catalog-core/src/fees.rs:195-199 and :213-237.)
-- 0.047259 is not a new constant: kvk-price-and-search-fix.sql already used it
-- as the EUR 0.05 tier for the KVK open-data endpoints.
--
-- KNOWN INCONSISTENCY, recorded rather than hidden. The CBS demographic
-- endpoints PDOK republishes (onboard-pdok.sql sections 2.12-2.14) sell at
-- list 0.017391, i.e. EUR 0.02. After this migration the catalogue therefore
-- carries CBS data at two prices, which is the exact thing
-- kvk-seven-cents-except-search.sql argued against: "two prices for the same
-- kind of answer is a menu that asks the reader to work out which endpoint is
-- the cheap one." The defence is that they are not the same answer — PDOK
-- sells CBS neighbourhood demographics for an ADDRESS as GeoJSON, this sells
-- CBS economic statistics for an SBI BRANCH — but a buyer who reads "CBS" in
-- both listings will not see that distinction from the price alone. If the two
-- are ever meant to match, this is the file to change.
--
-- cache_ttl_seconds is a day everywhere. These are annual and quarterly
-- statistics; the freshest table here moves once a month and most move once a
-- year. A day of cache costs a buyer nothing in accuracy and removes repeat
-- load from a free public service.
--
-- DELIBERATELY NO EXECUTION CONTRACTS
-- ---------------------------------------------------------------------------
-- This migration creates no `capability_execution_contracts` rows, and that is
-- a considered decision rather than an omission.
--
-- A CBS observation is selected by an OData `$filter` expression:
--     $filter=BedrijfstakkenBranchesSBI2008 eq '397200' and Perioden eq '2024JJ00'
-- The gateway v2 planner binds a fact to a parameter by putting the fact's
-- value in as the WHOLE parameter value — gateway/src/input_binding.rs:17-23
-- refuses, on purpose, to let a caller value become part of a field name or a
-- URL segment ("Never interpret a caller value as a field name, array index or
-- URL segment"). There is therefore no way today to compose that filter from a
-- KVK SBI code without either handing an untrusted expression through as a
-- fact, or teaching the binder a substitution form. Neither belongs in an
-- onboarding migration.
--
-- So CBS ships the way 20 of PDOK's 21 services and all 60 EODHD endpoints
-- already ship: executable, discoverable and documented, driven by a caller or
-- a model that writes the `$filter` itself. The endpoint descriptions below
-- carry everything needed to write one correctly, because in this catalogue
-- the description IS the interface.
--
-- The follow-up that would unlock planner-driven, SBI-parameterised CBS calls
-- is a reviewed template binding on `capability_execution_contracts.input_map`
-- ({"in":"query","name":"$filter","template":"Dim eq '{}'"}), where the
-- template is catalog-authored and the substituted value is validated against
-- the fact type's own `pattern` first. That is a change to a security-relevant
-- binding path and is out of scope here.
--
-- THE SBI TRAP, WHICH EVERY DESCRIPTION BELOW REPEATS
-- ---------------------------------------------------------------------------
-- CBS does NOT key its branch dimension on the SBI code. The dimension key is
-- an opaque internal identifier ('397200', '301000', 'T001081'); the SBI code
-- lives only as the leading token of the `Title` ('642 Financiele holdings').
-- There is no SbiCode field anywhere in the Codes entity set — the full row
-- schema is Identifier, Index, Title, Description, DimensionGroupId, verified
-- live.
--
-- Resolve it in ONE call: fetch a table's BedrijfstakkenBranchesSBI2008Codes
-- with $select=Identifier,Title and NO $filter — between 104 and 1487 rows
-- depending on the table — and match the leading token client-side, keeping the
-- longest prefix of the KVK code that hits. Truncating server-side instead
-- (startswith(Title,'64210 '), then '6421 ', then '642 ') is up to four charged
-- calls for one lookup. If you do filter server-side, the TRAILING SPACE is
-- mandatory: '64' also matches '641', '642' and '649'.
--
-- Titles carry Dutch diacritics ('642 Financiële holdings'). Match on the
-- numeric prefix, never on the label text; the SQL in this file writes those
-- labels without accents for portability and they are NOT match keys.
--
-- The Identifier<->Title binding is stable across tables (verified across 6
-- tables and 517 identifiers: 0 conflicts), but WHICH codes a table publishes
-- is per-table and incomplete. Never climb past the section letter to
-- 'T001081 A-U Alle economische activiteiten' — that would hand a buyer the
-- national total labelled as their sector.
--
-- Every endpoint below states which SBI depth it publishes and which sections
-- it omits, because getting a plausible number for the wrong population is the
-- only real failure mode of this source.
--
-- NO PAYOUT, NO WALLET, NO SETTLEMENT ROUTE
-- ---------------------------------------------------------------------------
-- There is no provider to pay here. CBS publishes StatLine as open data for
-- nothing, so a buyer's EUR 0.05 has no upstream leg: Apiosk keeps all of it.
-- This migration therefore sets `settlement_mode = 'apiosk_revenue'` and
-- `requires_payout_wallet = false`, and writes NO wallet_address and NO
-- payout_network at all. Asserting a payout address for a source that can
-- never receive one is not paperwork, it is a claim that is false.
--
-- That is also load-bearing rather than tidy. `require_payout_wallet_to_publish()`
-- (provider repo, 20260728, narrowed but NOT removed by 20260901) raises
-- 23514 for any row ENTERING status 'pending' or 'active' whose
-- wallet_address is empty, malformed, or the all-zero burn address — and the
-- column DEFAULTS to the burn address. An earlier draft of this file inserted
-- status 'active' with the burn address copied from onboard-kvk.sql and was
-- rejected by that trigger, which was right to reject it: on EVM the zero
-- address destroys anything routed to it.
--
-- So the row is inserted as 'inactive', the staged state EODHD uses
-- (20260908012000_onboard_eodhd_staged_source.sql), which keeps it out of both
-- the review queue and the payout guard. Whether this listing ever needs a
-- treasury address to go live is a platform-settlement question, not a
-- property of CBS, and it is deliberately not answered here.
--
-- APPLY : via the Supabase CLI migration history, then
--         `node scripts/sync-migration.mjs` to mirror it into app/.
-- AFTER : the source is inserted as a DRAFT (is_verified/active/discoverable
--         false). Flip it live in a separate migration once the endpoints have
--         been probed from the production egress IP.

begin;

do $migration$
declare
  source_id constant uuid := 'c5939334-ac41-40d0-80d5-ced0101a7ad6';
  platform_owner constant uuid := 'bf085fe8-41c4-4699-978f-e75e89518bf7';
  public_metadata jsonb := jsonb_build_object(
    'logo', 'https://www.cbs.nl/-/media/cbs/images/logo/cbs-logo.svg',
    'billing', jsonb_build_object('mode', 'apiosk_balance'),
    'base_url', 'https://datasets.cbs.nl/odata/v1/CBS',
    'operator_notice', 'Official statistics published by Statistics Netherlands (CBS). Open data, no API key.',
    'license', jsonb_build_object(
      'name', 'CC BY 4.0',
      'attribution', 'Bron: CBS',
      'commercial_use', 'permitted with attribution',
      'cost_upstream', 'EUR 0.00'
    ),
    'tags', jsonb_build_array(
      'cbs', 'statline', 'centraal bureau voor de statistiek', 'statistics netherlands',
      'statistiek', 'statistics', 'open data', 'odata', 'odata v4',
      'sbi', 'sbi-code', 'sbi 2008', 'bedrijfstak', 'branche', 'sector',
      'benchmark', 'sectorcijfers', 'branchecijfers',
      'faillissementen', 'bankruptcies', 'insolventie',
      'eigen vermogen', 'equity', 'balanstotaal', 'solvabiliteit',
      'bedrijfsopbrengsten', 'netto-omzet', 'turnover', 'bedrijfsresultaat',
      'operating result', 'personele kosten', 'afschrijvingen',
      'bedrijven', 'ondernemingen', 'bedrijfsgrootte', 'opheffingen',
      'omzetontwikkeling', 'nederland', 'netherlands', 'dutch'
    ),
    'sectors', jsonb_build_array('Government and public data', 'Company data', 'Finance', 'Economic statistics'),
    'service_groups', jsonb_build_array(
      jsonb_build_object(
        'name', 'Catalogue and code books',
        'description', 'Find a CBS table, then resolve its SBI branch codes, measure codes and periods. One code-book path per table serves all of that table''s Codes, Groups and Dimensions collections. These are the prerequisites for every filtered Observations call.',
        'endpoint_paths', jsonb_build_array(
          '/odata/v1/CBS/Datasets',
          '/odata/v1/CBS/81837NED',
          '/odata/v1/CBS/81156ned',
          '/odata/v1/CBS/82244NED',
          '/odata/v1/CBS/81589NED',
          '/odata/v1/CBS/83149NED',
          '/odata/v1/CBS/86414NED'
        )
      ),
      jsonb_build_object(
        'name', 'Sector financials',
        'description', 'Balance sheet and profit-and-loss aggregates per SBI branch — the benchmark layer under a filed annual account.',
        'endpoint_paths', jsonb_build_array(
          '/odata/v1/CBS/81837NED/Observations',
          '/odata/v1/CBS/81156ned/Observations'
        )
      ),
      jsonb_build_object(
        'name', 'Sector risk and demography',
        'description', 'Bankruptcies, company stock by size class, closures and turnover momentum per SBI branch.',
        'endpoint_paths', jsonb_build_array(
          '/odata/v1/CBS/82244NED/Observations',
          '/odata/v1/CBS/81589NED/Observations',
          '/odata/v1/CBS/83149NED/Observations',
          '/odata/v1/CBS/86414NED/Observations'
        )
      )
    )
  );
begin
  if not exists (select 1 from public.provider_profiles where id = platform_owner) then
    raise exception 'Apiosk platform owner % is missing', platform_owner;
  end if;

  if exists (select 1 from public.apis where slug = 'cbs-statline' and id <> source_id) then
    raise exception 'The slug cbs-statline already belongs to another source id';
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
    'cbs-statline',
    'CBS — Statistics Netherlands (StatLine open data)',
    'inactive',
    'Official Dutch statistics per economic sector (SBI): bankruptcies, sector balance sheets and profit-and-loss, company counts by size class, closures and turnover momentum.',
    'StatLine is the statistical database of CBS, the Dutch national statistics office, published as an open OData v4 service with no API key and no upstream fee. This listing curates the six tables that put a single company''s filed annual accounts in context, plus the four calls needed to address them. Sector financials: 81837NED gives equity (eigen vermogen) and total assets (balanstotaal) per branch for all limited companies in the non-financial sector — the same population that files a jaarrekening with KVK — and 81156ned gives the full cost and revenue structure (bedrijfsopbrengsten, netto-omzet, inkoopwaarde, personele kosten, afschrijvingen, bedrijfsresultaat, resultaat voor belastingen). Sector risk: 82244NED counts court-declared bankruptcies per branch monthly and annually, 81589NED gives the stock of businesses per branch and size class down to 5-digit SBI, 83149NED gives closures per branch and size class, and 86414NED gives the share of businesses in a branch whose turnover rose, held or fell. IMPORTANT for anyone writing a query: CBS does not key its branch dimension on the SBI code. The dimension key is an opaque identifier such as 397200 and the SBI code appears only as the leading token of the Title, so resolve it with startswith(Title,''642 '') — the trailing space is mandatory — and truncate a 5-digit KVK code one digit at a time until it matches. Two of the six tables (81837NED, 81156ned) and 86414NED exclude SBI section K, so companies in finance (64-66) have no benchmark there and must fall back to 82244NED or 81589NED. Open data under CC BY 4.0 with attribution to CBS.',
    'data',
    'https://www.cbs.nl/en-gb/our-services/open-data/statline-as-open-data',
    'https://www.cbs.nl/',
    'https://datasets.cbs.nl',
    'https://datasets.cbs.nl',
    jsonb_build_object(
      'type', 'none',
      'caller', 'Pay per request through the Apiosk balance. CBS StatLine requires no API key from anyone.',
      'upstream', jsonb_build_object(
        'note', 'CBS StatLine OData v4 is unauthenticated open data. No credential is stored or forwarded for this source.'
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

-- Endpoints. Every response_example below is a real trimmed body observed on
-- 2026-09-08; nothing here is a hand-written example of what CBS might send.
insert into public.api_endpoints (
  id, api_id, method, path, price, active, cache_ttl_seconds, description,
  query_params, response_body, request_example, response_example,
  tags, sectors, use_cases, endpoint_category, action_type, payment_required,
  external_docs_url, documentation_markdown, metadata_source, metadata_confidence
)
values
(
  'e9165ca8-bdd4-4736-8315-e070c2d34b11', 'c5939334-ac41-40d0-80d5-ced0101a7ad6', 'GET', '/odata/v1/CBS/Datasets', 0.047259, true, 86400,
  'Search the CBS StatLine catalogue — 4,871 datasets. Zoek een CBS-tabel op titel, onderwerp of status. Returns each dataset''s Identifier (the table id used by every other call here, e.g. 82244NED), Title, Description, Status, ObservationCount, Modified and ReleaseDate. Filter with $filter=contains(Title,''Faillissementen''), and ALWAYS add "and Status eq ''Regulier''" unless you want discontinued tables: 71 of the 105 SBI-keyed tables are Gediscontinueerd and still returned, leaving 34 Regulier. Use this to find a table, then /Dimensions to learn how it is keyed.',
  '{"type":"object","properties":{"$filter":{"type":"string","required":false,"description":"OData filter over the catalogue, e.g. contains(Title,''Faillissementen'') and Status eq ''Regulier''. Supported: eq, ne, and, or, contains, startswith, endswith, tolower.","x-parameter-location":"query"},"$select":{"type":"string","required":false,"description":"Comma-separated fields to return, e.g. Identifier,Title,Status,ObservationCount.","x-parameter-location":"query"},"$top":{"type":"integer","required":false,"description":"Maximum datasets to return. Maximum 100000; above that the API returns HTTP 400.","x-parameter-location":"query"},"$skip":{"type":"integer","required":false,"description":"Rows to skip, for paging.","x-parameter-location":"query"},"$orderby":{"type":"string","required":false,"description":"Sort expression, e.g. Modified desc.","x-parameter-location":"query"},"$count":{"type":"boolean","required":false,"description":"Set true to include @odata.count, the total number of matching datasets.","x-parameter-location":"query"}}}'::jsonb,
  '{"type":"object","properties":{"@odata.count":{"type":"integer","description":"Total matches, present only when $count=true."},"value":{"type":"array","items":{"type":"object","properties":{"Identifier":{"type":"string","description":"Table id, e.g. 82244NED. Case matters in the URL: 81156ned is lower case."},"Title":{"type":"string"},"Description":{"type":"string"},"Status":{"type":"string","description":"Regulier = actively maintained. Gediscontinueerd or Stopgezet = frozen; the data is still served."},"ObservationCount":{"type":"integer"},"Modified":{"type":"string"},"ReleaseDate":{"type":"string"},"Language":{"type":"string"},"Catalog":{"type":"string"},"Distributions":{"type":"array"}}}}}}'::jsonb,
  '{"$filter":"contains(Title,''Faillissementen'') and Status eq ''Regulier''","$select":"Identifier,Title,Status,ObservationCount","$top":3}'::jsonb,
  '{"@odata.context":"https://datasets.cbs.nl/odata/v1/CBS/$metadata#Datasets(Identifier,Title,Status,ObservationCount)","value":[{"Identifier":"82242NED","Title":"Faillissementen; kerncijfers","Status":"Regulier","ObservationCount":3096},{"Identifier":"82243NED","Title":"Faillissementen; natuurlijke personen, regio","Status":"Regulier","ObservationCount":53640},{"Identifier":"82244NED","Title":"Faillissementen; bedrijven en instellingen, SBI 2008","Status":"Regulier","ObservationCount":332568}]}'::jsonb,
  array['cbs','statline','catalogue','dataset search','odata'], array['Economic statistics','Government and public data'], array['Find a CBS table by subject','Check whether a CBS table is still maintained','List every SBI-keyed statistics table'],
  'statistics', 'search', true, 'https://www.cbs.nl/nl-nl/onze-diensten/open-data/open-data-v4/snelstartgids-odata-v4',
  'GET /odata/v1/CBS/Datasets — CBS StatLine catalogue search. Filter Status eq ''Regulier'' to exclude discontinued tables.', 'provider_documentation_and_live_probe', 1
),
(
  'fa6659b4-98f8-4b2a-b493-d5b24879afc3', 'c5939334-ac41-40d0-80d5-ced0101a7ad6', 'GET', '/odata/v1/CBS/81837NED', 0.047259, true, 86400,
  'Code books for CBS table 81837NED — "Financien alle ondernemingen; niet-financiele sector". This one path serves every metadata collection of the table, because the gateway resolves a stored path as a literal prefix: /odata/v1/CBS/81837NED/BedrijfstakkenBranchesSBI2008Codes, /MeasureCodes, /PeriodenCodes, /Dimensions and /MeasureGroups all bill and cache against this row, while /Observations has its own. Dimensions of this table: BedrijfstakkenBranchesSBI2008 and Perioden. Key measures: M000268 Eigen vermogen, M000360 Balanstotaal, M002394 Ondernemingengroep (the entity count behind each aggregate). RESOLVING AN SBI CODE, THE CHEAP WAY: fetch BedrijfstakkenBranchesSBI2008Codes ONCE with $select=Identifier,Title and no $filter — 104 rows, one call, one charge — then match client-side. CBS carries no SBI field: the code is the leading token of Title ("642 Financiele holdings"), so take everything before the first space and compare. Truncate the KVK code (64210, 6421, 642, 64) and keep the LONGEST match; report which level you matched, because a division-level figure presented as a subclass figure is a wrong answer that looks right. Do NOT issue one startswith($filter) per truncation — that is up to four charged calls for one lookup; if you must filter server-side, chain them with or, and keep the TRAILING SPACE (startswith(Title,''642 '')), because ''64'' also matches 641, 642 and 649. Never fall back to T001081 "A-U Alle economische activiteiten" — that is the national total, not a sector. Skip composite titles ("4711, 472 ...", "24-30, 33 ...", "B-E ...") and unparseable ones ("SBI-code onbekend"). Titles carry Dutch diacritics; match on the numeric prefix, never on the label text. SBI depth: 104 branch codes, mostly 2-digit SBI. Non-financial sector only: SBI section K (64-66) is absent, so a company in 64210 cannot be resolved here at all. Annual, PeriodenCodes are YYYYJJ00, latest 2024JJ00.',
  '{"type":"object","properties":{"$filter":{"type":"string","required":false,"description":"OData filter over the collection, e.g. startswith(Title,''642 '') on the branch codes. Omit it to fetch the whole code list in one call, which is usually cheaper.","x-parameter-location":"query"},"$select":{"type":"string","required":false,"description":"Comma-separated fields, e.g. Identifier,Title.","x-parameter-location":"query"},"$top":{"type":"integer","required":false,"description":"Maximum rows. Maximum 100000. Supplying $top suppresses @odata.nextLink, so rows beyond it are silently lost.","x-parameter-location":"query"},"$orderby":{"type":"string","required":false,"description":"Sort expression.","x-parameter-location":"query"}}}'::jsonb,
  '{"type":"object","properties":{"value":{"type":"array","items":{"type":"object","properties":{"Identifier":{"type":"string","description":"The opaque CBS key an Observations $filter compares against, e.g. 397200. It has no arithmetic relation to the SBI code."},"Index":{"type":"integer"},"Title":{"type":"string","description":"An SBI token then a Dutch label. The token may be a section letter (A), a range (B-E, 24-30, 33) or absent."},"Description":{"type":"string"},"DimensionGroupId":{"type":"string"},"Unit":{"type":"string","description":"On MeasureCodes only, e.g. Aantal, x mln euro, %."},"DataType":{"type":"string","description":"On MeasureCodes only. A String measure fills StringValue, not Value."}}}}}}'::jsonb,
  '{"$select":"Identifier,Title","$top":3}'::jsonb,
  '{"@odata.context":"https://datasets.cbs.nl/odata/v1/CBS/81837NED/$metadata#BedrijfstakkenBranchesSBI2008Codes(Identifier,Title)","value":[{"Identifier":"T001081","Title":"A-U Alle economische activiteiten"}]}'::jsonb,
  array['cbs','81837NED','sbi','sbi 2008','sbi-code','bedrijfstak','branche','code resolution','measure codes','metadata'], array['Economic statistics','Company data'], array['Turn a KVK SBI code into a CBS branch key for 81837NED','Read the measure labels and units of 81837NED','Check whether 81837NED covers a company''s sector'],
  'statistics', 'lookup', true, 'https://opendata.cbs.nl/statline/portal.html?_la=nl&_catalog=CBS&tableId=81837NED',
  'GET /odata/v1/CBS/81837NED — code books for 81837NED. Literal path: every Codes/Groups/Dimensions collection under it resolves here.', 'provider_documentation_and_live_probe', 1
),
(
  '57ec7089-7d33-4ce9-b1d8-f792a7384a44', 'c5939334-ac41-40d0-80d5-ced0101a7ad6', 'GET', '/odata/v1/CBS/81156ned', 0.047259, true, 86400,
  'Code books for CBS table 81156ned — "Bedrijfsleven; arbeids- en financiele gegevens, per branche". This one path serves every metadata collection of the table, because the gateway resolves a stored path as a literal prefix: /odata/v1/CBS/81156ned/BedrijfstakkenBranchesSBI2008Codes, /MeasureCodes, /PeriodenCodes, /Dimensions and /MeasureGroups all bill and cache against this row, while /Observations has its own. Dimensions of this table: BedrijfstakkenBranchesSBI2008 and Perioden. Measure titles here are not unique on their own — many read simply Totaal — so pair a group code ending _1 with its total ending _2, e.g. M002065_1 Personele kosten with M002065_2 Totaal. NOTE the table id is LOWER CASE in the URL. RESOLVING AN SBI CODE, THE CHEAP WAY: fetch BedrijfstakkenBranchesSBI2008Codes ONCE with $select=Identifier,Title and no $filter — 320 rows, one call, one charge — then match client-side. CBS carries no SBI field: the code is the leading token of Title ("642 Financiele holdings"), so take everything before the first space and compare. Truncate the KVK code (64210, 6421, 642, 64) and keep the LONGEST match; report which level you matched, because a division-level figure presented as a subclass figure is a wrong answer that looks right. Do NOT issue one startswith($filter) per truncation — that is up to four charged calls for one lookup; if you must filter server-side, chain them with or, and keep the TRAILING SPACE (startswith(Title,''642 '')), because ''64'' also matches 641, 642 and 649. Never fall back to T001081 "A-U Alle economische activiteiten" — that is the national total, not a sector. Skip composite titles ("4711, 472 ...", "24-30, 33 ...", "B-E ...") and unparseable ones ("SBI-code onbekend"). Titles carry Dutch diacritics; match on the numeric prefix, never on the label text. SBI depth: 320 branch codes, down to 4-digit SBI. Non-financial business economy only: sections K, O, P, Q, R, S94, T and U are absent. Annual, PeriodenCodes are YYYYJJ00, latest 2024JJ00.',
  '{"type":"object","properties":{"$filter":{"type":"string","required":false,"description":"OData filter over the collection, e.g. startswith(Title,''642 '') on the branch codes. Omit it to fetch the whole code list in one call, which is usually cheaper.","x-parameter-location":"query"},"$select":{"type":"string","required":false,"description":"Comma-separated fields, e.g. Identifier,Title.","x-parameter-location":"query"},"$top":{"type":"integer","required":false,"description":"Maximum rows. Maximum 100000. Supplying $top suppresses @odata.nextLink, so rows beyond it are silently lost.","x-parameter-location":"query"},"$orderby":{"type":"string","required":false,"description":"Sort expression.","x-parameter-location":"query"}}}'::jsonb,
  '{"type":"object","properties":{"value":{"type":"array","items":{"type":"object","properties":{"Identifier":{"type":"string","description":"The opaque CBS key an Observations $filter compares against, e.g. 397200. It has no arithmetic relation to the SBI code."},"Index":{"type":"integer"},"Title":{"type":"string","description":"An SBI token then a Dutch label. The token may be a section letter (A), a range (B-E, 24-30, 33) or absent."},"Description":{"type":"string"},"DimensionGroupId":{"type":"string"},"Unit":{"type":"string","description":"On MeasureCodes only, e.g. Aantal, x mln euro, %."},"DataType":{"type":"string","description":"On MeasureCodes only. A String measure fills StringValue, not Value."}}}}}}'::jsonb,
  '{"$select":"Identifier,Title","$top":3}'::jsonb,
  '{"@odata.context":"https://datasets.cbs.nl/odata/v1/CBS/81156ned/$metadata#BedrijfstakkenBranchesSBI2008Codes(Identifier,Title)","value":[{"Identifier":"T001081","Title":"A-U Alle economische activiteiten"}]}'::jsonb,
  array['cbs','81156ned','sbi','sbi 2008','sbi-code','bedrijfstak','branche','code resolution','measure codes','metadata'], array['Economic statistics','Company data'], array['Turn a KVK SBI code into a CBS branch key for 81156ned','Read the measure labels and units of 81156ned','Check whether 81156ned covers a company''s sector'],
  'statistics', 'lookup', true, 'https://opendata.cbs.nl/statline/portal.html?_la=nl&_catalog=CBS&tableId=81156ned',
  'GET /odata/v1/CBS/81156ned — code books for 81156ned. Literal path: every Codes/Groups/Dimensions collection under it resolves here.', 'provider_documentation_and_live_probe', 1
),
(
  '51091134-df8d-47fc-a990-8d604b25689b', 'c5939334-ac41-40d0-80d5-ced0101a7ad6', 'GET', '/odata/v1/CBS/82244NED', 0.047259, true, 86400,
  'Code books for CBS table 82244NED — "Faillissementen; bedrijven en instellingen, SBI 2008". This one path serves every metadata collection of the table, because the gateway resolves a stored path as a literal prefix: /odata/v1/CBS/82244NED/BedrijfstakkenBranchesSBI2008Codes, /MeasureCodes, /PeriodenCodes, /Dimensions and /MeasureGroups all bill and cache against this row, while /Observations has its own. Dimensions of this table: BedrijfstakkenBranchesSBI2008, TypeGefailleerde and Perioden. TypeGefailleerdeCodes matters as much as the branch codes: A047596 is the total across types, and leaving TypeGefailleerde unpinned in an Observations filter returns the same event under several breakdowns. The single measure is M001327 Uitgesproken faillissementen, unit Aantal. RESOLVING AN SBI CODE, THE CHEAP WAY: fetch BedrijfstakkenBranchesSBI2008Codes ONCE with $select=Identifier,Title and no $filter — 372 rows, one call, one charge — then match client-side. CBS carries no SBI field: the code is the leading token of Title ("642 Financiele holdings"), so take everything before the first space and compare. Truncate the KVK code (64210, 6421, 642, 64) and keep the LONGEST match; report which level you matched, because a division-level figure presented as a subclass figure is a wrong answer that looks right. Do NOT issue one startswith($filter) per truncation — that is up to four charged calls for one lookup; if you must filter server-side, chain them with or, and keep the TRAILING SPACE (startswith(Title,''642 '')), because ''64'' also matches 641, 642 and 649. Never fall back to T001081 "A-U Alle economische activiteiten" — that is the national total, not a sector. Skip composite titles ("4711, 472 ...", "24-30, 33 ...", "B-E ...") and unparseable ones ("SBI-code onbekend"). Titles carry Dutch diacritics; match on the numeric prefix, never on the label text. SBI depth: 372 branch codes, down to 3-digit SBI. This is the ONLY table in the listing that covers SBI section K, so it is the fallback for finance: 396400 is 64 Bankwezen, 397200 is 642 Financiele holdings. Mixed frequencies in one table: YYYYJJ00 annual, YYYYKW0n quarterly, YYYYMM0n monthly.',
  '{"type":"object","properties":{"$filter":{"type":"string","required":false,"description":"OData filter over the collection, e.g. startswith(Title,''642 '') on the branch codes. Omit it to fetch the whole code list in one call, which is usually cheaper.","x-parameter-location":"query"},"$select":{"type":"string","required":false,"description":"Comma-separated fields, e.g. Identifier,Title.","x-parameter-location":"query"},"$top":{"type":"integer","required":false,"description":"Maximum rows. Maximum 100000. Supplying $top suppresses @odata.nextLink, so rows beyond it are silently lost.","x-parameter-location":"query"},"$orderby":{"type":"string","required":false,"description":"Sort expression.","x-parameter-location":"query"}}}'::jsonb,
  '{"type":"object","properties":{"value":{"type":"array","items":{"type":"object","properties":{"Identifier":{"type":"string","description":"The opaque CBS key an Observations $filter compares against, e.g. 397200. It has no arithmetic relation to the SBI code."},"Index":{"type":"integer"},"Title":{"type":"string","description":"An SBI token then a Dutch label. The token may be a section letter (A), a range (B-E, 24-30, 33) or absent."},"Description":{"type":"string"},"DimensionGroupId":{"type":"string"},"Unit":{"type":"string","description":"On MeasureCodes only, e.g. Aantal, x mln euro, %."},"DataType":{"type":"string","description":"On MeasureCodes only. A String measure fills StringValue, not Value."}}}}}}'::jsonb,
  '{"$select":"Identifier,Title","$top":3}'::jsonb,
  '{"@odata.context":"https://datasets.cbs.nl/odata/v1/CBS/82244NED/$metadata#BedrijfstakkenBranchesSBI2008Codes(Identifier,Title)","value":[{"Identifier":"T001081","Title":"A-U Alle economische activiteiten"},{"Identifier":"301000","Title":"A Landbouw, bosbouw en visserij"},{"Identifier":"301100","Title":"01 Landbouw"}]}'::jsonb,
  array['cbs','82244NED','sbi','sbi 2008','sbi-code','bedrijfstak','branche','code resolution','measure codes','metadata'], array['Economic statistics','Company data'], array['Turn a KVK SBI code into a CBS branch key for 82244NED','Read the measure labels and units of 82244NED','Check whether 82244NED covers a company''s sector'],
  'statistics', 'lookup', true, 'https://opendata.cbs.nl/statline/portal.html?_la=nl&_catalog=CBS&tableId=82244NED',
  'GET /odata/v1/CBS/82244NED — code books for 82244NED. Literal path: every Codes/Groups/Dimensions collection under it resolves here.', 'provider_documentation_and_live_probe', 1
),
(
  '5e8cd995-1b19-47a8-b539-721873e169b6', 'c5939334-ac41-40d0-80d5-ced0101a7ad6', 'GET', '/odata/v1/CBS/81589NED', 0.047259, true, 86400,
  'Code books for CBS table 81589NED — "Bedrijven; bedrijfstak". This one path serves every metadata collection of the table, because the gateway resolves a stored path as a literal prefix: /odata/v1/CBS/81589NED/BedrijfstakkenBranchesSBI2008Codes, /MeasureCodes, /PeriodenCodes, /Dimensions and /MeasureGroups all bill and cache against this row, while /Observations has its own. Dimensions of this table: BedrijfstakkenBranchesSBI2008 and Perioden. With 1487 codes this is the one table where fetching the whole code list is worth a moment''s thought; it is still a single call and still one charge. RESOLVING AN SBI CODE, THE CHEAP WAY: fetch BedrijfstakkenBranchesSBI2008Codes ONCE with $select=Identifier,Title and no $filter — 1487 rows, one call, one charge — then match client-side. CBS carries no SBI field: the code is the leading token of Title ("642 Financiele holdings"), so take everything before the first space and compare. Truncate the KVK code (64210, 6421, 642, 64) and keep the LONGEST match; report which level you matched, because a division-level figure presented as a subclass figure is a wrong answer that looks right. Do NOT issue one startswith($filter) per truncation — that is up to four charged calls for one lookup; if you must filter server-side, chain them with or, and keep the TRAILING SPACE (startswith(Title,''642 '')), because ''64'' also matches 641, 642 and 649. Never fall back to T001081 "A-U Alle economische activiteiten" — that is the national total, not a sector. Skip composite titles ("4711, 472 ...", "24-30, 33 ...", "B-E ...") and unparseable ones ("SBI-code onbekend"). Titles carry Dutch diacritics; match on the numeric prefix, never on the label text. SBI depth: 1487 branch codes, down to full 5-digit SBI — the deepest in this listing, so a KVK SbiBusinessCode often matches exactly. Covers the financial sector. Quarterly, PeriodenCodes are YYYYKW0n, latest 2026KW03.',
  '{"type":"object","properties":{"$filter":{"type":"string","required":false,"description":"OData filter over the collection, e.g. startswith(Title,''642 '') on the branch codes. Omit it to fetch the whole code list in one call, which is usually cheaper.","x-parameter-location":"query"},"$select":{"type":"string","required":false,"description":"Comma-separated fields, e.g. Identifier,Title.","x-parameter-location":"query"},"$top":{"type":"integer","required":false,"description":"Maximum rows. Maximum 100000. Supplying $top suppresses @odata.nextLink, so rows beyond it are silently lost.","x-parameter-location":"query"},"$orderby":{"type":"string","required":false,"description":"Sort expression.","x-parameter-location":"query"}}}'::jsonb,
  '{"type":"object","properties":{"value":{"type":"array","items":{"type":"object","properties":{"Identifier":{"type":"string","description":"The opaque CBS key an Observations $filter compares against, e.g. 397200. It has no arithmetic relation to the SBI code."},"Index":{"type":"integer"},"Title":{"type":"string","description":"An SBI token then a Dutch label. The token may be a section letter (A), a range (B-E, 24-30, 33) or absent."},"Description":{"type":"string"},"DimensionGroupId":{"type":"string"},"Unit":{"type":"string","description":"On MeasureCodes only, e.g. Aantal, x mln euro, %."},"DataType":{"type":"string","description":"On MeasureCodes only. A String measure fills StringValue, not Value."}}}}}}'::jsonb,
  '{"$select":"Identifier,Title","$top":3}'::jsonb,
  '{"@odata.context":"https://datasets.cbs.nl/odata/v1/CBS/81589NED/$metadata#BedrijfstakkenBranchesSBI2008Codes(Identifier,Title)","value":[{"Identifier":"T001081","Title":"A-U Alle economische activiteiten"}]}'::jsonb,
  array['cbs','81589NED','sbi','sbi 2008','sbi-code','bedrijfstak','branche','code resolution','measure codes','metadata'], array['Economic statistics','Company data'], array['Turn a KVK SBI code into a CBS branch key for 81589NED','Read the measure labels and units of 81589NED','Check whether 81589NED covers a company''s sector'],
  'statistics', 'lookup', true, 'https://opendata.cbs.nl/statline/portal.html?_la=nl&_catalog=CBS&tableId=81589NED',
  'GET /odata/v1/CBS/81589NED — code books for 81589NED. Literal path: every Codes/Groups/Dimensions collection under it resolves here.', 'provider_documentation_and_live_probe', 1
),
(
  'c991b7c9-eca8-4e5e-a1eb-bdc091d80a17', 'c5939334-ac41-40d0-80d5-ced0101a7ad6', 'GET', '/odata/v1/CBS/83149NED', 0.047259, true, 86400,
  'Code books for CBS table 83149NED — "Bedrijven; opheffingen, bedrijfsgrootte, rechtsvorm, bedrijfstak". This one path serves every metadata collection of the table, because the gateway resolves a stored path as a literal prefix: /odata/v1/CBS/83149NED/BedrijfstakkenBranchesSBI2008Codes, /MeasureCodes, /PeriodenCodes, /Dimensions and /MeasureGroups all bill and cache against this row, while /Observations has its own. Dimensions of this table: BedrijfstakkenBranchesSBI2008, a size-class dimension, a legal-form dimension and Perioden. RESOLVING AN SBI CODE, THE CHEAP WAY: fetch BedrijfstakkenBranchesSBI2008Codes ONCE with $select=Identifier,Title and no $filter — 401 rows, one call, one charge — then match client-side. CBS carries no SBI field: the code is the leading token of Title ("642 Financiele holdings"), so take everything before the first space and compare. Truncate the KVK code (64210, 6421, 642, 64) and keep the LONGEST match; report which level you matched, because a division-level figure presented as a subclass figure is a wrong answer that looks right. Do NOT issue one startswith($filter) per truncation — that is up to four charged calls for one lookup; if you must filter server-side, chain them with or, and keep the TRAILING SPACE (startswith(Title,''642 '')), because ''64'' also matches 641, 642 and 649. Never fall back to T001081 "A-U Alle economische activiteiten" — that is the national total, not a sector. Skip composite titles ("4711, 472 ...", "24-30, 33 ...", "B-E ...") and unparseable ones ("SBI-code onbekend"). Titles carry Dutch diacritics; match on the numeric prefix, never on the label text. SBI depth: 401 branch codes, mostly 2- and 3-digit SBI. Covers the financial sector. Quarterly, PeriodenCodes are YYYYKW0n, latest 2026KW02 — one quarter behind 81589NED.',
  '{"type":"object","properties":{"$filter":{"type":"string","required":false,"description":"OData filter over the collection, e.g. startswith(Title,''642 '') on the branch codes. Omit it to fetch the whole code list in one call, which is usually cheaper.","x-parameter-location":"query"},"$select":{"type":"string","required":false,"description":"Comma-separated fields, e.g. Identifier,Title.","x-parameter-location":"query"},"$top":{"type":"integer","required":false,"description":"Maximum rows. Maximum 100000. Supplying $top suppresses @odata.nextLink, so rows beyond it are silently lost.","x-parameter-location":"query"},"$orderby":{"type":"string","required":false,"description":"Sort expression.","x-parameter-location":"query"}}}'::jsonb,
  '{"type":"object","properties":{"value":{"type":"array","items":{"type":"object","properties":{"Identifier":{"type":"string","description":"The opaque CBS key an Observations $filter compares against, e.g. 397200. It has no arithmetic relation to the SBI code."},"Index":{"type":"integer"},"Title":{"type":"string","description":"An SBI token then a Dutch label. The token may be a section letter (A), a range (B-E, 24-30, 33) or absent."},"Description":{"type":"string"},"DimensionGroupId":{"type":"string"},"Unit":{"type":"string","description":"On MeasureCodes only, e.g. Aantal, x mln euro, %."},"DataType":{"type":"string","description":"On MeasureCodes only. A String measure fills StringValue, not Value."}}}}}}'::jsonb,
  '{"$select":"Identifier,Title","$top":3}'::jsonb,
  '{"@odata.context":"https://datasets.cbs.nl/odata/v1/CBS/83149NED/$metadata#BedrijfstakkenBranchesSBI2008Codes(Identifier,Title)","value":[{"Identifier":"T001081","Title":"A-U Alle economische activiteiten"}]}'::jsonb,
  array['cbs','83149NED','sbi','sbi 2008','sbi-code','bedrijfstak','branche','code resolution','measure codes','metadata'], array['Economic statistics','Company data'], array['Turn a KVK SBI code into a CBS branch key for 83149NED','Read the measure labels and units of 83149NED','Check whether 83149NED covers a company''s sector'],
  'statistics', 'lookup', true, 'https://opendata.cbs.nl/statline/portal.html?_la=nl&_catalog=CBS&tableId=83149NED',
  'GET /odata/v1/CBS/83149NED — code books for 83149NED. Literal path: every Codes/Groups/Dimensions collection under it resolves here.', 'provider_documentation_and_live_probe', 1
),
(
  'bf661549-16ad-4457-be2c-867a55877ab0', 'c5939334-ac41-40d0-80d5-ced0101a7ad6', 'GET', '/odata/v1/CBS/86414NED', 0.047259, true, 86400,
  'Code books for CBS table 86414NED — "Bedrijfsgegevens; omzetontwikkeling (stijgers-dalers), SBI 2008". This one path serves every metadata collection of the table, because the gateway resolves a stored path as a literal prefix: /odata/v1/CBS/86414NED/BedrijfstakkenBranchesSBI2008Codes, /MeasureCodes, /PeriodenCodes, /Dimensions and /MeasureGroups all bill and cache against this row, while /Observations has its own. Dimensions of this table: Bedrijfsgrootte, BedrijfstakkenBranchesSBI2008 and Perioden — three dimensions, so BedrijfsgrootteCodes is needed as well. RESOLVING AN SBI CODE, THE CHEAP WAY: fetch BedrijfstakkenBranchesSBI2008Codes ONCE with $select=Identifier,Title and no $filter — 160 rows, one call, one charge — then match client-side. CBS carries no SBI field: the code is the leading token of Title ("642 Financiele holdings"), so take everything before the first space and compare. Truncate the KVK code (64210, 6421, 642, 64) and keep the LONGEST match; report which level you matched, because a division-level figure presented as a subclass figure is a wrong answer that looks right. Do NOT issue one startswith($filter) per truncation — that is up to four charged calls for one lookup; if you must filter server-side, chain them with or, and keep the TRAILING SPACE (startswith(Title,''642 '')), because ''64'' also matches 641, 642 and 649. Never fall back to T001081 "A-U Alle economische activiteiten" — that is the national total, not a sector. Skip composite titles ("4711, 472 ...", "24-30, 33 ...", "B-E ...") and unparseable ones ("SBI-code onbekend"). Titles carry Dutch diacritics; match on the numeric prefix, never on the label text. SBI depth: 160 branch codes, the coarsest here, so expect to truncate a 5-digit KVK code to 2 digits. No SBI section K. Quarterly, PeriodenCodes are YYYYKW0n, latest 2026KW02.',
  '{"type":"object","properties":{"$filter":{"type":"string","required":false,"description":"OData filter over the collection, e.g. startswith(Title,''642 '') on the branch codes. Omit it to fetch the whole code list in one call, which is usually cheaper.","x-parameter-location":"query"},"$select":{"type":"string","required":false,"description":"Comma-separated fields, e.g. Identifier,Title.","x-parameter-location":"query"},"$top":{"type":"integer","required":false,"description":"Maximum rows. Maximum 100000. Supplying $top suppresses @odata.nextLink, so rows beyond it are silently lost.","x-parameter-location":"query"},"$orderby":{"type":"string","required":false,"description":"Sort expression.","x-parameter-location":"query"}}}'::jsonb,
  '{"type":"object","properties":{"value":{"type":"array","items":{"type":"object","properties":{"Identifier":{"type":"string","description":"The opaque CBS key an Observations $filter compares against, e.g. 397200. It has no arithmetic relation to the SBI code."},"Index":{"type":"integer"},"Title":{"type":"string","description":"An SBI token then a Dutch label. The token may be a section letter (A), a range (B-E, 24-30, 33) or absent."},"Description":{"type":"string"},"DimensionGroupId":{"type":"string"},"Unit":{"type":"string","description":"On MeasureCodes only, e.g. Aantal, x mln euro, %."},"DataType":{"type":"string","description":"On MeasureCodes only. A String measure fills StringValue, not Value."}}}}}}'::jsonb,
  '{"$select":"Identifier,Title","$top":3}'::jsonb,
  '{"@odata.context":"https://datasets.cbs.nl/odata/v1/CBS/86414NED/$metadata#BedrijfstakkenBranchesSBI2008Codes(Identifier,Title)","value":[{"Identifier":"T001081","Title":"A-U Alle economische activiteiten"}]}'::jsonb,
  array['cbs','86414NED','sbi','sbi 2008','sbi-code','bedrijfstak','branche','code resolution','measure codes','metadata'], array['Economic statistics','Company data'], array['Turn a KVK SBI code into a CBS branch key for 86414NED','Read the measure labels and units of 86414NED','Check whether 86414NED covers a company''s sector'],
  'statistics', 'lookup', true, 'https://opendata.cbs.nl/statline/portal.html?_la=nl&_catalog=CBS&tableId=86414NED',
  'GET /odata/v1/CBS/86414NED — code books for 86414NED. Literal path: every Codes/Groups/Dimensions collection under it resolves here.', 'provider_documentation_and_live_probe', 1
),
(
  'abc42f88-a57d-440c-9887-2fcf55724ecc', 'c5939334-ac41-40d0-80d5-ced0101a7ad6', 'GET', '/odata/v1/CBS/81837NED/Observations', 0.047259, true, 86400,
  'Sector BALANCE SHEET per SBI branch — CBS table 81837NED, "Financien alle ondernemingen; niet-financiele sector". The closest thing to a peer group for a filed jaarrekening: its population is limited companies (N.V., B.V., cooperatie), the same population that files annual accounts with KVK, compiled from corporate tax returns and direct survey. Carries M000268 Eigen vermogen and M000360 Balanstotaal per branch, so a filer''s own equity and total assets can be placed against a sector solvency ratio, plus M002394 Ondernemingengroep, the number of entities behind each aggregate, which turns any euro total into a per-company average. Annual, Perioden format YYYYJJ00, latest 2024JJ00. 104 branch codes, so mostly 2-digit SBI: expect to truncate a 5-digit KVK code. COVERAGE GAP: the non-financial sector only — SBI section K (64-66, banking, insurance, holdings) is absent, so a company in 64210 has no benchmark here; use 82244NED or 81589NED instead.',
  '{"type":"object","properties":{"$filter":{"type":"string","required":false,"description":"OData filter. Compose from BedrijfstakkenBranchesSBI2008 (resolve the code first), Perioden and Measure, e.g. BedrijfstakkenBranchesSBI2008 eq ''395000'' and Perioden eq ''2024JJ00''. Supported: eq, ne, and, or, in, startswith, contains. NOT supported: $expand (400 everywhere). $apply is broken rather than absent: groupby returns 500, aggregate returns 400; only $apply=filter works.","x-parameter-location":"query"},"$select":{"type":"string","required":false,"description":"Comma-separated fields, e.g. Measure,Value,Perioden.","x-parameter-location":"query"},"$top":{"type":"integer","required":false,"description":"Maximum rows. WARNING: supplying $top suppresses @odata.nextLink entirely, so rows beyond it are silently lost rather than paged. Omit $top to get the nextLink chain. Maximum 100000.","x-parameter-location":"query"},"$skip":{"type":"integer","required":false,"description":"Rows to skip. This is how @odata.nextLink pages.","x-parameter-location":"query"},"$orderby":{"type":"string","required":false,"description":"Sort expression, e.g. Perioden desc.","x-parameter-location":"query"},"$count":{"type":"boolean","required":false,"description":"Set true to include @odata.count.","x-parameter-location":"query"}}}'::jsonb,
  '{"type":"object","properties":{"@odata.nextLink":{"type":"string","description":"Present only when $top was omitted and more rows exist."},"value":{"type":"array","items":{"type":"object","properties":{"Id":{"type":"integer"},"Measure":{"type":"string","description":"Measure code; resolve via /MeasureCodes."},"ValueAttribute":{"type":"string","description":"None = a real observation. Impossible = structurally not applicable, Value is null. Zero = a measured zero. There is no numeric suppression sentinel in this API."},"Value":{"type":"number","description":"Null when ValueAttribute is Impossible."},"StringValue":{"type":"string","description":"Populated instead of Value when the measure''s DataType is String."},"BedrijfstakkenBranchesSBI2008":{"type":"string"},"Perioden":{"type":"string","description":"YYYYJJ00 for annual."}}}}}}'::jsonb,
  '{"$filter":"BedrijfstakkenBranchesSBI2008 eq ''395000'' and Perioden eq ''2024JJ00'' and (Measure eq ''M002394'' or Measure eq ''M000360'' or Measure eq ''M000268'')","$top":3}'::jsonb,
  '{"@odata.context":"https://datasets.cbs.nl/odata/v1/CBS/81837NED/$metadata#Observations","value":[{"Id":35416,"Measure":"M002394","ValueAttribute":"None","Value":37538.0,"StringValue":null,"BedrijfstakkenBranchesSBI2008":"395000","Perioden":"2024JJ00"},{"Id":35417,"Measure":"M000360","ValueAttribute":"None","Value":182545.3,"StringValue":null,"BedrijfstakkenBranchesSBI2008":"395000","Perioden":"2024JJ00"},{"Id":35428,"Measure":"M000268","ValueAttribute":"None","Value":107136.0,"StringValue":null,"BedrijfstakkenBranchesSBI2008":"395000","Perioden":"2024JJ00"}]}'::jsonb,
  array['cbs','81837NED','eigen vermogen','equity','balanstotaal','total assets','solvabiliteit','sector benchmark','sbi','jaarrekening context'], array['Finance','Economic statistics','Company data'], array['Benchmark a filed balance sheet against its sector','Compute a sector solvency ratio','Get the average equity of companies in an SBI branch'],
  'statistics', 'lookup', true, 'https://opendata.cbs.nl/statline/portal.html?_la=nl&_catalog=CBS&tableId=81837NED',
  'GET /odata/v1/CBS/81837NED/Observations — sector balance sheet (equity, total assets, entity count) per SBI branch. Non-financial sector only: no SBI section K.', 'provider_documentation_and_live_probe', 1
),
(
  '32313c27-c87b-4e17-b301-f0037357b40e', 'c5939334-ac41-40d0-80d5-ced0101a7ad6', 'GET', '/odata/v1/CBS/81156ned/Observations', 0.047259, true, 86400,
  'Sector PROFIT AND LOSS per SBI branch — CBS table 81156ned, "Bedrijfsleven; arbeids- en financiele gegevens, per branche". The full cost and revenue structure of a branch: M001588_2 bedrijfsopbrengsten, M002061 netto-omzet, M002063_2 bedrijfskosten, M002065_2 personele kosten, D002078 afschrijvingen, D002079 bedrijfsresultaat, D002080 saldo financiele baten en lasten, D002081 resultaat voor belastingen — all in millions of euro — plus employment in jobs and labour volume. Lets a filer''s own income statement lines be set beside the branch aggregate and a sector operating margin derived. Annual, Perioden format YYYYJJ00, latest 2024JJ00. 320 branch codes, down to 4-digit SBI, so matches are usually closer than in 81837NED. NOTE the table id is LOWER CASE in the URL. COVERAGE GAP: the non-financial business economy only — sections K (finance), O, P, Q, R, S94, T and U are absent, so a company in 64210 or 88911 has no benchmark here.',
  '{"type":"object","properties":{"$filter":{"type":"string","required":false,"description":"OData filter over BedrijfstakkenBranchesSBI2008, Perioden and Measure, e.g. BedrijfstakkenBranchesSBI2008 eq ''395000'' and Perioden eq ''2024JJ00''. Supported: eq, ne, and, or, in, startswith, contains. NOT supported: $expand (400 everywhere). $apply is broken rather than absent: groupby returns 500, aggregate returns 400; only $apply=filter works.","x-parameter-location":"query"},"$select":{"type":"string","required":false,"description":"Comma-separated fields, e.g. Measure,Value.","x-parameter-location":"query"},"$top":{"type":"integer","required":false,"description":"Maximum rows. WARNING: supplying $top suppresses @odata.nextLink, so rows beyond it are silently lost. Maximum 100000.","x-parameter-location":"query"},"$skip":{"type":"integer","required":false,"description":"Rows to skip.","x-parameter-location":"query"},"$orderby":{"type":"string","required":false,"description":"Sort expression.","x-parameter-location":"query"},"$count":{"type":"boolean","required":false,"description":"Set true to include @odata.count.","x-parameter-location":"query"}}}'::jsonb,
  '{"type":"object","properties":{"@odata.nextLink":{"type":"string"},"value":{"type":"array","items":{"type":"object","properties":{"Id":{"type":"integer"},"Measure":{"type":"string"},"ValueAttribute":{"type":"string","description":"None, Impossible (Value null) or Zero."},"Value":{"type":"number","description":"Millions of euro for the financial measures; thousands of jobs for the employment measures. Read the unit from /MeasureCodes."},"StringValue":{"type":"string"},"BedrijfstakkenBranchesSBI2008":{"type":"string"},"Perioden":{"type":"string"}}}}}}'::jsonb,
  '{"$filter":"BedrijfstakkenBranchesSBI2008 eq ''395000'' and Perioden eq ''2024JJ00'' and Measure in (''2021320_1'',''M001588_2'',''M002061'',''M002063_2'')"}'::jsonb,
  '{"@odata.context":"https://datasets.cbs.nl/odata/v1/CBS/81156ned/$metadata#Observations","value":[{"Id":139559,"Measure":"2021320_1","ValueAttribute":"None","Value":263.3,"StringValue":null,"BedrijfstakkenBranchesSBI2008":"395000","Perioden":"2024JJ00"},{"Id":139563,"Measure":"M001588_2","ValueAttribute":"None","Value":71307.0,"StringValue":null,"BedrijfstakkenBranchesSBI2008":"395000","Perioden":"2024JJ00"},{"Id":139564,"Measure":"M002061","ValueAttribute":"None","Value":66339.0,"StringValue":null,"BedrijfstakkenBranchesSBI2008":"395000","Perioden":"2024JJ00"},{"Id":139566,"Measure":"M002063_2","ValueAttribute":"None","Value":65013.0,"StringValue":null,"BedrijfstakkenBranchesSBI2008":"395000","Perioden":"2024JJ00"}]}'::jsonb,
  array['cbs','81156ned','bedrijfsopbrengsten','netto-omzet','bedrijfsresultaat','operating result','personele kosten','afschrijvingen','sector benchmark','sbi'], array['Finance','Economic statistics','Company data'], array['Benchmark a filed income statement against its sector','Derive a sector operating margin','Compare a company''s staff costs with its branch'],
  'statistics', 'lookup', true, 'https://opendata.cbs.nl/statline/portal.html?_la=nl&_catalog=CBS&tableId=81156ned',
  'GET /odata/v1/CBS/81156ned/Observations — sector profit and loss per SBI branch. Lower-case table id. Non-financial business economy only.', 'provider_documentation_and_live_probe', 1
),
(
  '2f884bdc-79d7-45ac-9aeb-c52c88e6a0f2', 'c5939334-ac41-40d0-80d5-ced0101a7ad6', 'GET', '/odata/v1/CBS/82244NED/Observations', 0.047259, true, 86400,
  'Bankruptcies per SBI branch — CBS table 82244NED, "Faillissementen; bedrijven en instellingen, SBI 2008". Court-declared bankruptcies (uitgesproken faillissementen, measure M001327) per branch, monthly, quarterly and annually. This is what turns a weak balance sheet into a risk statement. Unlike the two financial tables it DOES cover SBI section K, so it is the fallback for companies in finance: 396400 is "64 Bankwezen", 397200 is "642 Financiele holdings". 372 branch codes down to 3-digit SBI. Perioden mixes frequencies: YYYYJJ00 annual, YYYYKW0n quarterly, YYYYMM0n monthly — pin one or you will mix them in a single sum. PIN TypeGefailleerde TOO: leaving it open returns the same event under several breakdowns; A047596 is the total. The two most recent months are provisional (voorlopig). There is no denominator here, so an unnormalised count means little for a large branch — divide by the company stock from 81589NED for a rate — and check that stock is non-zero first, because 81589NED rounds to multiples of 5 and a rare branch comes back as 0.',
  '{"type":"object","properties":{"$filter":{"type":"string","required":false,"description":"OData filter. Pin BedrijfstakkenBranchesSBI2008, TypeGefailleerde and Perioden, e.g. BedrijfstakkenBranchesSBI2008 eq ''397200'' and TypeGefailleerde eq ''A047596'' and Perioden eq ''2024JJ00''. Supported: eq, ne, and, or, in, startswith, contains. NOT supported: $expand (400 everywhere). $apply is broken rather than absent: groupby returns 500, aggregate returns 400; only $apply=filter works.","x-parameter-location":"query"},"$select":{"type":"string","required":false,"description":"Comma-separated fields.","x-parameter-location":"query"},"$top":{"type":"integer","required":false,"description":"Maximum rows. WARNING: supplying $top suppresses @odata.nextLink, so rows beyond it are silently lost. Maximum 100000.","x-parameter-location":"query"},"$skip":{"type":"integer","required":false,"description":"Rows to skip.","x-parameter-location":"query"},"$orderby":{"type":"string","required":false,"description":"Sort expression, e.g. Perioden desc.","x-parameter-location":"query"},"$count":{"type":"boolean","required":false,"description":"Set true to include @odata.count.","x-parameter-location":"query"}}}'::jsonb,
  '{"type":"object","properties":{"@odata.nextLink":{"type":"string"},"value":{"type":"array","items":{"type":"object","properties":{"Id":{"type":"integer"},"Measure":{"type":"string","description":"M001327 = uitgesproken faillissementen, unit Aantal."},"ValueAttribute":{"type":"string","description":"None, Impossible (Value null) or Zero."},"Value":{"type":"number"},"StringValue":{"type":"string"},"BedrijfstakkenBranchesSBI2008":{"type":"string"},"TypeGefailleerde":{"type":"string","description":"Pin this. A047596 is the total across types."},"Perioden":{"type":"string","description":"YYYYJJ00 annual, YYYYKW0n quarterly, YYYYMM0n monthly — all present in the same table."}}}}}}'::jsonb,
  '{"$filter":"BedrijfstakkenBranchesSBI2008 eq ''397200'' and TypeGefailleerde eq ''A047596'' and Perioden eq ''2024JJ00''"}'::jsonb,
  '{"@odata.context":"https://datasets.cbs.nl/odata/v1/CBS/82244NED/$metadata#Observations","value":[{"Id":76261,"Measure":"M001327","ValueAttribute":"None","Value":336.0,"StringValue":null,"BedrijfstakkenBranchesSBI2008":"397200","TypeGefailleerde":"A047596","Perioden":"2024JJ00"}]}'::jsonb,
  array['cbs','82244NED','faillissementen','bankruptcies','insolventie','sector risk','sbi','distress'], array['Finance','Economic statistics','Company data'], array['Count bankruptcies in a company''s sector','Track a sector''s insolvency trend','Add distress context to a weak balance sheet'],
  'statistics', 'lookup', true, 'https://opendata.cbs.nl/statline/portal.html?_la=nl&_catalog=CBS&tableId=82244NED',
  'GET /odata/v1/CBS/82244NED/Observations — bankruptcies per SBI branch. Pin TypeGefailleerde (A047596 = total) and one Perioden frequency. Covers section K.', 'provider_documentation_and_live_probe', 1
),
(
  '0c385117-f160-404b-8aa4-75e108cf8264', 'c5939334-ac41-40d0-80d5-ced0101a7ad6', 'GET', '/odata/v1/CBS/81589NED/Observations', 0.047259, true, 86400,
  'Company stock per SBI branch and size class — CBS table 81589NED, "Bedrijven; bedrijfstak". The denominator for every rate in this listing, and the most SBI-granular table here: 1487 branch codes, down to full 5-digit SBI, so a KVK SbiBusinessCode often matches exactly. Gives the number of businesses in a branch broken out by size class (1, 2, 3-5, 5-10, 10-20, 20-50, 50-100, 100+ werkzame personen) and by legal-person status, quarterly, latest 2026KW03. Use it to size a peer group, or to turn a bankruptcy or closure count into a rate. Values are ROUNDED TO MULTIPLES OF 5, so the size bands need not sum exactly to the total and a small branch is imprecise — never present a derived percentage from a small cell to more than whole-number precision, and NEVER use a zero cell as a denominator — a returned 0 means "fewer than 3", not "none", so any rate computed against it is undefined rather than merely large. The size bands, the roll-ups and the legal-form split all measure the SAME cell, so summing every measure double-counts.',
  '{"type":"object","properties":{"$filter":{"type":"string","required":false,"description":"OData filter over BedrijfstakkenBranchesSBI2008, Perioden and Measure, e.g. BedrijfstakkenBranchesSBI2008 eq ''395000'' and Perioden eq ''2026KW03''. Supported: eq, ne, and, or, in, startswith, contains. NOT supported: $expand (400 everywhere). $apply is broken rather than absent: groupby returns 500, aggregate returns 400; only $apply=filter works.","x-parameter-location":"query"},"$select":{"type":"string","required":false,"description":"Comma-separated fields.","x-parameter-location":"query"},"$top":{"type":"integer","required":false,"description":"Maximum rows. WARNING: supplying $top suppresses @odata.nextLink. Maximum 100000.","x-parameter-location":"query"},"$skip":{"type":"integer","required":false,"description":"Rows to skip.","x-parameter-location":"query"},"$orderby":{"type":"string","required":false,"description":"Sort expression.","x-parameter-location":"query"},"$count":{"type":"boolean","required":false,"description":"Set true to include @odata.count.","x-parameter-location":"query"}}}'::jsonb,
  '{"type":"object","properties":{"@odata.nextLink":{"type":"string"},"value":{"type":"array","items":{"type":"object","properties":{"Id":{"type":"integer"},"Measure":{"type":"string","description":"Size-class and roll-up measures over the same cell; resolve via /MeasureCodes and do not sum them together."},"ValueAttribute":{"type":"string"},"Value":{"type":"number","description":"Rounded to a multiple of 5."},"StringValue":{"type":"string"},"BedrijfstakkenBranchesSBI2008":{"type":"string"},"Perioden":{"type":"string","description":"YYYYKW0n quarterly."}}}}}}'::jsonb,
  '{"$filter":"BedrijfstakkenBranchesSBI2008 eq ''395000'' and Perioden eq ''2026KW03''","$top":4}'::jsonb,
  '{"@odata.context":"https://datasets.cbs.nl/odata/v1/CBS/81589NED/$metadata#Observations","value":[{"Id":1026286,"Measure":"M000201","ValueAttribute":"None","Value":93060.0,"StringValue":null,"BedrijfstakkenBranchesSBI2008":"395000","Perioden":"2026KW03"},{"Id":1026287,"Measure":"WP19079","ValueAttribute":"None","Value":80120.0,"StringValue":null,"BedrijfstakkenBranchesSBI2008":"395000","Perioden":"2026KW03"}]}'::jsonb,
  array['cbs','81589NED','bedrijven','company count','bedrijfsgrootte','size class','peer group','sbi','denominator'], array['Economic statistics','Company data'], array['Size a company''s peer group','Turn a bankruptcy count into a rate','Find how many businesses share an SBI code'],
  'statistics', 'lookup', true, 'https://opendata.cbs.nl/statline/portal.html?_la=nl&_catalog=CBS&tableId=81589NED',
  'GET /odata/v1/CBS/81589NED/Observations — company stock per SBI branch and size class, down to 5-digit SBI. Values rounded to multiples of 5.', 'provider_documentation_and_live_probe', 1
),
(
  '6c04264c-9f07-4b84-9de9-aa7be8a36822', 'c5939334-ac41-40d0-80d5-ced0101a7ad6', 'GET', '/odata/v1/CBS/83149NED/Observations', 0.047259, true, 86400,
  'Business closures per SBI branch and size class — CBS table 83149NED, "Bedrijven; opheffingen, bedrijfsgrootte, rechtsvorm, bedrijfstak". The other half of the risk picture, and the more common fate of a small B.V. with negative equity than bankruptcy: opheffingen include voluntary wind-downs, not just court-declared insolvency. Quarterly, latest 2026KW02, 401 branch codes, and the financial sector is present. Values are rounded to multiples of 5 like 81589NED, so bands need not sum to the total and a 0 can mean "fewer than 3". This table lags 81589NED by one quarter — do not divide a KW02 closure count by a KW03 company stock; align the periods explicitly.',
  '{"type":"object","properties":{"$filter":{"type":"string","required":false,"description":"OData filter over BedrijfstakkenBranchesSBI2008, Perioden and Measure. Supported: eq, ne, and, or, in, startswith, contains. NOT supported: $expand (400 everywhere). $apply is broken rather than absent: groupby returns 500, aggregate returns 400; only $apply=filter works.","x-parameter-location":"query"},"$select":{"type":"string","required":false,"description":"Comma-separated fields.","x-parameter-location":"query"},"$top":{"type":"integer","required":false,"description":"Maximum rows. WARNING: supplying $top suppresses @odata.nextLink. Maximum 100000.","x-parameter-location":"query"},"$skip":{"type":"integer","required":false,"description":"Rows to skip.","x-parameter-location":"query"},"$orderby":{"type":"string","required":false,"description":"Sort expression.","x-parameter-location":"query"},"$count":{"type":"boolean","required":false,"description":"Set true to include @odata.count.","x-parameter-location":"query"}}}'::jsonb,
  '{"type":"object","properties":{"@odata.nextLink":{"type":"string"},"value":{"type":"array","items":{"type":"object","properties":{"Id":{"type":"integer"},"Measure":{"type":"string"},"ValueAttribute":{"type":"string"},"Value":{"type":"number","description":"Rounded to a multiple of 5; 0 can mean fewer than 3."},"StringValue":{"type":"string"},"BedrijfstakkenBranchesSBI2008":{"type":"string"},"Perioden":{"type":"string","description":"YYYYKW0n quarterly."}}}}}}'::jsonb,
  '{"$filter":"BedrijfstakkenBranchesSBI2008 eq ''395000'' and Perioden eq ''2026KW02''","$top":3}'::jsonb,
  '{"@odata.context":"https://datasets.cbs.nl/odata/v1/CBS/83149NED/$metadata#Observations","value":[{"Id":260920,"Measure":"M002855","ValueAttribute":"None","Value":1395.0,"StringValue":null,"BedrijfstakkenBranchesSBI2008":"395000","Perioden":"2026KW02"},{"Id":260921,"Measure":"WP19117","ValueAttribute":"None","Value":1305.0,"StringValue":null,"BedrijfstakkenBranchesSBI2008":"395000","Perioden":"2026KW02"}]}'::jsonb,
  array['cbs','83149NED','opheffingen','closures','business exit','sector churn','sbi'], array['Economic statistics','Company data'], array['Measure how many businesses leave a sector','Distinguish voluntary wind-down from bankruptcy','Assess whether a sector is shrinking'],
  'statistics', 'lookup', true, 'https://opendata.cbs.nl/statline/portal.html?_la=nl&_catalog=CBS&tableId=83149NED',
  'GET /odata/v1/CBS/83149NED/Observations — business closures per SBI branch and size class. Lags 81589NED by one quarter; rounded to multiples of 5.', 'provider_documentation_and_live_probe', 1
),
(
  'f0c6460b-90f7-4a44-ba4d-572f91e1c44c', 'c5939334-ac41-40d0-80d5-ced0101a7ad6', 'GET', '/odata/v1/CBS/86414NED/Observations', 0.047259, true, 86400,
  'Turnover momentum per SBI branch, as a distribution — CBS table 86414NED, "Bedrijfsgegevens; omzetontwikkeling (stijgers-dalers), SBI 2008". Unusually useful for benchmarking one company because it reports a DISTRIBUTION rather than an aggregate: what percentage of businesses in a branch and size class saw turnover rise, hold steady or fall, split into fast and normal movers. That answers "is this company''s revenue drop its own problem or the whole sector''s" in a way a branch total cannot. The freshest table in this listing. 160 branch codes, the coarsest here, so expect to truncate a 5-digit KVK code to 2 digits. Values are percentages over overlapping sets — the risers total equals fast risers plus normal risers, and likewise for fallers — so never add every measure together. COVERAGE GAP: no SBI section K, so no benchmark for finance.',
  '{"type":"object","properties":{"$filter":{"type":"string","required":false,"description":"OData filter. This table has THREE dimensions — Bedrijfsgrootte (size class, e.g. WP19077), BedrijfstakkenBranchesSBI2008 and Perioden — so pin the size class too or one branch returns a row per class, e.g. Bedrijfsgrootte eq ''WP19077'' and BedrijfstakkenBranchesSBI2008 eq ''395000'' and Perioden eq ''2026KW02''. Supported: eq, ne, and, or, in, startswith, contains. NOT supported: $expand (400 everywhere). $apply is broken rather than absent: groupby returns 500, aggregate returns 400; only $apply=filter works.","x-parameter-location":"query"},"$select":{"type":"string","required":false,"description":"Comma-separated fields.","x-parameter-location":"query"},"$top":{"type":"integer","required":false,"description":"Maximum rows. WARNING: supplying $top suppresses @odata.nextLink. Maximum 100000.","x-parameter-location":"query"},"$skip":{"type":"integer","required":false,"description":"Rows to skip.","x-parameter-location":"query"},"$orderby":{"type":"string","required":false,"description":"Sort expression.","x-parameter-location":"query"},"$count":{"type":"boolean","required":false,"description":"Set true to include @odata.count.","x-parameter-location":"query"}}}'::jsonb,
  '{"type":"object","properties":{"@odata.nextLink":{"type":"string"},"value":{"type":"array","items":{"type":"object","properties":{"Id":{"type":"integer"},"Measure":{"type":"string","description":"Percentage measures over overlapping sets; resolve via /MeasureCodes and do not sum them."},"ValueAttribute":{"type":"string"},"Value":{"type":"number","description":"A percentage."},"StringValue":{"type":"string"},"Bedrijfsgrootte":{"type":"string","description":"Size-class dimension, e.g. WP19077. Present in this table and not in the others; pin it or the same branch returns one row per size class."},"BedrijfstakkenBranchesSBI2008":{"type":"string"},"Perioden":{"type":"string","description":"YYYYKW0n quarterly."}}}}}}'::jsonb,
  '{"$filter":"Bedrijfsgrootte eq ''WP19077'' and BedrijfstakkenBranchesSBI2008 eq ''395000'' and Perioden eq ''2026KW02''","$top":2}'::jsonb,
  '{"@odata.context":"https://datasets.cbs.nl/odata/v1/CBS/86414NED/$metadata#Observations","value":[{"Id":500416,"Measure":"D002756","ValueAttribute":"None","Value":40.0,"StringValue":null,"Bedrijfsgrootte":"WP19077","BedrijfstakkenBranchesSBI2008":"395000","Perioden":"2026KW02"},{"Id":500417,"Measure":"D002838","ValueAttribute":"None","Value":13.0,"StringValue":null,"Bedrijfsgrootte":"WP19077","BedrijfstakkenBranchesSBI2008":"395000","Perioden":"2026KW02"}]}'::jsonb,
  array['cbs','86414NED','omzetontwikkeling','turnover','stijgers','dalers','momentum','sector benchmark','sbi'], array['Finance','Economic statistics','Company data'], array['Tell whether a revenue drop is company-specific or sector-wide','Measure how many peers grew turnover','Add momentum context to a filed income statement'],
  'statistics', 'lookup', true, 'https://opendata.cbs.nl/statline/portal.html?_la=nl&_catalog=CBS&tableId=86414NED',
  'GET /odata/v1/CBS/86414NED/Observations — share of businesses per SBI branch whose turnover rose, held or fell. Overlapping percentage sets; no section K.', 'provider_documentation_and_live_probe', 1
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

-- Guard. onboard-pdok.sql:1410-1442 exists because a slug that does not match
-- production selects zero rows in silence, and kvk-resolvers-live-slug.sql
-- documents what that cost the last time it happened: "Nothing about the
-- engine was wrong. It had nothing to read." Fail loudly here instead.
do $verify$
declare
  source_id constant uuid := 'c5939334-ac41-40d0-80d5-ced0101a7ad6';
  -- Preserve endpoints registered before this migration. Verify the exact
  -- curated set below, rather than assuming the source was empty.
  curated_endpoint_ids constant uuid[] := array[
    'e9165ca8-bdd4-4736-8315-e070c2d34b11'::uuid,
    'fa6659b4-98f8-4b2a-b493-d5b24879afc3'::uuid,
    '57ec7089-7d33-4ce9-b1d8-f792a7384a44'::uuid,
    '51091134-df8d-47fc-a990-8d604b25689b'::uuid,
    '5e8cd995-1b19-47a8-b539-721873e169b6'::uuid,
    'c991b7c9-eca8-4e5e-a1eb-bdc091d80a17'::uuid,
    'bf661549-16ad-4457-be2c-867a55877ab0'::uuid,
    'abc42f88-a57d-440c-9887-2fcf55724ecc'::uuid,
    '32313c27-c87b-4e17-b301-f0037357b40e'::uuid,
    '2f884bdc-79d7-45ac-9aeb-c52c88e6a0f2'::uuid,
    '0c385117-f160-404b-8aa4-75e108cf8264'::uuid,
    '6c04264c-9f07-4b84-9de9-aa7be8a36822'::uuid,
    'f0c6460b-90f7-4a44-ba4d-572f91e1c44c'::uuid
  ];
  endpoint_total integer;
  priced_total integer;
  contract_total integer;
  visible_total integer;
begin
  select count(*) into endpoint_total
  from public.api_endpoints where api_id = source_id and id = any(curated_endpoint_ids);
  if endpoint_total <> 13 then
    raise exception 'CBS onboarding expects all 13 curated endpoints, found %', endpoint_total;
  end if;

  select count(*) into priced_total
  from public.api_endpoints
  where api_id = source_id and id = any(curated_endpoint_ids) and price = 0.047259 and active and cache_ttl_seconds = 86400;
  if priced_total <> 13 then
    raise exception 'CBS onboarding expects 13 endpoints at list 0.047259, active, cached a day; found %', priced_total;
  end if;

  -- This migration deliberately seeds no execution contracts; see the header.
  -- Asserting zero keeps that a decision rather than something that quietly
  -- changes when someone copies this file for the next source.
  select count(*) into contract_total
  from public.capability_execution_contracts contract
  join public.api_endpoints endpoint on endpoint.id = contract.endpoint_id
  where endpoint.api_id = source_id;
  if contract_total <> 0 then
    raise exception 'CBS onboarding refuses % execution contracts; the input binder cannot compose an OData filter yet', contract_total;
  end if;

  -- Draft on insert. A source that is discoverable before its endpoints have
  -- been probed from the production egress IP sells calls that may come back
  -- as somebody else's error.
  select count(*) into visible_total
  from public.apis
  where id = source_id
    and status = 'active' and coalesce(active, true) and is_verified
    and coalesce(discoverable, true) and suspended_at is null;
  if visible_total <> 0 then
    raise exception 'CBS source must stay a draft in this migration; go live in a separate one';
  end if;
end;
$verify$;

commit;

-- ---------------------------------------------------------------------------
-- AFTER APPLYING
-- ---------------------------------------------------------------------------
-- 1. node scripts/sync-migration.mjs        (mirror into app/supabase/migrations)
-- 2. Probe all ten endpoints from the production egress IP. CBS showed no
--    throttling at all from a laptop, but that says nothing about a datacentre
--    range, and the KVK open-data host already returns 429 to this network.
-- 3. Flip live in a separate migration, which has to clear TWO gates:
--    a. the review gate — set
--         perform set_config('apiosk.api_review_approved_id', <source_id>, true);
--       before the UPDATE, or 20260717020000_enforce_api_review_approval.sql
--       :41-46 silently rewrites status to 'pending' and the source never
--       appears;
--    b. the payout gate — require_payout_wallet_to_publish() raises 23514 on
--       the transition into 'pending'/'active' unless wallet_address is a
--       syntactically valid, non-burn address. This source has no payee, so
--       that is a question about which address the PLATFORM settles its own
--       revenue to, and it must be answered deliberately rather than by
--       copying a burn address from another migration.
--
-- Deliberately NOT onboarded: the CBS website OData service at
-- www.cbs.nl/odata/v1 (articles, publication calendar, vacancies — content,
-- not statistics) and CBS Microdata (contract-only, remote access, not a
-- per-call product). Neither fits this listing.
