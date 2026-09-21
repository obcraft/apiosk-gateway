# UK supplier verification

The request “We’re onboarding Anthropic as a supplier. Use Apiosk to verify its UK entity: ownership, filed accounts, VAT validity, and director screening.” maps to `company.uk-supplier-verification`.

## Scope and approval

One maximum quote covers twelve possible source calls. The existing app/card approval authorizes the entire plan; the backend worker executes it. Model-visible tools cannot approve. Unused director slots are skipped only when a complete roster proves they are absent, and are not dispatched or charged. The saved analysis produces the existing downloadable PDF and a short summary; reading or downloading never buys sources again.

| Source | Evidence |
| --- | --- |
| Companies House | UK entity search, profile (including account dates/overdue status), filing index and persons with significant control |
| Sirenic | Current UK officer roster, excluding secretaries from director screening |
| OpenSanctions | One separately priced Person/Company query per source-derived active director, maximum five |
| Tavily | Published GB VAT-number candidates and their source URLs |
| Ask Merlin | Validation of one unique GB candidate, registered identity and recorded check time; HMRC-derived intermediary, not a direct HMRC certificate |

Companies House and the GOV.UK register are the same official source, not two independent confirmations. Filing evidence is not a financial audit or a downloaded accounts document. OpenSanctions includes sanctions/PEP and other watchlist topics, not criminal-record clearance or a guarantee that a director is “clean.” Possible matches require human review.

VAT discovery is not identity proof. Missing or conflicting candidates remain unverified; a valid candidate must match the supplier's registered name/address. An invalid candidate is not proof that the company has no other VAT registration. Ask Merlin accepts known GB numbers, not company names: Tavily supplies the discovery step. Only non-stale live/cache results no older than one hour (plus 60 seconds clock tolerance) are accepted. Unknown/unavailable responses fail validation instead of becoming “invalid.” The free upstream API uses the existing Apiosk public-source tariff, equal to VIES; the approval quotes the buyer total.

Boards larger than five, truncated rosters and unsupported roles are reported as incomplete. The system does not silently screen five people and claim a complete board check. Corporate directors use Company queries; natural persons use Person queries with only source-published birth years.

## Verification and release boundary

- Gateway `cargo test --all-targets` passes; separately run the catalog-core UK extractor tests.
- `APIOSK_TEST_DB_SCRIPT=/Users/olivier/Documents/Apiosk/Production/gateway/tests/support-db.mjs node scripts/test-uk-supplier-flow.mjs` uses isolated local PostgreSQL and mocked source, model and money responses. It verifies one approval, backend continuation, unused calls, missing VAT, truncated rosters, saved PDF and no repurchase on read. This is not a paid production supplier report.
- `CHECK_FULL_CATALOG=1 cargo run --example check-uk-supplier-planner -- <env-file>` reads the current production catalog and calls the configured intent model; it never calls sources or debits customer balances. The exact prompt resolved across 321 services to the twelve intended steps before publication.
- MCP has 198 passing tests and card v48 (ChatGPT v14), with coverage notices before approval.
- The staged migration leaves the new source/endpoint disabled and eight contracts at `validated`. Publish only after runtime deployment and successful free production-egress probes.
- `/usr/local/bin/check-uk-vat` checks a known positive and a published candidate through the same normalizer. A source-egress check is not a paid buyer-flow or settlement test.

The live paid Anthropic execution requires the buyer's explicit approval. Neither fixtures nor free source checks establish a supplier clearance.

## Published 2026-09-22 (Amsterdam)

- Executor commit `9cd1b03`; MCP commit `02d80fa`, successful Fly workflow `35666558970`.
- Gateway Fly image `deployment-01M333YC8AHZ863XTN0N3T2RG2`, machine `891e969b6d4038` version 101, healthy. `/health` reports protocol 2 and ready payment executor; an unauthenticated task read returns 401.
- Both fixed free UK VAT egress probes passed from Fly. Cached source check times were 2026-09-21 22:44:06Z (positive) and 22:44:22Z (invalid candidate); both within the permitted hour at the probe. These are source-adapter checks, not Anthropic supplier clearance.
- All eight contracts are active. Source publication required completing the existing API-specific review gate in a follow-up migration; the first activation attempt remained `pending`, and the first live discovery correctly stayed unsupported. The gate was not disabled or weakened.
- MCP's live metadata serves card v48 and the supplied Apiosk A icon. OAuth discovery/unauthenticated transport checks pass.
- The exact prompt subsequently returned live `ready` task `72a6b6a7-b7a6-49f5-ba6a-49912fc9a35d`, one capped quote, twelve possible steps and five sources, with chatbot approval and backend execution enabled. At discovery no authorization or charge existed. Always read current status before reporting later payment/results.
- `CHECK_FULL_CATALOG=1 CHECK_PUBLISHED_ONLY=1` confirms routing against only the now-published catalog (321 accepted services), without fixture service additions.
- Release work used `/private/tmp/apiosk-uk-supplier-20260922` from Gateway base `69ce23b`, preserving the user's overlapping uncommitted work in the main Gateway checkout. That checkout was not reset or fast-forwarded over their changes.
