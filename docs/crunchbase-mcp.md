# Official Crunchbase MCP

The existing `apimarket-magicapi-crunchbase-bbcca0a6` Crunchbase service has four
additional **draft** operations, all POST:

- `/mcp/cb_entity_autocomplete`: official company, person, location/category identity search.
- `/mcp/cb_search_query`: explicit company, people and funding filters.
- `/mcp/cb_entity_get`: full readable fields and relationship cards, including leadership,
  board members, employees, funding rounds and investors when supplied by the account.
- `/mcp/cb_reference`: authoritative field/operator/card documentation.

Ten semantic capabilities distinguish company/person/funding search and profiles,
filtered searches, filter-value resolution and reference lookup. Tags, categories,
input schemas, result facts and source attribution are recorded in the catalog.
Existing API.market operations and their prices remain unchanged.

## Account connection and activation

The official MCP server is `https://mcp.crunchbase.com`. The API.market key cannot
be used here. The adapter supports OAuth bearer tokens with refresh, fixed token
and resource destinations, no redirect forwarding, and a cross-process refresh lock.
The Vault secret `CRUNCHBASE_MCP_OAUTH` contains the server-only OAuth bundle.
Never copy it into listing metadata, browser code, logs or chat.

`node scripts/connect-crunchbase-mcp.mjs` starts a 15-minute PKCE callback on
`http://127.0.0.1:18743/callback`. The public CIMD client document is served by the
Gateway at `/.well-known/crunchbase-client.json`. Its `client_id` and redirect URI
are fixed. The operator opens `.local/crunchbase/authorize-url.txt` in a browser
and grants the connection on Crunchbase. The callback stores a private 0600
`.local/crunchbase/oauth.json`. Alternatively, explicitly set `CRUNCHBASE_CLIENT_ID`
and `CRUNCHBASE_CLIENT_SECRET` for a pre-registered client with the same callback.

`node scripts/discover-crunchbase-mcp.mjs` performs initialize and tools/list only.
It saves the four real tool schemas under ignored `output/crunchbase-mcp/` and
fails if any reviewed tool is missing. It does not execute company searches or
activate public catalog entries. Compare discovered schemas with
`scripts/crunchbase-mcp-catalog.mjs` before live acceptance.

Activation still requires successful OAuth, confirmed account access and provider
usage limits, an explicit buyer tariff, and real responses for each activated
capability. Do not mark fixtures as live evidence. Crunchbase documents separate
arrangements for backend/programmatic use; verify that the connected account
supports the intended service use. Until then endpoints are inactive and contracts,
facts and capabilities remain draft. The existing two API.market routes stay active.

## Execution and result boundaries

The adapter forwards one reviewed tools/call without automatic retry. It handles
JSON and SSE, verifies RPC IDs and protocol versions, preserves structured results
or JSON/text content, and rejects provider/RPC/tool errors before success billing.
Response memory is capped at 4 MiB. Autocomplete has at most 25 results; search and
profile cards have at most 100 entities. Pagination is explicit, never automatic.
All requested filters and fields are passed unchanged. Exact collection, field,
operator, card and identifier values must come from Crunchbase's reference/results.

Jina Reader runs internally on at most one public Crunchbase profile URL per data
action, including existing API.market actions. It has a 12-second deadline, a
512 KiB response cap and a 16,000-character content cap. It receives no credentials,
query parameters, private URLs or caller cookies. There is no separate source,
endpoint, approval or buyer charge. `page_enrichment` identifies the original
Crunchbase URL and supplementary provenance; it never overwrites official fields.
CAPTCHA/login/verification pages, empty replies, failures and redirects are marked
unavailable and cannot become company data. A free Starbucks Reader probe on
2026-10-11 returned a verification page, so enrichment is not guaranteed.

## Verification

- `cargo test --test crunchbase` (Gateway): typed identity/filter binding, single
  source, planner index presence, complete result preservation and error rejection.
- `cargo test --bin apiosk-gateway proxy::crunchbase` (legacy executor): routing,
  JSON/SSE errors, identity-preserving enrichment, hostile URLs and challenge pages.
- `node scripts/test-crunchbase-mcp-migration.mjs`: PostgreSQL migration execution,
  idempotence, draft activation gates, source/contract/schema parity, no duplicate
  source, and preservation of existing API.market prices and credentials.
- `node scripts/render-crunchbase-mcp-migration.mjs` regenerates the draft migration
  and explicitly synthetic planner fixture; `node scripts/sync-migration.mjs`
  keeps the App migration copy aligned.

Official documentation: https://data.crunchbase.com/docs/tool-reference and
https://data.crunchbase.com/docs/connecting-ai-tools .
