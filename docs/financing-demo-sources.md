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

Recommended concise demo question:
> Assess ASML Holding N.V. for financing: its finances, ownership, sanctions and property risks. Find its address and provide a sourced report with maps, charts, supporting documents and information gaps.

News is excluded from the demo. Web research remains useful for official company publications and supporting documents.

Read-only rehearsal with the production catalog, locally staged address contracts and configured model returned ready plans of nine to ten source calls. The final route inspection uses KVK for the profile required by the Dutch address goal, without adding an unrelated French profile. This is planning evidence, not purchased or successful source results. The actual quote must show the selected services and ceiling before the normal single approval.

Do not count KVK search/profile/filings as three independent providers or PDOK layers as unrelated registers. Preserve one source row per used source in the UI. Preserve the original underlying source URLs in the dossier and evidence manifest. Never count Tavily cited through a Pulse synthesis as independent corroboration from both providers.
