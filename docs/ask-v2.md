# Ask v2 capability search

The App's `/ask` page and public `POST /v2/ask` share canonical interpretation,
source search, executable descriptors and literal input binding. Neither
creates a paid source request. The public API returns both origins as one JSON
response; the App requests them separately so each table can appear and retry
independently.

1. `POST /v2/ask-v2/parse` sends the fixed prompt and question to the Ask
   model (`ask_v2::DEFAULT_MODEL`, GPT-OSS 120B at low reasoning effort) in one
   request. Normalization validates the parse. Only when that request fails or
   its parse is invalid does one fallback request go to `ask_v2::FALLBACK_MODEL`
   (Qwen3 30B Instruct), with the validation feedback. There is no review,
   repair loop or rate-limit sleep. The route then runs the deterministic
   catalog search itself and returns `parsed_request`, `attempts`,
   `model_calls`, timing and a signed `planning_token` holding that search.
2. `/v2/ask-v2/search` and public Ask both call `search_catalog`. Apiosk matches
   registered contracts and directory metadata; Coinbase uses generic queries
   scrubbed of subject labels and identifiers. The parsed concepts are already
   English, so search performs no request or listing translation model calls.
   Cached catalog translations remain available. Search outages remain visible.
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

The parser streams bounded JSON with a 15-second transport deadline per request
and at most two requests per question. Provider routing prefers throughput, so
the short structured output finishes on the fastest provider that supports
every requested parameter. `ASK_V2_MODEL` documents the pinned model;
deployment overrides are ignored.

The App parse/search routes require an authenticated App user and apply the existing workspace
membership checks. Separate bounded per-owner limits cover parsing and search,
and the existing concurrent-request semaphore applies. No task, quote, approval,
payment, source execution or answer synthesis is created by these routes. Client
metadata is validated and used only for directory search. Public discovery
requires `ask:create`; selected execution requires `sources:execute`.

## Complete requests in the App

### Quick subject introduction

The App parse response also includes best-effort public web context in the
backwards-compatible `company_context` field. It covers up to four named
companies, organizations, people, places, objects or products. The optional
`subject_type` preserves the parsed type. The implementation is
`src/subject_context.rs`; the App uses `subject-context.ts` and
`SubjectWebContext`. Only the saved wire keys `company_context` and `company`
keep their old names so existing clients and chats continue to work. Coolblue
appears in test fixtures, not in the subject-selection or search logic.
Google HTML and Bing RSS searches run
concurrently with a two-second network budget. When both produce no relevant
pages, DuckDuckGo HTML and Wikipedia search/extracts race with a further
two-second budget. JS redirect pages, challenges and unrelated results are
never presented as evidence. A candidate homepage gets up to
1.5 seconds, using the existing bounded, DNS-pinned public-page reader. A
supplied homepage is fetched alongside search. Search snippets survive a failed
homepage read. This network budget is additional to question parsing.

The App shows a short extract from one attributed page above discovery, hides
the empty source table during parsing, and keeps a longer homepage excerpt
expandable. Missing web evidence produces an explicit unavailable message;
source discovery can still continue. Successful context is cached for 15 minutes; failures remain retryable. Context is saved
with the chat, so reopening it does not fetch it again. This adds no model call,
quote, paid source request or charge. Web context does not verify legal identity
or establish the result of requested registry or sanctions checks.

`node scripts/release-subject-context.mjs` requires clean, published main; runs
live free lookups for a company, person, object and supplied homepage; deploys
from this Gateway directory; and verifies the deployed SHA, executor, Fly
checks, authentication, MCP card and approval CORS. It stops on a failed gate.

### Planning and execution

The App waits for source discovery before allowing a new source selection and
automatically prepares the complete data question through `/v2/ask-v2/research`.
A prepared task opens even when the directory also returned alternatives.
The task owns the combined price ceiling, missing inputs and one approval.
After approval, the server starts every independent ready API call concurrently,
up to ten source calls per plan. Source options and manual selection also allow
at most ten. Larger requests must be narrowed or split, never silently truncated.
The same limit is checked again before approval and dispatch, including saved plans.
Ask calls must already have every
required input before approval. Results are saved as each source finishes; recovery reuses pending calls and their action IDs.

Ask plans independent source calls. Exact contract fields are compiled directly,
without a model. Only wording the compiler cannot bind goes through one bounded
semantic planning request against the discovered endpoints' current field
inventory (the same GPT-OSS model, with the same single Qwen fallback). There is no question registry or static output mapping
table. Requested subjects, fields, filters and selected endpoints remain binding.
A failed semantic validation gets a bounded repair; ambiguous or unavailable
scope returns a focused clarification.

Discovery keeps relevant alternatives that can use the supplied inputs, even
when a provider's detail endpoints occupy the initial shortlist. If lexical
matching excludes search, discovery also checks the same provider's admitted
contracts for a ready call that returns a shortlisted endpoint's missing input.
That call must bind the current subject and pass jurisdiction checks. It is an
offered standalone alternative, never an execution dependency. Within a
provider, usable options appear before endpoints with missing inputs. Planning
tries complete alternatives before requesting an identifier and proves output
coverage even for drafts that still need input. A standalone name search can
answer a broad request for available records, but cannot replace explicitly
requested detail fields it does not return or an explicitly selected endpoint.

