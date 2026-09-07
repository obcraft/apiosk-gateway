# Production cutover: Gateway v1 to v2

Cut over on 2026-09-07. `gateway.apiosk.com` now terminates on the
`apiosk-gateway-v2` Fly app. The former `apioskgateway` app has zero machines in
both its `app` and `research_worker` process groups.

## Runtime shape

One v2 release now supervises four responsibilities:

1. the native v2 protocol on port 8082;
2. the existing provider/payment executor on loopback port 8080;
3. the durable research worker on loopback port 8081; and
4. the settlement reconciliation worker on a five-minute loop.

The executor is pinned as the `legacy-executor` Git submodule. It is compiled
into the v2 image and is not reachable as a separate Fly service. Native v2
routes remain owned by the v2 router. Other existing gateway paths are passed
to the loopback executor with the original method, URI, Host and payment or
idempotency headers. The v2 health check fails when that executor is
unavailable.

The v2 database login remains limited to the v2 schema. The embedded executor
and workers use `EXECUTOR_DATABASE_URL`, which was copied from the retired Fly
service. Treasury, x402, facilitator, service-key and provider credentials are
Fly secrets. They are not stored in the repository or exposed to the App.

## Public routing

- Netlify DNS sends `gateway.apiosk.com` to the v2 Fly IPv4 and IPv6 addresses.
- The Fly certificate for `gateway.apiosk.com` belongs to
  `apiosk-gateway-v2`; the old app no longer owns that hostname.
- Supabase Edge Functions use
  `APIOSK_GATEWAY_URL=https://apiosk-gateway-v2.fly.dev`.
- MCP uses `APIOSK_GATEWAY_V2_URL=https://apiosk-gateway-v2.fly.dev`.
- The App build uses the v2 gateway and the hosted v2 MCP endpoint.

## Cutover evidence

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

Restore the saved old Fly scale counts, re-add its certificate, and point the
Netlify A and AAAA records back to the recorded old Fly addresses. Do not alter
payment, settlement, task or result history during rollback, and do not retry an
uncertain payment under a new idempotency key.
