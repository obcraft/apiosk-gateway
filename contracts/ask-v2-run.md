# Ask V2 run contract

Implemented locally, 2026-10-12. This is an implementation record, not deployment or live-provider proof. The full product specification is [source-planner-spec](../docs/ask-v2-source-planner-spec.md).

## Runtime path

Question → GPT-OSS 120B extraction → schema-2 field grounding → deterministic contract matching → bounded cost-aware selection → combined quote → one approval → existing background worker → saved-response validation → GPT-OSS 120B evidence selection/ordering → cited answer.

The existing composer, dark theme, keyboard submission and main navigation are retained. Source progress, the combined approval, missing coverage and expandable evidence appear below the composer.

## Public routes

Private production rollout: the Ask V2 navigation item, quick search entry, direct page and run endpoints are restricted to the verified auth user `bf085fe8-41c4-4699-978f-e75e89518bf7` (`ollybrinkman@gmail.com`). Authorization uses the immutable user ID, not editable email metadata or a browser flag. Other authenticated users receive 404 from the run and standalone intent endpoints. Existing V1 routes retain their behavior. All run routes also require existing owner/workspace access checks. API keys cannot impersonate an app user.

| Method | Path | Purpose |
| --- | --- | --- |
| POST | `/v2/ask-v2/runs` | Start or recover by client request UUID and unchanged question/workspace. |
| GET | `/v2/ask-v2/runs/:id` | Snapshot and authorized recovery. Never creates a second run. |
| GET | `/v2/ask-v2/runs/:id/events?after=N` | Replay ordered durable events, up to 200 per page. |
| POST | `/v2/ask-v2/runs/:id/approve` | Approve exact plan revision, quote reference and maximum micro-USD amount. |
| POST | `/v2/ask-v2/runs/:id/clarify` | Supply one schema-valid literal input for the current clarification/revision. |
| POST | `/v2/ask-v2/runs/:id/replan` | Refresh an unapproved quote without repeating extraction. |
| POST | `/v2/ask-v2/runs/:id/cancel` | Stop future dispatch; preserve incurred charges and saved results. |

Canonical Rust shapes are in `src/ask_v2/run_contract.rs`; TypeScript shapes are in the app's `src/lib/domain/ask-v2-run.ts`. Money is an integer micro-USD decimal string, never a floating-point amount. The UI shows every selected call, required dependencies, applicable existing gateway markup, one maximum total, and authoritative charged/reserved amounts.

## Planning rules

- Use admitted, verified, priced execution contracts with documented fact schemas. Unknown fields stay gaps; opaque schemas do not gain coverage.
- Honor capability, source restrictions, literal subject inputs, jurisdiction and supported resolver constraints. Resolve bounded prerequisites through the existing resolver.
- Deduplicate compatible calls across requests. Independent subject calls retain separate identity and execution keys.
- Select a bounded cost-aware covering set; do not buy all qualifying candidates. This heuristic is not a proven globally optimal set cover.
- `documented-contract-fit-v1` enforces a minimum 90/100. All eligibility gates must pass. A passing contract has baseline 90, validated sample extraction adds 7, and a positive declared freshness bound adds 3. Missing measured reliability remains explicitly unknown.
- This score is an uncalibrated contract-fit heuristic. It is not correctness probability, measured reliability, or evidence that 90 is the optimal threshold. A reviewed held-out scoring dataset and operational latency/reliability observations are still needed for that claim.
- Pin selected fact schemas with the plan. Approval rechecks selected service versions and authorization; changed or expired quotes must be refreshed.

## Execution and persistence

Migration `20261012200000_ask_v2_runs.sql` adds six private `gateway_v2` tables: runs, events, plans, scores, evidence and answers. RLS is enabled, browser roles have no direct access, and existing gateway runtime access remains backend-only.

The existing executor owns approvals, reservations, provider dispatch, dependency ordering and reconciliation. No parallel billing implementation was added. One approval releases the whole plan; ready branches execute concurrently within existing worker limits. Leaving the page does not cancel a running job.

Run leases prevent duplicate orchestration. Events have monotonically increasing per-run sequence numbers. Polling reads snapshots; the details panel can replay history. Local storage contains only the run ID under an owner/workspace-specific key. A page reload reuses the run and its saved responses.

Model attempts are recorded before sending. If a model outcome is uncertain after interruption, extraction is not automatically purchased again; synthesis recovery uses deterministic saved facts. Server restart recovery needs an authenticated request with an ephemeral bearer; no bearer token is stored and no startup recovery daemon was added.

