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
enabled and grant access to the gateway runtime only. The app and gateway changes
still need deployment; the Composio project has no outbound webhook subscription.
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
