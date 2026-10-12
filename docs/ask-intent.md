# Ask v2: local intent and capability intake

`/ask-v2` sends `{ "question": "..." }` to authenticated
`POST /v2/ask-v2/intent`. The local launcher enables the same workspace
membership checks as production; the selected workspace is not bypassed.

The gateway reads the existing `catalog.capabilities` taxonomy (active and
draft definitions). This is independent of source offers, execution contracts,
the legacy planner, tasks, quotes and execution. A compact catalog includes
all reusable definitions, local drafts, and up to 32 source-document entries
ranked by literal question overlap. This lexical shortlist can miss a
source-specific equivalent, especially across languages; it is not proof that
no equivalent exists anywhere in the catalog.

The local page offers GPT-OSS 120B (default), GPT-6 Luna, Ministral 3B,
8B and 14B, and Mistral Small 3.2 24B. The request may include `model`; the
gateway accepts only those six identifiers. All use strict JSON Schema and
latency routing. GPT-6 Luna disables reasoning; GPT-OSS uses low reasoning;
Mistral models omit reasoning parameters. Unsupported settings are not silently
ignored. The strict transport uses closed key/value entries for dictionaries,
which the server restores to the existing `identifiers` and `filters` objects.
Duplicate keys are rejected and literal-value validation still applies.

A malformed response can receive one structural correction within the same
15-second deadline. Transport errors and rate limits are not replayed. The model
credential stays on the gateway. OpenRouter's reported usage is collected from
the stream. The response's `usage` contains `cost_usd`, `input_tokens` and
`output_tokens`, including both attempts when a correction is needed. Missing
provider cost remains null; the UI never labels it as free. The page shows this
total beneath the input, and advertised token rates before a conversion.

The response is `{ schema_version: 1, intent, model, timing: { parse_ms }, attempts }`.
Every intent has the same keys:

- `language`: original input language, or `und` when indeterminate.
- `subjects`: request-local IDs, literal labels/types and supplied identifiers.
- `capabilities`: canonical definitions with `slug`, `name`, `description`,
  `category`, `status` (`active`/`draft`) and `origin` (`database`/`local`).
- `requests`: each has `id`, `capability_slug`, `subject_ids`, `kind`, `goal`,
  `required_fields`, `optional_fields`, `filters` and `sources`.
- `output`: format and requested transformations.

The model reuses exact existing capability slugs where their meaning matches.
Rust supplies existing slugs, descriptions and status from the database,
ignoring model rewrites. Display names/categories derive from the canonical
slug, respecting the gateway's existing column-level read grants.
If a valid newly classified slug has no definition prose, code derives a
reusable label/description from that slug and stores the draft. This preserves
the model's classification without retrying just to generate optional prose.
A new definition is reusable across subjects; it does not include named
companies/people, dates or IDs. Repeated requests retain their own bindings.
Greetings, writing, calculations and unclear messages use this same contract.

For local testing, new definitions are saved atomically as drafts in
`gateway/.local/ask-intent-capabilities.json`, configured by
`ASK_INTENT_DRAFT_PATH`. They are included in subsequent requests and survive
server restarts. Database definitions take precedence. No production database
writes occur. Without explicitly configured draft storage, a request needing
a new definition reports a storage error instead of claiming it was saved.
Drafts describe intent only: they do not create an API, offer, source contract,
quote, execution or payment permission.

The backend validates JSON, referenced capabilities and subjects, and literal
identifiers/filters/source names. Empty collections and duplicates normalize
consistently. Malformed semantic content is never replaced by a fake successful
parse. Validation cannot guarantee perfect model interpretation or universal
availability; network/authentication/provider failures remain real errors.

The page displays the validated JSON and end-to-end elapsed time. Editing input
cancels the previous request; switching accounts/workspaces resets the page.
