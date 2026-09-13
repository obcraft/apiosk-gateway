# Source integration audit — 13 September 2026

The root problem is incomplete onboarding: imported endpoint descriptions and prices were published before executable input/output contracts were built. This is not evidence that the upstream APIs are broken. A separate class of endpoints performs writes, session management or asynchronous jobs; the current read-only planner cannot execute those as ordinary data requests.

## Applied and verified

- CityFALCON: 0 → 4 of 4 registered endpoints. Actual authenticated upstream responses for AAPL_US; full-response extraction and all four natural-language model/planner routes pass. News, sentiment, insider transactions and investor-relations documents. Recent provider window, ticker-scoped, no exhaustive-history claim.
- Linkup: 0 → 2 of 2. Sourced search and page fetch, with real gateway delivery and confirmed x402 receipts.
- Browserbase: 0 → 2 of 6. Fixed missing treasury resell configuration and request schemas; search and fetch delivered data with confirmed x402 receipts. Browser session lifecycle operations remain unsupported by the read-only planner.
- StableEnrich: 3 → 4 of 36. Exa web search now has a validated execution contract following paid gateway delivery. The other 32 remain unintegrated; they are not claimed fixed.

## Remaining unavailable sources

- AgentBodega: the tested GitHub status endpoint returned HTTP 200 after payment, but the gateway did not receive a recognized payment receipt. A USDC transfer and used authorization were found on-chain. The exact AuthorizationUsed nonce and $0.005 USDC Transfer were verified on-chain and the treasury payment record reconciled to paid. The provider data was not recovered; no retry or fabricated data success. No execution contracts enabled.
- CrowdPull: its paid source-catalog operation successfully returned and settled. That does not validate its scraping/data workflows, which remain unintegrated.
- PixelForge: the configured temporary Cloudflare hostname does not resolve. A current provider URL is needed.
- EODHD: resale authorization is not recorded; its existing purchase restriction remains in force.

## Current registered operation coverage

Pulse Network is counted once. Counts below measure accepted execution mappings, not provider uptime, live validation of every operation or data accuracy.

| Source | Connected operations | Registered operations |
|---|---:|---:|
| AgentBodega | 0 | 71 |
| Browserbase | 2 | 6 |
| CBS Statistics | 11 | 17 |
| CityFALCON Financial API | 4 | 4 |
| Companies House | 5 | 15 |
| CrowdPull | 0 | 28 |
| Dutch Open Data Register | 3 | 3 |
| EODHD Market Data | 0 | 60 |
| Eurostat | 2 | 10 |
| Exa | 2 | 2 |
| GLEIF | 5 | 17 |
| KOOP Official Publications | 1 | 1 |
| KVK Business Register | 5 | 5 |
| Linkup | 2 | 2 |
| OpenSanctions | 4 | 7 |
| Overheid.io Public Data | 10 | 10 |
| PDOK Maps & Addresses | 19 | 27 |
| PixelForge x402 Data Services | 0 | 5 |
| Pulse Network | 34 | 980 |
| RDW Vehicle Technical Data | 1 | 1 |
| RVO Subsidy Descriptions — First Page | 1 | 1 |
| Sirenic | 63 | 63 |
| StableEnrich | 4 | 36 |
| TED European Procurement Notices | 1 | 1 |
| Tavily | 1 | 1 |
| TenderNed | 1 | 3 |
| WebberSites x402 Data API | 66 | 76 |

## Evidence and test scope

- `readiness-before.json`, `readiness-after.json`: public runtime audit.
- `catalog.json`: credential-free endpoint metadata snapshot.
- `evidence/2026-09-13/catalog-repair/cityfalcon/`: four direct authenticated provider response captures; provider subscription usage only, no Apiosk customer debit.
- `procurement/`: dry runs and one persisted paid attempt per operation. Two explicitly capped $0.03 provider batches; no prior attempt is resent.
- `all-repairs-production-check.log`: actual production catalog, nine real natural-language model selections, planner resolution and input binding; no additional provider purchases.
- `gateway-tests-final.log`: workspace regression suite.
- `chatgpt-sources-after.txt` and `.png`: live ChatGPT card now lists 23 executable sources, including CityFALCON, Linkup and Browserbase; Pulse Network remains one source.
- `inference-audit.json`: offline proposal audit with the older inference validator. Its legacy adapter/input rejections are NOT current runtime failures and were NOT automatically promoted. Current runtime acceptance is authoritative.

This release adds nine execution contracts. It does not complete all remaining imported endpoints. Further paid integration tests require a separately bounded test budget; price metadata alone is not authorization to buy every endpoint.
