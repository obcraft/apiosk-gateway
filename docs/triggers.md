# Inbound triggers — Hello world milestone

`/triggers` is a separate authenticated app page. Users choose their own trigger
name and optional Composio connection label. They can select a Slack conversation,
create a private channel, or pick 2–8 teammates to open a group chat. Slack displays
group chats using the members' names. A setup draft is retained for 24 hours in
owner-scoped session storage across OAuth; credentials and event text are never
stored in that draft.

Triggers match every new message by default, or a configurable case-insensitive
prefix. Slack bold formatting and extra whitespace do not affect prefix matching.
The supplier example is illustrative only. The sole action is a **Hello world**
alert in the open Triggers page, with durable activity history. No Ask execution,
paid research, supplier verification, or reply to Slack occurs.

## Enable in an environment

Production status on 2026-10-29: migration `20261029161428_inbound_triggers`
is applied to `apiosk-production` (`jgjoiyqdyypouskftzeq`). Both tables have RLS
enabled and grant access to the gateway runtime only. Gateway release 307 is live
as `c732afb-triggers-bf8d3bdee6`; the frontend was not deployed in this step. The
runtime image is 60.3 MB compressed (registry layer sizes). Composio now
has a V3 subscription for `composio.trigger.message` pointing to
`https://apiosk-gateway-v2.fly.dev/v2/triggers/composio`, with the matching signing
secret deployed on Fly. Live checks passed for health, session enforcement,
`https://app.apiosk.com` CORS, invalid/expired signature rejection, and a valid
signed database lookup that deliberately matched no trigger. A real Slack message
has not yet been tested.
The existing Slack auth configurations use Composio-managed OAuth, so the custom
OAuth ingress setup in step 4 is not needed for those configurations.

1. Apply `supabase/migrations/20261029161428_inbound_triggers.sql` to the selected
   database using the normal migration process. The two new tables live in
   `gateway_v2`, permit only the existing gateway runtime role, and enable RLS.
2. Deploy the gateway and app changes. Set `APP_ORIGIN=https://app.apiosk.com` so
   Composio returns the person to `/triggers` after consent. Existing Ask routes
   and integration callbacks are unchanged.
3. Keep the existing server-side `COMPOSIO_API_KEY`. Register the gateway's public
   HTTPS `/v2/triggers/composio` URL as a Composio V3 webhook subscription, with
   `composio.trigger.message` enabled. Store its signing secret as
   `COMPOSIO_WEBHOOK_SECRET` in the gateway's server environment and restart it.
   There is one delivery subscription per Composio project. Do not replace a
   different existing destination without reconciling its consumers.

   An optional helper creates/reuses a matching subscription without printing
   its secret or changing other destinations:

   ```sh
   mkdir -p .local
   node --env-file=.env scripts/setup-trigger-webhook.mjs \
     https://apiosk-gateway-v2.fly.dev/v2/triggers/composio \
     .local/triggers-delivery.env
   ```

   Load the generated file into the gateway's secret configuration. Do not commit
   it or put it into the frontend environment. This command makes a real Composio
   configuration change; it is not a test command.
4. Configure Composio's Slack event ingress for the auth config in use. This uses
   `SLACK_CHANNEL_MESSAGE_RECEIVED`, pinned to toolkit version `20261023_00`, with
   `channel_id` and `is_bot_message: false`. Custom OAuth apps need the provider
   event URL and signing secret configured through Composio's webhook endpoint
   setup. Public/private/group message history scopes and conversation access
   must be granted. A connection alone does not prove event delivery.
5. Open `/triggers`, name the trigger, connect/select Slack, choose a conversation
   and create the trigger. Send a **new** human message there. Verify an activity
   entry without the “Test event” label and a Hello world alert. The page's Live
   indicator describes the gateway event stream, not proof of Slack delivery.

Slack creation stays disabled when the delivery signing secret is absent. OAuth
connection and generic webhooks remain available. Successful subscription creation
does not prove a real provider event has arrived; verify step 5 before claiming it.

## Apiosk Slack app and replies

A message that names Apiosk (the word, or a mention of the Apiosk bot) gets a
short model reply: a normal channel message that tags the asker, or a thread
reply when the question was asked in a thread. It is always posted by that
workspace's **Apiosk bot**, never from the person's own account; without one
there is no reply. New private channels get an optional description and
invitations, and the Apiosk bot is invited automatically.

Each workspace adds Apiosk once with **Add Apiosk to Slack** on `/triggers`.
That is a Composio `slackbot` connection made with our own Slack app, so
Composio keeps one bot token per workspace and messages come from Apiosk.
Setup, once:

