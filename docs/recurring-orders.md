# Recurring Ask orders

Status: the frozen execution runtime, scheduling, funding, and Slack/Gmail review
flow are implemented and tested locally. This is **not the complete product
flow**: arbitrary manual Ask combinations, versioned editing, more delivery
channels, and delegated API/MCP recurrence remain later stages. The private
production rollout is enabled for the approved owner after applying the locked
workflow migration. No real provider purchase, funded live schedule, or real
Slack/Gmail send is part of deployment validation.

Production uses RECURRING_ORDERS_ENABLED="1". The App obtains
its capability from authenticated GET /v2/recurring-orders/access; there is no
browser or development-mode allowlist. The server returns `{enabled:boolean, management_enabled:boolean}`. The first
flag permits scheduling; the second permits access to existing-order management.
RECURRING_ALLOWED_OWNER_IDS contains immutable authenticated owner UUIDs and is
configured only server-side in Gateway and Edge. An absent, empty, malformed, or
nonmatching list denies access, including queue refill, execution and delivery.
Other users receive recurring_forbidden from recurring management routes.

## Local manual testing

`node gateway/scripts/dev-local.mjs --execute --recurring --no-open` enables workers when configured. Execution requires
`RECURRING_WORKER_SECRET` in `gateway/.env` (or the inherited environment), matching
Supabase's `recurring-execute` secret exactly. The launcher must not generate a
replacement: a local random value is rejected by hosted execution with 401. When
`gateway/.local/dev-stack/recurring-worker-secret` was provisioned beforehand,
the launcher can reuse that existing credential; it never creates that file.
Missing credentials skip recurring workers without blocking startup. The local
launcher sets `RECURRING_LOCAL_MANAGEMENT_ENABLED=1`; on a loopback bind,
allowlisted owners can still open Recurring orders and view or stop existing
orders. Scheduling and execution keep the worker-secret requirement. Production
management access retains the existing execution gate.

A manual-trigger 202 response only accepts an occurrence. The UI follows the
saved run and delivery states until completion; source or authorization failures
replace the progress message. An accepted occurrence cannot be triggered again
from a stale queued response. Manual tests consume the approved scheduled run.

## Reviewed behavior

