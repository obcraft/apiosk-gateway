# Discover searches every published provider

The App's `useDiscover` calls `canonicalAsk`, which reads `GET /v1/ask`. This is a different routing path from the typed v2 execution planner. It was still running with `ASK_ENGINE_V2=false`, loading the legacy `capability_offers`-rooted snapshot. Unmapped endpoints never reached the search pool.

The existing full-catalog query starts with active visible `api_endpoints` and LEFT JOINs existing capability offers. It searches descriptions, search terms, tags, use cases and task summaries without requiring a v2 execution contract to be discoverable.

Change: added the compatibility `contract_documented` column with its existing-engine default and enabled `ASK_ENGINE_V2=true` in the live Fly configuration and checked-in `fly.toml`. No metadata backfill, readiness promotion or execution-contract activation was performed. The separate readiness audit remains unchanged. Discovery does not purchase source data.

Before: a live RDW RZ874R query reported engine v1, 20 sources, 95 listings and 1,403 offer rows, and returned no RDW result.
After: the same query reported engine v2, 28 sources, 103 listings and 1,551 offer rows, returning the official RDW endpoint with model fit 95. Counts refer to searchable offer rows; an endpoint with multiple capability mappings can appear more than once before shortlist deduplication.

Rollback: set live `ASK_ENGINE_V2=false` and change the checked-in value to false; the additive column can remain. Do not unset the secret alone while the checked-in environment value is true.

Additional live search checks passed for KOOP, RVO, TED and CBS StatLine. Each response reported all 28 providers / 103 listings searched and returned the requested source. Queries only exercised discovery and model ranking; no paid data-source execution occurred.
