# EU and Dutch public sources

Six Apiosk sources, ten registered routes, eleven new execution capabilities and 39 typed facts. KOOP extends the existing source and retains its earlier official-notice search and detail capabilities.

Buyer total per call: **USD 0.097826**, comprising USD 0.085066 list price plus USD 0.012760 platform fee. This is EUR 0.08999992 at Apiosk's configured USD/EUR rate of 0.92 (dated 2026-08-21). EUR is a converted display amount, not a permanently fixed euro tariff. Each further page is a separate call requiring the normal quote/authorization flow.

| Source | New capabilities | Inputs | Outputs |
|---|---|---|---|
| VIES | VAT validation GET; VAT check POST; service status | Member-state code, national VAT number; optional paired requester identifiers for POST | Validity, request time, available name/address; service and member-state availability |
| KOOP | Staatscourant title search; CVDR local regulation search; BWB national regulation search | Literal title keyword, collection, page size, continuation start | Record identifiers, titles, metadata, original links, total and next start |
| EUR-Lex Cellar | Bounded SPARQL SELECT; CELEX metadata notice | SELECT with LIMIT 1–100; CELEX identifier | SPARQL variables/bindings; document identifier, dates, in-force metadata and language-expression references |
| EU Funding & Tenders | Keyword discovery | Keyword, result language, page and page size | Call identifiers, titles, source links, status codes, dates, programme and budget metadata, total and next page |
| PDOK Ruimtelijke Plannen | WMS GetFeatureInfo | RD BBOX, supported layer, image size/pixel, feature limit | Feature properties and identifiers, queried layer, extent and coordinate system |
| Bodemloket | WMS GetFeatureInfo | Same map inputs; WBB_locaties, Beschikbaarheid_gegevens or Bevoegd_gezag | Soil-map feature properties and data-availability information |

PDOK's documented adapter layer enum currently includes eleven common layers: enkelbestemming, dubbelbestemming, bouwvlak, bouwaanduiding, figuur, functieaanduiding, gebiedsaanduiding, lettertekenaanduiding, maatvoering, plangebied and bestemmingsplangebied. The upstream has additional layers outside this adapter enum. One layer and one pixel per call; RD extents are limited to 10 km on each axis.

OpenAPI 3.1 adapter contracts are in `openapi/`, using the six requested `.openapi.yaml` filenames. The files use JSON syntax, which is valid YAML. Every source has a category, tags, sectors, source documentation, per-route schemas, examples, typed input/output facts and execution mappings. The adapter supplies protocol constants and format headers; callers do not provide an upstream credential. SEDIA is a public API identifier.

## Source boundaries

- VIES unavailable/error responses are failures, distinct from a negative VAT validation. Missing identity data remains unknown.
- KOOP notices are not proof of a granted permit. Regulation metadata can include historical versions.
- Cellar returns metadata notices, not full legal documents. Its metadata redirect is followed only on publications.europa.eu, under `/resource/cellar/`, over HTTPS, at most twice.
- Funding queries use multipart query and displayFields parts, exclude FAQ data and filter the requested language. Counts can include historical and multilingual source records. Open status and eligibility are not inferred.
- WMS results do not establish current planning rights or clean soil. Bodemloket does not return the full underlying investigation reports.
- XML diagnostics, malformed JSON/XML, unsupported query inputs, response identity mismatches and schema failures are rejected before settlement. Large source results preserve their coverage warning in paged responses.

## Verification

- Eleven production-adapter routes tested against live upstreams, including nonempty metadata/map/search results.
- Adapter and catalog tests cover real fixtures, empty maps, invalid inputs, source errors, identity mismatch, exact pricing and all eleven capability contracts.
- Local durable-engine tests cover quote, rejection before approval, execution, persistence and replay for all eleven capabilities. Billing and model are fixtures in this test; it does not claim a real customer charge.
- Real intent-model tests cover seven Dutch questions across all six sources, checking capability choice, binding and the total quote. VIES test states the explicit country code `NL` to meet the existing literal-input contract.
- The onboarding migration is exercised twice in one rolled-back production transaction: still eleven contracts, no duplicate registration.

Operational evidence, readbacks and deployment logs: `output/eu-public-sources/` (ignored operational artifacts).

## Official documentation

- [VIES](https://ec.europa.eu/taxation_customs/vies/)
- [KOOP SRU 2.0](https://www.koopoverheid.nl/binaries/koop/documenten/instructies/2021/02/09/handleiding-voor-het-uitvragen-van-de-collectie-officiele-publicaties/HandleidingSRU2.0.pdf)
- [Cellar metadata](https://op.europa.eu/en/web/cellar/cellar-data/metadata)
- [EU Funding & Tenders APIs](https://ec.europa.eu/info/funding-tenders/opportunities/portal/screen/support/apis)
- [PDOK](https://www.pdok.nl/)
- [Bodemloket WMS and report limits](https://www.bodemloket.nl/content/kan-ik-bodemloket-ook-mijn-gis-inladen)

## Published state

All six sources and eleven new contracts are active in the production catalog. The final Amsterdam production probe passed all eleven adapter cases. Seven real-model questions passed against the freshly read production catalog. Three matching migration files are present in the Gateway and App checkouts and recorded as applied. No live customer debit was performed.
