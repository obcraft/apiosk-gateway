# Ask capability intake

App `/v2/chat` submissions now follow this order:

1. Extract and validate the WebGPU-style `language`, `subjects`, `capabilities`, and `deliverable` object. Add model, parsing time, and SHA-256 of the question on the server. Greetings use the conversation capability without discovery. Malformed output gets at most one correction attempt.
2. Build generic queries from capability name, description, input concepts, and explicit source hint. Scan every accepted Apiosk operation, score a shortlist per capability, and semantically match those candidates. The directory comes from the current catalog; Pulse Network's services remain individually searchable inside its one source group. No count is hardcoded.
3. Query public Coinbase Bazaar with those generic queries, after Apiosk matching. Redact parsed/saved subject labels and identifiers. Filter HTTP resources, reject invalid URLs, deduplicate, and retain at most three results per capability. Search failures leave local matches intact. Never request a returned resource URL here.
4. Bind the authoritative parsed subjects and identifiers to the matched execution contracts and their dependencies, retaining the complete question, selected services, and saved user context. Missing capability matches require clarification before a quote. Coinbase results are discovery metadata, not newly executable contracts.
5. Continue through the existing resolver, total-price quote, approval, and backend execution. Store the intake object with the task so reopening it does not repeat discovery or make a purchase.

The App exposes the latest capability object and ordered source matches in a collapsed “Capabilities and sources” section. Coinbase results appear after Apiosk and are labelled as discovery-only. Paid calls remain behind the existing approval.

Main code: `src/capability_intake.rs`, `Parser::prepare_ask`, `Parser::chat_from_capabilities`, and `Engine::intake_question`. The independent App repository renders `context_view.capability_intake` in `CapabilityDiscovery`.

Checks: `cargo test --offline --lib capability_intake`, the isolated `scripts/test-intake-flow.mjs` suite, and the App's capability-discovery and gateway-v2 tests. `tests/capability_intake_live.rs` contains explicit opt-in checks: an entirely local catalog inspection and a separate live model/public discovery check. The latter sends metadata to the configured model and must only be run with appropriately authorized data. Synthetic diagnostic capture refuses a real catalog.

The planning stage uses its own prompt to translate generic parsed slugs into exact matched operations. Validation rejects a generic slug used as an executable operation, an omitted requested retrieval, and a dependency lookup substituted for the actual result. Shared verbs such as “search” cannot by themselves shortlist unrelated data. An unspecified comparison is clarified rather than assumed to require financial statements.

Validation on 2026-09-27: 260 Gateway unit tests passed (four existing ignored tests), six isolated database intake/recovery tests passed, and 63 focused App tests plus the production build passed. Offline inspection of a fresh catalog snapshot considered 1,301 accepted operations across 112 listings grouped into 37 sources, retaining financial-statement and procurement operations. Three live questions passed with a synthetic company-only catalog: annual accounts proceeded to intent planning, an unspecified comparison requested clarification, and procurement remained unavailable instead of being replaced with company search. Those checks used public Bazaar discovery and created no purchases. Live verification with the full production catalog and deployment remain unverified.

Release validation on 2026-09-27: the workspace/all-targets suite reported 485 passed, 52 ignored, and one strict quality-tape failure (`quality_set_scores_every_replayed_case`, q01). That exact failure also reproduced in an isolated snapshot of unchanged `origin/main`; the recorded baseline was not rewritten. All seven isolated release-gate scenarios passed after updating their model/discovery fixtures and reading the task event after the SSE stream-opening handshake. These cover expiry, long data, lost-response recovery, missing sources, concurrent approval/workers, receipt replay, and workspace isolation.

The reported Coolblue sanctions failure exposed a source-matching defect: production traces showed successful capability extraction followed by unknown operation names or a source-search timeout. Search now sends only shortlisted candidate metadata, with short IDs constrained by a per-request JSON schema. The decoder resolves those IDs back to exact admitted operation slugs and continues to reject unknown candidates. The full directory is still considered locally. On the saved 37-source catalog, sanctions search context dropped from 42,414 to 11,086 bytes. The exact reported question passed live model parsing, matching and resolver compilation against 600 synthetic operations / 37 sources, without authorizing or executing a source call. This synthetic check is `tests/ask_sanctions_live.rs`; it is separate from production-flow verification.

