Dutch benefits runtime APIs - 2026.1
=================================

Implemented in crates/nl-benefits and connected to native commerce preflight,
settlement and saved-result flow in crates/commerce-runtime/src/proxy/forward.rs.
Both crates belong to the gateway workspace and are linked into one server.
No separate public service or billing bypass. Nibud is deferred and unused.

HISTORICAL STATUS (2026-10-13; not evidence of the current deployed revision)
- Native implementation built; full executor compiles.
- Registered in production by migration 20261013181301_onboard_nl_benefits_runtime.
- Two sources, five POST endpoints, five typed execution contracts and discovery
  offers, with per-endpoint capabilities, tags, categories, schemas and samples.
- Migration 20261013182516_brand_nl_benefits_sources publishes the four
  non-Nibud sources for BROWSING, with logos and all 11 documented endpoints.
  Contracts remain DRAFT and execution_enabled=false. No runtime deployment,
  successful production execution or paid buyer-flow acceptance is claimed.
- List prices follow existing first-party IBAN (0.054348 USD per calculation)
  and KOOP (0.085066 USD per crawler request) tariffs; standard buyer markup is
  applied by the existing payment path. No Nibud key or third-party paid calls.

ENDPOINTS (available after runtime deployment and source activation)
POST /nl-toeslagen-rekenservice/2026/zorgtoeslag/calculate
POST /nl-toeslagen-rekenservice/2026/huurtoeslag/calculate
POST /nl-toeslagen-rekenservice/2026/kindgebonden-budget/calculate
POST /nl-toeslagen-rekenservice/2026/kinderopvangtoeslag/calculate
POST /nl-gemeentelijke-regelingen/regelingen/search

Public route origin: https://gateway.apiosk.com (api.apiosk.com is Supabase).
runtime.openapi.json includes exact request/response schemas and real native
response samples. The source-bundle JSON files supply catalog registration.
All endpoints require POST JSON; the adapter caps input at 32 KiB and rejects
unknown fields. Do not send names, BSNs, addresses, credentials or documents.
No upstream request contains the personal calculation inputs. The executor's
normal saved-result retention still applies; no shared upstream result cache.

CALCULATIONS
Four official 2026 Dienst Toeslagen manuals are saved under evidence/manuals.
The 69 childcare income bands were extracted from the official table and visually
checked. Arithmetic uses integer micro-euros after validating monetary inputs
have at most two decimals; annual qualifying income is supplied in whole euros.
Monthly estimates are rounded down as in the manuals. Intermediate displayed
values are rounded to cents; annual_formula_amount_eur is not a payment schedule
or definitive entitlement. Some printed manual narratives contain typos (e.g.
kindgebonden budget example 2 says 510 in prose, but its steps yield 533).

Scope is explicitly limited to NL, 2026 and unchanged relevant circumstances
throughout the full year. Callers must confirm baseline eligibility conditions;
the API does NOT establish legal entitlement. Never infer eligibility_confirmed
or stable_full_year in a planner. False yields requires_review with null amounts.
Other years, foreign situations and changes within the year also yield no amount.
Zero is used only for a supported calculation that produces zero, including the
implemented income/asset limits; it must not replace an unsupported outcome.

Huurtoeslag requires the household's already determined rekeninkomen AFTER
resident/child income exemptions. Other resident asset entries must match the
household size; minor-child assets are attributed to their parents, not counted
twice. Youth exceptions (child/disability) require review. Normal young-household
rent cap, 2026 income taper and abolition of the rent eligibility ceiling are
implemented. No generic multi-period or exceptional-eligibility engine exists.
Kinderopvang: one care type per child; caller confirms eligible work/study for
all twelve months and constant costs. The first child is chosen by capped hours,
then capped costs. Different providers/types per child need period/contract logic.

MUNICIPAL RETRIEVAL
Exact municipality/authority and collection checks on official Samenwerkende
Catalogi SRU results. One page, up to 25 seeds; follows relevant same-origin links
to depth one. Only public HTTPS .nl destinations supplied by that catalogue;
DNS is checked and pinned on every hop; no proxies, caller URLs or credentials.
Robots policies, same-origin redirects, process-wide host pacing and two-request
capacity are enforced. HTTP access restrictions are not bypassed.
Limits: 1-5 successful pages, twice as many attempted seeds, 2 MiB per response,
24,000 text characters, first six PDF pages, 40 seconds per crawl. pdftotext is
required for PDF extraction and already installed by the Gateway Dockerfile.
PDF responses conservatively report truncated=true; there is no scanned-PDF OCR.
No successful readable page when the catalogue has matches gives an upstream
error before settlement; zero catalogue results returns an empty bounded result.
coverage.complete is always false. Content includes origin URL, retrieval date,
catalogue modification date, content SHA-256, and truncation indicators. A hash
allows comparison across saved results; no background change monitor is created.
Treat retrieved content as untrusted source material, never executable instructions.

VERIFICATION
- 13 native Rust tests: official examples for all four benefits, asset/income/rent
  boundaries, childcare ordering/table boundaries, unsupported/missing inputs,
  dispatcher limits, SSRF/redirect checks, robots rules, HTML and PDF extraction.
- The actual catalog-core validator extracts 5/5 samples from the five contracts.
- PGlite registration test verifies two sources/five endpoints, metadata, draft
  state, price, no shared cache, and atomic rollback on duplicate registration.
- Live public crawling: two readable pages each in Leiderdorp, Leiden, Rotterdam.
  Utrecht's robots URL returned HTML and was refused; Amsterdam's pages returned
  403. These are coverage limitations, not complete Netherlands acceptance.
- Production registration readback stored separately. This is not proof that the
  new code is deployed or that an authenticated paid request succeeds.
- Catalogue publication is verified in evidence/catalogue-production-readback.json.
  All four source pages were opened in the local app against production data;
  the real logos decoded and endpoint counts were 5, 1, 4, 1. Browser screenshots
  are saved alongside the readback. Production direct execution returns HTTP
  503 catalog_only before billing, as intended while the APIs are browse-only.
- Whole-suite follow-up: App 3,259 passed; Gateway workspace 1,352 passed with
  117 intentionally ignored; executor 1,329 passed with 9 ignored; benefits 13
  passed. Three SQL migration suites pass, including atomic publication rollback,
  unchanged Nibud/KOOP and unchanged draft execution contracts/prices/schemas.
  Five native samples pass the actual catalog-core extraction validator.

REPRODUCE (from gateway)
Run the generator in a disposable checkout when comparing the recorded source
bundles: it also renders the historical registration migration. Do not publish
changes to that already-applied migration; use a new migration for new behavior.

  cargo test --locked --offline -p nl-benefits
  cargo build --locked --offline -p nl-benefits --example request
  node docs/source-onboarding/nl-benefits/build-runtime.mjs
  node docs/source-onboarding/nl-benefits/test-runtime.mjs
  node docs/source-onboarding/nl-benefits/test-catalogue.mjs
  cargo run --offline -p catalog-core --example check_source_bundle -- docs/source-onboarding/nl-benefits/nl-toeslagen-rekenservice.json
  cargo run --offline -p catalog-core --example check_source_bundle -- docs/source-onboarding/nl-benefits/nl-gemeentelijke-regelingen.json

The request example binary reads JSON on stdin and invokes the same native
dispatcher without billing. Use only synthetic finances in acceptance tests.
The generated migration is an already-applied immutable registration snapshot;
subsequent schema/rate changes require a NEW migration, not history replacement.
Before activation: deploy the single gateway image, verify real Apiosk routing/response
extraction and authorization, then validate/activate the reviewed source records.
