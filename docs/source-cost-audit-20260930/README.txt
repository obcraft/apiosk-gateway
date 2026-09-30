Apiosk source cost inventory — 2026-09-30

Local report: http://127.0.0.1:8769/?view=1
Complete readable inventory: volledig-overzicht.txt
Machine-readable inventory: inventory.json
Only confirmed zero-upstream-cost scopes: free-operations.json
Operation-scoped research decisions and citations: assessments.json
Preserved read-only production response and exact SQL: live-catalog.json
Validation evidence: validation.json

Scope: 37 current grouped sources / 112 service listings / 1,524 endpoint records;
71 additional source/product concepts; 108 distinct records in total.
The 42 researched countries contain 57 national source/product records, not 42 APIs.
43 source records have an explicitly bounded zero-cost scope: 18 existing + 25 concept.
32 are commercial/no guaranteed permanent free production entitlement; 30 unknown;
2 are reference-only; Bodemloket has public access but unconfirmed commercial reuse.

KVK Search has zero marginal upstream cost in this comparison. The real EUR 6.40
monthly connection fee is deliberately ignored at the user's explicit request.
This exception does not change provider billing or Apiosk customer prices.
Do not infer upstream cost from the catalog_price field: it is a customer price.
Infrastructure, AI, storage, indexing and maintenance costs are outside this scope.
Concept operations are specifications and are not newly activated routes.
Enabled endpoints and active contracts do not constitute fresh execution proof.
No paid source execution or production write was performed for this inventory.

Reproduction: build_inventory.py derives the inventory from preserved local files.
It regenerates report/src/data.json with buildStatus=creating. Complete the local
report verification and set buildStatus=complete before the final report build.
Serve the complete report/dist directory because its JSON sidecar is required.
