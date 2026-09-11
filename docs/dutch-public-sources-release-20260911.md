# KOOP, RDW and RVO implementation — 11 September 2026

Three direct public-source adapters and four typed Gateway v2 capabilities have been implemented. KVK, CBS and PDOK were outside this change.

| Source | Capability | Boundaries |
|---|---|---|
| KOOP | `government.publication.search` | Literal title search; exact publishing authority; inclusive publication-date filters; pages of 1–25 records with totals and continuation. Modification-date descending order. Applications are not permit decisions. |
| KOOP | `government.publication.detail` | Exact publication identifier, normalized metadata and canonical document link. Does not claim consolidated current law. |
| RDW | `vehicle.technical-profile` | Exact normalized Dutch plate in dataset `m9d7-ebf2`; technical register fields and available APK dates. No owner identity, fleet association or separate fuel/emissions dataset. |
| RVO | `subsidy.scheme.first-page-search` | Keyword discovery in the first 50 descriptions only, with optional exact sector/audience filtering. Historical schemes included. Total population, application status and eligibility remain unknown. |

RVO is deliberately a smaller capability than the original proposed full subsidy search. Later pages returned HTTP 500; an offset parameter was ignored. The bare endpoint also intermittently returned HTTP 500 during production validation. The adapter explicitly requests `page=0`, whose 50-record response was verified. Complete search requires confirmed pagination documentation and successful multi-page tests; the YAML documentation request requires contacting RVO and has not been sent.

## Runtime and catalog

- The shared legacy executor adapter validates inputs before network access, pins supported routes, normalizes source data before cache insertion and settlement, and rejects invalid source schemas. Query-specific cache keys preserve locally applied RVO filters.
- Catalog onboarding stages listings as inactive and execution contracts as validated. A separate guarded publication migration enables only the verified adapters.
- Each request uses the existing dataset metadata tier: $0.047259 provider tariff plus $0.007089 Apiosk fee, **$0.054348 total**. These are Apiosk charges for integration/execution; the upstream public reads are anonymous. No new provider credentials are required.
- Normalized saved results include source URLs and coverage. The app renders dedicated tables and preserves unknown/missing fields. Incomplete KOOP pages and RVO first-page results retain partial coverage.
- App and Gateway migration files are mirrored. Executor edits are inside the independent `legacy-executor` Git submodule and must be committed there before its parent pointer can be recorded. No Git commit or push was requested or performed.

## Production release

Gateway and app deployments completed. All four production-egress adapter checks passed: KOOP search returned 2 records, KOOP detail 1, RDW 1 and RVO 26 keyword matches from 50 scanned descriptions. All three listings are active, verified and discoverable; all four execution contracts are active. The public directory increased from 24 to 27 providers.

Applied migrations: `20260911100204_onboard_koop_rdw_rvo.sql` and `20260911101255_publish_koop_rdw_rvo.sql`. The app deployment is `6aa3d1b5bfa1ed591daedc93`; Gateway runs on Fly machine `891e969b6d4038` in Amsterdam. The final passing production smoke output is `production-adapter-smoke-final.json`.

## Validation

- Real-source fixture tests cover empty/single/multiple KOOP records, date filters, SRU diagnostics, exact RDW identity, RVO no-match semantics, input validation and all four contract admissions/prices.
- The actual configured intent model routes four Dutch-language examples to the intended capabilities and preserves the supplied dates, authority and plate.
- A local durable Gateway flow using live public upstream data verifies quote → explicit approval → execution → saved result → replay without another provider call. Billing and the model in that test are fixtures; it is not evidence of an actual production balance debit.
- Existing durable authorization/recovery regression tests, workspace tests, targeted Clippy, app result-table tests and the production app build passed.
- The deployed app HTML references the exact assets in the verified local build.

Evidence is stored under ignored `output/source-expansion-20260911/`: normalized upstream research, local durable-flow results, model-planning results and production adapter smoke outputs. HTTP failures are retained alongside successful runs; these are bounded checks, not an upstream availability guarantee.

## Official documentation

- [KOOP SRU manual](https://www.koopoverheid.nl/binaries/koop/documenten/instructies/2021/02/09/handleiding-voor-het-uitvragen-van-de-collectie-officiele-publicaties/HandleidingSRU2.0.pdf)
- [RDW registered-vehicle dataset](https://opendata.rdw.nl/en/en/Vehicles/Open-Data-RDW-Gekentekende_voertuigen/m9d7-ebf2)
- [RVO open data](https://www.rvo.nl/onderwerpen/open-data) and [YAML documentation request](https://www.rvo.nl/onderwerpen/open-data/aanvraag-yaml)
