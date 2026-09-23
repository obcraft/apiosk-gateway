# Published source and endpoint coverage, 2026-09-23

The read-only audit in [`2026-09-23-source-coverage.json`](2026-09-23-source-coverage.json) covers every active, verified, discoverable listing and active endpoint in production. Run `node --env-file=.env scripts/audit-catalog-coverage.mjs <output-directory>` to refresh it. The raw snapshot stays outside Git because it includes provider request and response examples.

| Scope | Count |
| --- | ---: |
| Grouped sources | 37 |
| Services / API listings | 112 |
| Active endpoints | 1,476 |
| Registered but inactive endpoints | 48 |
| Endpoints with active validated read contracts | 271 |
| Endpoints with staged validated contracts, excluding active overlap | 602 |
| Endpoints without either contract | 603 |
| Endpoints without a provider response example | 1,059 |
| Endpoints without a capability offer or contract | 106 |

Pulse Network is one source with 76 services and 980 endpoints. FilingPulse is one of those services: it has 15 endpoints, zero active planner contracts, and four staged contracts. The staged contracts use synthetic document-adapter samples and do not establish a real response mapping. EODHD's 60 endpoints are explicitly blocked from resale. Do not activate either group by changing only a status column.

The guarded contract generator classifies the remaining 1,205 active endpoints as 602 document-adapter candidates and 603 requiring other review. The main blockers overlap: 440 need required-input alternatives reviewed, 106 lack a reviewed capability offer, 60 are source-blocked, 60 need effect review, 42 lack an input schema, and 35 have unsupported required inputs. One paid call for each of the 1,205 at listed prices would total about $204 before retries or fees; no such calls were made for this audit.

Production `catalog.endpoint_contract_coverage` contains one row per published endpoint with source, service, category, tags, use cases, offered and contracted capabilities, response evidence, block reason, and contract state. The gateway's accepted runtime catalog remains the authority for what can be selected and quoted. The Ask endpoint picker pages through the complete inventory and disables endpoints outside that catalog.
