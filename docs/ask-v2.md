# Ask v2 capability search

The App's `/ask-v2` page is independent of the existing Ask task and payment flow.
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

The fixed prompt in `contracts/ask-v2-webgpu-prompt.txt` is the prototype's
`parsePrompt([])` output. Known slugs and the original correction directive are
the only additions. For the 4B model, `ask-v2-webgpu-schema.json` enforces that
same output shape through OpenRouter structured output. This transport difference
prevents malformed array/object types from causing an unnecessary second request.
The generation remains streaming, temperature zero, and capped at 1,024 tokens.
It has a twelve-second total server deadline and bounded response size.

Both routes require an authenticated App user and apply the existing workspace
membership checks. Separate bounded per-owner limits cover parsing and search,
and the existing concurrent-request semaphore applies. No task, quote, approval,
payment, source execution or answer synthesis is created by these routes. Client
metadata is validated and used only for directory search.

The parser records actual elapsed parse time, model and number of attempts. Provider
speed and semantic extraction quality vary; neither two-second latency nor
complete extraction of every sub-question is guaranteed. Tests cover exact
WebGPU normalization parity, cancellation and stale responses, origin retries,
catalog relevance, stream decoding, bounds, and the isolated route contract.
