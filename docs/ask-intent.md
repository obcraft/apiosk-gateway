# Rebuilt Ask v2: intake only

The App's `/ask-v2` page posts `{ "question": "..." }` to the authenticated
`POST /v2/ask-v2/intent` endpoint. This is independent of the existing Ask
capability parser, catalog search, planner, tasks, quotes and execution.

One OpenRouter request uses `qwen/qwen3-30b-a3b-instruct-2507`, temperature 0,
JSON object mode, latency routing and a 15-second deadline. The model has no
catalog context and cannot select APIs. There is no correction call or delayed
rate-limit replay. The OpenRouter credential stays on the gateway.

The response is `{ schema_version: 1, intent, model, timing: { parse_ms } }`.
`intent` contains:

- `language`: language of the question.
- `subjects`: stable IDs, literal labels, types and supplied identifiers.
- `requests`: requested data/actions and their subject IDs, required and
  explicitly optional fields, literal selection filters and named sources.
- `output`: desired format and transformations such as comparing or ranking.

The extractor preserves source names only when the user supplies them. Empty
`sources` allows a future planner to choose fitting sources. A broad question
remains broad; it does not receive an invented mandatory field checklist.
Separate requirements for different subjects remain separate request instances.

The Rust types and validation in `src/ask_v2/intent.rs` define the versioned
contract. Unknown fields, dangling references, duplicated fields and nonliteral
identifiers/filters/source names are rejected. Validation does not prove full
semantic fidelity: the original question must remain available when reviewing
or compiling an intent. An intent grants no execution or payment authority.

The page shows the actual validated JSON and client elapsed time. Editing the
question clears the old result and cancels its request. Switching account or
workspace resets the page. Nothing is saved as a task or executed in this step.
