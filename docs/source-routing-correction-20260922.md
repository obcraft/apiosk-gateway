# Source routing and Ask verification — 22 September 2026

The reported Revolut task (`58768aac-6473-4aef-854a-8434db2983b6`) preserved `source=companies-house` on its filing-history goal, but the dependency planner chose Sirenic's cheaper European company search. Its optional GB identifier was absent. Companies House was consequently skipped with `missing_source_fact`, after the Sirenic lookup had been charged. The saved task was inspected read-only and was not replayed or changed.

## Changes

- A requested provider's native identifier lookup cannot be replaced by a cheaper reseller, including when another goal already schedules the reseller. An unavailable native dependency does not silently fall back to a reseller. Distinct cross-source bridges remain possible when the named provider does not produce the required input, such as registry addresses used by PDOK maps.
- Conditional output fields cannot displace a guaranteed identifier-producing contract. A planned optional field is not treated as an already available identifier. Existing grounded facts can still be reused.
- Filing-history summary periods stay in the requested analysis, with explicit page/date coverage limitations. Explicit exhaustive-history requirements remain constraints and cannot be satisfied by claiming a single page is complete.
- The existing local bounded parser-repair change is included: invalid invented query strings receive validation feedback and one correction attempt, with unchanged literal-input validation. This recovered the tested broad professional-research request.
- A Companies House PSC response with an empty `items` array, `total_results=0`, and `start_index=0` is a valid register response. It no longer produces a false source failure or triggers a paid fallback. It does not prove absence of owners.

