# Request-to-result performance — 20 September 2026

Implemented and verified locally in Gateway and App. No commit, push, deployment,
paid source request or live model comparison was performed in this follow-up.
Starting commits: Gateway `8b52890`, App `3361ee8`, MCP `dea6934`; all three
checkouts were clean on `main` before the work. MCP needs no source changes.

## Request interpretation

The complete source catalog is supplied once. This was already implemented at the
starting commit, along with direct KVK answers, batched source reads, local event
wakeups and immediate text display; the original line references were outdated.

Chat, intent parsing and planning review now use a separate interpretation
context. It omits old rendered proposals, result references and repeated evidence
payloads. It retains dialogue, every entity reference, focus, allowed identity
facts, the previous intent and constraints, and compact validated answer text with
its limitations. This preserves references to previous amounts and reporting years
without sending their evidence objects again. Saved client history remains intact.
The catalog remains complete: no keyword filter hides potentially relevant or
blocked sources. Actual output contracts still undergo the existing validation.

## Direct answers

The existing direct path is verified for exact Dutch/English requests for a KVK
visiting address, registration number or legal name. It requires one subject, one
profile goal, one complete reviewed KVK result, no failed steps, and no additional
constraints beyond a named source. Ambiguous/hidden addresses, unsupported
question shapes, comparisons and incomplete sources retain model analysis.

The saved-address integration test reaches `succeeded` without a model key and
with source execution disabled. It verifies exact evidence, report linkage and
restored history. This establishes that the supported lookup needs no additional
model or source purchase. The analysis limit remains 65 seconds across retries;
it is a deadline, not an observed average.

## Model and provider routing

`INTENT_MODEL` configures interpretation. Optional `ANALYSIS_MODEL` independently
configures source analysis, falling back to `INTENT_MODEL` when omitted. Existing
model defaults are preserved. Requests to the OpenRouter host use
`INTENT_PROVIDER_SORT=latency` by default; `ANALYSIS_PROVIDER_SORT` is optional.
Allowed values are `latency`, `throughput` and `price`. Parameter requirements,
response schemas, validation and retry deadlines remain in force. Other API hosts
do not receive the new sorting extension.

OpenRouter documents these as separate provider ordering modes: `latency` is
not the same as the `:nitro` throughput shortcut. See its
[provider routing explanation](https://openrouter.ai/blog/insights/model-routing/).
No claim is made that a different model or provider is fastest or equally correct;
that still requires representative live questions and independently reviewed
answers. Provider ordering can affect internal model costs.

## Lighter status updates

The App opts into `compact=true` on its signed, read-only event URL. Each new
connection starts with a complete task snapshot. Subsequent `task_delta` events
carry shallow field replacements and explicit removals, with context fields
compared separately. Null values remain distinct from deletion. Updates identify
the exact task and base revision; a missing base closes the stream and uses the
existing saved-task recovery. Worker-only changes at the same revision work too.
Older App and MCP clients continue receiving complete `task` events.

Each event stream reuses immutable result documents for that one task, fetching
only newly referenced results in one batch. Every view still checks task ownership,
workspace access and expiry. The cache retains at most 2 MiB of serialized source
documents; larger results can be read normally. Signed report links are still
refreshed. Unchanged context avoids retransmission; refreshed links count as a
change. Local saves wake streams immediately. The 750 ms fallback check remains
for missed signals and changes on other Gateway instances, so this is not a
claim of cross-instance push delivery.

## Measurement

Structured logs now identify request stages and quote-scoped milestones:

- `question_to_price`: validated request intake through persisted plan and response
  construction. Excludes browser/network time and request claim/authentication.
- `approved` to `sources_ready`: the first successful quote authorization through
  source availability at answer-stage entry, including worker scheduling and saved
  source reads. Repeated approval does not create another approval milestone.
- `sources_to_answer`: source availability through answer validation, persistence
  and response construction, including direct answers and failed analyses.

Model logs include stage, model, provider when returned, input bytes, token counts,
attempt count and elapsed time. View logs distinguish referenced documents from
newly fetched documents. No prompts, answers, source fields or credentials are
added to logs.

`node scripts/summarize-latency.mjs --help` explains the local JSONL log reader.
It reports p50/p95 and sample counts by stage and model, validated provider samples,
and model failures. Missing quote windows and failed/partial answers are excluded
from successful timing percentiles and counted separately. Missing data is null,
never zero. Contract-valid model output is not an independent correctness score.
These log summaries need complete log windows and synchronized server clocks;
requests without an answer stage do not supply an approval-to-sources sample.

## Local evidence and verification

- The final saved-address SSE event is **8,561 bytes as a full snapshot versus
  2,020 bytes as a compact update: 76.4% less JSON**. These are local fixture bytes,
  excluding HTTP/SSE framing and compression, not a production time saving.
- Eight result reads use **8 versus 1 database round trips**. The latest isolated
  PGlite fixture measured medians of 2,653 versus 771 microseconds over 20 runs.
  This is not a production database benchmark or a concurrency proof.
- Gateway `cargo test --all-targets --offline`: **151 passed**, 25 existing
  environment/live tests gated out. The relevant ignored integration tests below
  were run separately against the isolated fixture database.
- `node scripts/test-integration.mjs`: durable buyer flow and saved-lookup/SSE tests
  passed, including exact cap/approval, replay, expiry, workspace access, evidence,
  history, reports, full/compact client compatibility and reconnect snapshots.
- `node scripts/test-worker-flow.mjs`: passed; four fixture calls, one approval,
  peak concurrency two, 474 ms. Cancellation, ambiguity recovery, budget ceiling
  and private lease checks passed.
- MCP `npm test`: **195 passed**.
- App `npm test -- --run`: **2,198 passed**. Four pre-existing failures were repaired
  in tests: URL query parameters are separated from manifest/icon file paths, and
  workspace tests use the menu's keyboard interaction and await selection rendering.
  Product icons, menu behavior and payer rules were not changed for these repairs.
- App lint and `npm run build:production` passed, including both TypeScript checks.
- The latency-summary reader test and `git diff --check` passed.

Logs are saved in `Production/evidence/2026-09-20/performance-followup/`.
Production latency percentiles, live provider/model comparisons, and installed
ChatGPT/Claude buyer journeys remain unmeasured in this follow-up. The changes are
local and uncommitted; the production rollout has not happened.