1. Create the Slack app from the manifest below at api.slack.com/apps and turn
   on **Manage Distribution → Public Distribution** so other workspaces can
   install it. No App Directory listing is needed.
2. In Composio, create a **Slackbot** auth config with the app's own Client ID
   and Client Secret (scopes `chat:write,im:write`). Never use a
   Composio-managed one: it would post as Composio's app.
3. Set `APIOSK_SLACKBOT_AUTH_CONFIG=ac_…` (gateway environment) and the app's
   Signing Secret as the Fly secret `SLACK_SIGNING_SECRET`, for button clicks.

```yaml
display_information:
  name: Apiosk
  description: Checkt bedrijven en leveranciers tegen officiële bronnen.
  background_color: "#111111"
features:
  bot_user:
    display_name: Apiosk
    always_online: true
oauth_config:
  redirect_urls:
    - https://backend.composio.dev/api/v1/auth-apps/add
  scopes:
    bot: [chat:write, im:write]
settings:
  interactivity:
    is_enabled: true
    request_url: https://apiosk-gateway-v2.fly.dev/v2/triggers/slack/interactions
```

The bot's Slack user and workspace come from `auth.test` through Composio's
proxy; with several workspaces, the event's `team_id` picks the bot. Existing
channels need `/invite @Apiosk` once.

Owners in `TRIGGER_TOOL_TEST_OWNER_IDS` see **Test tools** on `/triggers`. They
post each Slack agent response (chat reply, screening, approval request,
monitoring) through their Apiosk bot with labelled test data. Only the chat reply calls
a model; nothing is screened, bought or scheduled. Slack responses and approval decisions
are persisted in the owner’s Live activity feed, including response type, preview,
approver and outcome. Approval buttons update the DM and post the decision in the
channel thread. The first decision is stored durably, so duplicate clicks across
restarts do not create conflicting outcomes. Existing inbound messages are unchanged.
Apply `20261030094736_trigger_response_activity.sql` before deploying this version.
Monitoring is shown as a demo simulation: production monitoring is paused and
has no news source yet.

## Normal trigger approvers

Every new Slack trigger requires an active teammate as its approver, including
**Record activity and alert**. The recipient (`approver_id`) is independent of
`approval_mode`: `on_request` records matching activity without sending automatic
review requests; `automatic` sends matching messages to the configured approver.
After a completed screening, `request approval` in the dossier's Slack thread
uses the same saved approver in either mode. The decision is recorded in Slack,
Activity and the dossier. This review does not authorize a purchase.

`POST /v2/triggers/{id}/approver` can change the teammate and optionally the mode.
Omitting the mode preserves it. A null approver is rejected. Ownership, active
workspace membership and the installed Apiosk bot are checked before saving.
Existing triggers without a reviewer show **Approver required** and cannot be
resumed until configured; existing active listeners keep recording activity.
The migration preserves automatic behavior for previously assigned approvers.

Apply `20261030133913_trigger_approval_mode.sql` before deploying this version.

## Generic webhook and Teams

Webhook creation returns a UUID endpoint and a random bearer key shown once.
Only the SHA-256 hash is stored. “New webhook key” revokes the previous key.

```http
POST /v2/triggers/{id}/incoming
Authorization: Bearer trg_...
Content-Type: application/json

{"event_id":"unique-message-id","text":"Any message you want"}
```

Reuse the same event ID for delivery retries; use a new ID for a new message.
Message text is limited to 8,000 characters. Accepted events, including unmatched
messages, are limited to 120 per trigger per minute. A paused trigger records
nothing and runs no action. Unpause does not replay messages received while paused.

The Composio project returned no Microsoft Teams trigger types when checked on
2026-10-29. Teams is therefore explicitly **via webhook**, not native Composio
trigger delivery. Its optional named Composio connection remains available.
In Power Automate, choose “When a new message is added to a chat or channel”, select
the intended conversation, convert HTML message content to plain text if needed,
and add an HTTP POST with the URL/key above. Map message ID to `event_id` and the
plain text to `text`. HTTP connector licensing and provider delivery latency depend
on the Microsoft environment; some Teams triggers poll rather than push.

## Routes and boundaries

- `GET/POST /v2/triggers`: list and create personal triggers; create takes a UUID
  from the browser for retry safety. One trigger per account/conversation; 50 per owner.
