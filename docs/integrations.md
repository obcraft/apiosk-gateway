# App integrations (Composio)

A person connects HubSpot, Salesforce, Slack, Gmail, Outlook or Google Sheets.
Consent happens on Composio's hosted page; tokens stay with Composio. Code:
`src/integrations.rs` (client) and `src/server/integrations.rs` (routes).

## Routes

Any person credential works: an App session, or a connected agent's
`apk_access_…` token. Organisation keys are refused (`integrations_person_only`),
because there is no person behind them to consent.

```http
GET  /v2/integrations
POST /v2/integrations/{toolkit}/connect
POST /v2/integrations/{toolkit}/disconnect
```

`toolkit` is one of `hubspot`, `salesforce`, `slack`, `gmail`, `outlook`,
`googlesheets`; anything else is `404 unknown_integration`.

```json
// GET /v2/integrations
{ "integrations": [ { "toolkit": "hubspot", "name": "HubSpot", "status": "connected", "connected_at": "2026-09-27T10:00:00Z" }, … ] }

// POST /v2/integrations/slack/connect
{ "toolkit": "slack", "redirect_url": "https://…composio…", "expires_at": "…" }

// POST /v2/integrations/slack/disconnect
{ "toolkit": "slack", "removed": 1 }
```

`status` is `connected`, `expired` (reconnect) or `not_connected`.

An agent that calls `connect` shows `redirect_url` to its person. After consent
Composio sends the browser to `{APP_ORIGIN}/integrations?integration={toolkit}`,
which re-reads the list. The callback is never taken from the caller.

Each Apiosk account is Composio user `apiosk_user_{owner_uuid}`. Every list and
delete is filtered on that id by Composio and checked again here.

## Actions in Ask

A connected app's actions are Composio's featured tools for it (at most 12 per
app, cached an hour), in the planner's capability shape:

```http
GET  /v2/integrations/actions                     # any person credential
POST /v2/integrations/actions/{slug}/prepare      # App session: { question, results }
POST /v2/integrations/actions/{slug}/execute      # App session: { arguments }
```

```json
{ "slug": "app.slack.sendMessage", "name": "Send message", "domain": "app", "toolkit": "slack",
  "tool": "SLACK_SEND_MESSAGE", "inputs": ["channel", "markdown_text"], "outputs": […], "input_schema": { … } }
```

- The planner never sees these actions: its prompt, schema and speed are the
  same with or without connected apps. The Ask page reads the question itself
  (`findAppActions` in `app/src/lib/domain/integrations.ts`) for an app named as
  the place of an action ("in Slack", "to my Google Sheet", "a HubSpot contact")
  next to a writing verb, and adds the matching action to the capabilities
  object. Source search still receives the planner's request unchanged.
  Measured on 30 questions × 3 runs with `google/gemma-3-4b-it`: offering the
  actions to the planner added ~19% latency (more output tokens) and dropped
  the data half of 4/18 mixed questions; reading the question does neither.
- Only non-destructive write actions are offered (`is_write_action`): reads
  would compete with source search for data questions, and deletes never run.
- `prepare` asks the model for arguments from the question and the purchased
  results (`contracts/action-arguments-prompt.txt`). Keys outside the schema
  are dropped, and empty required keys are reported in `missing`. Nothing runs.
- `execute` runs only an action found for an app this person has connected,
  never an arbitrary Composio tool, and only from an App session. The person
  sees and can edit every argument before they press Run.

## Configuration

| Env | |
| --- | --- |
| `COMPOSIO_API_KEY` | Project key, `ak_…`. A `ck_…` consumer key is for MCP clients and is rejected by the REST API. Unset → `503 integrations_not_configured`. |
| `COMPOSIO_AUTH_CONFIGS` | Optional `{"salesforce":"ac_…"}` to use your own OAuth app for a toolkit. Otherwise the first enabled Composio-managed auth config is used, or one is created on first connect. |

```sh
fly secrets set COMPOSIO_API_KEY=ak_… -a apiosk-gateway-v2
```
