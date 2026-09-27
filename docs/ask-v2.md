# Ask v2 capability search

The App's `/ask-v2` page searches independently of the existing Ask planner.
It ports the current WebGPU project's parser, normalization and validation into
`app/src/lib/domain/ask-v2-parser`. It accepts the current question only.

1. `POST /v2/ask-v2/parse` sends the WebGPU system prompt and question to OpenRouter.
   The server keeps the API key. The default is **Gemma 3 4B**
   (`google/gemma-3-4b-it`), four billion total parameters. This is deliberately
   different from the prototype's 26B A4B default. `ASK_V2_MODEL` controls only
   this route; it does not change the existing planner.
2. The App applies the original WebGPU validation and normalization. It permits
   the original one correction attempt for invalid model output and adds the
   model, parse timestamp and SHA-256 metadata.
3. The App retains the validated object and sends independent
   `/v2/ask-v2/search` requests for `apiosk` and `coinbase`. Results appear directly
   in a table with source logo, name, short description and price. Parsing
   metadata is not part of the main result view.
4. Apiosk searches the full catalog locally, ranking metadata per capability.
   Admitted operations retain their contract identifiers; directory-only sources
   are explicitly marked `discovery_only`. The catalog index retains one version.
5. Coinbase searches the fixed public Bazaar endpoint with generic capability
   queries, scrubbed of parsed subject labels and identifiers. Its matches appear
   after Apiosk in the UI. Only successful discovery metadata is cached, with a
   bounded five-minute cache. Failure does not discard the parsed object or the
   other origin's results. Retrying a source search does not rerun the model.
   An explicitly named source also filters Bazaar candidates by their metadata.

The fixed prompt in `contracts/ask-v2-webgpu-prompt.txt` is the prototype's
`parsePrompt([])` output. Known slugs and the original correction directive are
the only additions. For the 4B model, `ask-v2-webgpu-schema.json` enforces that
same output shape through OpenRouter structured output. This transport difference
prevents malformed array/object types from causing an unnecessary second request.
The schema is sent verbatim, retaining the prototype's field order. Serializing
it through an alphabetically sorted map changed the 4B model's behavior and
roughly doubled generation time in the same-question comparison.
The generation remains streaming, temperature zero, and capped at 1,024 tokens.
It has a twelve-second total server deadline and bounded response size.

Both routes require an authenticated App user and apply the existing workspace
membership checks. Separate bounded per-owner limits cover parsing and search,
and the existing concurrent-request semaphore applies. No task, quote, approval,
payment, source execution or answer synthesis is created by these routes. Client
metadata is validated and used only for directory search.

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
ten-minute, owner/workspace-scoped quote under the immutable request UUID.
`/external/:id/authorize` accepts the displayed quote and cap, atomically records
one approval and selection, and starts one existing `use-selection` execution.
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
complete extraction of every sub-question is guaranteed. Tests cover exact
WebGPU normalization parity, cancellation and stale responses, origin retries,
catalog relevance, stream decoding, bounds, and the isolated route contract.