## Evidence and answers

Evidence records include request, subject, canonical field ID, exact value, source, endpoint, contract, execution, saved response, JSON Pointer and retrieval time. Invalid, ambiguous, stale or missing extracted fields do not become factual claims. Source failures and valid empty results remain different statuses. Conflicting values remain visible.

Reviewed casts/adapters and composite mappings are saved under `_ask_normalized`, retaining the original raw data and transformation metadata. Evidence points either at a raw value or the exact saved normalized value. No public URL is invented.

GPT-OSS 120B selects and orders accepted evidence IDs; the server renders exact factual blocks and gaps, checking required coverage, values and citations. This intentionally constrains synthesis: arbitrary free-form analysis, model-generated calculations, custom tables and export formatting are not introduced here. Unsupported IDs or model timeout fall back to deterministic cited facts. The normal successful path uses one extraction and one synthesis call. Name-only results are shown as source-reported candidates, not independent identity confirmation or a sanctions verdict.

Freshness currently uses each admitted contract's declared retention bound and retrieval time. Source-record dates, units and currencies are only available when represented by the fact value; no dates or metadata are inferred from arbitrary payload keys. Cross-run paid-result reuse is not added: the same run is reused on reload.

## Verification and activation

Automated checks cover schema/score rules, wrong restrictions, opaque/unresolved fields, dependency pricing, deduplication, field provenance, normalization and citation rejection. The isolated integration test covers one combined approval, no pre-approval provider calls, stale-quote rejection, requoting without another extraction, actual overlap of independent calls, dependent calls, saved-answer reload, owner isolation, event ordering and cancellation before dispatch. Its mock provider and model avoid real purchases. PGlite provides one database connection; real multi-connection Postgres concurrency and paid-provider E2E remain separate acceptance checks.

```sh
# From gateway; start isolated database in a separate terminal:
PGTEST_PORT=54349 node tests/support-db.mjs
TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:54349/postgres cargo test --offline --test intake_flow ask_run_one_quote_approval_background_evidence_and_reload -- --ignored --nocapture
cargo test --offline --lib ask_v2::
# From app:
npx tsc -p tsconfig.app.json --noEmit
npx tsc -p tsconfig.test.json --noEmit
npx vitest run tests/components/ask-v2/ask-intent-page.test.tsx
```

Desktop and 390px mobile fixture previews verify price → approval → concurrent progress → cited result, with a visible fixture banner. They are not a live-provider proof.

Before use against the configured environment, apply the migration through the project's normal database release process, rebuild/restart the gateway, and enable execution only for the intended deployment scope. The local launcher uses the configured Supabase project and explicitly sets `V2_EXECUTION_ENABLED=0`; it is not an isolated database. Production activation is tracked separately from these implementation checks. The corrected six-table migration and its references to `actions` were verified in production on 2026-10-12. No real paid provider call is part of the release checks.

### SQL editor error: `gateway_v2.tasks` does not exist

The domain migration `20260912130830_physically_organize_domain_tables.sql` moved the existing task, execution and result tables into `actions`. Do not recreate these tables. The corrected Ask migration resolves foreign-key targets using a transaction-local `actions, gateway_v2` search path, preferring canonical physical tables while supporting the isolated pre-domain fixture. If neither layout exists, it stops with a named prerequisite error before creating Ask tables. Re-run the corrected complete migration after a failed attempt; its transaction rolls back on failure. The migration does not enable execution or make a purchase.

### Shared response UI and euro pricing

Ask V2 now reuses V1 provider marks, rich `AnswerBody` values, result tables, and the `GatewayV2Outputs`/`GatewayV2OutputDetails` saved-result widgets. The tools disclosure exposes existing charts, media, source-specific tables, downloads and full saved-result reading when the saved data supports them. It does not execute providers to change presentation. Provider marks appear beside plan names, progress entries, citations and evidence rows; published logo metadata is preferred with the existing curated/initials fallback.

Approval, call breakdowns and charge totals display euros using the existing published FX-rate query and exact integer conversion helpers. Rounding is allocated across the quoted source rows so they add up to the displayed total. The blue `Approve & pay` action submits the original exact backend quote. No FX rate means no displayed substitute dollar price and approval remains disabled until conversion loads. The shared result widgets inherit this euro formatter locally without changing global account preferences or the payment ledger. Source-reported financial facts retain their actual currency; they are not relabelled as euros.
