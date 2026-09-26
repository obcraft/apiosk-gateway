# Alexandria pilot: source and endpoint audit (2026-09-26)

Read-only production query of `catalog.endpoint_contract_coverage` returned 37
grouped source slugs. The runtime catalog is still the final authority: a
published endpoint or a database contract is not sufficient if Gateway rejects
its contract. The curated endpoint allowlist is
`contracts/eu-capabilities.json`; `POST /v2/capabilities/resolve` intersects
that allowlist with published sources and runtime-accepted services.

| Capability | Primary sources and accepted routes | Supplementary sources and role |
| --- | --- | --- |
| `eu.company.profile` | KVK: `/api/v2/zoeken`, `/api/v1/basisprofielen` (NL); Sirenic: `/v1/eu/recherche`, `/v1/eu/entreprise/{pays}/{id}`, French search/profile; Companies House: `/search/companies`, `/company` (GB) | GLEIF: legal entity/LEI lookup; VIES: VAT validation; Overheid.io: OpenKVK search/lookup (NL) |
| `eu.tenders.search` | TenderNed: `/v2/publicaties` for Dutch public-contract browsing and organisation mentions; TED: `/apiosk/organisation-notices` by KVK or RSIN | Sirenic: French company public-procurement links |
| `eu.statistics.query` | CBS: dataset catalogue and selected StatLine `Observations` (NL); Eurostat: dataset search and `/statistics/1.0/data` | Data.overheid.nl: dataset discovery/metadata, not statistical observations |

The other 26 registered sources were reviewed but do not supply a relevant
accepted route for these three exact capabilities: `agentbodega`, `ask-merlin`,
`bloomberg`, `bodemloket`, `browserbase`, `cityfalcon-financial-api`,
`crowdpull`, `domain-intelligence`, `eodhd`, `eu-funding-tenders`,
`eurlex-cellar`, `exa`, `iban-check`, `koop`, `linkup`, `opensanctions`,
`pdok`, `pdok-ruimtelijke-plannen`, `pixelforge-x402-data-services`,
`pulsenetwork`, `rdw`, `reuters`, `rvo`, `stableenrich`, `tavily`, and
`webbersites`. Some may be useful for a later, differently scoped capability.

The important scope corrections are endpoint-specific: the current TED
contract does not provide general notice search; EU Funding & Tenders returns
funding calls; PDOK provides geospatial records. The public source directory
may still show these sources for other searches, but they are excluded from
the fitting-operation response for this pilot.

The resolver is a free preflight. `/v2/capabilities/plan` builds a saved,
quoted plan from the fitting operations; `/v2/asks/{id}/run` provides one
explicit approval before backend execution. The result envelope includes
each source document, retrieval metadata, cost, and a sourced answer.
Structured capability runs also expose `normalized_records`: contract-extracted
facts grouped by subject, with exact duplicate facts combined and every
source reference retained. Subjects with matching country-specific or global
identifiers are joined; names alone never join subjects. A Firecrawl-specific
schema and broader cross-provider identity resolution
remain separate contract work; source membership does not establish those
properties.

Local verification used an isolated database, model fixture and payment
fixture: the public API test resolved KVK and Sirenic, saved one total quote,
required one approval, executed two provider calls, and returned their
metadata and normalized facts. The durable and latency integration flows also
passed. No production provider call or Firecrawl request was made for this
pilot.
