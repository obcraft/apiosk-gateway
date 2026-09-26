# European capability source groups

Deployed to Gateway v2 and MCP on 2026-09-25. The canonical source membership is
`contracts/eu-capabilities.json`. These are curated source groups, not new
execution contracts. The existing planner, source restrictions, prices and
approval flow are unchanged. `POST /v2/capabilities/resolve` accepts structured
input and returns only fitting runtime-accepted endpoint contracts without a
provider call or charge.
`POST /v2/capabilities/plan` uses the same structured body to choose
runtime-accepted operations, save one priced multi-source plan, and return its
total ceiling. `POST /v2/asks/{id}/run` is the one explicit approval and
execution step. Exact CBS observation operations need an `operation` selector;
the planner does not infer a metric from a country alone.
Eurostat data accepts an explicit dataset code plus `country`; the resolver
fills `eurostat.input.geo`, mapping ISO `GR`/`GB` to Eurostat `EL`/`UK`.
Capability plans and runs return `normalized_records` from contract-extracted
provider facts, grouped by subject. Subjects with matching stable identifiers
are joined; exact duplicate facts retain each source reference, retrieval time
and freshness expiry. The original documents stay in `results` for inspection.

- `eu.company.profile`: KVK, Sirenic, Companies House. Supplementary: GLEIF,
  VIES, Overheid.io.
- `eu.tenders.search`: TED, TenderNed. Supplementary: Sirenic.
- `eu.statistics.query`: CBS, Eurostat. Supplementary: Data.overheid.nl.

TED currently has organisation-notice lookups by KVK/RSIN, not a general
notice-search execution contract. TenderNed has Dutch public-contract browse.
EU Funding & Tenders returns funding calls rather than procurement notices.
PDOK returns geospatial data, not statistical observations. The endpoint
allowlist in `contracts/eu-capabilities.json` keeps unrelated operations out.

The authenticated `GET /v2/sources?capability=<slug>` route returns published
primary sources only, before pagination, with supplemental membership in
`selected_capability`. Exact-slug source search uses the same membership.
The unfiltered catalog publishes the aliases for the App's capability facets.
A bare slug in Ask/discovery returns a free source overview without guessing
an entity or creating a purchase. Natural-language source questions receive the
same groups in the interpretation context. Actual data requests keep using the
existing executable operations.

## Verification

- Rust library: 237 passed, 4 ignored.
- Local HTTP/database integration: durable flow, latency flow and public API
  flow passed. The public API test checks all three groups through the real
  authenticated route with a read-only organisation key and zero provider calls
  during discovery.
- App: 36 source-browser, directory and WebMCP tests passed. Production build
  passed using `npm run build:production`.
- MCP: 28 targeted runtime and protocol tests passed, including validation of
  the new output fields against the advertised MCP schema.
- Tests use local fixtures, not paid production sources.

Production verification on 2026-09-25:

- Gateway runtime commit `9505abb` deployed to `apiosk-gateway-v2`; the public
  health endpoint reports protocol `2` and `payment_executor: ready`. Fly's
  HTTP health check passes.
- MCP commit `24b6f37` deployed to `apiosk-mcp`; its public health endpoint
  reports `status: ok`.
- Live MCP source queries return exactly the primary sources listed above
  (3 company, 2 tender, 2 statistics sources) and all supplementary memberships.
  These checks only browse the catalog and do not execute paid provider calls.
- The App changes are on `main` at `04a42a6`; its production rollout was not
  independently verified during this deployment.

The current checkouts are the sibling directories `gateway`, `mcp`, and `app`.
The integration runner can target the MCP runtime explicitly with:

```sh
TEST_MCP_RUNTIME_URL="file:///absolute/path/to/mcp/src/runtime.mjs" node scripts/test-integration.mjs
```

## Release order

Release the MCP output schema first (its new fields are optional and accept the
old response), then Gateway v2, then the App. The gateway adds response fields
that older strict MCP output schemas do not accept. No database migration is
needed. Existing App versions already consume capability aliases from the
catalog; the App change preserves exact `eu.*` labels and exact-slug searching.