The [Companies House filing-history API reference](https://developer-specs.company-information.service.gov.uk/companies-house-public-data-api/reference/filing-history/list) documents pagination and category inputs. A returned page is not proof of complete history or a review of the filed documents.

## Verification

- Gateway full regular suite: 211 passed, zero failures, 32 explicitly ignored environment-dependent cases; relevant isolated integration cases were run separately below.
- Resolver regressions failed before the fix and passed afterwards, covering the cheaper reseller, reuse of optional identifiers, native-lookup failure, and the recorded Companies House contract fixture.
- The isolated durable-flow test covers quote, refusal before approval, one approval, two dispatches to the named source, cited analysis, idempotent replay and saved-result recovery without extra source calls.
- All eleven workflow acceptance cases and the separate UK supplier acceptance test pass with local model, provider and billing fixtures. Their empty-PSC failures were first reproduced on the unchanged `da94f66` baseline, then corrected without weakening their success assertions.
- App: 2,257 tests passed; lint and production build passed.
- MCP: 223 tests passed. Its pre-existing local invocation-label edit was preserved; MCP was not changed for this fix.
- Configured live model plus read-only production catalog: the exact Revolut question quoted Companies House search and filing history on App and connector planning surfaces. A request for every filing retained `coverage=complete` and required clarification. Broad person/company research, two-company accounts comparison, and Companies House plus OpenSanctions also produced plans. These checks did not execute paid data sources.

Local diagnostic evidence is in `output/source-routing-audit/`; `examples/audit-ask-planning.rs` can repeat App/connector planning against an explicit catalog snapshot without buying data or writing tasks.

## Production verification

Runtime commit `53f1413` was pushed to `origin/main` and deployed to Fly as `deployment-01M34Y2VE3F8J1CP4SS6FAV1RM`. Rolling deployment, smoke/health checks and DNS verification passed. The release checker confirmed the Gateway is ready, task reads require authentication, MCP exposes five tools, and approval CORS remains App-only.

- Exact Revolut question: [saved App task](https://app.apiosk.com/gateway-v2?task=95bc3914-1958-4177-ac79-2c6cf65d9aad). The App displays Companies House only and a €0.10 ceiling. The persisted quote contains exactly Companies House company search and Companies House filing history, cap 108,696 USD micro. Zero executions at verification.
- Broad professional-research question: [saved App task](https://app.apiosk.com/gateway-v2?task=c23935bd-22b3-4e99-a814-6f5285c2b6c1). The previous production attempt failed during interpretation; after deployment it produced a €0.216106 quote for StableEnrich domain resolution, Reuters news and Exa web search, cap 234,898 USD micro. Zero executions at verification.

The paid live execution, answer-quality and saved-result acceptance checks are awaiting explicit approval for at most €0.35 combined. Earlier paid requests were not replayed. The tests and quotes above do not certify every source combination or upstream availability.

## Execution-chain follow-up

The reservation boundary now rechecks all unfinished primary services, including dependent calls whose identifiers are not available yet. If any quoted service or price has changed, the current step is refused before a selection, execution or payment reservation is created. Further calls under that invalidated quote are refused as well. Conditional fallback services are checked when eligible. Existing receipt replay precedes this validation, so catalog changes cannot turn recovery into another purchase or hide an existing receipt.

A new backend-worker acceptance test exposed a second failure: a refused call was removed from the pending list only after the next action had been calculated. The saved action could therefore poll a receipt that did not exist, leaving the task running after the worker stopped. Settled calls are now removed before advancing, for both successful and failed outcomes.

Additional isolated acceptance scenarios verify:

- Native search pauses for an ambiguous company, resumes after selection under the original single approval, retrieves the report and produces a saved, cited answer. Exactly two native-provider calls occur.
- A native search without a company identifier ends with explicit incomplete coverage after one search call; it does not buy the report or remain running.
- Withdrawal of the dependent report blocks the direct execution path before buying the search. Repricing of that report blocks the background worker before any purchase, and reaches a terminal failed state.
- Previously saved findings remain readable during catalog changes, without provider calls.

The Gateway regular suite, durable/latency integration, eleven workflow cases and separate UK supplier acceptance suite were rerun against an isolated checkout. This kept concurrent public-API/organisation-key changes out of the tested release. Model, billing and provider responses in these acceptance tests are fixtures; the paid live acceptance boundary above remains unchanged.

Follow-up runtime commit `4425bac` is deployed as `deployment-01M34Z7SV26B9585QB0DN59SBB` on machine `891e969b6d4038` in Amsterdam. Rolling deployment, smoke checks, machine health and DNS verification passed. Post-deployment checks confirmed Gateway readiness, unauthenticated task reads returning 401, MCP's five tools/card v55 and App-only approval CORS; the App returned HTTP 200. The regular Gateway suite passed 211 tests with 32 explicitly ignored environment-dependent cases. These surface checks do not replace the pending paid live acceptance run.

## Unnecessary period clarification

The next screenshot matches saved task `b51ca1ab-9024-4105-b67f-ea570c44dd26`, created at 18:12:20 Amsterdam time. Its intent retained both `period="3 years"` and an answer operation to summarize events over that period. The planner treated that period as an unsupported API parameter and asked permission to perform the local date selection the user had already requested.

The resolver now accepts a nonempty analysis period for the reviewed Companies House GET filing-history contract. It preserves that period in the intent; it does not invent a source date-filter parameter, remove the requested source, add calls, or claim complete history from a page. Exhaustive coverage, source-side date filtering and unsupported filing selection remain explicit constraints. Period selection for annual accounts is unchanged. Ask instructions also direct this ordinary overview request straight to a quote.

The regression test reproducing the saved intent failed before the change and passes afterwards, including English/Dutch period descriptions and checks that exhaustive retrieval and annual-accounts restrictions remain enforced. The Gateway suite now passes 212 tests; durable/latency integration and the separate UK supplier acceptance also pass. The configured live model produced native Companies House search-plus-history quotes for the exact English question and its Dutch equivalent, without a clarification or paid source calls.

A read-only recheck also found that the previously prepared Revolut task `95bc3914-1958-4177-ac79-2c6cf65d9aad` had subsequently reached revision 8 with two successful executions, charged 108,696 USD micro in total, and a saved, cited Companies House analysis. It was not replayed. That result explicitly covers a bounded page; it is not proof of exhaustive history. This supersedes the earlier zero-execution observation for that task; the broad professional-research quote has not been reverified here.

Runtime `798022c` was deployed as `deployment-01M3503SK3NNVSPF0FJBJZXHWD`; rolling, smoke, health, DNS and release-surface checks passed. A fresh live App submission of the exact screenshot question produced task `258c5221-fb89-44ab-95e1-120219cb69d1`: the visible reply proceeds directly to a €0.10 maximum quote with Companies House as its sole source. The saved plan has two Companies House steps, no clarification, and zero executions. No new paid purchase was made for this verification.
