# Dutch public procurement browsing

## Problem and change

The question `Show me public goverment contracts in netherlands` stopped at a
mandatory-keyword clarification. TenderNed supports requests without a keyword,
but the existing `company_intel.search` execution contract required one.

The new active `procurement.nl-public-contracts` capability reuses the existing
TenderNed endpoint, tariff, raw publication-page extraction, and approval flow.
It requests one page with `page=0`, `size=20`, and
`sort=publicatieDatum,desc`. A keyword is optional; no company identifier or
synthetic search term is required. Existing search contracts remain unchanged.

This is a mixed procurement-publication feed, including announcements (`AAO`),
awards (`AGO`), corrections (`REC`), and cancellations (`VBE`). Preserve each
source type, publication date, closing date where supplied, and source link.
Do not describe all records as open opportunities, awarded contracts, signed
contract documents, or exhaustive national coverage. The response includes
pagination metadata; this capability fetches only its first page.

## Source evidence

On 2026-09-24, an anonymous request to
<https://www.tenderned.nl/papi/tenderned-rs-tns/v2/publicaties?page=0&size=20&sort=publicatieDatum,desc>
returned HTTP 200 with 20 records, mixed publication types, and a larger total.
The captured response is retained in `tests/fixtures/tenderned-browse.json`.
This public-source probe did not purchase data through Apiosk.

Do not add a `typePublicatie=AGO` filter on this evidence: the public endpoint
ignored that parameter during probing. Award-only browsing requires a separately
verified source filter or explicitly bounded post-filtering.

## Validation and release

- Three new regression tests cover no-keyword planning, explicit keyword/date
  binding, and lossless extraction of source types and pagination.
- `cargo test --test tenderned_browse --test direct_source_repairs`: 6 passed.
- `cargo test --all-targets --no-fail-fast`: 244 passed, 4 failed, 34 ignored.
  All four failures also reproduced on unmodified Gateway HEAD `38605d7`:
  `followups::tests::topical_filing_followup_offers_filingpulse_with_exact_question`
  and three `source_operations` tests rejecting synthetic provider evidence.
- The migration passed a transaction rolled back before production application.
- Production applied migration `20260924100502_tenderned_public_contracts_browse`.
  Identical migration files are retained in the Gateway and App repositories.
- Live discovery of the exact question returned `ready` with one TenderNed step
  and a maximum total price of EUR 0.050001 on 2026-09-24 at 10:05 UTC.
  Task reference: `2ee3b124-f92d-4db0-b85c-2d7cd3afede0`.
- No paid Apiosk execution or settlement was performed for this validation.
  A ready proposal is not evidence of a completed paid result. Recover this task
  through read-only status before any later execution; do not replay it for QA.

No runtime binary or UI change is required because the catalog is read dynamically.
The migration adds a capability and execution contract; it does not change
endpoint prices, billing, authentication, or RLS policies. If a rollback is
needed, retire only the new capability/contract after reviewing pending tasks.

The generator `scripts/prepare-tenderned-browse.py` accepts a captured public
response and an existing CLI-created migration path. It performs no network
requests or database writes. The checked-in timestamp matches production's
migration history, replacing the initial local CLI timestamp.
