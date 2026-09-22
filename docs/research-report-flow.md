# Combined research and analysis PDF

A single discovery question can request annual accounts, a registry profile and
PDOK context for the profile's disclosed main visiting address. The existing
`company.registered-address-context` contract binds the profile address collection.
It requires one unambiguous Dutch visiting address; it does not enumerate every
company establishment. Missing, foreign or ambiguous addresses must remain explicit.
Additional location capabilities need their own quoted steps.

PDF output and requested analysis are answer operations, not external services.
The unchanged tariffs for annual accounts, the KVK profile and this PDOK operation
produce one cap of 239131 micro-USD ($0.239131), approximately EUR 0.22 at EUR/USD
0.92. The client uses its configured exchange rate. No fixed price range is invented.
One approval authorizes the source steps; analysis and repeated PDF downloads make
no further source purchases. Existing KVK cache and shared admission remain intact.

After analysis completes, `context_view.report` contains a combined PDF link.
Archived conversation outputs expose their own report. Downloads bind owner, task,
quote, document digest and expiry in a separate HMAC domain. The server loads only
owner-scoped saved results and verifies each analysis evidence value against its
original JSON pointer. A changed snapshot cannot replace the approved link's data.
Partial research is labelled; unavailable analysis is retried explicitly without
repurchasing sources. The PDF contains analysis, limitations and selected source
fields; the complete original responses remain in the saved task.

Validation:

- `python3 scripts/test-chatbot-flow.py --suite research_report_flow`: three-source
  fixture, one approval, address binding, cached filing, replay and combined PDF.
- `python3 scripts/test-chatbot-flow.py`: HTTP/auth/MCP fixture, tampered report
  links, repeated free downloads, archived-turn report, recovery and billing.
- `cargo run --example check-research-planner`: real configured model, fixture
  catalog, no provider execution; validates all goals and preservation of PDF output.
- App component tests and MCP transport tests cover combined download links.

Production discovery is free; an actual paid connector acceptance run still needs
the user's approval of its returned exact quote through the existing approval UI.

## Maps, charts and evidence bundles

Report/PDF/map requests capture two official PDOK PNGs after a single unambiguous
company-address or BAG-address result: buildings and cadastral surroundings.
Soil and spatial-plan results also capture an image of the exact reviewed layer and extent, when the official WMS supplies it.
A report overlay marks the source address coordinate on the address maps. It does not assert that
all surrounding parcels belong to the company. PNGs, exact request URLs, retrieval
times and SHA-256 digests are persisted with that result. Capture failures remain
explicit. The capture client has no redirects, fixed map hosts, bounded time,
bytes and dimensions. Original PDF links in saved results are captured only from
reviewed official domains, at most two files of 4 MiB per result. Unsupported links
remain in the original source response. This is not interior floor-plan retrieval
or a comprehensive title/soil report service.

Binary payloads are excluded from chatbot/model projections. Downloads use the
existing owner/task/quote/snapshot signature and workspace access checks. The
new `evidence.zip` route includes report.pdf, source JSON, captured images/PDFs,
and a manifest. Neither download route contacts a provider or purchases data.

Financial bars require unambiguous same-filing amounts, reporting year and
currency; unknown scales, mixed/duplicate contexts and missing values are not
charted. Original numeric labels are preserved. The graph is not a risk score.

The address dependency migration exposes the preserved PDOK RD point through
the reviewed point.rd-extent adapter and gives soil/planning services documented
default layers. Deploy the adapter before applying the migration. Existing
quoted contract hashes must be refreshed after contract changes.

Validation commands:
- `cargo test --all-targets`
- `RUST_MIN_STACK=16777216 python3 scripts/test-chatbot-flow.py` (large HTTP fixture)
- `python3 scripts/test-chatbot-flow.py --suite research_report_flow`
- `cargo test --lib report_assets::tests::live_maps -- --ignored` (free public maps)
- `cargo run --example research_dossier` (clearly labelled fixture data + live maps)
- `cargo run --example check-financing-planner` (read-only catalog + real model;
  stages the address migration in memory, no source purchase)
