# Sirenic

## Production coverage — 2026-09-13

Sirenic provides **63 verified fixed-price JSON routes through 65 planner operations**, up from six. The two extra operations are country-specific variants of shared routes. All 65 contracts accept actual paid responses; independent Base receipts verify the provider payments. The planner can use Sirenic through the normal Apiosk flow: question → plan and total ceiling → approval → sourced result. The treasury pays the upstream x402 charge; the user uses the existing Apiosk balance and does not need a separate wallet or Sirenic account.

Coverage includes French company search/profile, annual financials, KYB, establishments, legal alerts/court cases, procurement, intellectual property, capital and business health, intelligence reports, employment, permits, invoice checks, regulator alerts, VAT/IBAN checks, association records, and European profiles, accounts, officers, ownership, insolvency and events where the official registers support them. Country-specific coverage and missing-data notices remain in the source response. Filing indexes are not represented as downloaded financial documents. A negative or incomplete result is not evidence of zero activity or an absence of risk.

The current route inventory is `tests/fixtures/sirenic/expanded-catalog.json`; the nullable SIREN correction for association records is in `verified-contract-adjustments.json`. The production activation migration is generated only from successful paid extraction and verified receipts.

Two candidate routes remain disabled after unsuccessful production checks:

| Route | Observed limitation |
| --- | --- |
| `/v1/eu/entreprise/ES/{hoja}/actes` | The documented `VI-23141` example returned upstream HTTP 404 without settlement confirmation. |
| `/v1/associations/recherche` | The published search example returned upstream HTTP 503 without settlement confirmation. |

These observations do not establish that every possible input fails. They are insufficient to enable a planner purchase. Variable-price batches/comparisons/custom dossiers, binary downloads, monitoring subscriptions and filing-reference/date operations requiring additional document selection are also excluded. Country profile aliases are covered by the unified profile operation.

## Branding and Ask

The official 512 px Sirenic wordmark from the agent card is served at `https://app.apiosk.com/brands/sirenic.png`. Listing metadata supplies that image to the shared source directory and source results. It was visibly verified in Explore. The App also includes a curated fallback.

Three tested Ask examples are stored in `app/src/lib/domain/sirenic-examples.json`:

- Use Sirenic to find DANONE and summarize its available filed annual accounts, including the reporting year and scope.
- Use Sirenic to find DANONE and check its legal alerts and court cases, preserving dates and the company's role.
- Use Sirenic to retrieve the Norwegian annual accounts of Equinor ASA, with reporting years and source notes.

The real intent model builds the expected search → record plans (two, three and two calls). An additional source-neutral French legal-record question checks automatic Sirenic selection. Example buttons fill the draft without making a purchase. The live App financial example displayed Sirenic and a €0.012696 approval ceiling; that test request was cancelled and showed €0.00 charged. Operator payment tests exercise the production `/v1/do` treasury path; no customer balance was debited by this campaign.

## Payment contract and evidence

The integration pins x402 v2 exact payment, Base (`eip155:8453`), USDC (`0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913`), the Sirenic recipient (`0x76A672EEe56D29D475b0715cc03B8C99D70EC8A2`) and token domain `USD Coin` / `2`. Prefer the complete `PAYMENT-REQUIRED` header over the descriptive JSON body. Send `PAYMENT-SIGNATURE`. The accepted authorization lifetime is 45–200 seconds: invoice checks use 45 seconds; health, intelligence and capital reports use 200. Signing never extends a shorter quote. Customer totals remain provider price plus the existing 15% fee, with separate provider/fee receipt amounts.

The expansion campaign completed **59 new paid checks for $2.586 USDC**, within its $2.61 treasury ceiling. Three unused authorizations were closed as failed only after an on-chain `authorizationState=false` check beyond their maximum expiry. One was a Belgian dealing lookup for an unrelated issuer; a distinct subsequent check used UMICORE's documented enterprise number and succeeded. Five old-parser failures had no payment claim and were checked again after the timeout fix. The original attempts are preserved under `previous-attempts/`; paid attempts are never automatically retried. No unresolved payment remains.

Evidence under ignored `output/sirenic-20260913/` includes `payments/`, `payments/verified-receipts.json`, `paid-contract-evidence.json`, `reconciliation/`, `reviewed-rechecks.json`, `ask-plan-evidence.json`, `promotion-summary.json`, UI checks and deployment/test logs. The original six paid results and their $0.18 receipts remain under `output/sirenic-20260912/`. The first original search had a pre-fix reporting discrepancy (2645 rather than 2300 micro-USDC); its actual provider payment was 2000. Subsequent receipts use the corrected split.

## Implementation and validation

- `record.country-identifier` extracts an identifier only from the same record's matching country, preserving leading zeros. It prevents cross-country ID reuse.
- Optional country projections do not mark otherwise complete results partial. Missing required outputs still stop dependent purchases.
- Partial or unreliable Sirenic search results stop dependent purchases. Full source provenance, quality notes and nulls are retained.
- Association records retain a nullable SIREN in their full record without inventing a company-registration fact.
- The staging migration leaves new contracts draft. The final migration activates only paid, extracted, receipt-verified operations, retires the old EU contracts and disables the two failed candidate endpoints. Both migrations are mirrored into the App repository with the same production migration versions.

Validation includes 1,275 executor tests, the Gateway/catalog test suite, 15 focused App example tests, six Sirenic regression tests, the production App build, real-model plan checks, live treasury calls and independent Base receipt verification. Migrations were applied twice against a disposable copy of the actual database schema, including its triggers, with the real executor route-matching test. Live customer balance debit was not performed by the expansion campaign.

Manual checks (the output directory contains the pinned OpenAPI and physical schema captures):

```sh
python3 scripts/test-sirenic-migration.py --expansion --promote
cargo run --offline --example check-sirenic-expansion -- tests/fixtures/sirenic/expanded-catalog.json --reviewed --paid
python3 scripts/verify-sirenic-receipts.py --expansion
```

`check-sirenic-expansion-live.mjs` defaults to dry-run. Paid mode persists each attempt before dispatch and stops on an unresolved payment. `inspect-sirenic-unconfirmed.py` is read-only, never signs, and never resends an authorization. Its expiry evidence must be paired with a guarded database reconciliation before continuing. The production scripts are operator verification aids, not background buyer workflows.