All selected APIs must have their required inputs before a quote is created.
Missing inputs produce schema-bound input requests; they are never obtained by
adding dependency calls. A ready batch has one total price and one approval.
The worker validates the whole batch before queuing any call, starts every call
concurrently and retains frozen pending inputs and action IDs for recovery.
Explicit workflow recipes retain their separate execution contracts.

When at least two distinct requested retrievals have server-preferred executable
matches from different providers, the App keeps them together: no individual
source picker while preparing, retrying or reopening that request. Alternatives
for one retrieval and multiple endpoints from one provider do not increase this
count. The first supported match per request occurrence is used, respecting the
server's order and subject binding; parser confidence is not a source score.
The backend still validates complete coverage before quoting. A clarification or
unsupported part stays with the full saved request rather than silently dropping
into separate purchases. With insufficient complementary matches, individual
source review remains available. Existing explicit source choices and receipts
retain their saved input and approval flow; reopening never prepares or buys again.

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

## Web research answers

`/v2/ask-v2/web-research` returns a short cited answer, not a report. The
synthesis prompt asks for one to three short paragraphs of about 120 words in
total, leading with the direct answer; only a multi-part question may use one
short paragraph per part. The JSON schema sent with the model call and
`validate_answer` enforce the same ceiling, `MAX_ANSWER_BLOCKS` (6) blocks of
`MAX_BLOCK_CHARS` (600) characters, with `max_tokens` sized for it. The App keeps
its older, wider validation so chats saved before this ceiling still open.
Related paid sources are discovered alongside the web answer and shown under it;
they never gate, delay or change the web answer.

## Optional supporting news

Completed or partial answers with primary findings can include a saved
`analysis.supporting_news` list. These are recent, relevant publisher feed
excerpts for additional context, separate from purchased source evidence,
requirement coverage and supplier verdicts. News never completes a missing
check or replaces an unavailable primary answer. No match means no visible
news section. The App and research PDF show publisher, publication date,
headline, excerpt and a link to the original article.

The fixed public feed allowlist is:

| Publisher | Feed |
| --- | --- |
| BBC News | `https://feeds.bbci.co.uk/news/rss.xml` |
| The Guardian | `https://www.theguardian.com/world/rss` |
| NPR | `https://feeds.npr.org/1001/rss.xml` |
| Al Jazeera | `https://www.aljazeera.com/xml/rss/all.xml` |
| The New York Times | `https://rss.nytimes.com/services/xml/rss/nyt/HomePage.xml` |
| Deutsche Welle | `https://rss.dw.com/rdf/rss-en-all` |
| Ars Technica | `https://feeds.arstechnica.com/arstechnica/index` |
| NOS | `https://feeds.nos.nl/nosnieuwsalgemeen` |

One background worker per Gateway process warms the fixed feeds at startup
and refreshes them every ten minutes with concurrent, bounded requests.
Answer composition reads only the available cache and never waits for feed
requests or locks. A cold or busy cache yields no supplement for that answer;
feed errors cannot delay or block the primary result. This is a current-news
cache, not a historical news archive. Questions, customer identifiers and
source results never go to publishers: matching runs locally. News does not
change parsing, source selection, planning, status, approval, source count or
charges. Reopening an answer and manually recomposing its saved primary
findings reuse the stored selection, including an empty selection.

Selection is conservative: topical overlap beyond a named entity is required;
generic terms, ambiguous names and geographic overlap alone are insufficient.
Only recent dated articles with publisher-owned links are eligible. Feed text
is treated as untrusted plain text. It is never a prompt, an instruction or
proof of an official registry fact, and no full article is fetched.

The result contains at most four articles, with at most two per publisher,
from the past seven days. Explicit shorter relative time windows and known
geographic qualifiers narrow the selection further. Historical-only queries
that the current feeds cannot cover receive no supplemental news. Matching
uses local text normalization and a small set of English/Dutch topic aliases;
it does not add a model call or promise exhaustive semantic news search.

## Standalone public news search

`POST /v2/ask-v2/news` accepts only `{ "question": "..." }`, without authentication.
It handles whole standalone Dutch/English news requests, such as
“Wat wordt er vandaag in het nieuws gezegd over: Sinterklaas?” or
“Latest news about Sinterklaas”. Additional retrievals, unsupported source,
date, location, output or action clauses return `{ "handled": false }` so the
ordinary Ask flow can handle them. Shared classifier cases live in
`contracts/standalone-news-cases.json` and are checked by Gateway and App tests.

This separate route may wait for the same bounded public-feed refresh; its
eight-request semaphore and 8 KiB body limit are independent of paid tasks.
It uses no parser model, catalog, database, billing or task status. Topics may
contain one meaningful term. At most twenty matching articles are returned,
newest first, with publisher, publisher domain, title, excerpt, article URL,
publication timestamp and feed URL. The App uses its fixed publisher logo map.

Handled responses contain `kind: "rss_news"`, the captured `query`,
`time_window`, `articles`, and `coverage`. Status is `ready` for matches,
`empty` for no matches in at least one available successful feed snapshot,
or `unavailable` when no usable snapshot exists. Coverage reports configured
and available feed counts and the latest successful refresh timestamp;
attempted or failed refreshes do not count as available. Last successful
snapshots may be reused for at most seven days, and feed coverage is partial.
“Today” uses the Europe/Amsterdam calendar day, including 23/25-hour DST days.
Other accepted requests cover the past seven days. Future articles are always
excluded. The conservative supplementary scorer and all existing Ask flows
are unchanged by this standalone route.
