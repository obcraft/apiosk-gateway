# Eleven workflows — production release

Released and verified on 2026-09-21, Europe/Amsterdam (deployment timestamps are 2026-09-20 UTC).

## Try it

Open **https://app.apiosk.com/workflows**. Choose a workflow, fill its inputs, then select **Review checks and price**. Purchases start only after approval. Refresh an existing browser tab if it still shows the older list.

## Deployment

| Component | Published version / evidence |
|---|---|
| Gateway runtime | `a61c793` on `origin/main`; Fly image `deployment-01M30EQYC89865H0B27WKEGSWY` |
| Gateway image digest | `sha256:ecafb097fc715b76943189192df1f298d839e7b177dcc70ec7195ebb6bbaebae` |
| App | Feature commit `7092652`; migration-history alignment `da4b608`, both on `origin/main` |
| Netlify | Deploy `6ab05d7bd999d6000930ed47`, state `ready`, published at https://app.apiosk.com |
| Supabase | `20260920222320_eleven_workflow_catalog.sql` applied to project `jgjoiyqdyypouskftzeq`; mirrored filenames match production migration history |
| Edge function `use-selection` | Active version 68; `verify_jwt=true` preserved |
| Edge function `agent-gateway` | Active version 59; custom authentication and `verify_jwt=false` preserved |
| MCP / website | No changes or deployments |

The follow-up Gateway commit contains only this evidence, the implementation report, and the migration filename/test-reference alignment. Its runtime sources are identical to deployed commit `a61c793`.

## Production verification

- Fly deployment finished successfully, including smoke, machine, health and DNS checks. Machine in `ams` is started and its health check passes.
- Release script passes: Gateway ready, unauthenticated task reads return 401, MCP has its expected five tools and App-only approval, and arbitrary-origin approval is not allowed.
- A signed-in production browser shows all eleven workflows, including the existing Supplier payment check.
- Each of the ten new forms created a persisted task and displayed one maximum price plus its approval button. Production database readback confirms exactly one `quoted` run for each test.
- Reloading the saved Company financial health task recovered the same quote.
- No approval buttons were clicked. These checks did not exercise paid upstream calls, live report quality or settlement.

### Example quote observations

These are the displayed caps for the specific smoke-test inputs, not fixed advertised prices. The vehicle example uses deliberately fictitious dealer and advertisement inputs and is only a quote/form check.

| Workflow | Displayed maximum | Live form → saved quote |
|---|---:|---|
| Find my next customers | EUR 0.184368 | PASS |
| New customer onboarding check | EUR 0.459132 | PASS |
| Competitor and pricing comparison | EUR 0.162288 | PASS |
| Find relevant tenders | EUR 0.084083 | PASS |
| Find potential subsidies | EUR 0.157128 | PASS |
| Property pre-check | EUR 0.395688 | PASS |
| Who is behind this company? | EUR 0.260871 | PASS |
| Company financial health snapshot | EUR 0.229566 | PASS |
| Used business vehicle pre-check | EUR 0.230171 | PASS |
| Company news and events brief | EUR 0.441068 | PASS |

## Release test gates

- Gateway workspace: **241 passed, 0 failed**, with 27 tests requiring special environments ignored by the standard run.
- Supplier and ten-workflow durable acceptance: **all pass** on an isolated database, including fixture execution, one approval, recovery, history and failure cases.
- App: **2,203 passed**, with **four pre-existing failures** in manifest/icon and workspace-selection tests reproduced unchanged on baseline `5c70046`. Workflow tests pass; rerun after incorporating the upstream typography merge: 17 passed.
- App production build, TypeScript and ESLint pass.
- Matching migration copies and whitespace checks pass; migration constraints and active PDOK contract verified in production.

Recorded-source and fixture-payment execution tests are local tests. Production acceptance in this release ends at a saved, approval-ready quote; the user can test real purchases in the live App.

## Checkout preservation

The release was built from isolated worktrees. Pre-existing local latency changes were excluded from commits and deployment and preserved in the user's original App and Gateway checkouts. The App release also preserves the upstream auth typography merge `e0ac313`.
