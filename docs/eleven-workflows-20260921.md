# Eleven workflows — implementation and test report

**Status: committed, deployed and verified in production on 2026-09-21 (Europe/Amsterdam).**

The App now offers the existing Supplier payment check plus ten input-driven workflows. Each new workflow uses fixed, validated source operations, the existing saved-task execution engine, one quoted cap, one approval, a 20% workflow fee, and a sourced result. Inputs, source coverage and limits are shown before approval.

## Workflow coverage

| Workflow | Inputs | Source combination | Result | Local acceptance |
|---|---|---|---|---|
| Supplier payment check | Invoice identifiers / existing upload flow | Register, VAT, sanctions, bank-format and domain checks | Existing payment verdict and invoice-history comparison | PASS: two invoices, OK then HOLD |
| Find my next customers | Target location, business description, optional employee range | StableEnrich, Exa, Linkup | Ranked prospect shortlist | PASS: 3 operations |
| New customer onboarding check | Legal name, country, registration, domain, optional VAT | KVK / Companies House / Sirenic, OpenSanctions, RDAP/DNS, optional VIES | Onboarding findings and discrepancies | PASS: 5 operations in the NL example |
| Competitor and pricing comparison | Two or three pricing URLs, comparison criteria | Linkup, StableEnrich, Exa | Published pricing, features and terms compared | PASS: 5 operations for two companies |
| Find relevant tenders | Topic, geography, optional buyer | TenderNed, Linkup, optional KVK | Prioritized procurement notices | PASS: 3 operations with buyer search |
| Find potential subsidies | Project/topic and geography | RVO, EU Funding & Tenders, Linkup | Funding candidates and eligibility questions | PASS: 3 operations |
| Property pre-check | Full Dutch address | PDOK address/BAG, planning maps, Bodemloket, KOOP | Property facts, planning/soil signals and notices | PASS: 6 operations; ambiguous-address selection also tested |
| Who is behind this company? | UK company number/name, optional LEI | Companies House, OpenSanctions, optional GLEIF | Officers, significant control and available parent relationships | PASS: 4 operations without LEI |
| Company financial health snapshot | Country, registration and name | KVK / Companies House / Sirenic, Reuters | Available filings/financials, reporting periods and warnings | PASS: 3 operations for NL; UK variant also exercised |
| Used business vehicle pre-check | Dutch plate, dealer name/KVK number, advertisement URL | RDW, KVK, Linkup, RDAP | Advertisement/vehicle comparison and seller signals | PASS: 4 operations |
| Company news and events brief | Company name, source-supported ticker, optional US insider coverage | CityFALCON, Reuters, Bloomberg | Company news, sentiment and events | PASS: 6 operations with insider coverage |

Operation counts are the tested examples, not a promise that every input costs the same. No fixed prices are advertised: the current source prices are summed into an exact cap before approval.

### Explicit coverage boundaries

- Prospect results are a bounded shortlist. Automatic registry verification of every returned prospect is not included.
- VAT identity fields depend on the national register. Sanctions name matches require review.
- Pricing comparison uses the public pages returned by sources; hidden plans stay unknown.
- Tender and funding records can be historical. RVO covers the first 50 public descriptions; no match does not prove no funding exists.
- Property screening does not establish ownership, valuation, building rights, structural condition or clean soil.
- Ownership coverage uses UK officers/controllers and optional GLEIF corporate parents. Only the named company is screened for sanctions.
- Financial coverage differs by country: supported NL XBRL accounts, UK filing/insolvency records, or available French financials/alerts. This is not a credit rating.
- RDW does not identify the owner or guarantee mileage, mechanical condition or accident history.
- Company news has bounded source windows. Bloomberg supplies broad latest-news context; Reuters matches can be loose; insider coverage is US only.

## Repositories and database changes

### Gateway

- `src/workflow_catalog.json` and `src/workflow_catalog.rs`: ten recipes, public descriptions, fields, validation, source bindings, report instructions and availability checks.
- `src/server.rs`: authenticated App endpoint `POST /v2/workflows/:slug/start`. New clients request `GET /v2/workflows?catalog=2`; old clients retain the invoice-only response they understand.
- `src/engine.rs`, `src/engine/workflows.rs`, `src/state.rs`, `src/resolver.rs`, `src/execution.rs`: named workflow context, plan metadata, accurate fee display, idempotent starts and workspace-scoped run history.
- `src/parser.rs` and `src/engine/execute.rs`: room for all requested checks while retaining scalar-source evidence validation; explicit coverage limits survive model output. Failed or partial reports remain labelled accordingly.
- `src/engine/results.rs` and `crates/catalog-core/src/facts/adapter.rs`: readable address candidates and deterministic RD point-to-map-extent conversion for the selected property.
- `src/engine/followups.rs`: ordinary follow-up purchases do not inherit the previous named workflow context.
- New catalogue and durable acceptance tests, source-response fixtures, and `scripts/test-workflows-flow.mjs`.

