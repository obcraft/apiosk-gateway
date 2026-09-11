# Shared source-readiness assessment

The gateway computes one assessment in `src/readiness.rs`, using catalog metadata and only contracts accepted by `Catalog::build`. It is exposed to the intent model, the planning-review model, authenticated `/v2/sources`, and public `/v1/source-readiness`. The website consumes the public assessment; it does not calculate a competing score.

## Rules

- **Blocked**: the listing has an explicit planner restriction. A provider whose entire membership is blocked is also blocked.
- **Discovery-only**: no runtime-accepted contract is available.
- **Planner-ready**: all registered active endpoints have accepted, sample-checked contracts; source name and documentation references are present; descriptions, tags, categories and input schemas are documented; metadata has provenance and is not marked inferred. Pending/unresolved execution outcomes or failures without any success prevent this status.
- **Partially supported**: executable capabilities exist but coverage, identity, metadata, sample evidence or execution concerns remain. Mixed providers retain blocked-listing reasons.

Readiness applies to registered operations, not the full upstream API. It is not an accuracy, ownership or reliability certification. Missing live execution history is reported as unmeasured, not as success or failure. The audit counts recorded v2 execution outcomes from the last 30 days; upstream calls outside that system are not included. Stored response-sample checks are a separate measurement.

Identity checks detect missing references and generic placeholder names; they do not authenticate legal ownership. Metadata checks establish completeness and provenance, not semantic correctness. Inferred tags may guide relevance but cannot establish executable support.

## Website

Every source card has a status and an expandable “Why this status?” explanation. Details include supported/registered endpoint counts, metadata counts/provenance, stored-sample evidence, recorded execution outcomes, restrictions and coverage notices. Unknown or failed audit reads show “Readiness unavailable.” Membership still comes from the public provider directory. Provider groups use the server-generated aggregate, while individual listings use their own assessment.

The existing nine-source homepage limit is preserved. All directory cards show readiness. The planner receives source descriptions, categories, tags, capability descriptions and the same assessment. Descriptive text never grants execution authority; blocked and unsupported sources remain unavailable.

## Data and access

The migration grants only the additional catalog columns and the selected source slug needed to aggregate existing execution records. The new public route exposes no customer identifiers, input/output payloads, credentials, balances or billing details. Catalog reads cache for 60 seconds; website pages revalidate on their existing schedule.

## Validation

- Gateway unit/integration suite passed, including blocked precedence, inferred/missing metadata, zero contract coverage, distinct endpoint counting, mixed providers, and equality between parser and public audit.
- Production read-only audit: 103 listings, 196 accepted contracts, no rejected contracts at validation time.
- Production model checks: EODHD purchase rejected for its recorded restriction; x402 Seller purchase rejected without validated contracts; RDW plate request selected `vehicle.technical-profile`.
- Website build/link/query checks passed. No paid data-source calls were made for readiness validation.

Examples at audit time: EODHD blocked; x402 Endpoints and x402 Seller discovery-only; RVO, RDW and TED planner-ready for their registered operations; KOOP, CBS, PDOK and KVK partially supported due to metadata or coverage gaps. RVO's first-page limitation remains visible.

## Published verification

Gateway image: `deployment-01M2847PYP8CXHW2T8ZMVDYQPF` (healthy).
Website deployment: `6aa3e93a62fd1b11ff7b44ef`.

The live browser check passed at 1440px and 390px: all 28 website cards matched the public audit, the EODHD restriction was visible, and there was no horizontal overflow. Current website totals: 3 planner-ready, 16 partially supported, 8 discovery-only, 1 blocked. Underlying listing totals: 3 planner-ready, 25 partially supported, 74 discovery-only, 1 blocked (103 listings grouped into 28 provider cards).

Re-run: `PUPPETEER_EXECUTABLE_PATH=/path/to/chrome node scripts/check-source-readiness.mjs` from the website repository. The gateway's ignored `readiness_live` tests compare every production listing's public and parser assessment and test three model decisions without purchasing data.
