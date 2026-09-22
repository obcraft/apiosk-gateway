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

- Resolver regressions failed before the fix and passed afterwards, covering the cheaper reseller, reuse of optional identifiers, native-lookup failure, and the recorded Companies House contract fixture.
- The isolated durable-flow test covers quote, refusal before approval, one approval, two dispatches to the named source, cited analysis, idempotent replay and saved-result recovery without extra source calls.
- All eleven workflow acceptance cases and the separate UK supplier acceptance test pass with local model, provider and billing fixtures. Their empty-PSC failures were first reproduced on the unchanged `da94f66` baseline, then corrected without weakening their success assertions.
- App: 2,257 tests passed; lint and production build passed.
- MCP: 223 tests passed. Its pre-existing local invocation-label edit was preserved; MCP was not changed for this fix.
- Configured live model plus read-only production catalog: the exact Revolut question quoted Companies House search and filing history on App and connector planning surfaces. A request for every filing retained `coverage=complete` and required clarification. Broad person/company research, two-company accounts comparison, and Companies House plus OpenSanctions also produced plans. These checks did not execute paid data sources.

Local diagnostic evidence is in `output/source-routing-audit/`; `examples/audit-ask-planning.rs` can repeat App/connector planning against an explicit catalog snapshot without buying data or writing tasks.

Production deployment and real paid execution are separate acceptance steps; the tests above do not certify every possible source combination or upstream availability.
