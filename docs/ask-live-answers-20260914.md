# Ask live updates and sourced answers

The Ask page now supplements task events with read-only saved-task requests. A silent or broken event connection no longer requires a reload; pageshow, reconnect and foreground events trigger a status read. Revision checks prevent an older response replacing newer task state. Recovery never resubmits approval or execution.

New Ask source requests always generate an answer from the saved results, including simple factual lookups. Numeric prose uses validated evidence placeholders: the model selects source pointers and trusted code inserts their exact values. GeoJSON geometry is omitted from model input while all original property pointers remain valid. The stored-response bound increased from 64 KB to 512 KB so ordinary demographic responses remain structured; oversized responses still get an explicitly truncated preview. Previously truncated records are not overwritten or repurchased.

Answers use progressive word-by-word rendering after validation. This is a presentation stream of a validated answer, not unvalidated model token streaming. Saved answers display immediately. Reduced-motion and screen-reader users receive complete text. The chat keeps one small source/details button; evidence, reports, billing, task controls, source tables and raw JSON open in a side drawer.

## Verification

- 77 focused App tests passed, including silent-event completion, dropped SSE, PWA resume, progressive text, no purchase replay and GeoJSON preservation.
- Two Rust analysis-validator tests passed, including rejected invented numbers, missing pointers and nonexistent evidence placeholders.
- Isolated durable-flow suite passed: consent, replay, result reads, recovery, workspace access checks and simple Ask answer generation without another provider call.
- Real free PDOK postcode response was passed through the live answer model. It selected postcode 3011, 20440 residents and reporting year 2024, with all three values resolved from exact saved JSON pointers.
- Browser preview visibly showed partial text with a typing cursor and a compact source/details button. No customer source purchase was made for these checks.
- TypeScript production build, focused lint and diff checks passed.

Evidence from this run is under `output/ask-live-fix/`; deployment logs are in `/tmp/apiosk-ask-*-deploy*`.

## Deployment

The App, Gateway and the `use-selection` and `agent-gateway` Supabase functions are deployed together after the validated repository changes are committed. Production health, static-asset cache headers and the public source probes are checked after each deploy. Paid completion recovery is verified in the isolated regression flow without purchasing another customer result.
