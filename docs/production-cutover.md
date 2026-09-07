# Production cutover: gateway v1 to v2

Verified on 2026-09-07. Gateway v2 is running locally against the shared Supabase database. Fly currently hosts `apioskgateway` and `apiosk-mcp`; v2 has no Fly app, container image, or public URL yet.

## Why replacing the existing Fly image is not enough

V2 owns planning, task state, quotes, result parsing, App chat, history and related-service previews. Its executor still calls Supabase `use-selection` or `agent-gateway/v1/run` with `deterministic: true`. Those functions use the existing v1 executor for provider calls, treasury signing and settlement. V2 does not replace the existing `/v1` routes or research worker.

Deploying the v2 binary over `apioskgateway` now would remove services that v2 and existing clients still need. Do not change that app's source/image as the first migration step.

## Required rollout

1. Deploy v2 alongside v1 behind HTTPS. Add a reproducible Linux container and Fly configuration, expose the configured port on `0.0.0.0`, and add a health check. Configure the existing dedicated database login with verified TLS and a container-readable Supabase CA path. Set a shared signing key, OpenRouter key/model and production App origin through the platform's configuration. Do not ship local `.env` files or create another balance ledger.
2. Keep `apioskgateway` and its worker running. Verify the deployed deterministic guard and both Edge Function paths against the exact release. Existing grants/migrations are already installed in the shared database; do not apply a second copy or blindly push unrelated pending migrations.
3. Initially keep purchases disabled in the new public v2 instance. Verify authentication, live catalog loading, real question parsing, plan/price, history and state recovery. Check CORS and that approval links point to the production App, not localhost.
4. Set MCP `APIOSK_GATEWAY_V2_URL` to the verified public v2 origin. Build the App with `VITE_GATEWAY_V2_URL` and `VITE_GATEWAY_V2_MCP_URL`. Deploy MCP with `fly deploy`; preserve its current OAuth credentials and public MCP origin. Deploying MCP code without its v2 URL retains the v1 runtime.
5. Enable a controlled, explicitly approved paid test. Verify quote ceiling, actual ledger charge, provider result, x402 settlement/reconciliation, related-service reapproval, duplicate-request protection, restart recovery and cancellation. Then test the installed ChatGPT and Claude connectors separately. Fixture tests and a healthy server do not prove live payment settlement or installed-client behavior.
6. Retire v1 only after its execution/treasury responsibilities, legacy routes and worker obligations have been migrated or retained as a separate internal service. Drain in-flight jobs and resolve pending settlements before switching off any worker. Public gateway routing can move to v2 while an internal v1 executor remains, but that is not complete v1 removal.

## Rollback

Preserve the previous MCP image, App deployment and v1 runtime. Unset the MCP v2 URL and rebuild the App without its v2 URL to restore the previous entry points. Keep v2 task, result and payment records for recovery and audit; never roll back financial history or retry an uncertain payment with a new identity.

## Current validation

Local tests cover account isolation, missing/ambiguous inputs, exact price approval, deterministic execution, request replay, saved-result reads, recovery and related-service previews with fresh approval. The complete follow-up purchase journey uses fixture providers. No live paid acceptance or production v2 cutover is claimed by this release.