A later production replay reached source matching but failed in the planning stage twice with `unknown_input_type`. The generic reply schema left `subjects.values` keys open, allowing the model to invent a fact name despite the prompt. Capability planning now receives a per-request schema with only the selected catalog's exact input types, exact matched goal operations, and saved entity references. Existing literal and fact-value validation remains in force. Selected contract dependencies and source restrictions are retained while unrelated available-source metadata and the repeated discovery groups are omitted. Regression tests reject guessed fact names, retain dependency inputs, and verify routing preserves the constrained schema. `tests/ask_planning_live.rs` is an opt-in production-catalog check requiring authorization to send that metadata to the configured model; it never creates tasks or purchases.

## WebGPU parity and the execution adapter

The reference implementation is `WebGPU/src/parse/{parser,schema}.ts`,
`WebGPU/src/discovery/x402.ts`, and `WebGPU/src/main.ts` (`finishParse` and
`showEndpoints`). That implementation ends at the parsed object and discovered
endpoint cards. It does not contain paid provider execution or a sourced-answer
renderer. Those steps continue through Apiosk's existing quote, approval,
execution journal, and saved-result rendering.

The parse is now authoritative for subjects and identifiers. The adapter in
`src/parser/capability_binding.rs` offers references such as `s1`, `s1.label`,
and `s1.identifiers.kvk_number`. The model maps these to the exact discovered
operation and input types; it no longer emits a second company label or entity
UUID. Code copies the original values. Additional literal inputs are still
checked against user text and the contract's fact schema. Unknown references,
rewritten subjects, unsupported operations, and omitted subject-specific goals
are rejected before a quote.

`capabilities[].missing` ports WebGPU's `missingInputs` rules: identifier concepts
must appear on a related subject; other concepts are missing when no subject is
present. This describes supplied input, not dependency availability. The adapter
asks a focused question only when essential meaning or inputs cannot be resolved
from user context or a supported lookup. Earlier user messages can supply a
clarification's subject and requirements; assistant replies and source prose
cannot supply literal inputs. The request hash remains that of the current
submission. This conversational continuation extends the single-prompt WebGPU UI.

The 11:08 production trace on 2026-09-27 reached parsing and discovery, but both
planning attempts failed with `non_literal_subject`. This adapter removes that
second subject-generation boundary instead of relaxing literal validation.

Clarified analysis/comparison stays in `deliverable.operations` when concrete data
capabilities are present. A stale `general.analysis` from the earlier ambiguous
question cannot become an extra required source. Standalone ambiguity remains a
clarification. The response schema also forces `discover=false` and `intent=null`
when any essential retrieval lacks a match, even if another retrieval is matched.

Verification for this adapter: 272 unit tests, seven isolated intake tests, seven
isolated release-gate tests, and 33 focused App tests passed. Both reported
Coolblue questions compiled with the configured Gemma model against synthetic
catalogs. A live synthetic two-turn comparison first asked a clarification, then
preserved both companies and Excel and compiled a plan. Offline checks also bound
the exact company name against the saved production KVK and sanctions contracts.
These are distinct from an authenticated production question or paid result.

A larger synthetic 37-group catalog exposed duplicate blocked-source reasons
and 12-second planning timeouts. The adapter retains each complete reason once;
chat gets a 20-second attempt within one shared 30-second stage deadline. The
57,263-byte synthetic planning request passed with the configured model on its
first attempt. The complete workspace/all-targets suite reported 498 passed,
57 ignored, and the previously reproduced baseline quality-tape failure q01.
In this run, automatic approval review blocked sending production catalog
metadata to the configured model pending explicit user permission. Authenticated
production verification remains outstanding; synthetic tests and offline
contracts are not that verification.