### App

- Workflow listing, ten forms, quote handoff to Ask, readable desktop/mobile cards, and workflow history labels.
- `supabase/functions/_shared/pricing.ts`: exact allow-list of all eleven workflows for the existing 20% fee. Both `use-selection` and `agent-gateway` need this shared-code update when released.
- Form/browser acceptance tests and recorded catalogue fixtures.
- Fixed one pre-existing midnight-sensitive test by freezing only its test clock. The failure was reproduced on clean commit `5c70046`; no production date logic changed.

### Supabase

One migration, mirrored in App and Gateway: `20260920222320_eleven_workflow_catalog.sql`.

- Allows the eleven workflow slugs and explicit `partial` history status.
- Adds one address-resolution capability on the existing PDOK endpoint, preserving BAG identity and the selected address's map coordinates.
- Uses existing providers, endpoints and fact schemas. It does not onboard another source.
- The full migration was applied twice successfully in the isolated test database. The migration was applied to production and its constraints and address contract were verified.

**MCP and website:** no changes. Named form starts are an App feature; the existing MCP execution/recovery contract is unchanged.

Pre-existing latency edits in App/Gateway were preserved and excluded from the release using clean worktrees. The user authorized commits, pushes and production deployment on 2026-09-21.

## Test evidence

| Check | Result |
|---|---|
| Gateway workspace suite | 244 passed, 0 failed; 27 environment-dependent tests ignored by the normal suite |
| Supplier + ten-workflow durable acceptance | PASS; ignored integration tests explicitly run against an isolated database |
| Catalogue/variant validation | PASS; NL/GB/FR routes, optional third competitor, LEI and insider choices, unavailable sources and invalid inputs |
| Billing and recovery | PASS; zero purchase before approval, one authorization per run, exact fee metadata, duplicate-start/execution protection, saved-result recovery |
| Failure handling | PASS; cancellation, refunded source failure, wrong company identity, ambiguous address selection, failed analysis and partial history |
| Access scope | PASS; another owner or another workspace cannot read the saved run |
| KVK queue/concurrency | PASS on disposable real PostgreSQL; ten buyers share a job, no reservation while waiting, replay protection |
| App suite | 2,211 tests passed in 184 files |
| Browser | PASS; eleven tiles, ten forms, quotes and saved results; desktop/mobile layouts inspected |
| Build / types / lint | PASS; production build, App and Edge Function TypeScript, ESLint |
| Migration / whitespace | PASS; migration replayed twice, matching copies, `git diff --check` |

The durable tests use the real resolver, engine, task storage, authorization and recovery paths with recorded source payloads and fixture model/payment responses. Browser tests use those recorded Engine responses. **These are not live upstream, real-money or live answer-quality tests.** KVK concurrency tests use a real local database with a fixture upstream.

Local evidence is under `gateway/output/eleven-workflows/` and `app/artifacts/eleven-workflows/`. These generated folders are ignored by Git. Browser scripts are checked into the respective working trees.

## Production release

The production release ships App and Gateway workflow commits, the mirrored database migration, and the `use-selection` / `agent-gateway` functions. The catalogue opt-in protects older browser tabs during rollout. Deployment evidence is recorded in `docs/eleven-workflows-production-20260921.md`. All eleven tiles and all ten new live quotes were verified; no paid purchase was approved.

### Isolated release checks

- Gateway workspace: 241 passed, zero failed, 27 environment-dependent tests ignored by the normal suite; supplier and all ten new workflow integrations pass when explicitly run on an isolated database.
- App: 2,203 passed; four failures in standalone manifest/icon and workspace-selection tests reproduce unchanged on baseline commit `5c70046`.
- Production build and ESLint pass.
- Earlier full-suite counts above include the preserved local latency work. The isolated release excludes that work.

Production verification covers published assets, catalogue, forms and quote creation. Paid source execution is not included in deployment verification; the user can approve those purchases in the production App.
