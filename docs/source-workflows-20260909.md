# Direct-source workflow validation — 9 September 2026

## Outcome

Nine of the ten questions have usable source answers after the contract, planner and presentation repairs. The HEMA/AFAS annual-account comparison remains blocked by actual upstream 404 responses, not by a missing planner dependency. This is not a claim that all ten have passed the live paid browser journey.

All provider checks used the original upstream APIs, not an Apiosk connector/plugin. The rerun made 23 bounded source requests: 21 HTTP 200 and two HTTP 404. The annual-account calls respected the one-minute interval. No Apiosk purchase or balance authorization was made.

| Question | Result and limits |
| --- | --- |
| Compare HEMA B.V. and AFAS Software B.V. annual accounts | **Blocked.** Searches resolve KVK 34215639 and 31046821; both annual-account calls return 404 / IPD7002. Planning correctly contains two searches and two dependent account calls. No financial comparison is fabricated. |
| Coolblue B.V. company profile and sanctions check | Profile for KVK 24330087, nine trade names, and zero screening matches. A zero-match screen is not a safety guarantee. Legal name no longer incorrectly comes from a multi-valued trade-name field. |
| ASML Holding N.V. direct subsidiaries in GLEIF | Eleven returned records for LEI 724500Y6DUVHQD6OXN27. Entity and registration status are visible; duplicate/lapsed records are not described as eleven unique active subsidiaries. |
| MONZO BANK LIMITED profile, officers and significant control | Company 09446231; 30 officer records (nine active, 21 resigned) and three PSC records (one active). Not merely a company search. |
| TenderNed publications about charging stations | Ten publications of 221 total. Native rows preserve authority, publication type and dates; the grounded summary succeeds. Results include historical/awarded notices, not only open opportunities. |
| CBS bankruptcy figures and definitions | Newest-first 100-observation page, currently period code 2026MM07. Official measure, sector and entity-type codebooks are joined by table and identifier. This is a bounded page, not every sector or a trend series. Period codes remain explicit; no period-codebook lookup is claimed. |
| Netherlands vs Germany GDP in 2023 | Eurostat nominal GDP at market prices, current-price million EUR: NL 1,050,133 and DE 4,254,930. German provisional flag retained. Both quoted calls bind the requested year; not real GDP/growth/per-capita GDP. |
| Water-quality datasets, publishers and source links | Twenty returned datasets of 87 matches. Publisher and all resource URLs come from the same source record. These are catalogue metadata, not water-quality measurements. |
| Official address Dam 1 Amsterdam | Exact address candidate selected for display: 1012JS, Amsterdam, Oude Kerk e.o. The street-only first search hit is not mistaken for the requested address. Ambiguous/nonexact results remain candidates. |
| BAG addresses for postcode 1012JS | All 33 returned address records shown, including house-number additions. |

## Repairs

- Production contract versions supersede broken KVK, TenderNed, PDOK and CBS mappings without mutating historical contracts.
- Five source-verified presets cover CBS definitions and explicitly parameterized NL/DE GDP. Every codebook request is separately priced through the existing quote; approval and the 15% markup are unchanged.
- EODHD remains visible but resale-blocked. No unsupported execution has been enabled.
- Native tables preserve row associations, identifiers, numeric formatting, units, statuses, counts and pagination. Large dataset pages have a bounded readable projection while original JSON remains pageable.
- OpenRouter analysis uses strict structured output, validated source pointers, bounded retries and a manual retry action using saved results only. Retry does not authorize or re-purchase sources.
- An early generated dataset summary associated the wrong publishers with titles despite valid pointers. Dataset listings now use deterministic native rows instead of treating that model output as a pass. Early TenderNed analysis failure is retained in the audit; the stricter structured-output rerun succeeded.

Applied migrations, mirrored in App:

- `20260909130730_add_validated_statistics_presets.sql`
- `20260909130757_repair_direct_source_contracts.sql`

## Verification and reproduction

Gateway regression tests, contract extraction fixtures and Clippy; local durable-flow test against the isolated PGlite adapter (including approval, replay, saved results and recovery). App: 1,881 tests passed and production build passed. Local browser QA rendered real saved source responses using the production table components; verified all 20 dataset rows, all 33 BAG rows, 11 GLEIF rows and the exact PDOK address.

Live natural-language planning is tested against the production catalog and configured model, without source purchase. Saved real upstream responses are then extracted and passed through the actual analysis method where the planner requests analysis. HTTP success, planning readiness, valid pointers and content adequacy are separate checks.

```sh
cargo test --offline
cargo clippy --offline --all-targets -- -D warnings
# In a separate local terminal:
node tests/support-db.mjs
cargo test --offline --test durable_flow -- --ignored --nocapture

# Explicit upstream test: consumes provider API quota, not Apiosk balance.
node --env-file=.env scripts/test-direct-usecases.mjs
# Set DIRECT_AUDIT_DIR to the directory printed by that command:
DIRECT_AUDIT_DIR=output/direct-usecases-jn7cJ6 cargo test --offline --test ten_questions_live -- --ignored --nocapture
DIRECT_AUDIT_DIR=output/direct-usecases-jn7cJ6 DIRECT_AUDIT_PLAN_OPERATIONS=1 DIRECT_AUDIT_FINAL=1 cargo test --offline --test direct_usecase_audit -- --ignored --nocapture
```

Local evidence: `output/direct-usecases-jn7cJ6/manifest.json`, individual source response files, `question-planning.json`, `final-answer-audit.json`. Earlier unsuccessful analyses remain available alongside the rerun. Live audit files are not required for the normal offline regression suite.

## Remaining source-access decision

The [KVK Open Dataset Jaarrekeningen API](https://developers.kvk.nl/nl/documentation/open-dataset-jaarrekeningen-api) only supplies its supported subset of adopted XBRL filings. A missing API result does not prove the company never filed accounts. The [KVK Dataservice Jaarrekeningen](https://www.kvk.nl/producten-bestellen/kvk-dataservice-jaarrekeningen/) is a possible route for deposited XBRL/PDF filings, but requires separate access and verification of coverage for these exact companies. No such new subscription or access was purchased or assumed in this change.
