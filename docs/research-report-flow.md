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
