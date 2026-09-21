# Postcode question rejected during interpretation

The production Ask question about postcode `7521 BE, Enschede`, local news and government data failed on 21 September 2026. Gateway logs at 15:45:38 and 15:45:42 UTC recorded two invalid chat-model outputs, before any quote or source purchase.

A planning-only reproduction with the production catalogue and model returned `web.query = "7521 BE Enschede"`. Removing the comma caused the strict literal-input check to reject the entire request as invented input. The first request contained 470,686 model-input bytes and 226 catalogue capabilities.

The parser now restores the exact original question span when a model string contains the same contiguous words in the same order with different separators. It does not add words, join separate mentions, reorder words, change goals or infer identifiers. The restored source text still passes through the existing literal and input-schema validation. Both App chat and direct intent parsing use the repair.

Verification before release:

- The captured invalid model response now validates in one model call.
- Regression cases reject new locations, changed postcodes, reordered words, dropped words and invalid input schemas; Unicode source text is retained.
- `cargo test --all-targets` passes.
- The same question with the real configured model and read-only production catalogue produces a plan for PDOK neighbourhood data, KOOP publications and Exa web search. No approval or provider purchase was executed.

Separately reported REST 404s were reproduced for the old public locations of `fx_rates` and `call_runs`; their current billing/actions schema locations return HTTP 200, and the App's existing schema fallback retries them. Workspace preflight currently returns HTTP 204 with the correct App origin. That does not prove the earlier intermittent workspace CORS failure did not happen, but it is distinct from the reproduced parser failure.
