# Local TypeSafe comparison

Start from the Gateway root:

```sh
cargo run --offline --example typesafe-compare
```

Open http://127.0.0.1:5280. Enter a question and click **Compare**. The two
panels independently show their interpretation, source plan, elapsed time,
model-call count, and model cost when available. This is a parsing experiment;
it does not retrieve source data, create a task, authorize payment, or purchase
the resulting plan. Both methods use the same catalog snapshot. The current
method uses the existing configured intent model and prompt. The TypeSafe
panel never silently substitutes that model's answer.

Shared Gateway v2 code owns `Catalog::build`, the catalog SQL and contracts,
`Intent::validate`, `engine::bind_subjects` (entity references, schema checks,
fact provenance, expiry and deduplication), and `resolver::resolve` (dependencies,
source constraints and pricing). The production Engine and both comparison
methods use the same binding implementation. Only the TypeSafe model decisions
are new; there is no parallel catalog, planner, payment ledger or executor.

The existing `INTENT_API_KEY`, `INTENT_API_URL`, `INTENT_MODEL`, and
`INTENT_PROVIDER_SORT` settings apply to the current method. Add the TypeSafe
credential to the untracked `gateway/.env`:

```dotenv
TYPESAFE_API_KEY=your-key
TYPESAFE_MODEL=jev-1.13.0
TYPESAFE_TIMEOUT_MS=2000
```

Credentials are read again on each comparison; adding the key does not require
a server restart. They stay on the server. The page binds only to loopback,
rejects other Host/Origin values, and allows two concurrent calls. Each click
can incur model usage charges. `TYPESAFE_COMPARE_PORT` changes the port.

The current local test uses a five-second total TypeSafe deadline; the reusable
adapter still defaults to two seconds. `TYPESAFE_TIMEOUT_MS=5000` reproduces the
local setting. Rejections display the failed confidence check or transport
status, without substituting the other method. Live responses can contain
rounded probabilities summing to 0.99: validation allows at most 0.02 total
rounding error, while retaining all option/range/argmax checks and the original
0.95 selected-probability, 0.90 confidence and 0.99 yes/no thresholds. It never
renormalizes a weak probability upward.

The default catalog is loaded read-only from the configured database, with the
Gateway's normal schema search path. For a wholly synthetic, limited catalog:

```sh
TYPESAFE_COMPARE_CATALOG=tests/fixtures/integration-catalog.json cargo run --offline --example typesafe-compare
```

TypeSafe currently handles a new, single-subject, single-goal lookup with one
literal input, and an optional source requirement. It enumerates every
contiguous span of questions up to twenty-two words and every executable
capability (at most 254 plus the escape option). After selecting a capability,
it derives possible input types from the complete declared dependency graph and
selects the input and source. A final call independently checks the assembled
intent for omitted goals, constraints, wrong subjects and source substitutions.
Code copies the literal, validates its schema, and verifies that the resolver
can compile the intent. Multi-entity questions, contextual follow-ups, complex
filters and uncertain decisions require the existing parser. These limits are
prototype coverage limits, not evidence of model quality.

The reusable Parser adapter is opt-in through `with_typesafe`; **the production
entry point does not enable it**. Its `on` mode supports fallback and its
`shadow` mode retains the baseline result. It also supports a bounded single
scalar answer from saved evidence, using the existing evidence validator.
The comparison page intentionally tests the two parsers separately.

Timing covers each server-side model method and deterministic plan creation,
excluding initial catalog loading, browser delivery, and paid source execution.
Current-model cost uses the provider's reported cost. TypeSafe cost is an
estimate from reported input tokens at the published Jev 1.13.0 rate of
$0.042/million; unknown model versions remain unpriced. Retries and failed
attempts stay counted. If any attempt lacks a cost, the UI says cost unavailable
rather than treating it as zero. These figures do not establish production
question-to-answer p50/p95 or semantic accuracy.

Verification:

```sh
cargo test --offline --test typesafe
node scripts/test-typesafe-flow.mjs
```

The integration test uses the real Gateway state, quote, approval, worker,
analysis and recovery code with local model/provider/billing fixtures, for both
App and MCP entry points. It demonstrates one approval and no paid replay,
not real TypeSafe accuracy, source settlement, or production latency.

Before choosing a method, compare independently labelled Dutch and English
requests, including unsupported sources, omitted qualifiers, follow-ups and
multiple entities. Report correct plans and refusal/fallback coverage as well
as time and total model spend. Live TypeSafe verification requires its API key;
the mocked tests do not establish that it is faster, cheaper or more accurate.

## Initial live check — 2026-09-21

The local key authenticated successfully. For `Search KVK for Mollie`, a final
browser run returned the existing Gateway's `company_registry.search` plan,
constrained to `kvk-dutch-business-register`, in 2.52 seconds with reported model
cost $0.012287 (one call). TypeSafe returned in 1.19 seconds with estimated cost
$0.001038 (one call), but its single-subject judgment was 0.98 against the 0.99
acceptance threshold: **no TypeSafe intent or plan was accepted**. An earlier
address request also failed selection confidence. These are individual attempts,
not a benchmark or evidence of an accuracy improvement. Provider caching and
response variability can affect subsequent timings and costs.

All-target Gateway tests and the isolated quote/approval/worker/recovery test
passed after the shared-binding refactor. Production TypeSafe routing remains
disabled; the local UI performs no source purchases or durable task writes.

API references: https://docs.typesafe.ai/api,
https://docs.typesafe.ai/models,
https://docs.typesafe.ai/model-jaggedness/jev-1.13.
