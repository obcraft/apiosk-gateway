# Editable multi-source examples: release evidence

Date: 2026-10-04

**Status: all fourteen production examples completed, independently reviewed and explicitly reloaded.** Every answer combines findings from its two requested data providers. All 593 citation occurrences resolve to the exact saved source values; source documents, approvals, executions, receipts and source-charge totals remain unchanged through saved-result recovery and reload. The original fourteen unavailable analyses and subsequent intermediate failures remain in the audit. Six answers carry minor wording notes described below; none blocks the requested output.

## Outcome required

Each of the fourteen visible cards must prefill an editable question, preserve its submitted values, use at least two distinct actual data providers under one total price and approval, and combine useful returned facts into one cited answer. Reopening or retrying analysis must reuse saved results without another source purchase. Provider identity is measured by `source.provider` / `service.api_slug`, not by endpoint or codebook count.

The final answer must preserve source identities, reporting periods, units, negative results and real limitations. A returned web page or sourced Linkup result counts as source material; a generic model response alone does not.

## Release identities and checks

| Component | Verified identity | Evidence boundary |
|---|---|---|
| Gateway | `baaaf727f906c9f11b0381915ba89bbd841fcf0e` | Committed and pushed; full gate 836 passed, 0 failed, 89 ignored across 82 targets. Live Fly release `01M43VH4KK7YYNYG0XHDCF5T5Q`, image `sha256:ba04f5668f5b985f389d67d4ffcacd3348894bb3955b3a2a6af5864443aa7974`; exact health version, payment executor ready, protocol and unauthenticated task boundary verified. The isolated saved-refresh payment/source preservation integration test passed separately. |
| Deadline and identity-context release | `1c91498c0131ae3ea9f5c1a924a0b35a988a4b4d` | Deployed as Fly `01M43T2RMW37SR3ZC6GDW4TBPM`, digest `sha256:4143bea3a510ca22cc43ee0d4ba5ae744f9d7c16262e8ad59472c5ec21903ee8`; exact healthy version/payment executor and public boundary checks passed. Case 03 recovered on this version. |
| First synthesis release | `cd715d8021cd13d9f2e71fa60f903b188f43e477` | Deployed as Fly `01M43GKF4SJNX4K0M6G5PVF0Q3`, digest `sha256:f5756446ceb16110b30f279eeeb26cacc21e1ba8c236b19cdd441010c8259350`; twelve answers recovered on this version. |
| App | `d42a840b215128c9340fe6a29b201f3f537cedd4` | Committed/pushed; remote main matches. Full gate 2,582 tests across 235 files, ESLint, TypeScript and production build passed. Includes the saved-filing fallback and history-switch identity fix. |
| App deployment | Netlify `6ac274cfbdf14500085d0dc1` | Ready, published `2026-10-04T15:47:06Z`, exact commit reference. History-switch → open saved request checked live at `2026-10-04T15:52:56.589Z`, opening the correct RDW/Louwman task. |
| Earlier browser verification | App `974a2cffdedd2cd652b3665e43fa613d443d3337`, Netlify `6ac24eec07c0820008c011e8` | All fourteen prefill interactions and the saved-source presentation were verified live before the isolated annual-filing fix. |

## Paid source baseline and final verdicts

The frozen baseline has **14 approvals, 48 successful source executions, 48 saved payment receipts (status `charged`) and 48 saved results**. Every task has one matching approval, the saved source charges reconcile, and actual charges stay within the quoted cap. Receipt rows have status `charged`, stage `settling` and a populated `settled_at`; this audit does not independently verify external or on-chain settlement. Total actual source charge: **2,689,678 micro-USD ($2.689678)**. These are source charges including the platform fee, not model operating costs or a conversion of the EUR UI display.

Every baseline analysis was `unavailable`; source success is not treated as answer success. The frozen [paid execution and recovery evidence](evidence/multisource-examples-20261004.json) retains baseline, intermediate, recovered and explicit-reload stages separately. Its fourteen final analyses are completed and hash-bound to independent semantic reviews: eight passes and six passes with minor wording notes. All fourteen cite both requested providers, all 593 citation occurrences validate, and every explicit reload preserves the final answer hash. Recovery and reload added **zero source purchases or source charges**.