A question with an explicit English or Dutch schedule (for example "iedere
maandag ... mail me alleen nieuwe signalen") now prepares the recurring form
inline below the Ask plan. It prefills weekdays, time when supplied, Gmail/Slack
and change-only delivery. The user reviews the default time, run count and
timezone. Source intake preserves the requested company information categories;
schedule and delivery of those results are not additional retrieval requirements.
No customer-list upload or private invoice balance is implied.

The existing completed-receipt requirement remains: approve the first check and
resolve ambiguous companies through the normal choice flow. That check is billed
separately. The inline form retains edits while the check finishes, then prepares
the exact recurring quote automatically without approving it. Before completion
it labels the plan price and finite total as estimates; afterward it shows the
current per-run price, total to reserve and delivery destination. A separate
explicit schedule approval reserves the reviewed amount. Failed or partial
checks cannot activate a schedule. Ordinary one-off requests retain **Make
recurring**. These are locally tested UI and prompt changes, not evidence of
live source coverage, a funded schedule or actual email delivery.

After a completed purchased task, **Make recurring** recovers the original sources
and receipts on the server. The review shows ordered sources, dependencies,
fee-inclusive cap per run, schedule, funding mode, date rules, connected account,
and destination. Approval submits the exact total, per-run cap, and saved workflow
fingerprint. The browser cannot replace requests or set authoritative prices.

The frozen recipe repeats source A, then source B, in the approved order. It pins
endpoint/provider identities, contracts and prices, extraction schemas, literal
inputs, dependency mappings, approved date rules, processing version, and delivery
contract. Values may change each run; sources, order, bindings, and output rules
do not. There is no replanning, provider substitution, or fallback.

- Search → company ID → accounts consumes the ID returned by **this occurrence's**
  search. It does not replay the old resolved ID. Explicitly supplied literal IDs
  stay fixed. Missing, invalid, ambiguous, or truncated dependency results stop
  the sequence.
- Every primary plan step needs its matching completed receipt. Matching uses
  service and entity identity, not completion order. The backend can combine
  several completed tasks in the supplied order. Independent purchases stay
  independent, without inferred cross-task edges. A general manual multi-source
  selection editor in Ask is not yet implemented.
- Direct external purchases preserve their reviewed path/query/body mapping,
  input schema, wire names, and JSON types. Inputs and input_parts come from the
  frozen contract, not guesses based on field names. Unreviewed external
  dependency chains are refused.
- ISO date/timestamp inputs can stay fixed or follow an approved relative offset.
  Suggestions use supported English/Dutch expressions in the original question
  and purchase time. Date-only offsets use local calendar days; timestamps use
  elapsed seconds. Rules cannot overwrite dynamic dependency fields. Relative
  expressions inside provider queries remain provider-evaluated.
- Source contracts and prices are checked before a run and again before every
  undispatched call. A change while A executes therefore stops B and puts the
  order in attention. Paid A results remain saved. New prices or sources are
  never silently accepted.

## Output scope and delivery

source_snapshot_v1 is a deterministic **saved source report**. It stores source
documents, identities, retrieval times, charges, and simple source-backed report
entries. It does not call a model or regenerate the original Ask analysis, PDF,
comparison, or another free-form deliverable. Those outputs need separate
versioned processing contracts before being offered as repeatable work. Review
and delivery must describe this output scope explicitly.

Slack and Gmail are the current recurring channels. Preparation pins the exact
Composio tool slug and version, connected-account ID, schema, fixed arguments,
and destination. Runs execute that saved tool directly without rediscovery or a
switch to latest. Only declared message-content fields receive the new saved
report. An unavailable approved account/contract stops delivery. Delivery is
included in the displayed total; no extra delivery charge is introduced here.

Self delivery uses `{toolkit, connected_account_id, recipient: "self"}`.
Gmail resolves the authenticated owner's verified email through the private
`billing.recurring_self_email` function. Slack resolves the owned account's human
identity and opens that identity's private self-DM through Composio.

Slack also accepts `recipient: "conversation"` with a selected `destination` ID.
The account-scoped, paginated `/v2/recurring-orders/destinations` endpoint returns
joined channels and group chats using metadata only. It does not read messages,
join channels, or send anything. The selected conversation is checked again at
quote, approval, pre-purchase and delivery; archived or inaccessible conversations
are refused. The reviewed conversation is pinned into the immutable contract.
Changing the account or destination invalidates the displayed quote. A retry of
the same quote ID keeps the reviewed destination. Connecting another app opens a
new tab so the pending schedule remains in place.

A changed or no-longer-verified Gmail address requires a new review. A destination
failure before purchase finishes the occurrence and releases its unused funds;
after source execution, it fails delivery while retaining the saved result.

With **Only deliver when source results change**, the first successful result is
sent and later equal results remain saved without a new delivery. Comparison
includes provider data in approved source order. Generated receipt IDs and
retrieval times do not count as changes; provider-supplied timestamps still do.
Checks continue to incur normal source costs when delivery is suppressed.

The outbox retries a proven HTTP 429 with bounded exponential backoff, up to five
attempts. A transport timeout or crash after dispatch becomes uncertain; there is
no blind resend. Delivery retries never purchase sources. Cancellation removes
queued sends, including failure notifications, and prevents a later worker claim.
A send already claimed before cancellation can still complete; cancellation
cannot recall a message already in flight.

## Schedule, funding, and management

The UI supports daily, weekly with selected weekdays, and monthly schedules, with
an interval from 1 through 12, local time, and an IANA timezone. Monthly schedules
use the original start-day: January 31 becomes February's last day and then March
31. Weekdays include public holidays. This is a bounded schedule editor, not an
arbitrary cron-expression editor.

- **Prepaid:** choose an inclusive end date or 1–366 executions. Approval reserves
  the complete period total. Each occurrence owns its share of that hold.
- **Per run / ongoing:** no end date or count and no upfront reservation. The
  server extends the durable occurrence queue. Before contacting any source, it
  atomically reserves the whole occurrence cap. Insufficient balance produces
  skipped_insufficient_balance; future occurrences remain scheduled. A partially
  funded sequence never starts.

The routes are /recurring-orders for the list and
/recurring-orders?order=<uuid> for history. Reading saved results is free and never
reruns sources. Pause, resume, and cancellation require current owner/workspace
authorization. Pause or cancel during a run prevents the next source; paid
results remain available. Cancellation releases unused queued holds. The owner
can release future holds even after workspace spending access is revoked.

An occurrence more than one hour late is skipped instead of purchased as
catch-up work. Paused/attention orders release overdue unused occurrences.
An attention order cannot resume with a changed contract: it needs a new reviewed
quote. Approved configuration is immutable in the database; editing an existing
order into a new approved version remains a later product stage.

Delivery is the supported exception: an owner with spending access can edit an
active or paused order's Slack/Gmail account and destination from its details.
`POST /v2/recurring-orders/:id/delivery` requires the current workflow fingerprint
and resolves the caller-owned destination before preparing its frozen action.
The database increments the delivery revision and records the old/new contracts,
while preserving sources, schedule and financial authorization. Running or
unresolved occurrences and queued/sending/uncertain deliveries block editing.
The change-only baseline resets so the next completed result reaches the new
destination. Saving performs metadata discovery only; it sends no message.

## Durable execution and money

### Manual localhost testing

From the Main directory, run
`node gateway/scripts/dev-local.mjs --execute --recurring --no-open`.
The recurring-order details page shows **Manual trigger** only in a development
build on localhost/loopback. The launcher enables
`RECURRING_MANUAL_TRIGGER_ENABLED=1`; the endpoint also requires a loopback
`BIND_ADDRESS`, enabled recurring execution, an allowed authenticated owner and
workspace spending permission.

The button starts the next approved, queued occurrence early. This is a real
run: normal source charges and the saved delivery policy apply. It consumes that
occurrence, preserves its scheduled date (including relative date inputs), and
does not create an extra occurrence or alter the remaining schedule. Paused,
cancelled, completed and attention orders cannot be triggered. An earlier
unfinished run blocks later ones. The request names one run ID, so retries cannot
silently consume the following occurrence. The regular worker handles execution,
lease recovery, saved results and delivery. No schema migration is needed.

Check the claim SQL without paid calls with
`node scripts/test-recurring-manual-trigger.mjs` from the Gateway directory.

billing.recurring_orders stores the mandate; recurring_runs is the occurrence
queue; recurring_calls holds call identity, inputs, and receipts;
recurring_deliveries is the independent delivery outbox. Four execution consumers
and one delivery consumer start per enabled Gateway process. PostgreSQL
FOR UPDATE SKIP LOCKED coordinates replicas. Leases renew every 20 seconds and
expire after three minutes. Only the oldest unfinished occurrence of an order
can run. Calls within it execute sequentially. No Redis, cron extension, browser
tab, saved user JWT, or refresh token is needed.

Approval/funding and cancellation use transactions and a consistent account/order
lock path. Each source cap transfers from the occurrence hold into the existing
reserve_balance_for_call ledger in one transaction; it does not debit available
balance twice. Normal settlement/refund functions book actual cost and return
differences. One occurrence cannot consume another occurrence's funding.

The conservation rule is **approved/funded = remaining reserved + transferred to
calls + released**. For ongoing orders the cumulative funded amount increases
only when an occurrence is funded. Transfer is a ledger allocation, not
necessarily final spend. Final charges/refunds remain authoritative in the
payment journal. Duplicate approval, funding, queue insertion, and settlement are
idempotent. Identical active mandates cannot be funded again under another ID.

A restarted worker reads existing charges and saved call results instead of
resending a dispatch that may already have cost money. Unknown payment or missing
complete paid-result evidence stops as unresolved and puts the order in attention.
The hold/journal remains available for operator review. Automatic reconciliation
and manual repayment tooling are not added here.

The internal Edge recurring-execute entrypoint accepts a 90-second HMAC bearer
bound to one call. Its database lookup supplies owner, approved selection, inputs,
and frozen input_parts; the HTTP body cannot replace them. It uses the existing
paid source implementation. Ordinary use-selection JWT verification is unchanged.
Internal RPCs are unavailable to anon and authenticated. Gateway access is
owner/workspace scoped; authenticated balance reads expose only permitted hold
columns under RLS.

## Staged ownership and acceptance

| Stage | Status | Boundary and acceptance |
| --- | --- | --- |
| 1. Frozen recipe | Implemented locally | Same sources/order, current-run dependencies, frozen vocabulary/external mapping, no replanning. Owned by src/recurring/recipe.rs, external_inputs.rs, processing.rs, and worker.rs. |
| 2. Money and schedule | Implemented locally | Prepaid count/end date and ongoing full-run funding; idempotency, skips, conservation, and cancellation. Owned by mirrored recurring migrations, schedule.rs, App recurring domain/form, and ledger tests. |
| 3. Slack/Gmail | Implemented locally | Exact account/destination and source-report scope; pinned execution, change-only delivery, cancellation-aware outbox. Owned by src/integrations/frozen.rs, worker, and App recurring UI. Real delivery remains unverified. |
| 4. Manual Ask combinations | Remaining | Add multi-selection/order review to Ask and purchase groups; retain every selected task rather than only items[0]. Preserve automatic research and validated dependencies. |
| 5. Versioned editing | Remaining | New quote/version with old/new comparison, approval, atomic future-hold transfer/release, and occurrence cutover. Never edit an approved recipe in place. |
| 6. More channels | Remaining | Teams, WhatsApp, Telegram, Discord, etc. via Composio, with channel schemas, destination/account review, content rules, permissions, and uncertain-send tests. Generic integration listing is not recurring delivery support. |
| 7. API/MCP parity | Remaining | Explicit delegated recurrence rights, connection-aware money/execution, destination consent, and protocol parity. Current recurring routes reject agent connections and organization API keys. |
| 8. Production acceptance | Remaining | Migrate/release behind gates, validate real schema/grants and concurrency, then funded source-to-receipt-to-delivery acceptance. Local fixtures are not production proof. |

### Stage 7: delegated API/MCP recurrence

Removing person() from src/server/recurring.rs alone is insufficient: the current
worker uses connection: None and creates personal selections. That would discard
the delegate's budget and revocation context.

1. **Billing/auth ownership:** a new mirrored migration must persist immutable
   creator connection/key identity and an explicit owner-approved recurrence
   grant. Reuse src/execution.rs connection budget/automatic-approval rules, while
   including recurring holds in shared agents.connection_spend_since,
   gateway_v2.connection_budget, and reservation accounting. Existing definitions
   are in App 20260922160000_organisation_api_keys.sql and
   20260929120000_connection_auto_approve.sql. New scopes/settings belong in
   Gateway src/auth.rs and App src/lib/domain/api-keys.ts,
   src/lib/domain/connections.ts, src/components/connections/connection-list.tsx,
   and src/components/workspaces/organisation-api-keys.tsx. Recurrence authority
   must be opt-in, including in the API-key form's default-scope selection.
2. **Runtime/API ownership:** src/server/recurring.rs, src/recurring.rs, and the
   worker must scope tasks/orders to the creator connection and carry it into
   selections, workspace checks, and ledger metadata. Revalidate permissions,
   expiry, revocation, and the entire run cap at approval/funding and dispatch.
   Owner cancellation must release unused funds after revocation. Personal MCP
   integrations already have read/connect routes; organization keys need an
   explicit consented Composio principal/delegation in src/server/integrations.rs
   and src/integrations/frozen.rs, rather than a personal identity inferred from
   an organization billing UUID.
3. **MCP ownership:** add mcp/src/gateway-v2-recurring.mjs for tool definitions,
   schemas, order results, and request mapping; integrate in gateway-v2.mjs.
   Update gateway-v2-instructions.md, whose current guidance delegates scheduling
   to the host. Add mcp/test/gateway-v2-recurring.test.mjs. Preserve request IDs
   and distinguish standing delegated mandates from human approval. Never retry
   an uncertain paid execution or send.

Acceptance must cover equal recipes/prices through App/API/MCP; default-off
recurrence rights; cross-connection isolation; matching total/cap/fingerprint;
real-Postgres concurrent direct spending and recurring holds within daily/monthly
ceilings; expiry/revocation after A stopping B and delivery; owner refund after
revocation; exact delegated Composio account/destination; and one order,
occurrence, charge, and delivery under idempotent retries.

## Local verification

From gateway, run focused Rust checks:

~~~sh
cargo test --lib recurring::
cargo test --lib integrations::
~~~

The isolated source-to-quote-to-worker harness needs a **fresh** local
tests/support-db.mjs instance for every invocation. In one terminal from gateway:

~~~sh
PGTEST_PORT=54349 node tests/support-db.mjs
~~~

In another terminal, also from gateway:

~~~sh
TEST_DATABASE_URL='postgresql://postgres@127.0.0.1:54349/postgres?sslmode=disable' cargo test --test locked_recurring_flow -- --ignored --nocapture
~~~

Stop the fixture afterward. This harness installs both real recurring migrations
over explicit local tenancy/ledger fixtures. It passed five A→B cycles with
changing IDs, zero model calls, one charge per source, pinned Composio account and
version, restart without repurchase, in-flight pause/cancel preserving paid data,
post-A price/contract drift stopping B, and cancellation of queued sends. Its HTTP
regression also removes workspace access: reads/resume remain forbidden, only
the correct owner/workspace can cancel, the acknowledgement reveals no sources,
and unused funds return to the workspace rather than the owner's personal
balance. Sources, Auth, and Composio are local stubs. PGlite's one connection does
not prove production concurrency.

From app, focused checks include:

~~~sh
npx vitest run tests/lib/recurring-auth.test.ts tests/lib/recurring-request.test.ts tests/lib/recurring-config.test.ts tests/lib/recurring-order-domain.test.ts tests/lib/recurring-ledger.test.ts tests/lib/locked-recurring-ledger.test.ts tests/components/ask/recurring-order.test.tsx tests/routes/catalogue-recurring.test.tsx
npm run build
~~~

Use python3 scripts/test-locked-recurring-concurrency.py <local-postgres-url>
from app against an **empty disposable localhost PostgreSQL database** for
multi-connection approval, funding, allocation, cancellation, and conservation
tests. This remains an isolated ledger fixture, not a production-schema replay.

The broader existing public_api_flow fixture is not an all-green release gate.
Its unchanged multi-source analysis case currently returns partial where it
expects completed (source_coverage_missing). Auth, scope, direct execution,
idempotency, and initial Ask checks passed before that assertion. This change
does not modify that fixture or its analysis implementation.

## Production acceptance sequence

1. Review the mirrored migrations in App/Gateway:
   20260930091005_recurring_orders.sql and
   20261009110915_locked_recurring_workflows.sql, plus the narrow verified-email
   helper 20261009142403_recurring_self_email.sql. Apply each once to the shared
   database. Verify actual roles, RLS, grants, and existing ledger behavior.
   Existing active/paused legacy orders become attention, preserving financial
   holds. They need a new reviewed recipe rather than blind legacy replay.
2. Configure the same random RECURRING_WORKER_SECRET of at least 32 characters in
   Gateway and Supabase Edge secrets. Never expose it through a VITE_ variable.
   Configure the same RECURRING_ALLOWED_OWNER_IDS allowlist in both, initially
   containing only the explicitly authorized, verified production owner UUID.
   Configure needed Composio accounts/toolkits and verify pinned-version execution
   with the actual service.
3. Deploy recurring-execute with its configured verify_jwt=false and HMAC/DB
   mandate checks. Deploy updated use-selection with verify_jwt=true unchanged,
   then Gateway and App while gates remain off.
4. Keep Gateway recurrence disabled until the locked migration is applied.
   Edge execution also requires the matching worker secret and owner allowlist.
   Enable only with V2_EXECUTION_ENABLED=1,
   RECURRING_ORDERS_ENABLED=1, the worker secret and the one-owner allowlist;
   restart/redeploy for worker startup. Workers recheck the gates before
   claiming, funding, paid calls and sending. Runtime configuration changes still
   require a restart. Read, pause and cancel routes require an allowed owner.
   Verify that the selected owner's access endpoint returns true, an unrelated
   authenticated owner's returns false, and that direct management requests by
   the unrelated owner are denied. Do not infer API protection from hidden UI.
5. With explicit funded/live authorization, verify real price → approval →
   scheduled A→B → settlement → saved report → chosen delivery. Exercise
   insufficient balance, revocation, cancellation, contract drift, and recovery.
   Confirm the production UI and source usage notices.

Monitor attention orders, unresolved calls/runs, uncertain deliveries, missed
occurrences, stale leases, and reservation balances. Production enablement does
not establish paid/live acceptance; a real funded schedule and delivery remain
separate checks.

Financial status comes from the complete charge journal, independently of the
latest 100 run cards. The response separates `reserved`, `pending`, `spent`,
`refunded`, and unused `released` amounts. The UI shows the finite total as run
count × price per run, displays returned funds, and refreshes the global balance
when the order's financial state changes. An unavailable refresh is labelled;
missing lifetime totals are never replaced by the truncated run-card subtotal.
