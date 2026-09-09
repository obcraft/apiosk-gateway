# Public statistics planner integration

Verified 2026-09-09. Migration `20260909093944_connect_public_statistics_planner.sql` is applied to production and mirrored in App. Gateway deployed as `deployment-01M22RM2QHM7TFNE2RMWH16YQE`.

## Coverage

| Source | New validated contracts | Scope |
| --- | ---: | --- |
| CBS Statistics | 10 | Dataset catalogue, dimensions, branch/measure codes, six existing economic observation tables |
| Dutch Open Data Register | 3 | Dataset search, dataset metadata, publisher list |
| Eurostat | 2 | Dataset search and parameterized JSON-stat data |
| EODHD Market Data | 0 | Known to the planner, explicitly blocked from purchase |

EODHD's 60 existing endpoints are marked `x-apiosk-sellable=false`. The migration records the existing resale block as a planner-readable reason; it does not remove the restriction. Enabling execution requires resolving recorded resale authorization and separately validating execution contracts.

CBS responses retain periods, units, codes and page counts. A raw observation page is not a complete or automatically sector/year-matched answer. data.overheid.nl supplies catalogue metadata, not the underlying publishers' data rows. Eurostat dataset/dimension codes must be supplied or obtained from evidence, not invented. This is not validation of every upstream endpoint or every natural-language analysis.

## Evidence

- 134 standard Rust tests passed (workspace); two new opt-in live tests passed separately.
- In-memory PGlite migration applied twice: 15 contracts load through actual `src/catalog.sql`, endpoint prices unchanged, EODHD stays blocked.
- Live public CBS, data.overheid.nl and Eurostat response shapes passed extraction/schema validation. No paid data call.
- Configured OpenRouter parser selected the correct source/capability for CBS catalogue, Dutch charging-station dataset search, Eurostat inflation dataset search, and a fully parameterized Eurostat GDP request.
- Production-only parser test requires all 15 contracts and the EODHD reason to exist in the production catalogue; it does not supplement production rows with fixtures.
- Logged-in App WebMCP confirmed all three sources available and returned `ready` plans. CBS and Dutch Open Data Register each quoted USD 0.054348; Eurostat search quoted USD 0.010870. Quotes are temporary ceilings, not charges. All had authorization inactive, reserved 0 and charged 0.
- Opened the Dutch Open Data Register plan in the actual App: source, pending step, approval control and "No purchase yet" visible. No approval clicked.
- Local durable consent/payment integration suite passed using fixture providers, not paid upstream execution.

## Repeat checks

Run from Gateway (live tests load the configured environment without printing secrets):

```sh
cargo test --workspace --offline
node scripts/render-public-statistics-migration.mjs --check
node scripts/test-public-statistics-migration.mjs
PUBLIC_STATS_DEPLOYED=1 cargo test --offline --test public_statistics live_public_statistics_planning -- --ignored --nocapture
cargo test --offline --test public_statistics live_public_response_shapes -- --ignored --nocapture
node scripts/test-integration.mjs
```

Existing unrelated checks remain non-green: Clippy `type_complexity` in the pre-existing parser test state type; host-instruction drift in `sync-contracts.mjs --check` (MCP instructions are newer than Gateway's copy). These were not overwritten or suppressed. Supabase advisor output includes existing advisories; this migration changes catalogue data only, not schema, RLS or grants.
