# Financing demo source audit — 22 September 2026

Scope: ASML Holding N.V., source-derived registered address, company and site research.
This is a capability audit, not a completed ASML diligence assessment.

| Source | Relevant evidence | Verified integration boundary |
| --- | --- | --- |
| KVK | Legal entity, filed accounts, main visiting address | Existing executable contracts; automatic name resolution |
| PDOK / BAG / cadastral map | Address match, building and parcel context, PNG maps | Existing address contract; maps captured and saved by the report pipeline |
| Bodemloket | Soil-information coverage at the resolved point | Executable feature-info contract; default layer Beschikbaarheid_gegevens; not a complete soil report |
| PDOK spatial planning | Spatial-plan layer at the resolved point | Executable feature-info contract; default layer enkelbestemming; not proof of current permission |
| Sirenic | Sanctions search; European entity search | 66 active contracts in current catalog. ASML sanctions lookup is supported. Country-specific financial/director routes must not be assumed to cover NL. |
| StableEnrich | Company enrichment, domain resolution, Exa web search | Five active contracts. Solar imagery, Exa content extraction and Firecrawl scraping are documented endpoints but are not currently active planner contracts. |
| Tavily | Search for official publications, public news and document links | Existing web.search contract. A result/snippet is not independently verified legal title. |
| Pulse Network / FilingsPulse | Filing search, exchange research, financial-series candidates | Official documentation inspected; endpoints listed but zero active FilingsPulse execution contracts. Do not promise these in the current live quote. Narrative endpoints use Tavily plus Claude synthesis, not an independent primary register. |
| Kadaster ownership information / deeds | Authoritative title-related documents | Official product exists. No executable title-order contract found in the current catalog. Report the connection/order gap rather than claiming land registers do not publish title information. |

Provider documentation reviewed:
- https://api.sirenic.eu/openapi.json
- https://stableenrich.dev/openapi.json
- https://docs.tavily.com/documentation/api-reference/endpoint/search
- https://filingspulse.theaslangroupllc.com/llms.txt
- https://www.kadaster.nl/zakelijk/producten/eigendom/eigendomsinformatie
- https://www.kadaster.nl/producten/akte-en-onderzoek/eigendomsakte-opvragen

Recommended demo question (registration and location, replacing financing/ownership checks):
> Create a company and location report on ASML Holding N.V. Check its registration, sanctions and website asml.com. Find its registered address and include building and parcel maps, available soil and planning information, and the latest five relevant news articles. Find the identifiers yourself and give a short sourced conclusion.

Finances, filed accounts, corporate ownership and property title are outside this revised demo question. Public maps and register context must not be presented as substitutes proving ownership or creditworthiness.

News is included when requested: retain publication date, publisher, link and content scope; do not treat search snippets as full articles. Web research also locates official company publications and supporting documents.

Read-only rehearsal with the production catalog, locally staged address contracts and configured model returned ready plans of nine to ten source calls. The final route inspection uses KVK for the profile required by the Dutch address goal, without adding an unrelated French profile. This is planning evidence, not purchased or successful source results. The actual quote must show the selected services and ceiling before the normal single approval.

Do not count KVK search/profile/filings as three independent providers or PDOK layers as unrelated registers. Preserve one source row per used source in the UI. Preserve the original underlying source URLs in the dossier and evidence manifest. Never count Tavily cited through a Pulse synthesis as independent corroboration from both providers.

## Browser fallback in new report quotes

A company report can reserve one conditional Browserbase search per company, within
the existing twelve-step plan limit. It is part of the original quoted ceiling and
uses the same markup, approval, expiry, payment journal and recovery rules. It waits
for its eligible checks, then searches only if a source failed, was incomplete, or
returned a recognized empty record collection. Successful zero-match sanctions
screening does not trigger a search. Unused reserves are shown as skipped and incur
no source charge. Explicit source-only requests are not silently substituted.

The query targets the failed checks using the company name and quoted search scopes.
Browser results are supplementary evidence, never a replacement for authoritative
VAT validation, sanctions screening, filing status or property title. One contingency
search is bounded, not an unlimited crawl or a guarantee that unavailable records exist.
Old approved plans and saved reports do not acquire new purchases automatically.

During new report execution, source-linked allowlisted official pages can be saved
with their text and table rows, retrieval time and hash. At most two HTML pages and
two PDFs are captured per result (up to eight MiB per PDF). Only links actually observed in the source response
or captured page are followed; no redirects, guessed paths or export-time downloads.
The evidence ZIP preserves captured HTML alongside original PDFs and map images.

Research analysis receives all source results belonging to the approved report plan,
including discovery dependencies and conditional fallback results. A dependency is
not excluded merely because the question did not name its capability. The analysis
still has to cite actual evidence; failed/empty results are not promoted to findings.

Verification: isolated report flow with empty filing response, one approval, one
Browserbase fallback, idempotent replay and a combined cited PDF. Read-only production
catalog/model rehearsal returned an eight-step ASML quote including the contingency.
Free official-page capture retrieved ASML's shares page and the linked 2025 financial
performance and corporate governance PDFs. This is not a new paid ASML assessment.
