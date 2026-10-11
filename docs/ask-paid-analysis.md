# Optional paid analysis in Ask

An App user can choose extended analysis on an unapproved source plan, or request
it for a saved output. `POST /v2/tasks/:id/analysis-upgrade` changes the quote only;
`authorize` still approves the exact displayed total once. Existing source data
is reused for a later upgrade. Connected agents and recurring orders cannot opt
into this App-only feature.

The server-owned registry in `src/compute.rs` offers twelve models, from DeepSeek
V4 Flash through Claude Opus 5.5 and GPT-6 Astra. The cheapest option is the default;
analysis remains off until explicitly selected. Both an unapproved source plan
and a saved output expose the same model picker and inclusive price ceiling.
Changing a model creates a new quote and approval hash. Approved model choices
cannot be changed. Version-1 saved quotes and receipts retain their Opus identity.

Provider ceilings range from $0.015 to $1.73; buyer ceilings include 15% markup.
The exact x402 challenge must fit the selected model's ceiling. The App ledger
records actual provider price plus markup and releases unused reservations.
The provider's prepaid token budget is not itself refunded for unused tokens.
All models use bounded text-only JSON. Standard models use high reasoning and at
most 8,192 output/reasoning tokens. Astra uses a compact prompt, low reasoning,
4,096 output/reasoning tokens and a 7,000-byte request limit. Long text fields keep
a bounded set of exact excerpts; omitted coverage remains explicit. This keeps
the largest buyer maximum at $1.9895, approximately EUR 1.83 at the App rate of
0.92, below the user-requested EUR 2 add-on ceiling. Extra compute does not guarantee a
better answer; missing source information stays unknown. Models are not evidence
sources. The App displays these as maxima, never advertised starting prices.
The provider's conservative byte-based input reserve requires a compact prompt:
indexed evidence is capped at 9,500 UTF-8 bytes, with at most 13,200 bytes for the
entire serialized request, including escaped JSON. Fields use compact tuples;
repeated routing metadata is omitted. Details focus on records
already cited by the included answer, with exact original indices and visible
selection scope; this preserves entity identity and relevant sibling metrics.

The fixed provider/resource, input, action identity, quote hash, owner and
workspace are server-owned. Input and evidence projection are saved before
payment. One action has one payment journal identity. Interrupted, expired or
cancelled tasks recover existing receipts without another provider request.
A settled provider error keeps its real charge visible. Failed evidence
validation preserves the ordinary answer; there is no automatic paid repair.

Activation: apply `allow_ask_analysis_compute`, deploy the gateway (including its
embedded payment executor) and `use-selection` Edge Function, then enable
`ASK_PAID_ANALYSIS_ENABLED=1`. Until that flag is enabled the App offers no new
upgrades. Already bought receipts remain readable even with the flag disabled.

Verification:
- `cargo test --lib compute`
- isolated `tests/compute_upgrade_flow.rs` with `tests/support-db.mjs`
- App `tests/components/ask-v2/ask-v2-task.test.tsx`
- live paid acceptance requires its own explicit spending approval. The earlier
  company-source test budget does not authorize compute purchases.

The offline flow checks old-quote rejection, exact total approval, a compute-only
saved-result quote, one provider call, fixed destination, repeated receipt reads,
owner isolation and recovery after cancellation and authorization expiry.

## Model catalog verification (2026-10-11)

All twelve models were advertised as available with text input, reasoning and
JSON response support by `https://x402engine.app/api/services`. Free `/quote`
requests accepted synthetic maximum payloads with exact request parameters:
13,200 bytes for standard models, and 7,000 bytes for compact Astra. These checks do not establish paid generation quality for each model.
Only Opus has a live paid acceptance result; its original receipt remains valid.

| Model | Free provider quote at maximum input | Buyer maximum incl. fee |
| --- | ---: | ---: |
| DeepSeek V4 Flash | $0.010833 | $0.017250 |
| GLM 5.3 Flash | $0.020730 | $0.028750 |
| MiniMax M3 | $0.042513 | $0.051750 |
| GPT-5.6 Luna | $0.067421 | $0.086250 |
| DeepSeek V4 Pro | $0.093309 | $0.115000 |
| Gemini 3.6 Flash | $0.113442 | $0.143750 |
| Qwen 3.7 Plus | $0.153855 | $0.195500 |
| Qwen 3.7 Max | $0.217328 | $0.264500 |
| Claude Sonnet 5 | $0.474783 | $0.575000 |
| GPT-6.1 Sol | $0.624134 | $0.747500 |
| Claude Opus 5.5 | $0.948136 | $1.150000 |
| GPT-6 Astra (compact) | $1.711058 | $1.989500 |
