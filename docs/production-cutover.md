# Production cutover: one native gateway

The current source builds one server process in the `apiosk-gateway-v2` image.
Provider, payment and directory handlers are linked from the local
`crates/commerce-runtime` workspace library. There is one repository, one
Cargo.lock and no separate executor checkout or process. This describes the
release architecture; the dated cutover evidence below does not establish that
the latest source has already been deployed.

## Runtime shape

The gateway listens on port 8082. Native v2 routes keep precedence; existing
provider/payment routes are dispatched in process with their method, URI, Host,
payment and idempotency headers preserved. Directory lookups also use this
in-process router. Retired Ask routes remain blocked before the fallback.

`/health` retains its response contract, including `payment_executor: ready` when
the native commerce health handler succeeds and `version` identifying the image
commit. There is no loopback executor on port 8080, worker server on port 8081,
or supervisor launching either one. The separately deployed KVK source-queue
worker is documented in [the KVK runbook](kvk-annual-accounts-queue.md).

The v2 database login remains limited to the v2 schema. Commerce uses its own
pool with `EXECUTOR_DATABASE_URL`, preserving the existing database-role boundary
inside one process. Treasury, x402, facilitator, service-key and provider
credentials remain server secrets. The existing App ledger still handles buyer
reservations, charges and refunds.

## Release procedure

Use the [build and deployment pipeline](build-and-deploy.md). It validates the
complete workspace, checks the actual Linux image, deploys that immutable image
digest, and verifies each Machine plus the public health revision.

For the first migration from the previous image, the only expected listener
configuration removals are `EMBEDDED_EXECUTOR_ORIGIN` and
`EMBEDDED_EXECUTOR_PORT`. The deployment helper records the previous image and
these previous settings in the release evidence and rollback command. Keep
`EXECUTOR_DATABASE_URL`, payment flags, worker flags and secrets unchanged.
Configuration differences outside the expected cutover need review before
deployment. No paid request or database migration is part of this pipeline.

## Public routing recorded at the 2026-09-07 cutover

These are historical observations, not a fresh DNS or hosted-service check:

- Netlify DNS sends `gateway.apiosk.com` to the v2 Fly IPv4 and IPv6 addresses.
- The Fly certificate for `gateway.apiosk.com` belongs to
  `apiosk-gateway-v2`; the old app no longer owns that hostname.
- Supabase Edge Functions use
  `APIOSK_GATEWAY_URL=https://apiosk-gateway-v2.fly.dev`.
- MCP uses `APIOSK_GATEWAY_V2_URL=https://apiosk-gateway-v2.fly.dev`.
- The App build uses the v2 gateway and the hosted v2 MCP endpoint.

## Historical cutover evidence, 2026-09-07

The hostname cutover used the former multi-process image. At that time the old
`apioskgateway` app had zero machines in its `app` and `research_worker` groups.
The following evidence is retained as recorded; it does not validate the current
single-process migration or establish the current status of old jobs.

After the old app and both old worker machines were removed:

- `https://gateway.apiosk.com/health` returned protocol v2 with
  `payment_executor: ready`;
- native v2, `/v1/do`, provider-slug and browser CORS routes responded through
  the combined v2 release;
- the hosted MCP initialized with protocol `2025-03-26` and advertised exactly
  `apiosk_sources`, `apiosk_discover` and `apiosk_execute`;
- the production App returned its current Netlify release;
- Gateway unit, resolver, integration and Clippy checks passed;
- all 120 MCP tests passed; and
- all 1,853 App tests passed before the production build and lint checks.

The live deterministic executor dry-run used service authentication, catalog
selection and input binding without paying or calling a provider. A paid live
acceptance call was not made during this cutover. Historical reconciliation
still reports 35 old settlements that failed to confirm on-chain; they predate
this release and remain audit debt. One old research job is waiting for the
person to choose between two company matches, so it cannot progress until that
input is supplied; the v2 worker is available to resume it.

## Rollback

Use the rollback command saved by `scripts/deploy-image.py` with the previous
immutable image and matching runtime configuration. For the first native
migration, rolling back to the previous image must also restore its recorded
`EMBEDDED_EXECUTOR_ORIGIN` and `EMBEDDED_EXECUTOR_PORT`; that image still needs
its listener settings. The helper includes them when required. Do not restart
the retired Fly application or change DNS as part of this image rollback.

After rollback, check every Machine and the public `/health` revision and
`payment_executor` value. Do not alter payment, settlement, task or result
history, and do not retry an uncertain payment under a new idempotency key.
