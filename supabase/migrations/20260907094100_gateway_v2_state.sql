-- Private operational state. No second balance ledger and no Data API grants.
CREATE SCHEMA IF NOT EXISTS gateway_v2;
REVOKE ALL ON SCHEMA gateway_v2 FROM PUBLIC, anon, authenticated;
CREATE TABLE gateway_v2.tasks (
 id uuid PRIMARY KEY, owner_id uuid NOT NULL, connection_id uuid,
 revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0),
 document jsonb NOT NULL CHECK (octet_length(document::text) <= 2097152),
 created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now(),
 expires_at timestamptz NOT NULL DEFAULT now() + interval '30 days'
);
CREATE INDEX tasks_owner_recent ON gateway_v2.tasks(owner_id,updated_at DESC);
CREATE INDEX tasks_expiry ON gateway_v2.tasks(expires_at);
CREATE TABLE gateway_v2.requests (
 owner_id uuid NOT NULL, request_id uuid NOT NULL, payload_hash text NOT NULL,
 response jsonb, created_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(owner_id,request_id)
);
CREATE TABLE gateway_v2.authorizations (
 id uuid PRIMARY KEY, task_id uuid NOT NULL REFERENCES gateway_v2.tasks(id) ON DELETE CASCADE,
 owner_id uuid NOT NULL, quote_ref uuid NOT NULL, plan_hash text NOT NULL,
 cap_atomic bigint NOT NULL CHECK(cap_atomic >= 0),
 expires_at timestamptz NOT NULL, revoked_at timestamptz,
 UNIQUE(task_id,quote_ref)
);
CREATE INDEX requests_owner_time ON gateway_v2.requests(owner_id,created_at);
CREATE INDEX requests_expiry ON gateway_v2.requests(created_at);
CREATE TABLE gateway_v2.executions (
 id uuid PRIMARY KEY, owner_id uuid NOT NULL,
 task_id uuid NOT NULL REFERENCES gateway_v2.tasks(id) ON DELETE CASCADE,
 action_id uuid NOT NULL, authorization_id uuid REFERENCES gateway_v2.authorizations(id),
 payload_hash text NOT NULL, selection_id uuid NOT NULL,
 reserved_atomic bigint NOT NULL CHECK(reserved_atomic >= 0),
 actual_atomic bigint CHECK(actual_atomic >= 0),
 status text NOT NULL CHECK(status IN ('dispatching','succeeded','failed','unresolved')),
 response jsonb, created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE(owner_id,action_id)
);
CREATE INDEX executions_budget ON gateway_v2.executions(authorization_id,status);
CREATE INDEX executions_task_status ON gateway_v2.executions(task_id,status);
CREATE INDEX executions_reconcile ON gateway_v2.executions(status,updated_at) WHERE status IN ('dispatching','unresolved');
CREATE TABLE gateway_v2.results (
 id uuid PRIMARY KEY, owner_id uuid NOT NULL,
 task_id uuid NOT NULL REFERENCES gateway_v2.tasks(id) ON DELETE CASCADE,
 execution_id uuid NOT NULL UNIQUE REFERENCES gateway_v2.executions(id),
 document jsonb NOT NULL CHECK(octet_length(document::text) <= 8388608),
 created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX results_task ON gateway_v2.results(owner_id,task_id);
ALTER TABLE gateway_v2.tasks ENABLE ROW LEVEL SECURITY;
ALTER TABLE gateway_v2.requests ENABLE ROW LEVEL SECURITY;
ALTER TABLE gateway_v2.authorizations ENABLE ROW LEVEL SECURITY;
ALTER TABLE gateway_v2.executions ENABLE ROW LEVEL SECURITY;
ALTER TABLE gateway_v2.results ENABLE ROW LEVEL SECURITY;
-- Only a server-side database role receives grants at deployment. All queries
-- additionally bind owner_id. No SECURITY DEFINER or browser write policies.
REVOKE ALL ON ALL TABLES IN SCHEMA gateway_v2 FROM PUBLIC,anon,authenticated;
