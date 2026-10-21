# Host-scheduled company monitoring

Additive to the frozen Ask flow. Claude/ChatGPT owns its Monday schedule and mail
connector; Gateway owns fixed company identities, task/occurrence IDs, original
source evidence, comparisons and delivered-signal state. Existing spending and
approval checks apply to every fresh run. No new recurring spending mandate.

## Routes

All routes use existing connected-user authentication and check owner, workspace
and connection before accessing child rows. Organisation API keys remain on their
separate scoped API. Another MCP connection cannot take over the monitor.

| Route | Behavior |
|---|---|
| `POST /v2/company-checks` | Create/recover the check and occurrence. Body: check_id UUID, occurrence stable key, companies [{name,kvk_number}] on first use. Maximum three companies; KVK strings have eight digits. |
| `POST /v2/company-checks/:id/collect` | Body: occurrence. Collect terminal settled task evidence and compare, with no source calls. |
| `GET /v2/company-checks/:id` | Saved list, recent occurrences and latest report. |
| `GET /v2/company-checks/:id?occurrence=...&result_ref=...` | Original retained evidence after ordinary task expiry. |
| `POST /v2/company-checks/:id/email` | Body: occurrence, delivery_id UUID. Atomically prepare an immutable new-signal email handoff; never sends. |
| `POST /v2/company-checks/:id/acknowledge` | Body: delivery_id, provider, provider_message_id from a successful host email receipt. Acknowledge only the saved payload's signal IDs. |

## Persistence, comparisons and email

The migration creates four private RLS-protected runtime-only tables. A check
row/transaction lock and unique indexes serialize occurrences and mail handoffs.
Only one open source run and one unconfirmed mail handoff can exist per check.
The durable discovery request ID and task ID recover retries without a second
purchase. No transaction is held across provider or planning requests.

A separate bounded worker retains terminal source documents every 30 seconds,
even after the host closes. It cannot plan, approve, purchase, mail or schedule.
Unsettled billing and changing task revisions wait. If the table is absent, the
worker logs once and exits without affecting Ask. Snapshot writes are immutable;
read presentation reapplies the delivered-signal ledger to suppress old signals.

The first KVK result is a baseline. Missing fields/results never erase a previous
value. Supported arrays ignore ordering and link metadata. Unsent registry
changes remain available while the same value is last observed. Undelivered news
keeps its original evidence beyond the next search window, labelled historical;
that evidence never counts as fresh coverage or a new source charge. Screening
fingerprints ignore scores, retrieval times and ordering. News fingerprints use
the canonical article URL across providers, stripping tracking parameters.

Preparing email reads only saved evidence and unreported signal IDs, at most 100
per message. Zero new signals returns suppressed, including when coverage is
missing. A fresh prepare returns dispatch_allowed=true once. Every repeat or
other attempt during a pending handoff returns false. The host sends the exact
plain-text payload using its own authorized mail connector, then supplies the
successful receipt. Confirmation and acknowledgement commit atomically. It is
explicitly a host-reported receipt, not a Gateway-verified provider response.

There is no automatic resend or timeout expiry. If the prepare response or send
result is lost, reconcile the original connector action or sent-message record.
A handoff with no recoverable receipt remains blocked for operator reconciliation.
Displaying a report/draft cannot acknowledge anything. Checks can still save
source evidence while a delivery is blocked. A schedule-completion notification
is not a substitute for the requested sourced email.

## Evidence adapters

- `kvk-dutch-business-register`: exact returned kvkNummer and naam. Compare name,
  trade names, SBI activities, worker count, owner legal name/form, main-branch
  addresses/websites and registration end when those fields are present. This
  covers available profile fields, not all registry events.
- `opensanctions`: one query response, bound to the exact requested KVK subject.
  match=true remains a potential screening match. Complete coverage requires
  explicit total matching the returned array; partial results may show positive
  evidence without claiming completeness.
- `exa-search` / `tavily`: saved results arrays, exact subject binding, public
  article URL, publication date within seven days of retrieval, and a company-name
  phrase in title/text. Names do not prove identity; all mentions carry this
  limitation. Unsupported/undated rows leave news coverage incomplete. Results
  are limited to the returned search window, never an exhaustive news archive.

Original raw documents, evidence pointers, dates and actual source charges remain
saved. The public projection is bounded. No CSV, balances or CIR source is needed.
The source question requests current KVK, OpenSanctions and news for each company;
unsupported sources remain gaps rather than invented findings.

## Validation and rollout

Apply `20261021114309_company_check_snapshots.sql`, deploy Gateway then MCP.
Before production, validate the SQL in PostgreSQL: tenant isolation, simultaneous
occurrences, repeated mail prepares, distinct concurrent mail IDs, lost replies,
receipt mismatch, atomic acknowledgement, and reopen after normal task expiry.

The socket-free `node scripts/test-company-check-storage.mjs` applies the real
migration to PGlite and checks runtime grants, RLS, scoped reads, one-open-run
and one-pending-delivery constraints, receipt uniqueness and rollback. It does
not establish native PostgreSQL multi-connection concurrency.

Run `cargo test --lib company_checks --offline` and `cargo check --bin
apiosk-gateway-v2 --offline`. MCP has focused protocol, renderer and forwarding
tests. An installed-client acceptance run in BOTH hosts must still demonstrate:
real company lookup → source approval → saved evidence → Monday task → email
connector receipt → unchanged repeat with no email. It must also demonstrate
partial coverage and uncertain-send recovery. Fixtures do not prove these live
outcomes; no local test sends mail or buys source data.
