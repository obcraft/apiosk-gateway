# European company and tender dossiers — implementation 2026-09-23

Status: implemented locally; migration, runtime, edge pricing, App, MCP and docs are not deployed. Provider replies and settlement in acceptance tests are fixtures. No new live paid evidence is claimed.

## Product boundary

Two reproducible offerings use the existing engine, durable task, quote, authorization, billing and sourced analysis:

- `european-company-dossier`: registered identity → screening of the registry-returned name + available financial/filing records. Three source operations in each country variant.
- `tender-company-dossier`: the company dossier plus two bounded procurement sources, five operations. One counterparty per task, with candidate links kept separate from verified awards.

| Company country | Identity | Filings/account scope | Tender dossier |
| --- | --- | --- | --- |
| NL | KVK | Supported KVK XBRL account subset | TenderNed name mentions + TED KVK-digit matches |
| GB | Companies House | Filing-history metadata | Not included |
| FR | Sirenic French registry | Available annual financial figures | Sirenic French public-contract records + European notice links |
| BE, FI, NO, LV, EE, SE, DK, SK | Sirenic national registry | Country-specific accounts/deposits/references; source periods and gaps retained | Not included |

All listed countries use OpenSanctions company-name matching with country hint. No-match is not clearance. Officers and owners are not individually screened. Country support is a bounded recipe over admitted contracts; neither catalog admission nor a fixture pass proves current records for every company.

The gateway returns country-specific planning availability through both its App definitions and `GET /v2/ask/workflows`. A failed required source prevents preparing that country's dossier. Another country can remain available. The App displays identity, screening, filing and procurement scope for the selected country and blocks that unavailable variant without disabling the country selector.

Country notes also enter the saved answer instructions. Supplied name/label is not a `company.name` fact: the registry must return the actual screening name first. The new SQL migration adds that name projection to the existing GB/FR profile contracts, preserving the original response, identifiers, prices and admission status. It also admits the two workflow slugs to saved history.

## Partner contract

The buyer API accepts `POST /v2/ask` with exactly one of `question` or `{ "workflow": { "slug": "tender-company-dossier", "input": { "name": "Mollie B.V.", "country": "NL", "registration": "30204462" } } }`. The ordinary `execution_mode`, cap, permissions and idempotency rules apply. Planning cannot approve. `/v2/asks/{id}/run` runs the saved quote; `/continue` handles returned choices without starting another task.

MCP uses the same structured `workflow` object in `apiosk_discover`, routed to the shared gateway workflow starter; no additional model tool or execution engine. The App uses the existing Workflows/Ask routes. Connected-agent runs are labelled correctly in saved history. The existing 20% workflow fee is included in every step and total quote; the payment-edge allowlist now includes both slugs.

## Validation

- Gateway catalog tests: 11 company-country variants and 2 tender-country variants; exact 3/5-operation plans, registry before screening, no opportunistic web/news sources, unsupported-country rejection and country-specific source outage handling.
- Durable flow: every existing recipe plus the two dossiers quotes, approves once, executes, recovers and retains history using stored source fixtures.
- Buyer HTTP flow: free country discovery, fixed tender plan, no planning purchase, changed-body idempotency refusal, country refusal, one approval, five provider calls, actual screening input comes from the registry and repeat execution makes no further purchase.
- App: country scope changes with selection; an unavailable country blocks purchase while another remains selectable; both new recipes preserve quote-before-approval and workflow pricing.
- MCP: fixed recipe forwarding, no implicit execution, validation of unsupported identifiers/countries/extra state, existing protocol regression checks.
- Production App build, documentation link validation and OpenAPI validation.

Local logs: `/tmp/european-workflow-tests.log`, `/tmp/european-workflow-flow.log`, `/tmp/european-api-regression.log`, `/tmp/european-app-tests.log`, `/tmp/european-app-build.log`, `/tmp/european-mcp-tests.log`, `/tmp/european-docs-validate.log`, `/tmp/european-openapi-check.log`.

## Release order and next acceptance gates

1. Apply the reviewed migration `20260923162344_registry_names_for_european_dossiers.sql` once (identical copy in Gateway and App). It is idempotent. Inspect migration history: its generated timestamp can precede other already-applied same-day catalog migrations; do not blindly push unrelated pending files.
2. Deploy the existing payment functions that bundle `_shared/pricing.ts` (`use-selection` and `agent-gateway`) before enabling the new gateway recipes, so execution uses the same 20% fee as the quote.
3. Deploy Gateway including buyer continuation and these recipes, then App/MCP/docs. Read the new country matrix and prepare NL, GB, FR and the remaining variants without purchases. Check migration outputs, auth scopes, one cap and unavailable-country behavior against the live catalog.
4. Run explicitly bounded live dossier checks with a reviewed treasury/customer spending ceiling. Store source results, actual charges, single-approval and recovery evidence per country. Promote only proven claims in partner material. No live payment has been attempted here.
5. Next tender milestone: general TED discovery by country/CPV/date, a typed notice/award model (notice ID, lot, buyer/winner identifier scheme, dates, amount/currency, provenance and pagination), and a saved selection handoff to a counterparty dossier. Until those pass, do not present numeric TED matches as confirmed legal-entity awards, aggregate public spending or exhaustive European tender coverage. The current product delivers the counterparty dossier, not bulk screening of a tender result page.
