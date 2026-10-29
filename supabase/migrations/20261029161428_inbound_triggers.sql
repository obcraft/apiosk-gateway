-- Inbound triggers are personal connections. They never authorize paid research.
CREATE TABLE gateway_v2.inbound_triggers (
    id uuid PRIMARY KEY,
    owner_id uuid NOT NULL REFERENCES auth.users(id) ON DELETE CASCADE,
    name text NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    source text NOT NULL CHECK (source IN ('slack', 'webhook', 'teams')),
    match_prefix text NOT NULL DEFAULT '' CHECK (length(match_prefix) <= 100),
    connected_account_id text,
    channel_id text,
    channel_name text,
    provider_trigger_id text,
    webhook_secret_hash text,
    enabled boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((source = 'slack' AND connected_account_id IS NOT NULL AND channel_id IS NOT NULL AND provider_trigger_id IS NOT NULL AND webhook_secret_hash IS NULL)
        OR (source IN ('webhook','teams') AND webhook_secret_hash IS NOT NULL AND connected_account_id IS NULL AND channel_id IS NULL AND provider_trigger_id IS NULL)),
    UNIQUE (id, owner_id)
);
CREATE INDEX inbound_triggers_owner ON gateway_v2.inbound_triggers(owner_id, created_at DESC);
CREATE UNIQUE INDEX inbound_triggers_slack_channel ON gateway_v2.inbound_triggers(owner_id, connected_account_id, channel_id) WHERE source = 'slack';
CREATE INDEX inbound_triggers_provider ON gateway_v2.inbound_triggers(provider_trigger_id) WHERE source = 'slack';

CREATE TABLE gateway_v2.inbound_trigger_events (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    trigger_id uuid NOT NULL,
    owner_id uuid NOT NULL,
    event_key text NOT NULL CHECK (length(event_key) BETWEEN 1 AND 300),
    text text NOT NULL CHECK (length(text) <= 8000),
    matched boolean NOT NULL,
    is_test boolean NOT NULL DEFAULT false,
    received_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (trigger_id, owner_id) REFERENCES gateway_v2.inbound_triggers(id, owner_id) ON DELETE CASCADE,
    UNIQUE (trigger_id, event_key)
);
CREATE INDEX inbound_trigger_events_owner ON gateway_v2.inbound_trigger_events(owner_id, id DESC);
CREATE INDEX inbound_trigger_events_rate ON gateway_v2.inbound_trigger_events(trigger_id, received_at DESC);
ALTER TABLE gateway_v2.inbound_triggers ENABLE ROW LEVEL SECURITY;
ALTER TABLE gateway_v2.inbound_trigger_events ENABLE ROW LEVEL SECURITY;
-- Gateway-only access; neither trigger secrets nor incoming messages are Data API data.
REVOKE ALL ON gateway_v2.inbound_triggers, gateway_v2.inbound_trigger_events FROM PUBLIC, anon, authenticated;
GRANT SELECT, INSERT, UPDATE ON gateway_v2.inbound_triggers, gateway_v2.inbound_trigger_events TO gateway_v2_runtime;
GRANT USAGE, SELECT ON SEQUENCE gateway_v2.inbound_trigger_events_id_seq TO gateway_v2_runtime;
CREATE POLICY runtime_access ON gateway_v2.inbound_triggers TO gateway_v2_runtime USING (true) WITH CHECK (true);
CREATE POLICY runtime_access ON gateway_v2.inbound_trigger_events TO gateway_v2_runtime USING (true) WITH CHECK (true);
