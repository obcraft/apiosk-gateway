-- A Slack screening is a durable dossier before a monitoring schedule exists.
-- Source execution still uses the existing app-authorized monitoring task.
CREATE TABLE gateway_v2.slack_dossiers (
 id uuid PRIMARY KEY,
 owner_id uuid NOT NULL REFERENCES auth.users(id) ON DELETE CASCADE,
 trigger_id uuid NOT NULL,
 team_id text NOT NULL,
 channel_id text NOT NULL,
 thread_ts text NOT NULL,
 message_ts text,
 request_text text NOT NULL CHECK (length(request_text) <= 8000),
 workspace_id uuid,
 company jsonb,
 sources jsonb NOT NULL DEFAULT '["kvk-dutch-business-register","opensanctions"]',
 establishment_number text,
 task_id uuid,
 state text NOT NULL DEFAULT 'draft' CHECK (state IN ('draft','prepared','running','checked','failed')),
 snapshot jsonb,
 order_id uuid,
 published_state text,
 next_refresh_at timestamptz NOT NULL DEFAULT now(),
 created_at timestamptz NOT NULL DEFAULT now(),
 updated_at timestamptz NOT NULL DEFAULT now(),
 FOREIGN KEY (trigger_id,owner_id) REFERENCES gateway_v2.inbound_triggers(id,owner_id),
 UNIQUE(trigger_id,thread_ts),
 CHECK (company IS NULL OR jsonb_typeof(company)='object'),
 CHECK (jsonb_typeof(sources)='array')
);
CREATE INDEX slack_dossiers_owner ON gateway_v2.slack_dossiers(owner_id,created_at DESC);
CREATE INDEX slack_dossiers_refresh ON gateway_v2.slack_dossiers(next_refresh_at);
ALTER TABLE gateway_v2.slack_dossiers ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON gateway_v2.slack_dossiers FROM PUBLIC,anon,authenticated;
GRANT SELECT,INSERT,UPDATE ON gateway_v2.slack_dossiers TO gateway_v2_runtime;
CREATE POLICY runtime_access ON gateway_v2.slack_dossiers TO gateway_v2_runtime USING(true) WITH CHECK(true);
