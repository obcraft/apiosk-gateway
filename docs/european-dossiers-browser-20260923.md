# European dossiers: browser acceptance, 2026-09-23

## Scope

Tested through Codex Computer/Browser tools in the in-app browser, using the real local App, Gateway routes, resolver, executor and temporary SQL storage. Authentication, model responses, provider payloads and payment settlement were explicit fixtures. No production deployment, real provider purchase or live country coverage is claimed. The temporary test servers were stopped after verification.

The reusable harness is `scripts/test-workflows-browser-server.mjs`. It starts a temporary PostgreSQL-compatible database on loopback port 54422 and the test-only Gateway/fixture servers on ports 5283/5284. The App used port 5282 with its Gateway and Supabase configuration pointed at those fixtures. The browser branch lives only in the integration-test binary, not the production server.

## Observed acceptance

| Scenario | Browser evidence |
| --- | --- |
| Open both dossier cards | Dedicated input form opens from Workflows, including country coverage and the source plan. |
| Invalid NL registration number | Friendly field validation; no quote approval or provider execution. |
| NL tender dossier | Quote for 5 calls / 4 sources, maximum 0.419661 USD; one approval; saved result; payment completed for 0.419661 USD. |
| NL company dossier | Quote for 3 calls / 2 sources, maximum 0.306239 USD; one approval; saved result; payment completed for 0.306239 USD. |
| Reload completed tasks | Results restored, no new approval requested and no repeated provider calls. |
| Cancel GB company quote | TESCO PLC / 00445790 quote cancelled before approval; no purchase and zero spend. |
| Company-country selection | NL, GB, FR, BE, FI, NO, LV, EE, SE, DK and SK each display their own identity/filing scope and three planned steps. This is UI/planning evidence, not paid execution in all countries. |
| Tender-country selection | NL and FR show their distinct procurement sources and five planned steps. |
| History | Completed NL tender dossier appears under Usage > Workflows and opens its saved task. |
| Narrow viewport | Both dossier forms start at scroll offset 0; no document horizontal overflow (measured content width and viewport both 278 CSS pixels). The requested browser override was 390 x 844, but the embedded surface scaled it; no exact device-size claim is made. |
| Normal viewport | Form, coverage and sources inspected visually after resetting the override. No captured browser console errors at the final check. |

Final fixture counters remained **2 approvals, 8 provider calls and 8 settled charges**, including after refresh and country/form navigation. The GB cancellation did not add an approval or purchase. Fixture balances and AI prose are synthetic; these runs do not validate production balance availability or answer quality from the live model.

Task references in the temporary database:

- NL tender: `0c9fb9bb-5466-49a3-b935-12dfcc0ab288`
- NL company: `a7a2c491-0a4c-496f-a524-1e72929e0d49`
- Cancelled GB quote: `41169d06-d6cb-47be-863f-4470b8a96f32`

## Fixes found by browser testing

1. Dossier cards previously redirected into conversational Ask and bypassed the country-aware form. Both dossier routes now render their form; existing conversational workflows retain their path. Two route-rendering regression cases were added.
2. Invalid identifiers exposed an internal fact name. Validation now names the input field and asks the user to check its format/country.
3. Completed saved tasks still displayed “Review your request.” They now display “Your results” and a saved-result explanation.
4. Query-only navigation retained the workflow list's scroll position. The router now resets the App's actual scroll container after rendering. Verified on both dossier links.

The test payment adapter also now writes settled ledger entries, so browser checks exercise the payment-completed state instead of mistaking a returned provider payload for settlement.

## Automated verification after fixes

- App workflow/Ask regression: **43 tests passed** across 3 files.
- Gateway catalog integration: **6 tests passed**, including 11 company-country and 2 tender-country plans.
- Gateway catalog scope unit test: **1 passed**.
- Durable supplier and workflow suites: **2 passed**; all 12 named recipes, one-approval execution, recovery/history, and the partner tender HTTP flow passed. The partner case checks registry-bound screening and replay safety.
- App production build: passed.
- App and Gateway `git diff --check`: passed.

Logs: `/tmp/european-browser-regression.log`, `/tmp/european-browser-catalog-integration.log`, `/tmp/european-browser-catalog.log`, `/tmp/european-browser-flow.log`, `/tmp/european-browser-build.log`.

The release and live provider acceptance gates remain in [the implementation report](european-dossiers-20260923.md). The buyer's ambiguous-company continuation has separate HTTP acceptance coverage; it was not exercised through this dossier browser fixture.