- `POST /v2/triggers/connect/{slack|teams}`: optional `{ "alias": "My team" }`.
- `GET /v2/triggers/channels?account=...&cursor=...`: paginated Slack conversations.
- `GET /v2/triggers/members?account=...&cursor=...`: paginated human teammates.
- `POST /v2/triggers/channels`: create a private channel or open a group chat.
- `POST /v2/triggers/{id}/control`: `{ "enabled": false }` pauses local reception.
- `POST /v2/triggers/{id}/rotate`: rotate a generic/Teams webhook bearer key.
- `POST /v2/triggers/{id}/test`: same matcher/history path, labelled as a test.
- `GET /v2/triggers/events?after=...`: bearer-authenticated SSE with durable cursor.
- `POST /v2/triggers/composio`: raw-body HMAC-SHA256 verification, five-minute
  timestamp tolerance, and exact owner/account/trigger/channel mapping. V3 only.

App management requires a personal session. Organisation keys and connected-agent
credentials cannot manage triggers. Account ownership and active state are checked
with Composio before tools or subscriptions run. User text is data, never a prompt.
Bot events and message edits are ignored; duplicate Slack messages use their
channel/timestamp identity, independent of webhook retry IDs. Insertion is serialized
per owner, so later SSE cursors cannot overtake uncommitted earlier events.

SSE sends newly committed events immediately on this gateway, with one-second
recovery reads for other gateway instances. Connections end after 55 seconds and
reauthenticate on reconnect. The frontend deduplicates delivery, never alerts for
history on initial load, and cancels streams on unmount/account changes. Alerting
requires the page to be open; this milestone does not send OS notifications.

## Verification

```sh
# Gateway unit tests
cargo test --lib server::triggers::tests
# Dedicated, disposable localhost PostgreSQL database only; never production.
TRIGGERS_TEST_DATABASE_URL=postgresql://USER@127.0.0.1:PORT/triggers_test \
  cargo test --test triggers_flow -- --ignored --nocapture
# App directory
npx vitest run tests/lib/triggers.test.ts tests/components/triggers-page.test.tsx
npx tsc -b
```

The HTTP/PostgreSQL test covers real runtime grants, unauthenticated rejection,
owner isolation, exact Slack account selection, private/group creation, teammate
filtering, signed delivery, wrong owner/channel rejection, bot rejection, concurrent
duplicate delivery, pause, key rotation, SSE, and prevention of Data API access.
Composio and Supabase Auth are fixtures in this test; it proves no live Slack event.

References: [Composio triggers](https://docs.composio.dev/docs/triggers),
[creating triggers](https://docs.composio.dev/docs/setting-up-triggers/creating-triggers),
[signed event delivery](https://docs.composio.dev/docs/setting-up-triggers/subscribing-to-events),
[webhook subscriptions](https://docs.composio.dev/reference/api-reference/webhook-subscriptions),
[Teams connector](https://learn.microsoft.com/connectors/teams/).

### Real screenings and saved dossiers

A message mentioning Apiosk with `screen`, `screening`, `check`, `controleer` or
`onboard` creates a durable dossier linked to that trigger and Slack thread.
For example: `@Apiosk screen Example B.V. KVK 12345678`. This is separate from the
owner-only sample tool: sample screenings remain explicitly fake and never
create paid source checks.

`Open dossier` opens `/monitoring?dossier=<id>`. The owner selects the exact KVK
company and sources in the existing company-check wizard, reviews the price,
and explicitly authorizes the existing task. A thread message or human review
never authorizes spending. Drafts are assigned to the selected app workspace at
first preparation; afterward the company, workspace and selected sources are
immutable for that screening. The original request remains visible in the app.

The durable background queue observes already-authorized tasks, retains the
same monitoring baseline (including evidence and source coverage), and updates
the original bot card. It resumes after gateway restarts. It does not execute
unapproved tasks. Missing sources and possible sanctions matches are not
reported as clearance. New Slack text is English.

In the same thread, `status` returns the saved dossier; `request approval`
notifies the trigger's configured approver only after a baseline is saved. That
review and the selected person's signed decision are linked to the dossier and
Activity. `monitor` opens the same dossier for the existing schedule/pricing
flow, reusing its saved task; activation links the resulting schedule back to
the dossier. Starting a new company or another screening uses a new Slack
thread. Slack drafts and one-off screenings do not enable recurring monitoring
for accounts where that feature is disabled.

Storage: `gateway_v2.slack_dossiers`, migration `slack_dossiers`; gateway-runtime
access only, owner/workspace checks in handlers, no public Data API grants.
