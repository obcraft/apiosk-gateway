# Official Crunchbase MCP

The existing `apimarket-magicapi-crunchbase-bbcca0a6` Crunchbase service has four
additional operations, all POST, validated against the connected account:

- `/mcp/cb_entity_autocomplete`: official company, person, location/category identity search.
- `/mcp/cb_search_query`: explicit company, people and funding filters.
- `/mcp/cb_entity_get`: full readable fields and relationship cards, including leadership,
  board members, employees, funding rounds and investors when supplied by the account.
- `/mcp/cb_reference`: authoritative field/operator/card documentation.

Ten semantic capabilities distinguish company/person/funding search and profiles,
filtered searches, filter-value resolution and reference lookup. Tags, categories,
input schemas, result facts and source attribution are recorded in the catalog.
Existing API.market operations and their prices remain unchanged.

## Buyer tariff

Each of these four MCP tools costs **EUR 0.05 per trigger, including the Apiosk
fee**, at the configured display rate on 2026-10-11. This also applies to
`cb_reference`; additional searches, profile fetches and explicit next-page
requests are separate triggers and must appear in the approved total. Internal
Jina enrichment is included, with no extra charged step.

The catalog stores `0.047258` USDC before the standard 15% fee. Integer fee
rounding gives a buyer total of `0.054347` USDC, displayed as EUR 0.05 at the
configured EUR/USD rate `0.92` (rate date 2026-08-21). Settlement remains in USDC:
this is not an exchange-rate lock, and a future display-rate update changes the
EUR equivalent. The tariff regression uses the App's actual source-price and
quote-ceiling formatters, so neither can round this tariff up to EUR 0.051.

## Account connection and activation

The official MCP server is `https://mcp.crunchbase.com`. The API.market key cannot
be used here. The adapter supports OAuth bearer tokens with refresh, fixed token
and resource destinations, no redirect forwarding, and a cross-process refresh lock.
The Vault secret `CRUNCHBASE_MCP_OAUTH` contains the server-only OAuth bundle.
Never copy it into listing metadata, browser code, logs or chat.

`node scripts/connect-crunchbase-mcp.mjs` starts a 15-minute PKCE callback on
`http://127.0.0.1:18743/callback`. The public CIMD client document is served at
`https://app.apiosk.com/.well-known/crunchbase-client.json`. It explicitly declares
a native client for the loopback redirect. Its `client_id` and redirect URI
are fixed. The operator opens `.local/crunchbase/authorize-url.txt` in a browser
and grants the connection on Crunchbase. The callback stores a private 0600
`.local/crunchbase/oauth.json`. Alternatively, explicitly set `CRUNCHBASE_CLIENT_ID`
and `CRUNCHBASE_CLIENT_SECRET` for a pre-registered client with the same callback.

`node scripts/discover-crunchbase-mcp.mjs` performs initialize and tools/list only.
It saves the four real tool schemas under ignored `output/crunchbase-mcp/` and
fails if any reviewed tool is missing. After discovery, `node scripts/install-crunchbase-oauth.mjs` stores the approved account in Vault and removes the local token file. It refuses to replace an existing account. These scripts do not execute company searches or
activate public catalog entries. Compare discovered schemas with
`scripts/crunchbase-mcp-catalog.mjs` before live acceptance.

OAuth was connected and stored in Vault on 2026-10-11. Authenticated `tools/list`
and `cb_reference` supplied the real account schemas. The account exposed a
50,000-data-token monthly allowance and a maximum search page of 15 rows.
Nine real public responses cover name search, public-field filtering, reference,
and company/person/funding profiles. The real Rust adapter also passed a free
Vault-to-Crunchbase reference request. No buyer balance was charged; these checks
do not claim a paid Ask execution. The reviewed adapter and four endpoints were
activated in production. The runtime role has column-level read access to the
endpoint origin and documentation URL for correct official-source attribution.
Existing API.market routes retain their prices.

The company-profile capability explicitly indexes founders, employees, board
members, funding rounds and investors as outputs of one profile call. Ask binds
an explicitly supplied Crunchbase organization/person/funding-round profile URL
to its exact permalink, with a matching public host and entity path. It never
guesses a permalink from a company name or buys a redundant identity lookup.
Automatic profile contracts expose only that identifier; advanced field/card
selectors remain available in the endpoint schema. The real production model
and a production-catalog snapshot compiled the complete Starbucks profile
question after this correction, stopping before approval and paid execution.
The reviewed output schemas describe the actual labelled `properties` and
`cards` arrays, including pagination flags. Coverage aliases are specific to
company, person and funding profiles; an identity search does not cover a
relationship roster. Headcount remains the provider's range, not an exact count.
Regression checks include both one combined requirement and the real model's
five separate requirements, through input binding, shared-goal coverage,
resolution and the final output-coverage gate. A large-directory fixture keeps
common words such as "profile" discoverable for the explicitly named source.
The shortlist retains distinct typed MCP operations sharing an endpoint, while
keeping room for three different endpoint alternatives.

Discovery also treats the phrase "board members" as the same governance concept
as "directors". A real extraction split the Starbucks question into six parts
and described its board as a "board of directors"; the lexical facet gate
previously discarded the matching company-profile operation for that part.
The regression now verifies both five-part and six-part extractions through one
priced profile call, while unrelated board-game/member descriptions remain out.
If a model draft invents a constraint, recovery can also combine those field
requirements into one verified retrieval. It uses only the saved subjects and
ranked source matches, proves every requested output, validates literal inputs,
and resolves the entire plan before offering a price. Different subjects or
unsupported fields cannot be merged or silently omitted.

## Execution and result boundaries

The adapter forwards one reviewed tools/call without automatic retry. It handles
JSON and SSE, verifies RPC IDs and protocol versions, preserves structured results
or JSON/text content, and rejects provider/RPC/tool errors before success billing.
Response memory is capped at 4 MiB. Autocomplete has at most 25 results, search
15 rows, and profile cards a total requested related-entity budget of 100.
Pagination is explicit, never automatic; `has_more_entities` is retained.
All requested filters and fields are passed unchanged. Exact collection, field,
operator, card and identifier values must come from Crunchbase's reference/results.

`legacy-executor/src/proxy/crunchbase/public-schema.json` records the reviewed
public fields and cards from this account. The adapter restricts collections,
fields and cards to public data and chooses explicit public profile cards by
default, including employees **and** board/advisors. Private notes, CRM, saved-list
membership and recursive relationship queries are rejected. The search limit is
also enforced in the adapter, with an explicit default of 15. Provider errors in
`structuredContent.error` or `response_status` cannot become billed successes.

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
  idempotence, OAuth/price/schema activation gates, ten real-sample contracts,
  no duplicate source, and preservation of API.market prices and credentials.
- `node scripts/check-crunchbase-mcp.mjs --adapter-reference`: operator-only real
  adapter/Vault acceptance with one free reference request and no buyer charge.
- `node scripts/check-crunchbase-mcp.mjs cb_metering_get_state`: free quota check.
  Other explicit reviewed tool calls consume the account's included data tokens;
  results are saved under ignored `output/crunchbase-mcp/` without credentials.
- `node scripts/render-crunchbase-mcp-migration.mjs --fixture-only` refreshes the
  synthetic planner fixture without changing an applied registration migration.
  The separate account and activation generators reproduce their named migrations.
  Copy only relevant migrations to the App; avoid unrelated historical copies.

Official documentation: https://data.crunchbase.com/docs/tool-reference and
https://data.crunchbase.com/docs/connecting-ai-tools .