| # | Card | Actual providers | Saved source calls | Actual USD charge | Production task | Final answer verdict |
|---|---|---|---:|---:|---|---|
| 01 | Check a company | Domain Intelligence + KVK | 4 | 0.380435 | [487b46a0-1116-4d75-9e87-60b13658ae6a](https://app.apiosk.com/?task=487b46a0-1116-4d75-9e87-60b13658ae6a) | Completed; semantic pass; reloaded |
| 02 | Find opportunities | Linkup + TenderNed | 2 | 0.065848 | [cc12b820-6676-4f18-a570-9bc9b9b48b7d](https://app.apiosk.com/?task=cc12b820-6676-4f18-a570-9bc9b9b48b7d) | Completed; semantic pass; reloaded |
| 03 | Research a market | CBS StatLine + Eurostat | 6 | 0.156087 | [94050261-eb9c-4376-901b-70129a6b11de](https://app.apiosk.com/?task=94050261-eb9c-4376-901b-70129a6b11de) | Completed; final production semantic pass; reloaded |
| 04 | Check locations | KOOP + PDOK | 6 | 0.325218 | [d4168970-97ba-4336-bd01-15fe8159b0aa](https://app.apiosk.com/?task=d4168970-97ba-4336-bd01-15fe8159b0aa) | Completed; final production semantic pass with minor wording note; reloaded |
| 05 | Volve Capital | Domain Intelligence + KVK | 3 | 0.282609 | [c24f232a-cb94-4549-a890-c538850cfd58](https://app.apiosk.com/?task=c24f232a-cb94-4549-a890-c538850cfd58) | Completed; semantic pass; reloaded |
| 06 | Cybersecurity | Linkup + TenderNed | 2 | 0.065848 | [4358df34-3c63-494f-af28-8e1a574e5706](https://app.apiosk.com/?task=4358df34-3c63-494f-af28-8e1a574e5706) | Completed; semantic pass; reloaded |
| 07 | Solar panels | Linkup + TenderNed | 2 | 0.065848 | [b1d7a840-7e70-4421-9530-75420f1e67a5](https://app.apiosk.com/?task=b1d7a840-7e70-4421-9530-75420f1e67a5) | Completed; semantic pass; reloaded |
| 08 | Cleaning | Linkup + TenderNed | 2 | 0.065848 | [fc210dc0-c4d3-4283-8a09-bd15fc54bcf6](https://app.apiosk.com/?task=fc210dc0-c4d3-4283-8a09-bd15fc54bcf6) | Completed; semantic pass with minor wording note; reloaded |
| 09 | Coolsingel, Rotterdam | KOOP + PDOK | 6 | 0.325218 | [af2a0b46-b394-4aa8-b85c-ff3a4bc4f7a5](https://app.apiosk.com/?task=af2a0b46-b394-4aa8-b85c-ff3a4bc4f7a5) | Completed; final production semantic pass with minor wording note; reloaded |
| 10 | Grote Markt, Groningen | KOOP + PDOK | 6 | 0.325218 | [a0959278-ab24-438b-af3c-3a5df9334744](https://app.apiosk.com/?task=a0959278-ab24-438b-af3c-3a5df9334744) | Completed; final production semantic pass with minor wording note; reloaded |
| 11 | Accountant | KVK + VIES | 2 | 0.173913 | [d5d8afd0-2ae1-4095-ae86-a13c4bb55d9d](https://app.apiosk.com/?task=d5d8afd0-2ae1-4095-ae86-a13c4bb55d9d) | Completed; semantic pass; reloaded |
| 12 | CFO / treasury | Frankfurter + IBAN Check | 2 | 0.120001 | [f0ea3a06-a928-4b9c-bf48-7248a37f3031](https://app.apiosk.com/?task=f0ea3a06-a928-4b9c-bf48-7248a37f3031) | Completed; semantic pass; reloaded |
| 13 | Fleet manager | KVK + RDW | 2 | 0.130435 | [e4e5cefc-194a-40d4-98c4-382efc6bc0e3](https://app.apiosk.com/?task=e4e5cefc-194a-40d4-98c4-382efc6bc0e3) | Completed; semantic pass with minor wording note; reloaded |
| 14 | Research journalist | Domain Intelligence + Linkup | 3 | 0.207152 | [05f0195a-1856-4d17-8833-cd609ff526c1](https://app.apiosk.com/?task=05f0195a-1856-4d17-8833-cd609ff526c1) | Completed; semantic pass with minor wording note; reloaded |

## Live interface verification

[Final-release prefill evidence](evidence/multisource-browser-prefill-final-20261004.json) records all fourteen live card clicks on App `d42a840` at `2026-10-04T16:00:16.242Z`: each inserted the exact question, focused the input, kept the task-region count at zero and left the URL at `/`. Editing both `example.org` occurrences to `iana.org` changed the text without submitting it. The [earlier release check](evidence/multisource-browser-prefill-20261004.json) is retained separately.

[UI review evidence](evidence/multisource-ui-review-20261004.json) records the live saved journalist result: three useful Domain Intelligence/Linkup tables, no generic analysis-failure text, and two loaded provider images at 20×20 pixels, clipped by round wrappers. Main headings measured 24px/600, source headings 16px/600 and table headings 14px/700. The committed shared CSS uses one body scale and 4px heading-to-description spacing.

The final App version was also visually inspected on the recovered fleet answer: compact combined content, the same heading/body hierarchy, both round KVK/RDW images loaded, and no generic failure. This was an actual viewed browser screenshot; no local screenshot artifact was saved.

The task header now shows the conversation title without the old “Review your request” heading and explanatory subtitle. Source marks are deduplicated per answer, retain accessible names, and have image/initial fallbacks. Specific source or analysis limitations remain visible; only the generic synthesis failure is removed when usable saved content renders.

The original reported KVK task was reopened separately: its ten-row profile showed Bas Grave Web Development, Onderstreept and a readable address, without the generic error or the false missing-name warning. That revisit made no purchase and did not retry analysis. It is not counted among the fourteen new cases.

The App release also closes the annual-filing-only gap. The fallback selector and renderer share the same checked KVK filing projection. A real numeric financial figure must be visible inside the bounded rendered rows; filing dates/years alone, empty/malformed trees and error responses cannot hide a failure. Zero and negative values survive; financial currency is taken only from the source body. Original saved data, source gaps and analysis status remain unchanged. Fourteen focused annual-filing cases passed within the complete App gate.

## Source and answer semantics

The final Gateway release also offers a person-initiated “Retry analysis of saved results” action for a completed or partial saved answer. It uses the current saved plan, intent and documents, with no source execution. Pending workers, dispatches, choices or intake recovery block that path. An old clarification string in a partial answer does not prevent explicit regeneration of its existing evidence. If model synthesis fails, previously verified findings remain visible with a specific refresh limitation; this does not launch a new clarification call or bypass a reviewed source-validation failure. The isolated integration test verifies expired-quote recovery, owner/worker guards, unchanged authorizations/executions/results/receipts/balance, and zero source calls. The final production refreshes of cases 04, 09 and 10 completed through this action and were explicitly reloaded without changing any paid-source or billing identity.

All fourteen recovered production answers have been independently reviewed against their exact saved sources and explicitly reloaded with unchanged answer hashes, source documents and payment identities. Case 04 initially exposed a false neighbourhood-identity unknown: the already-saved BAG address document contained the exact neighbourhood code, but the compound source selector omitted that dependency. Cases 09 and 10 were completed but carried the same unnecessary caveat. The follow-up release restores only the completed, approved, same-entity PDOK dependency feeding the requested neighbourhood contract; it does not override completeness or manufacture a join. The exact selection over the saved task retains four of six original documents. The final production answers now cite matching original BAG/CBS codes for all three addresses and complete every requirement. Case 04 was explicitly reloaded at `16:31:10.206Z`, case 09 at `16:36:19.989Z` and case 10 at `16:37:43.095Z`, on App `d42a840` and Gateway `baaaf72`.

Case 03 exposed the separate per-attempt synthesis deadline: a complete answer validated in about thirty-two seconds, beyond the previous thirty-second attempt limit. The bounded multi-source attempt now uses the existing overall stage budget. The exact local replay and the subsequent production recovery both preserve every sector, unit, population and period. The recovered production answer was independently reviewed and explicitly reloaded unchanged.

Minor non-blocking wording notes remain recorded rather than hidden. The cleaning answer inlines long web excerpts. The fleet/domain answers describe retrieval timestamps as missing from saved fields even though the outer source envelope contains them; raw provider bodies omit those timestamps. The three location answers broadly describe floor area as unavailable: their selected building records do not establish total building area, but separate saved occupancy-unit dependencies contain unit-area values. Those caveats should distinguish unit area from total building area. The requested building/neighbourhood facts and original-pointer joins are accurate, and no ownership, valuation or current-permit conclusion is inferred. Final verdicts and these notes are bound to actual analysis hashes in the compact audit.

All fourteen saved-source model replays produced validated useful combined answers after the synthesis changes. These are **local model replays over previously purchased data**, not final production recovery evidence. Independent review checked actual subject bindings and conclusions in addition to JSON/citation validation:

- Company cases retain the supplied financial year, report raw amounts when currency/scale are absent, distinguish gross margin from revenue, and do not infer company ownership from domain/DNS co-occurrence.
- Tender cases identify named notices, publication/closing dates and official links; awards, consultations, prior notices, termination and closed third-party listings are distinguished from potential opportunities. The shortlists remain explicitly non-exhaustive.
- The CBS comparison preserves all twenty disjoint sector sections, the separate unclassified category and national total, the latest verified common monthly period, publication status, count unit and business population. Its annual Eurostat GDP context is kept separate; no bankruptcy rate or causal relationship is inferred.
- Location cases join exact BAG and neighbourhood identifiers, retain the CBS reporting year and separate neighbourhood aggregates from building facts. KOOP title-search zero matches and related permit notices are scoped to what was actually returned, without asserting ownership or current legal status.
- VIES returns validity, check date, name and address; cross-source correspondence is not presented as an independently verified ownership relationship.
- IBAN results establish format/check-digit and bank-directory facts, not account existence or payee identity. The ECB result is a dated daily reference observation from the executed latest-rate contract, not an executable bank quote.
- RDW vehicle facts and the separately requested dealer's registry record do not establish a vehicle-company ownership relationship.
- Domain and fetched-page findings distinguish registration, null MX/no-mail configuration and the publisher's stated documentation purpose.

The first generic CBS projection artificially omitted relevant sectors/codebooks. The final adapter reuses the existing validated source join, retaining every needed original-index record and original evidence pointer. Explicit CBS periods, missing scope/codebooks, mismatched entities, duplicate references and invalid/truncated joins cannot be labelled a complete comparison. All twelve CBS module tests passed, including four new projection cases.

## Dynamic inputs and regression scope

[Local check evidence](evidence/multisource-local-checks-20261004.json) records commands, results and log hashes. The full Gateway/App gates include the planning and rendering regressions. The model-free compound tests exercise the actual `Parser::ask_intake` → `chat_from_capabilities` → `bind_subjects` → `resolve` path for all fourteen exact published questions, asserting distinct providers, source-bound input values, expected operations and one summed cap. They also cover company/name/KVK/domain changes, leading-zero identifiers, addresses, VAT, IBAN plus EUR/GBP, plate/company combinations and fetched URLs. Unsupported extra clauses cannot be silently quoted as only a supported prefix.

Five additional focused regressions passed: clause reorder across four source families; an unpublished cloud-migration topic; an unpublished address shared across PDOK/KOOP; complete-compound GDP year/country changes while retaining Netherlands CBS scope; and editing only one repeated address so both independently submitted values stay distinct.

The deterministic compound path has a **bounded prose grammar**: it composes supported whole clauses and variable inputs, not card titles or fixed subject values. Other paraphrases/contextual follow-ups use ordinary normalized/model interpretation. Passing the canonical example tests does not certify every possible rephrasing. Repeated values in separate clauses do not automatically propagate when one is edited.

## Final verification and handoff

The deployed Gateway image and App commit are verified above. The compact audit records fourteen completed production answers, fourteen independent semantic passes, fourteen explicit UI reloads, all 593 exact citation checks, and unchanged payment/source identities across baseline, recovery and reload. It also preserves nineteen actual UI retry clicks and the initial/intermediate failures; this is a recovered end-to-end result, not a claim that the first attempts succeeded. The source receipt states support the recorded charges, not independently verified external settlement.

At `2026-10-04T16:38:19.055Z`, the final App was left on the **Research a market** example with its full canonical CBS/Eurostat question prefilled and focused, URL `/`, and zero task regions. It remained unsent. The editable handoff, local regressions, paid source execution, final synthesis, ledger comparison and browser reload checks are recorded as distinct evidence stages.
