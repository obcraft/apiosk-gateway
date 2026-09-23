# Apiosk API

One API for machine-to-machine data purchases. Your server authenticates with an
organisation API key, discovers sources, and either calls one source directly or
asks a question and lets Apiosk plan across sources. Every paid call is charged
to your organisation's main balance, within the key's limits.

Base URL: `https://gateway.apiosk.com`

Direct source calls and Ask are two interfaces over the same thing. Both use the
same source registry, planner, executor and ledger as the Apiosk app. There are
no API-only sources and no API-only prices.

1. [Authentication](#1-authentication)
2. [Quickstart](#2-quickstart)
3. [List sources](#3-list-sources)
4. [Call a source](#4-call-a-source)
5. [Ask Apiosk](#5-ask-apiosk)
6. [Run a plan](#6-run-a-plan)
7. [Balance and billing](#7-balance-and-billing)
8. [Errors](#8-errors)
9. [Idempotency](#9-idempotency)
10. [Limits](#10-limits)

## 1. Authentication

An organisation owner or admin creates keys in **Apiosk > Organisation > API keys**.
Each key has:

- a name, for example `Nomi Production`;
- an expiry date;
- a per-request limit and a monthly limit;
- a set of permissions (scopes).

The secret is shown **once**, when you create the key. Apiosk stores only its SHA-256 hash.

```http
Authorization: Bearer apk_org_live_…
```

| Scope | Allows |
|---|---|
| `sources:read` | `GET /v2/sources`, `GET /v2/sources/{id}` |
| `sources:execute` | `POST /v1/execute/{source}/{operation}`, `POST /v2/sources/{id}/run` |
| `ask:create` | `POST /v2/ask` with `execution_mode: "plan"` |
| `ask:execute` | `POST /v2/asks/{id}/run`, and `POST /v2/ask` with `execution_mode: "auto"` |
| `billing:read` | `GET /v2/balance`, `GET /v2/usage` |

A key belongs to the organisation, not to the person who created it. Its money
is the organisation's main balance. All keys of one organisation spend from that
same balance, each within its own limits. Revoking a key stops it immediately,
including runs it has not started yet.

## 2. Quickstart

One call pays for one source. The path names the source and the operation; the
body is the input values themselves.

```bash
curl https://gateway.apiosk.com/v1/execute/kvk/company_registry.search \
  -H "Authorization: Bearer $APIOSK_KEY" \
  -H "Idempotency-Key: nomi-request-983472" \
  -H "Content-Type: application/json" \
  -d '{"company.name":"Mollie B.V."}'
```

It answers when the source does, with the result, what it cost and the balance
left. Operation ids and input fields come from `GET /v2/sources/{id}`; the ones
above are examples.

## 3. List sources

```http
GET /v2/sources?search=tenders&category=…&tag=…&sector=…&capability=company.search&offset=0&limit=20
```

This returns the same catalogue the app and MCP show, grouped by source and paginated.

```http
GET /v2/sources/{source_id}
```

This returns one source with its operations. For each operation you get its
inputs (field, whether it is required, and a JSON schema), the facts it produces,
and its price per call.

```json
{
  "id": "kvk",
  "name": "KVK",
  "operations": [
    {
      "id": "company_registry.search",
      "input": [{ "field": "company.name", "required": true, "schema": { "type": "string" } }],
      "output": ["company_registry.kvknummer"],
      "pricing": { "model": "per_call", "price": { "amount": 0.023, "amount_micro_usd": "23000", "currency": "USD" } }
    }
  ]
}
```

## 4. Call a source

### The short call

```http
POST /v1/execute/{source}/{operation}
POST /v2/execute/{source}/{operation}
```

The body is the operation's inputs and nothing else:

```json
{ "company.name": "Mollie B.V." }
```

Everything optional is a query parameter: `?max_spend=1.00&wait_seconds=25&limit=5`.

- **The path is lenient.** A source by its id (`kvk`) or its name; an operation
  by its capability (`company_registry.search`), its endpoint id, or its name.
- **Inputs are lenient too.** Use the fact type (`company.name`), the name the
  source's own API uses (`naam`), or the unambiguous last word (`name`).
- **One source document comes back as `data`**, beside the full `results`.

### The explicit call

```http
POST /v2/sources/{source_id}/run
```

```json
{ "operation": "company_registry.search", "input": { "company.name": "Mollie B.V." }, "max_spend": 1.00, "wait_seconds": 25 }
```

Same engine, same price, same response; the envelope is spelled out. Use it
when a client generates requests from the catalogue.

### Both forms

- **No model interprets anything.** You name the source, the operation and the inputs.
- **Inputs are checked before anything is priced.** Unknown fields, missing
  required fields and values that do not match the schema return
  `422 invalid_source_input`. Nothing is charged.
- **A direct call buys only the named source.** If the operation needs an input
  that only another source can supply, you get `422`. Supply that input, or use
  Ask.
- **`max_spend` is optional.** Your key's per-request and monthly limits always apply.

The response has the same shape as any run; see [Run a plan](#6-run-a-plan).

## 5. Ask Apiosk

```http
POST /v2/ask
```

```json
{ "question": "Find the five strongest recent buying-intent signals for Dutch cybersecurity companies", "execution_mode": "plan", "limit": 5 }
```

Apiosk interprets the question, matches sources and operations, and prices a
plan. The plan uses the same registry as direct calls.

- **`execution_mode: "plan"`** (the default) returns the plan with
  `status: "requires_approval"`. Run it with `POST /v2/asks/{id}/run`.
- **`execution_mode: "auto"`** requires `max_spend`. Apiosk runs the plan
  immediately, but only if the plan's maximum cost is within `max_spend`, the
  key's limits and the balance. If the maximum cost is above `max_spend`, you get
  the plan back with `status: "requires_approval"` and an
  `approval_required_reason`. Nothing is charged.

```json
{
  "id": "5c1f…",
  "status": "requires_approval",
  "plan": {
    "quote_ref": "…",
    "expires_at": "2026-09-22T18:40:00Z",
    "steps": [
      { "source_id": "tenderned", "operation": "tender.search", "estimated_cost": { "amount": 0.18, "amount_micro_usd": "180000", "currency": "USD" } },
      { "source_id": "kvk", "operation": "company_registry.profile", "estimated_cost": { "amount": 0.21, "amount_micro_usd": "210000", "currency": "USD" } }
    ],
    "estimated_total": { "amount": 0.39, "amount_micro_usd": "390000", "currency": "USD" }
  }
}
```

Other statuses:

- **`needs_input`**: the question lacks something. `needs_input` lists what, with a schema;
  when the planner asks a clarifying question instead, `message` holds it.
- **`needs_selection`**: several entities matched. `candidates` lists them. Ask
  again with a precise identifier, or call the source directly.
- **`unsupported`**: no source can answer the question.

## 6. Run a plan

```http
POST /v2/asks/{id}/run
```

```json
{ "max_spend": 1.00, "limit": 5, "wait_seconds": 25 }
```

This approves the plan's maximum cost against the key's limits and the balance,
then runs it. Running an ask again returns the same run. It never starts a second
purchase.

```http
GET /v2/runs/{id}?wait_seconds=10
```

`GET /v2/asks/{id}` is the same endpoint. A run's id is its ask's id. Only the
key that created a run can read it.

Every run, direct or Ask, has one shape:

```json
{
  "id": "5c1f…",
  "status": "completed",
  "question": "…",
  "plan": { "steps": [ … ], "estimated_total": { … } },
  "answer": { "status": "completed", "observations": [ … ], "limitations": [ … ] },
  "results": [ { "result_ref": "…", "source": { … }, "data": { … } } ],
  "usage": {
    "sources": [
      { "source_id": "tenderned", "operation": "tender.search", "calls": 1, "cost": { "amount": 0.18, "amount_micro_usd": "180000", "currency": "USD" } },
      { "source_id": "kvk", "operation": "company_registry.profile", "calls": 3, "cost": { "amount": 0.21, "amount_micro_usd": "210000", "currency": "USD" } }
    ],
    "cost": { "amount": 0.39, "amount_micro_usd": "390000", "currency": "USD" },
    "held": { "amount": 0, "amount_micro_usd": "0", "currency": "USD" }
  },
  "balance": { "remaining": { "amount": 47.82, "amount_micro_usd": "47820000", "currency": "USD" } },
  "errors": []
}
```

- **`status`** is one of `requires_approval`, `running` (HTTP `202`),
  `completed`, `partial`, `needs_input`, `needs_selection`, `unsupported`,
  `cancelled`, or `failed` (HTTP `502`, with an `error` object).
- **`usage.cost`** is what the ledger charged, and it equals the sum of the per-source costs.
- **`usage.held`** is money reserved for calls that are not settled yet.
- **Nothing extra is charged.** The Apiosk fee is included in each source's price.
- **`limit`** caps how many result documents are returned.
- **`data`** is present when a run produced exactly one source document, which
  is what a direct call normally is.

## 7. Balance and billing

Amounts are in USD, the currency the ledger charges in. Each amount carries both
a decimal and the exact integer in micro-USD.

```http
GET /v2/balance
```

```json
{
  "organisation_id": "…",
  "balance": { "available": { "amount": 50.0, "amount_micro_usd": "50000000", "currency": "USD" } },
  "key": { "id": "…", "name": "Nomi Production", "scopes": ["…"], "expires_at": "2027-09-22T…" },
  "limits": { "per_request": { … }, "monthly": { … }, "month_spent": { … }, "month_remaining": { … }, "month_start": "2026-09-01T00:00:00Z" }
}
```

```http
GET /v2/usage?limit=50
```

This returns this key's spending this month and its recent charges. Each charge
has its status, cost, source, operation and run id.

Money moves in this order:

1. The plan's maximum cost is approved.
2. Each call is reserved on the balance before the provider is called.
3. The call executes.
4. The actual cost is settled and the remainder is released.

A failed call is refunded. A call whose outcome is unknown stays held until it is
reconciled; it is never charged twice.

## 8. Errors

```json
{ "error": { "code": "insufficient_balance", "message": "…", "required": { … }, "available": { … } } }
```

| HTTP | Code | Meaning |
|---|---|---|
| 400 | `invalid_request` | Malformed body, `max_spend`, `limit` or `Idempotency-Key` |
| 400 | `invalid_workspace` | Organisation keys do not take `x-apiosk-workspace` |
| 401 | `invalid_api_key` | Missing or unknown key |
| 401 | `api_key_expired` / `api_key_revoked` | Create a new key |
| 402 | `insufficient_balance` | Top up the organisation's main balance |
| 402 | `spend_limit_exceeded` | `limit` is `per_request`, `monthly` or `max_spend` |
| 403 | `insufficient_scope` | `required_scope` names the missing permission |
| 403 | `organisation_unavailable` | The organisation cannot spend from its main balance right now |
| 403 | `public_api_required` | App/MCP protocol routes do not accept organisation keys |
| 404 | `source_not_found` / `operation_not_found` / `run_not_found` | |
| 409 | `idempotency_conflict` | The same `Idempotency-Key` with a different body |
| 409 | `request_in_progress` | The first request with this key is still running |
| 409 | `plan_not_ready` / `plan_expired` / `plan_changed` | Read the status or ask again |
| 422 | `invalid_source_input` | `field`, `expected`, `missing` or `schema` say what to fix |
| 422 | `unsupported` | No source can answer this question |
| 429 | `rate_limit_exceeded` | Wait and retry with the same `Idempotency-Key` |
| 502 | `source_error` | The run failed; see `errors` and `usage` |
| 503 | `source_unavailable` / `service_unavailable` / `approval_unconfirmed` | Retry, or read the run's status |
| 504 | `source_timeout` | |

Every refusal before a purchase says **"Nothing was charged."**

## 9. Idempotency

Send `Idempotency-Key` (1–255 visible ASCII characters) on `POST /v2/ask`,
`POST /v1/execute/{source}/{operation}` and `POST /v2/sources/{id}/run`.

- **A retry with the same key and body** returns the same ask or run, and pays at most once.
- **The same key with a different body** returns `409 idempotency_conflict`.
- **Keys are scoped** to your API key and the endpoint.
- **Without the header**, every call is a new request.

`POST /v2/asks/{id}/run` is idempotent by itself: an ask runs once.

## 10. Limits

- **Per-request limit.** A plan's maximum cost must fit within it.
- **Monthly limit.** Covers everything the key spent this calendar month (UTC).
  Reservations count until they are refunded.
- **Balance.** The organisation's main balance is shared by all its keys.
- **Checked twice.** Limits are checked when a plan is approved and again, under
  a lock, when each call is reserved. Twenty parallel requests cannot overspend.
- **Rate.** 60 requests per minute per organisation, and a bounded number of
  requests running at once.
- **Keys.** At most 25 active keys per organisation. Expiry is between 1 and 730 days.
