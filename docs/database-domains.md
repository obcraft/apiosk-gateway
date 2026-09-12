# Physical database domains

Production: `jgjoiyqdyypouskftzeq` (`apiosk-production`). Applied migration: `20260912130830_physically_organize_domain_tables.sql`.

The physical migration replaces the earlier administrative aliases. The canonical tables and original views move into 14 domain schemas, using `ALTER ... SET SCHEMA`. Original relation names and OIDs remain intact; data is not copied. `kvk_cache` remains its existing physical schema.

`public` contains RPC and helper functions for API compatibility, but no application tables or views after this migration. Supabase-managed schemas remain unchanged.

| Schema | Tables | Original views | Purpose |
| --- | ---: | ---: | --- |
| `kyb` | 9 | 0 | Bedrijfsverificatie, documenten, personen en KYB-providerkoppelingen |
| `providers` | 25 | 18 | Providers, API-aanbod, endpoints, providerinstellingen en provideradministratie |
| `catalog` | 9 | 1 | Capabilities, uitvoercontracten, feiten en discovery |
| `actions` | 14 | 0 | Selecties, vergelijkingen, beslissingen en bronuitvoering |
| `billing` | 28 | 5 | Kopersaldo, betalingen, goedkeuringen, afrekening en uitbetalingen |
| `agents` | 12 | 1 | Agentverbindingen, toestemming, MCP-sessies en toolaanroepen |
| `wallets` | 10 | 2 | Wallets, wallettransacties, netwerken en tokens |
| `research` | 11 | 0 | Onderzoeksplannen, jobs, resultaten en gesprekken |
| `integrations` | 13 | 1 | Marketplace Connect, webshops, GitHub en WebMCP-koppelingen |
| `observability` | 15 | 2 | Verkeer, requestlogs, health en gebruiksmetingen |
| `support` | 4 | 0 | Supportgesprekken, contact en feedback |
| `content` | 2 | 0 | Kennisbankartikelen en categorieën |
| `operations` | 4 | 0 | Beheerregistraties, snapshots en operationele berichten |
| `identity` | 3 | 0 | Gebruikerslevenscyclus, voorkeuren en platformbeheerders |

## Runtime references

- SQL connections in both gateways use the ordered domain search path recorded in `supabase/physical-domain-schemas.json`. Public RPC names remain unchanged. Function bodies referencing moved tables are rewritten by the migration. Existing empty search paths and additional members such as `auth` and `vault` are preserved.
- App, website, buyer, provider and admin clients use `domain-fetch.ts` to route PostgREST table requests to the correct domain. REST clients outside these applications must send `Accept-Profile: <domain>` for reads and `Content-Profile: <domain>` for writes. Supabase clients can use `.schema("providers").from("apis")`, for example.
- During deployment and rollback, routing retries only missing-schema or missing-relation responses. HEAD reads handle the corresponding bodyless errors. Authorization failures, rate limits, server errors and uncertain writes are never retried by this adapter.
- All 39 production Edge Functions use the same routing adapter with their previous JWT verification settings. Corresponding local function entrypoints are updated. MCP telemetry writes route to `agents`.
- Realtime subscriptions include the new physical schema. Existing publication membership follows the preserved table OID. Browser tabs loaded before the deployment must refresh.
- Domain schemas are exposed to PostgREST. Table ACLs, owners and RLS settings are preserved. Schema USAGE is added only for roles with existing source access, including column-level grants. The follow-up migration `20260912131123_preserve_domain_column_access.sql` preserves access for the restricted Gateway role. No new table privileges are granted.

## Migration and recovery

The canonical mapping is `supabase/physical-domain-schemas.json`. Keep original table names, for example `kyb.kyb_profiles`, `agents.agent_connections`, `providers.api_endpoints` and `actions.tasks`.

The historical `20260912121232_organize_domain_schemas.sql` migration and `domain-schemas.json` describe the superseded view directory. Do not rerun or extend it. New schema changes require incremental migrations against the physical domains.

The physical migration aborts on unmapped source relations, changed aliases, unexpected dependencies or lock contention. It does not use `DROP CASCADE`. Before applying, deploy all consumers with domain routing.

Local evidence and recovery artifacts are under ignored `output/schema-physical-20260912/`: `before.sql` is the production schema snapshot; `rollback.sql` moves the same relations back and restores the original function definitions. The rollback deliberately does not recreate the obsolete aliases. It must be reviewed against any subsequent migrations before use.

## Validation

`python3 scripts/test-physical-domains.py` restores the schema into disposable PostgreSQL 17, applies the migration, reads all 189 moved relations, verifies identity and data preservation, tests own-row RLS, rejects cross-owner writes, exercises an upsert and trigger plus the balance RPC, and tests rollback. Production data is not copied into this test.

Frontend routing tests cover request headers, upsert bodies, migration/rollback fallback, HEAD counts, external requests and error handling. Gateway workspace library tests cover 127 passing tests; the separate KVK database integration test is excluded from that command.

Production verification is recorded separately in `output/schema-physical-20260912/verification.json` after the live migration. Pricing changes and new KVK provider requests are outside this migration.

## Verified production outcome

On 2026-09-12, 159 canonical tables and 30 original views were moved into the 14 domains. `public` and `gateway_v2` each have zero tables/views; all five KVK cache tables and five stored annual accounts remain. Before/after fingerprints match for constraints, policies, routine privileges, publication membership and endpoint prices. The relation identities, storage files, owners, ACLs and RLS flags are preserved.

Eighteen REST and Gateway checks pass, including real HEAD routing. All 39 deployed Edge Functions retain their previous JWT settings. The Schema Visualizer shows the actual `agents` table graph. Reloading the deployed app retrieves the saved MoneyBird annual account and still displays the original EUR 0.10 charge. The final app release uses the production Gateway URL from `netlify.toml`; local development environment values must not override this during future manual builds.

The Supabase security advisor reported no new finding categories. Existing policy, public-function and Auth findings remain outside this organizational migration.

## Reproducible releases

Use `npm run build:production` for a manual App build so local Gateway URLs cannot enter a production bundle. The legacy image overlay requires `GATEWAY_IMAGE` pointing to the exact Gateway image built from the reviewed commit. The standalone legacy repository vendors the same `kvk-accounts` crate; keep both copies byte-identical and include its queue migration so a standalone checkout builds and tests. Active Laevitas and PulseNetwork syncs use the `providers` PostgREST profile.

Run `python3 scripts/test-chatbot-flow.py` for the full local MCP → HTTP/Auth → SQLx → provider-fixture acceptance test. It requires the adjacent App and MCP checkouts with their installed dependencies, starts a fresh local PGlite database on an available port, and checks approval, replay, refund handling, result retrieval, recovery, PDF downloads and source isolation. `python3 scripts/test-kvk-accounts.py` separately verifies real PostgreSQL concurrency and the physical `actions` schema.
