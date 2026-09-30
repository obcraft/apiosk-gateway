# Recurring Ask orders

Status: implemented locally; no production migration, deployment, funded schedule,
provider purchase, or Slack/Gmail delivery has been performed as part of this change.

After a completed Ask purchase group or saved task, **Make recurring** creates a
finite schedule for the purchased API calls. The review shows individual sources,
API subtotal and fees per run, the exact occurrence count, the period total, the
remaining available balance, and the delivery destination. One **Reserve &
schedule** approval funds the period. The original order is recovered from
owner/workspace-scoped tasks on the server; the browser cannot supply prices or
replacement API requests.

## Current product behavior

- Daily or selected-weekday recurrence at one local time, with an inclusive start
  and end date, an IANA timezone, and at most 366 occurrences. Weekdays include
  holidays. Runs start at the selected time; delivery follows source processing.
- The saved purchased calls and inputs are repeated. This does not re-run source
  discovery or substitute a different provider. Resolved identifiers remain
  pinned to the original order. Source contract or price changes stop internal
  execution for review; provider ceilings also remain enforced by the existing
  execution path.
- ISO date/timestamp input fields are individually reviewable. Common English and
  Dutch relative expressions (today, yesterday, last/past N hours/days/weeks) are
  suggested from the original question and purchase timestamp. Fixed date
  literals are preserved. Each date field can explicitly remain fixed or advance
  relative to the scheduled occurrence. Native relative expressions inside API
  query strings remain API-evaluated. Rules never replace arbitrary text or
  unrelated numeric fields. Date-only offsets use local calendar days; timestamp
  offsets use elapsed seconds, including across daylight-saving changes.
- Delivery currently supports connected Slack and Gmail accounts. Slack takes a
  channel ID; Gmail takes one recipient address. The destination and all date
  rules are part of the approved immutable quote. Delivery is included in the
  displayed total; this change introduces no extra delivery charge.
- `/?recurring=all` lists schedules; `/?recurring=<id>` opens a schedule and its
  history. Results use the existing answer renderer. Opening a saved run only
  reads its result. Pause, resume, and cancellation are server-authorized.
- An occurrence more than one hour late is skipped, releasing its unused share.
  Paused/attention schedules also release overdue unused occurrences. Cancellation
  releases queued occurrences immediately; an in-flight charge keeps its own
  journal identity until its outcome is known.

## Queue, authorization, and money

`billing.recurring_orders` stores the mandate and finite schedule; `recurring_runs`
is the durable occurrence queue; `recurring_calls` records each dispatch;
`recurring_deliveries` is a separate result delivery outbox. Four execution
consumers and one delivery consumer run per enabled Gateway process. PostgreSQL
`FOR UPDATE SKIP LOCKED` claims runs across replicas. Execution leases renew every
20 seconds and expire after three minutes. No user JWT or refresh token is stored.

Approval atomically debits the existing `billing.user_balances` available balance
and creates the occurrences. Each source call transfers its exact cap from the
schedule hold into the existing `reserve_balance_for_call` ledger **within one
transaction**; it does not debit the available balance a second time. Normal
settlement/refund functions continue to book actual spend and return differences.
Schedule funds obey `approved = remaining reserved + transferred + released`.
Balance summaries include both future schedule holds and unresolved call holds.
Repeated approval and settlement are idempotent. A unique active mandate prevents
accidentally funding an identical schedule again under a new quote ID.

The worker's HMAC bearer is bound to one durable call and expires in 90 seconds.
The internal Edge entrypoint loads the owner, selection, and inputs from that call,
then uses the existing `runExecution` implementation. It does not accept those
fields from the HTTP body. The ordinary `use-selection` endpoint retains platform
JWT verification. New internal RPCs are revoked from `anon` and `authenticated`.
Recurring-order data is accessed through scoped Gateway routes; authenticated
balance reads receive only owner/workspace/remaining-hold columns under RLS.

A restarted worker never resends a dispatch with an uncertain payment outcome.
It reads the existing charge and saved call result. An unresolved charge pauses
the schedule for attention and keeps money held. This implementation does not add
a new automatic reconciliation or manual repayment workflow: the existing payment
journal remains authoritative, and an unresolved run needs operator review.

Delivery retries a proven HTTP 429 with bounded exponential backoff (five total
attempts). Delivery transport timeouts and crashes become `uncertain`, rather than
risking duplicate messages. A delivery retry cannot purchase source data again.
Results remain available in Apiosk if delivery fails. The existing source-backed
analysis builds a fresh answer from each run's saved results; if analysis is
unavailable, delivery links to the saved source results without inventing an answer.

## Release sequence

1. Review/apply `20260930091005_recurring_orders.sql` once. Identical copies live
   in App and Gateway migration directories, consistent with the shared database.
   Review database advisors and verify the real schema's role grants before
   enabling the feature.
2. Configure the same new random `RECURRING_WORKER_SECRET` (at least 32 characters)
   in Gateway and Supabase Edge secrets. Never put it in a `VITE_` variable.
3. Deploy `recurring-execute` with its configured `verify_jwt = false`; the
   call-bound HMAC and database mandate are its authentication. Deploy the updated
   `use-selection` handler with its existing `verify_jwt = true` unchanged.
4. Deploy Gateway and App, then set `RECURRING_ORDERS_ENABLED=1` on Gateway.
   `V2_EXECUTION_ENABLED=1` must also be set. Disabling either stops worker startup
   and new approvals; existing schedules can still be read, paused, or cancelled.
5. With an explicitly approved funded test order and real connected destination,
   verify one scheduled purchase, receipt, balance movement, saved answer and
   delivery end to end. This paid/live validation has **not** been performed.

No cron extension, Redis instance, or browser tab is required. PostgreSQL queue
rows and leases are the durable source of truth. Monitor `attention` orders,
`unresolved` runs, `uncertain` deliveries, stale leases and worker log errors.

## Local verification

- App TypeScript and production Vite build; Edge TypeScript includes the ordinary
  and recurring execution entrypoints.
- Focused App and existing Ask/integration tests, including review-before-approval,
  stale quote invalidation, insufficient funds, HMAC expiry/call binding, reservation
  conservation, duplicate approval, cancellation, and Amsterdam daylight saving.
- Rust recurring-module tests for schedule validation, fixed/relative dates and
  field-specific materialization.
- `scripts/test-recurring-concurrency.py` against an empty disposable PostgreSQL
  database checks simultaneous approvals, queue claims and cancellation. Its schema
  is an isolated fixture of the existing ledger, not a full production-schema replay.
- Desktop (1200 px) and mobile (390 px) browser checks use local test data. They
  verify form layout and the review/approval/status transition, not live purchases.
