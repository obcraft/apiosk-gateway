# Ask v2 capability search

The App's `/ask` page and public `POST /v2/ask` share canonical interpretation,
source search, executable descriptors and literal input binding. Neither
creates a paid source request. The public API returns both origins as one JSON
response; the App requests them separately so each table can appear and retry
independently.

1. `POST /v2/ask-v2/parse` sends the fixed prompt and question to the configured
   Ask model. `Client::interpret` validates and normalizes the response, allows
   one correction, and returns `parsed_request`, `attempts` and elapsed time
   alongside the raw text for older clients. The App consumes that canonical
   object; its old TypeScript normalization is a fallback for older Gateways.
2. `/v2/ask-v2/search` and public Ask both call `search_catalog`. Apiosk matches
   registered contracts and directory metadata; Coinbase uses generic queries
   scrubbed of subject labels and identifiers. Search outages remain visible.
3. Admitted choices carry `api_id`, the exact capability, required input fields,
   schemas, known `input_values`, `missing_inputs`, output contracts and buyer
   prices. External offers are re-resolved against the executor's authoritative
   directory before being marked executable. Unpublished output schemas are
   `null`. Actual output values exist only after execution.
4. Public `POST /v2/execute` accepts the selected `api_id`, capability and input,
   plus an explicit total `max_spend` and required `Idempotency-Key`. It calls
   the same `Engine::prepare_endpoint` or external purchase functions as the
   App. Registered execution uses existing authorization and worker machinery;
   external execution uses the same balance executor with the caller's
   connection scope. `GET /v2/runs/:id` recovers its saved result.
5. The old question-to-plan API is `/v2/ask/plan`. Explicit workflow requests
   retain `/v2/ask`; their saved plan, run and continuation paths are unchanged.

The prompt remains streaming, temperature zero, with bounded output and a
20-second transport deadline per parse attempt. `ASK_V2_MODEL` selects the
configured model; production's default is defined in `parser::MODEL`.

The App parse/search routes require an authenticated App user and apply the existing workspace
membership checks. Separate bounded per-owner limits cover parsing and search,
and the existing concurrent-request semaphore applies. No task, quote, approval,
payment, source execution or answer synthesis is created by these routes. Client
metadata is validated and used only for directory search. Public discovery
requires `ask:create`; selected execution requires `sources:execute`.

## Selected-source purchases

Selecting **Use** opens the source's input contract below the results table. The
model's output and discovery price never authorize execution or determine what
the user pays. Input review and quote preparation make no paid source request.

For registered normal and x402 APIs, `GET /v2/ask-v2/endpoints/:id?capability=...`
reads the current executable input contract. `POST /v2/ask-v2/prepare` accepts
only that endpoint's canonical inputs and saves a quote pinned to its endpoint
and capability. It skips model planning and uses the existing V2 authorization,
worker, internal-balance ledger, result and read-only recovery routes.

For Coinbase Bazaar, `/v2/ask-v2/external/source` resolves the exact resource and
method against the server's settled source directory. It requires a supported
input schema, an eligible payment rail and an authoritative buyer total. The
client cannot supply a price or settlement policy. `/external/prepare` saves a
ten-minute, owner/workspace/connection-scoped quote under the immutable request UUID.
`/external/:id/authorize` accepts the displayed quote and cap, atomically records
one approval and selection, and starts one existing `use-selection` execution for an App user, or
`agent-gateway/v1/run` for a connected API caller.
That executor reserves internal balance, pays x402 from the Apiosk treasury, and
settles or refunds through the same ledger as V1. The migration granting only
`INSERT(resource_url)` on the selection table must ship before this route.

`GET /external/:id` recovers the saved receipt and source output without executing
or repurchasing. Concurrent or repeated approvals cannot dispatch twice. Unknown
payment outcomes remain pending reconciliation. The App retains a receipt per
user/workspace, refreshes balance caches when billing changes, and restores the
purchase through read-only requests after a reload. The browser needs no wallet.

The parser records actual elapsed parse time, model and number of attempts. Provider
speed and semantic extraction quality vary; neither two-second latency nor
complete extraction of every sub-question is guaranteed. Tests cover canonical App/API interpretation and contract parity, legacy
browser normalization, cancellation and stale responses, origin retries,
catalog relevance, stream decoding, bounds, and the isolated route contract.
