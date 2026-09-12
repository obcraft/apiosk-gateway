# Pulse Network is one source

The source browser previously exposed one row per listing. The live catalog contained 101 such rows, including 76 listings owned by pulsenetwork. The source browser now returns 26 sources, with Pulse Network counted once.

The grouping uses the registered provider handle, never a name or suffix match. Independent upstreams distributed by Apiosk keep their own source identities. Grouping happens before pagination. Category, tag, sector, capability and service-name filters still inspect each underlying listing. The parent retains its full service count; matching services are nested beneath it. Exact execution slugs, contracts and result provenance are preserved. A Pulse provider constraint only admits that provider's registered services.

The App and MCP approval summaries also count multiple Pulse services as one source. MCP service details are collapsed by default. Every row in a returned page is rendered; the former eight-row display versus twenty-row pagination mismatch is removed.

## Verification

- Gateway: 166 tests passed, 22 service-dependent tests ignored.
- MCP: 170 tests passed, including source presentation on every response for hosts with cached tool metadata.
- App: 38 targeted tests and production build passed.
- MCP-to-Gateway integration passed.
- A regression with 87 Pulse services checks one parent source, complete pagination, filter/search behavior, aggregate capabilities and preservation of unrelated upstream identities.
- Live installed ChatGPT connector: the unfiltered source overview displayed 26 matching sources and one Pulse Network row, with 76 services collapsed beneath it.
- Live TaxPulse search: one matching source, Pulse Network, with one matching nested service. Both calls were free and bought nothing.
- ChatGPT used its previously cached v27 resource identifier; the backwards-compatible resource route successfully delivered the updated card. Newly advertised tools reference v35.
- A plain request to show all available sources displayed one card and a short confirmation, without a second table or source list. The source tool description, response and widget metadata share that presentation contract. Text-only hosts and explicit text-table/export requests retain their fallback. Evidence is in `Production/evidence/2026-09-12/source-single-view/`.

## Release evidence

Gateway v59: deployment-01M2BJH14ET45541FRCYSYFCFG.
App Netlify: 6aa5ac9b1250a0492ebc6ff6; index-GB6tLPCv.js verified against the live origin.
MCP v297: deployment-01M2BJVVV2QFFCPAQXPVC7BT3E (card v34).
Screenshots, accessibility evidence, tests and deployment logs are in that directory. Live verification chat: https://chatgpt.com/c/6aa5acf3-a6e0-83eb-a3e1-6fbd13033368.

These initial deployments were made from the working trees. The subsequent commit/deploy verification is recorded in `Production/evidence/2026-09-12/committed-release/` alongside repository commit IDs and deployment results.
