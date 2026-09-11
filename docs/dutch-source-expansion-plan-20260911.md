# Dutch government source expansion — research and implementation plan

Research date: 11 September 2026. Scope: extend Apiosk beyond the already implemented KVK, CBS and PDOK. This research backlog is followed by the [implementation and release report](dutch-public-sources-release-20260911.md) for the first three sources. Estimates below are engineering estimates for one engineer, excluding external access delays.

Recommendation: start with **KOOP, direct RDW and RVO subsidy discovery**. Follow with **DUO and Rijkswaterstaat**, then **KNMI, DSO and NDW**. Add the government API register as an internal discovery feed alongside this work. Extend existing TenderNed and Data.overheid.nl capabilities rather than onboarding duplicate sources.

## Existing coverage and corrections to the supplied list

The public production directory returned 24 providers during this research, including TenderNed, Data.overheid.nl and Overheid.io as well as KVK, CBS and PDOK. A listing alone does not prove executable coverage. Current repository fixtures, scripts and the September 9 workflow report additionally show TenderNed `/v2/publicaties` search and Data.overheid.nl CKAN dataset search work. Their paid end-to-end journeys were not rerun today.

- **TenderNed:** extend procurement coverage; investigate the official support and durability of the configured publication-search route before depending on it for further capabilities. Official bulk JSON is historical, updated half-yearly; the latest listed file covers 2026 Q1/Q2. New XML API accounts currently go onto a waiting list that may take months. Bulk ingestion cannot support a promise of complete current opportunities. [TenderNed documentation](https://www.tenderned.nl/cms/nl/aanbesteden-in-cijfers/datasets-aanbestedingen).
- **Data.overheid.nl:** reuse dataset discovery. Dataset descriptions and resource URLs are not the underlying measurements. Add monitored resource metadata and deduplication where useful.
- **RDW:** Overheid.io already has vehicle-related implementation work. Direct RDW is a distinct upstream option; measure overlapping fields and total buyer cost before selecting defaults.
- **API register:** the current website lists 315 APIs, including many PDOK entries and protected services. This is not 315 new, open, executable Apiosk sources. Missing OpenAPI security declarations do not establish anonymous access. [Official register](https://apis.developer.overheid.nl/apis).
- **KOOP:** the list describes XML, but a live request accepting JSON returned JSON. Validate multi-record and empty-result shapes before choosing JSON over an XML adapter.

Local implementation evidence: `src/catalog.rs`, `src/sources.rs`, `scripts/test-direct-usecases.mjs`, `tests/fixtures/direct-source-repaired.json`, and `docs/source-workflows-20260909.md`. The catalog validates typed inputs, output extraction and sample payloads; keep that admission process for every new capability.

## Prioritized source registry

Effort includes a narrow adapter, contract, fixtures and integration verification, assuming access is available. It excludes broad provider coverage.

| Priority | Source / first capability | Access and format | First deliverable | Effort / dependency |
|---|---|---|---|---|
| 1 | KOOP official publications | Anonymous SRU/CQL; JSON observed, XML also available | Search notices by text, authority and period; return publication details and original links | 3–5 days; validate pagination and JSON shape |
| 2 | Direct RDW vehicle data | Anonymous sample succeeded; Socrata JSON; inspected dataset CC0 | Exact plate lookup, technical vehicle fields; fuel/emissions as separately verified dataset joins | 2–3 days; compare Overheid.io coverage |
| 3 | RVO subsidy discovery | Anonymous JSON sample succeeded; catalog marks dataset CC0 | Find relevant scheme descriptions, audience, sectors and official links | 3–5 days; verify pagination and separate current status from content |
| 4 | DUO RIO | Anonymous collection request succeeded; REST/HAL JSON, OpenAPI | Registered course offerings with institution/location relationships | 3–5 days; RIO identifiers and effective-date semantics |
| 5 | Rijkswaterstaat WaterWebservices | Anonymous POST/JSON observation request succeeded | Station-based water-height time series with units, reference level and quality | 3–5 days; station/parameter resolution |
| 6 | KNMI EDR | Registered EDR key; JSON/GeoJSON/CoverageJSON | Temperature/precipitation observations for a station and period | 3–5 days; key and CoverageJSON normalization |
| 7 | DSO environmental documents | DSO key; REST/JSON, OpenAPI | Documents and source rules applicable to a resolved location/date | 5–8 days; key, geometry and temporal joins |
| 8 | NDW traffic signs | Documented GeoJSON/CSV and event API; bounded event probe timed out | Signs for a municipality or road segment | 2–4 days after access/reliability check |
| Parallel | Government API register | Read API key; machine-readable registry and specifications | Internal candidates with owner, documentation, status and spec hash | 2–3 days plus key wait |
| Extension | TenderNed | Existing publication search; official historical JSON files; XML credentials/waitlist | Historical award search with explicit coverage and identity confidence | 4–6 days; schema and access review |
| Extension | Data.overheid.nl | Existing CKAN metadata discovery | Resource checks and normalized discovery candidates | 1–2 days |
| Hold | National WOZ values | National supported value-query access not established here | Access/coverage investigation only | No delivery estimate yet |

## Concrete implementation scopes

### KOOP: notices and decisions

Entry: `GET https://repository.overheid.nl/sru` with `operation=searchRetrieve`, `version=2.0`, a bounded CQL query and `maximumRecords`. The one-record probe returned `gmb-2025-384139`, an application notice from Nuenen, with authority, publication date, geometry and a canonical publication URL. No key was used. The unsorted first record was historical; default ordering must not be presented as newest-first.

Proposed capabilities: `government.publication.search` and `government.publication.detail`. Normalize zero/one/many records, SRU diagnostics, result totals, `startRecord` continuation and document URLs. Start with municipality/date/text search; add PDOK radius search only after validating the collection's supported spatial index and coordinate system. Treat company-name matches as mentions unless identity is independently established. An application notice is not an issued permit; official publications are not automatically consolidated current law. [Official SRU manual](https://www.koopoverheid.nl/binaries/koop/documenten/instructies/2021/02/09/handleiding-voor-het-uitvragen-van-de-collectie-officiele-publicaties/HandleidingSRU2.0.pdf).

### RDW: direct vehicle lookup

Verified candidate: `GET https://opendata.rdw.nl/resource/m9d7-ebf2.json?$limit=1`. The result contained plate, vehicle type, brand, model, APK expiry and technical fields. The dataset metadata identified it as official and CC0. Normalize plates to uppercase without separators and validate an exact `kenteken` filter. Pin dataset identifiers; inspect the fuel dataset before promising CO2 or fuel coverage. Preserve multiple fuel records, missing values and observation dates. Do not infer an owner or company fleet from a plate. Validate throttling and any app-token policy before production load. [Official RDW dataset](https://opendata.rdw.nl/en/en/Vehicles/Open-Data-RDW-Gekentekende_voertuigen/m9d7-ebf2).

### RVO: subsidy descriptions first

Verified: `GET https://www.rvo.nl/api/v1/opendata/subsidies` returned 50 records. Fields include `id`, `title`, `intro`, `changed`, `countries`, `sectors`, `subjects`, `targets` and relative `url`. Resolve URLs against RVO and preserve modification time. The inspected first record described Transitiefaciliteit in the past tense and had no explicit open/closed, deadline or eligibility fields. Do not inherit the catalog's stronger description that all returned schemes are open.

Proposed capability: `subsidy.scheme.search`, producing relevant schemes and source links. Fetch and validate documented status/detail information before adding `subsidy.scheme.status`; do not advertise an eligibility decision from a text match. Confirm the actual pagination parameter and ordering from the YAML specification before indexing. EP-online, project awards and agricultural data are separate later integrations with their own access and licensing checks. [Dataset and API link](https://data.overheid.nl/dataset/subsidies-en-financiering-rvo-nl), [RVO data documentation](https://www.rvo.nl/onderwerpen/open-data).

### DUO: registered education offerings

The documented base URL alone returned 404; this did not indicate an unavailable API. The OpenAPI-defined `GET https://lod.onderwijsregistratie.nl/api/rio/v2/aangeboden-opleidingen?pageSize=1` returned a record with linked institution, location and course resources. Its specification documents `page`, `pageSize` up to 100, `datumGeldigOp` and typed identifier filters.

Normalize HAL links and preserve RIO identifiers and effective dates. Validate the institution/location joins before offering geographic course search. Student counts require a separate DUO statistical dataset and school-year/suppression handling. [Official DUO overview](https://duo.nl/open_onderwijsdata/overzicht-open-data.jsp), [RIO specification and version](https://apis.developer.overheid.nl/apis/a2amj_gDg).

### Rijkswaterstaat: real observations

Use `POST /METADATASERVICES/OphalenCatalogus` for station/parameter availability and `POST /ONLINEWAARNEMINGENSERVICES/OphalenWaarnemingen` on `https://ddapi20-waterwebservices.rijkswaterstaat.nl`. A one-hour query at `ameland.nes` returned `Succesvol=true` and seven water-height measurements, in centimetres relative to NAP, with timestamps and quality/status metadata.

Cache the catalog, bind station + quantity + period, and distinguish `meting`, `verwachting` and astronomical data. Preserve missing-value conventions, quality flags and time offsets. Official limits include 160,000 observations per request; start with much smaller bounded intervals. [WaterWebservices documentation](https://rijkswaterstaatdata.nl/waterdata/).

### KNMI, DSO and NDW

- **KNMI:** use `https://api.dataplatform.knmi.nl/edr/v1/collections`, then collection-specific locations/queries. Start with a current observation collection, not deprecated `observations`. The documented EDR key allowance is 200 requests/sec but only 1,000/hour, so enforce both limits. Preserve units and measured/modelled distinctions. No authenticated query was run. [EDR documentation](https://developer.dataplatform.knmi.nl/edr-api).
- **DSO:** begin at the documented Ontsluiten v2 and document-presentation APIs, using `x-api-key`. Pin production versions and specification hashes. Resolve PDOK coordinates to applicable document identifiers and dates before extracting rules. A fuzzy document suggestion is not proof of spatial applicability. A full permit check requires activity selection and the applicable-rule question flow; it is a later capability. No authenticated query was run. [Ontsluiten v2](https://developer.omgevingswet.overheid.nl/api-register/api/omgevingsinformatie-ontsluiten/).
- **NDW:** current signs endpoint is `https://data.ndw.nu/api/rest/static-road-data/traffic-signs/v4/current-state`; filter by municipality/RVV code and request GeoJSON. The current state updates daily. The separate bounded events probe timed out after 20 seconds; resolve that before promotion. Preserve placed/planned/removed status, supplementary signs and coordinate system. A sign record alone does not establish the currently applicable speed limit. [NDW v4 documentation](https://docs.ndw.nu/data-uitwisseling/interface-beschrijvingen/verkeersborden-api/).

## Discovery and WOZ boundaries

Request a read key for the government API register when implementation starts. Store provider/service identity, OpenAPI URL/hash, lifecycle, auth, license, operations, identifiers and geographic/temporal scope. Deduplicate against existing services. Generate draft mappings from specifications, then validate actual inputs, outputs, pagination and user-question coverage before activation. OpenAPI describes transport structure; it does not prove semantic completeness. [Registry access documentation](https://apis.developer.overheid.nl/apis/toevoegen).

National WOZ value lookup remains unverified. There are official WOZ tax-report API specifications and Amsterdam object/deelobject APIs, but neither establishes anonymous nationwide property-value coverage. Amsterdam's documentation also announces an API-key transition. Inspect actual value fields and coverage before considering a city-specific capability. [Waarderingskamer specification context](https://www.waarderingskamer.nl/voor-gemeenten/woningen/woz-taxatieverslag-woningen), [Amsterdam WOZ documentation](https://api.data.amsterdam.nl/v1/docs/datasets/woz.html).

## Delivery sequence and release criteria

1. **Baseline and common contracts — 1–2 days:** read back active production capabilities; identify overlap; agree narrow user questions. Record auth, reuse terms, documented limits, pagination, freshness, source cost and proposed buyer price. Store unknowns explicitly. Upstream free access does not by itself determine Apiosk's buyer price.
2. **First release — KOOP, RDW, RVO — 8–13 days:** implement the three bounded capabilities above. Keep RVO at description discovery until current-status coverage is proven. Existing KVK/PDOK facts should resolve dependencies without duplicate purchases.
3. **Second release — DUO and RWS — 6–10 days:** add education relationships and water observations. Add the API-register discovery worker when its key is available.
4. **Third release — KNMI, DSO, NDW — 10–17 days after prerequisites:** prioritize according to actual usage and access readiness. TenderNed historical awards are a separate optional 4–6-day extension.

The first useful release is roughly **2–3 working weeks**, including baseline work. This is an estimate, not a delivery commitment. External key waits and newly discovered source semantics may change it.

For every capability:

- Capture official documentation and small real response fixtures, including empty, ambiguous and incomplete results.
- Add typed input/output contracts and source-specific normalization. Record returned counts, continuation, source URLs, query scope, publication/measurement date, units and freshness.
- Test exact identifiers, date boundaries, pagination and extraction against source responses. Use bounded retries, provider limits and appropriate caches.
- Verify discovery → dependency resolution → total quote → one approval → source execution → sourced result → saved-result reuse. Include the failure/refund path. A raw HTTP 200 or catalog listing is not the release criterion.
- Activate only the verified capabilities. Confirm production catalog readback and a real buyer journey before marking them shipped.

Acceptance examples: KOOP notices preserve application/decision status; RDW returns the requested plate; RVO shows a historical scheme without labelling it open; DUO keeps course and institution relationships correct; RWS returns the requested interval with centimetres/NAP and quality flags. Keep unsupported questions explicitly unanswered.

## Evidence from this research

Small direct upstream requests were used; no Apiosk purchase, deployment or source activation was performed. Five sources returned usable records: KOOP, RDW, RVO, DUO and RWS. NDW timed out; KNMI and DSO remain documentation-only in this run. These samples establish feasibility, not complete integration validation or latency guarantees.

Raw responses, the DUO OpenAPI snapshot and request manifest are saved under `output/source-expansion-20260911/` (ignored generated evidence). The manifest retains the unsuccessful DUO root request alongside the successful documented collection request.
