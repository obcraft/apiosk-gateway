# KVK annual account PDF reports

The active Gateway v2 App and MCP flows expose a PDF download for each successful KVK annual-account result, including retained older results. The report is rendered from the saved provider response on download, without another KVK call or charge. Legacy v1 response contracts are unchanged.

The report groups every returned field into general information, balance sheet, cash flow and income statement sections. Source hierarchy, years, consolidation markers, units and source keys remain visible. Missing sections and partial source responses are explicit. No figures or currency are inferred. A 20,000-row guard returns an explicit error rather than silently truncating a report.

Financial-section amounts use Dutch number formatting: `18539000` becomes `18.539.000` and `1234567.50` becomes `1.234.567,50`. Formatting preserves the sign and source decimal precision without floating-point conversion. Years, codes, identifiers and raw stored data are unchanged. The PDF's wider amount column keeps ordinary large values on one line.

The result carries report.download_path; App resolves the gateway origin and MCP adds report.url. Paginated results retain the descriptor. Downloads use a domain-separated HMAC bound to owner, task, result and a 24-hour expiry. Database reads also enforce owner and task retention. Reading the saved task creates a fresh link. Anyone receiving the signed link can download that one report until expiry. No account token is placed in the URL. Responses use private/no-store and no-referrer.

Embedded Liberation Sans fonts and their license live under assets/fonts and are included in Docker. Generate the example with `cargo run --offline --example annual_report`.

## Price

Migration 20260908173901_kvk_annual_accounts_ten_euro_cents updates only the KVK annual-account endpoint. List price 94,518 plus the existing rounded-up 15% fee 14,178 equals 108,696 micro-USD. The catalog EUR rate read on 8 September was 0.92: EUR 0.10000032, displayed as EUR 0.10. USD settlement remains unchanged; later FX changes can change the euro display. Search stays EUR 0.02 and other KVK calls EUR 0.07 at that rate. A name-to-number lookup is a separately quoted step.

## Verification - 8 September 2026

Workspace unit/contract tests passed. PDF tests cover nesting, missing sections, zero/negative values, 150 fields, long values and signature scope/expiry. SQLx/HTTP integration downloads twice, rejects altered ownership and expired task retention, and asserts unchanged provider-call counts. All 52 focused MCP and 16 App tests passed. App production build and Gateway production-target Clippy passed. Full all-target Clippy is blocked by an existing type_complexity warning in a parser test.

Production price readback verified EUR 0.10 and unchanged other KVK prices. A live stored result returned a valid PDF with HTTP 200; a tampered signature returned 403. The live App displays the download button on the completed task. No new paid KVK call was made for verification.
