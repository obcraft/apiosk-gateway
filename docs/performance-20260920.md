# Request-to-result latency improvements — 20 September 2026

The chat now sends the complete catalog once. Saved source documents are fetched
once per task view and reused for current results, conversation history and report
hashes. Report downloads and the answer stage also batch their source reads.
Ownership, task expiry, result order, pagination and signed download contracts are
preserved; a missing result still fails the read.

Exact Dutch/English questions about a company's visiting address, KVK number or
legal name can use reviewed KVK profile fields without an additional model call.
This deliberately covers a narrow set of questions: one subject, one profile goal,
one complete result and no failed steps. Broader questions, additional constraints,
unknown providers, incomplete results and ambiguous/hidden addresses retain model
analysis. Direct answers use the same evidence-pointer validator and response
format as model answers.

Committed local task changes and worker start/finish now wake live event streams
immediately. The existing 750 ms database check remains as recovery for missed
notifications and changes on another instance. App recovery waits four seconds
after the most recent event instead of making duplicate reads during healthy live
updates. Received text is displayed in full without simulated typing.

Logs include model-stage duration and input bytes, task-view duration and unique
result count, direct-answer use, and worker duration. No prompt, answer, credentials
or source fields are added to these logs. These changes do not select a different
model or alter source pricing, approval or payment identity.

## Measured evidence

- Supplier-check catalog fixture: the chat's question/context/catalog JSON is
  14,404 → 8,920 bytes (5,484 bytes, 38.1% less). This excludes the unchanged system
  prompt and is not a measured reduction in model latency.
- Eight saved result reads: 8 → 1 database round trips. Across 20 local fixture
  repetitions, median duration was 2,749 → 814 microseconds (approximately 3.4×
  faster for this read stage). This uses the isolated PGlite-backed test server,
  not the production network or database.
- Saved-address execution reaches `succeeded`, including exact source evidence,
  a matching report snapshot and restored conversation history, with no model key
  and source execution disabled. No second model request or source purchase is
  needed for this supported answer shape.

## Verification

- Gateway `cargo test --all-targets --offline`: 148 passed; 25 environment/live
  tests skipped by their existing gates.
- `node scripts/test-integration.mjs`: durable buyer flow and latency regression
  passed, including approval, idempotency, recovery, history, PDFs, and batch-read
  ownership, missing-reference and expiry checks.
- `node scripts/test-worker-flow.mjs`: parallel execution, cancellation, budget
  ceiling and lease checks passed (four fixture calls, one approval, 486 ms).
- MCP `npm test`: 195 passed; no MCP source changes required.
- App: 2,188 passed, four pre-existing failures. The same four failures reproduce
  at unchanged commit `132a026`: two manifest/icon tests and two organisation
  owner/admin workspace-selector tests. The changed answer/recovery tests pass.
- App lint, TypeScript checks and `npm run build:production` passed. A plain build
  initially used local development endpoints; the existing production build entry
  point correctly selects the configured public endpoints.
- Changed roots pass `git diff --check`.

These checks establish local behavior and the measured fixture improvements.
Production question-to-price and approval-to-answer percentiles still require
representative live measurements; fixture timings are not end-to-end promises.
