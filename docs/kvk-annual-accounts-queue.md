# KVK annual accounts: durable cache and queue

The gateway checks a shared PostgreSQL source cache after approval and before
creating a buyer payment reservation. A miss creates one job; workers retrieve
and save the source response independently of the browser request. The gateway
waits for that job, then continues through its existing payment flow. The embedded
payment executor reads the same cache and cannot bypass the queue for this source.

## Deployment and capacity

One standalone `apiosk-kvk-worker` process runs on Fly.io in
`apiosk-kvk-worker-1`, with one Machine in `ams`,
256 MiB memory, two database connections, and no public HTTP service. The gateway
runs with `KVK_WORKERS_ENABLED=0` so only dedicated workers fetch source data.

All production workers share **one persistent source budget**, with a 61-second
start-to-start interval. Separate routes do not multiply that budget. This
release does not claim five new source documents per
minute. Cached requests do not consume a source allowance. A provider-approved
capacity increase would require a separate implementation and acceptance test.

The deployment was reduced from five to one worker on 2026-09-12 to lower
costs. Workers 2–5 are retired; future deployments target only worker 1.
The remaining worker retains its fixed egress IP. At published Amsterdam prices,
one continuously running shared-cpu-1x/256 MiB Machine and one static egress IP
pair cost approximately $5.62 per 30 days, excluding traffic and taxes, versus
$28.10 for five (a saving of $22.48). The database queue and all saved annual
accounts remain in Supabase. A single worker relies on Fly restart and durable
queue recovery, without a second live worker for failover.

Worker startup checks the schema and configured outgoing address. The dedicated
`apiosk_kvk_worker` database login inherits `kvk_worker_runtime`, limited to the KVK
cache and queue tables. It cannot read customer balances. A separate session pool
avoids exhausting the gateway login's pool. Its credential lives in the ignored,
owner-only `.local/kvk-worker.env`; never put it in Git or command output.

The existing deployment can be inspected with `scripts/deploy-kvk-workers.py`
(default mode is read-only). Fly image builds use `deploy/kvk-workers/Dockerfile`;
the deployment script accepts a built image and ensures one started Machine per
app. Static egress allocations and running Machines are billable.

## Storage

`kvk_cache.annual_accounts` stores the original provider JSON. Its primary key is:

`kvk_number + financial_year + document_type + version`

- `financial_year` comes from the returned `FinancialYear`.
- `document_type` is `kvk-hvds-xbrl-v1`: partial structured source data, not a
  complete signed annual report or an original PDF.
- `version` is SHA-256 of canonical serialized JSON. Different years and corrected
  versions coexist; existing versions are retained.
- `source_url` and `fetched_at` retain provenance. Missing financial fields remain
  missing. Source data contains no shared buyer receipts or balances.

The provider URL includes the literal `kvknummer` path segment:
`https://opendata.kvk.nl/api/v1/hvds/jaarrekeningen/kvknummer/{kvkNummer}`.
The source accepts a KVK number, without a historical year/version selector.
See the [official OpenAPI definition](https://developers.kvk.nl/cms/api/uploads/hvds_jaarrekeningen.yaml).

`kvk_cache.jobs` holds one refreshable latest lookup per KVK number. Ten concurrent
misses for the same company therefore share one job. The resulting document is
indexed by its actual source year and version. Successful latest lookups refresh
on demand after 24 hours; old document versions stay archived. Exact stored
versions can also be read by year/version.

`kvk_cache.fetch_attempts` retains each claim, outgoing identity, timestamps and
outcome independently of the refreshable job. Failed attempts remain evidence and
are never stored as annual-account documents.

## Queue, failures and payments

PostgreSQL row locks, five durable slots and 120-second fenced leases prevent
competing claims. HTTP has a 30-second deadline and a 4 MiB body limit. A stale
worker cannot overwrite a later claim. Process crashes can require a repeated
fetch after lease expiry; this is not an exactly-once network-delivery guarantee.

429, 5xx, transport and schema failures retry at most five times. `Retry-After`
extends the shared provider budget. Missing documents and exhausted retries have
a five-minute negative cache. Waiting jobs create no buyer reservation. Cached
delivery still follows existing advertised pricing, approval and buyer-specific
idempotency rules. A source fetch does not charge a user in the background.

Legacy direct cold requests return `503 kvk_queued` with `Retry-After: 2`. V2 waits
before entering that payment boundary. Existing saved-result PDF downloads remain
unchanged; a generated report must not be described as an original KVK PDF.

The schema is private and RLS-enabled. Only the server roles
`gateway_v2_runtime`, `kvk_worker_runtime` and `service_role` have the applicable
grants; anonymous and authenticated browser roles have no schema/table access.

## Migration and validation

Both migrations are applied to production and mirrored in the App repository:

- `20260912111106_kvk_annual_accounts_queue.sql`
- `20260912112028_kvk_worker_database_role.sql`

The gateway and embedded executor both depend on `crates/kvk-accounts`. Publish
both the parent changes and the `legacy-executor` submodule changes together.

Local tests use disposable PostgreSQL and a loopback HTTP fixture:

```sh
python3 scripts/test-kvk-accounts.py
cargo test --offline --workspace --lib
cargo check --offline --manifest-path legacy-executor/Cargo.toml --bin apiosk-gateway
```

They cover shared production admission across different routes, single-flight,
queue waiting, source versions, retries, crash fencing, role access and the gateway
payment boundary. Simulated independent fixture budgets are not live throughput
proof. Live source tests do not exercise a real buyer payment.

## Live source verification

`examples/kvk_queue_live.rs` accepts one to five company records and a JSON evidence
path. It schedules a short queue delay, adds ten duplicate requests, validates the
stored source year and content hash, and replays every lookup three times to check
that no new source fetch was created. Existing successful documents are never
deleted to manufacture a cold cache. Prior failed attempts remain in a separate
baseline; only new attempts count toward the run result.

The 2026-09-12 verification stored five distinct companies: Mollie (2020), Volve
Capital (2025), Bitvavo (2020), Moneybird (2024) and WeFact (2025). Picnic and
Coolblue returned 404 and remain recorded as failures. Initial individual tests
were paced at least 65 seconds apart; the final two-company batch observed
61.619 seconds between starts under the shared production limiter. All 21 replay
lookups (15 successful documents and six negative-cache reads) added no fetches.
Evidence is saved in `output/kvk-fly-live-20260912/verification.md` and its JSON
companions. It validates queueing, storage and cache reuse, not five cold results
per minute. Both gateway and workers run the `20260912-v3` release.
