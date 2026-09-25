# European capability source groups

Implemented locally; not deployed. The canonical source membership is
`contracts/eu-capabilities.json`. These are discovery groups, not new execution
contracts. The existing planner, source restrictions, prices and approval flow
are unchanged.

- `eu.company.profile`: KVK, Sirenic, Companies House. Supplementary: GLEIF,
  VIES, Overheid.io.
- `eu.tenders.search`: TED, TenderNed. Supplementary: EU Funding & Tenders,
  Sirenic.
- `eu.statistics.query`: CBS, Eurostat. Supplementary: PDOK, Data.overheid.nl.

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

The original sibling `mcp` checkout is older and has unrelated local edits.
The MCP changes are in the separate sibling `mcp-current` checkout. Integration
was run against it with:

```sh
TEST_MCP_RUNTIME_URL="file:///absolute/path/to/mcp-current/src/runtime.mjs" node scripts/test-integration.mjs
```

## Release order

Release the MCP output schema first (its new fields are optional and accept the
old response), then Gateway v2, then the App. The gateway adds response fields
that older strict MCP output schemas do not accept. No database migration is
needed. Existing App versions already consume capability aliases from the
catalog; the App change preserves exact `eu.*` labels and exact-slug searching.
