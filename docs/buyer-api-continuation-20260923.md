# Saved buyer-API continuation

Implemented locally on 23 September 2026. Not deployed or tested with a live paid source in this change.

`POST /v2/asks/{id}/continue` accepts a returned signed state, action ID and input, using the originating key with `ask:execute`. An `Idempotency-Key` is required. Paused run responses now include a `continuation` containing only `select_entity` and `supply_input` actions and their schemas.

Selecting a candidate uses the existing engine action and resumes the saved worker under its existing approval. The task ID, quote, paid search results and payment journal remain unchanged. Supplying a missing value can create a new quote; that quote is not automatically approved. Expired quotes, cancellation, altered states, stale actions, unoffered candidates and another connection are refused. A revoked approval cannot restart paid work. Natural-language planning clarifications without an action and document uploads are outside this endpoint.

The public `needs_input` projection also now checks the actual public action names (`supply_input`, `upload_document`), so its schemas are no longer silently omitted.

## Verification

- The extended `public_api_flow` runs over HTTP, the SQL store and real task worker, with local model/payment fixtures. It proves one search plus one selected report, one authorization and the original quote/cap, including resumption through a fresh API/Engine instance.
- Concurrent identical submissions and later retries buy nothing twice. Changed input under a reused key returns a conflict; stale state with a new key is refused. Input schema, scope, connection isolation, quote expiry, revoked authorization, cancellation and attempts to invoke paid/cancel actions through this endpoint are covered.
- `PGTEST_PORT=54420 node scripts/test-integration.mjs`: durable-flow, latency-flow and buyer-API suites all passed. The runner now gives each suite its own fresh database because durable-flow deliberately inserts a cheaper reseller. It defaults to a 16 MiB test-thread stack for the large durable fixture. These are local PGlite fixtures using one SQL connection; they do not certify production database concurrency or live provider behavior.
- `cargo test --offline --lib`: 135 passed, four ignored, one failed. The failing `followups::tests::topical_filing_followup_offers_filingpulse_with_exact_question` was reproduced on an untouched HEAD archive at `src/followups.rs:171`; it is independent of this continuation change. Its old source-operation fixture is rejected by the current catalog admission rules.
- Docs: Mint build validation and OpenAPI validation passed; local schema references resolve; `git diff --check` passed in both modified repositories.

Gateway files: `src/public_api.rs`, `src/server.rs`, `src/engine/execute.rs`, `tests/public_api_flow.rs`, `scripts/test-integration.mjs`. Public usage documentation and OpenAPI changes are in the independent `docs` repository, including `api/continue.mdx`.

Evidence logs: `output/buyer-api-continuation-20260923/` (ignored local artifacts). No database migration, App/MCP change, deployment, new source purchase or customer debit was made.
