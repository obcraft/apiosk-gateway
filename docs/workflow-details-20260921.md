# Workflow rows, steps and source selection

Follow-up to the eleven-workflow release, 21 September 2026.

## Product changes

- One full-width row per workflow on desktop and mobile.
- Eleven distinct custom SVG icons, used on the list and input forms.
- Source names on every row; an expandable Steps & checks section shows the actual recipe, conditions and service behind each step.
- Input forms show the current steps and sources, updated when country or optional inputs change. This preview is derived from the same recipe conditions used by the Gateway. The approval quote remains authoritative for source calls and price.
- Checklists cover each workflow's useful outcomes. Their lengths are not targets for paid source calls; most have four to seven grouped checks.
- Supplier payment details distinguish IBAN validity from account ownership and show the configured bank-validation source.

## Avoiding unnecessary calls

| Workflow | Planned source calls |
|---|---|
| Prospect shortlist | 2; removed the duplicate Linkup market-context call |
| Customer onboarding | 4, or 5 with VAT; one country register |
| Pricing comparison | 2 or 3 supplied pricing pages; removed unrelated company enrichment and web search |
| Tender finder | 2, or 3 with buyer lookup |
| Subsidy finder | 2; add RVO for Netherlands projects only. NL, Nederland and common English names are recognized without case sensitivity |
| Property pre-check | 6 |
| Company ownership | 4, or 7 with LEI/parent relationships |
| Company health | 3 for NL, 4 for GB or FR |
| Vehicle pre-check | 4 |
| Company news | 4, or 5 with US insider records; removed the broad Bloomberg feed |
| Supplier payment | Existing invoice-dependent routing; searches and optional identifier checks depend on invoice contents |

These counts describe the current recipe variants, not a requirement that future workflows contain this many checks. There is no ten-check minimum or quota, and there is no requirement to buy from multiple sources when one source satisfies the job.

## Verification

- Gateway workspace: 242 passed, zero failed; 27 environment-dependent tests ignored by the standard run.
- Existing supplier plus ten named workflow integrations: all pass, explicitly run on an isolated database, including fixture execution, one approval, saved results, recovery and failure handling.
- Backend regression checks verify that country, VAT, third pricing page, LEI, US insider and Dutch funding conditions change the planned calls correctly.
- App: 2,206 passed; the four known baseline manifest/icon and workspace-selection failures remain. Twenty workflow tests pass, including source-list updates without starting a purchase.
- Production build, TypeScript, lint and whitespace checks pass.

App and Gateway changes only. No database migration, Edge Function, MCP or website change is required. Paid upstream verification is outside this update; production UI and free quote checks are recorded separately under ignored release artifacts.
