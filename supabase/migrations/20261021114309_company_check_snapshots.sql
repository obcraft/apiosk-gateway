-- Private, durable host-scheduled checks. No new spending mandate or ledger.
CREATE TABLE gateway_v2.company_checks (
 id uuid PRIMARY KEY,
 owner_id uuid NOT NULL,
 connection_id uuid,
 workspace_id uuid,
 companies jsonb NOT NULL CHECK (jsonb_typeof(companies)='array' AND jsonb_array_length(companies) BETWEEN 1 AND 3),
 created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX company_checks_owner ON gateway_v2.company_checks(owner_id, created_at DESC);
CREATE TABLE gateway_v2.company_check_runs (
 check_id uuid NOT NULL REFERENCES gateway_v2.company_checks(id) ON DELETE CASCADE,
 occurrence text NOT NULL CHECK (length(occurrence) BETWEEN 1 AND 128),
 request_id uuid NOT NULL UNIQUE,
 task_id uuid,
 snapshot jsonb,
 created_at timestamptz NOT NULL DEFAULT now(),
 completed_at timestamptz,
 last_checked_at timestamptz,
 PRIMARY KEY (check_id, occurrence),
 CHECK ((snapshot IS NULL) = (completed_at IS NULL))
);
-- A second occurrence must not race an unfinished check and lose its baseline.
CREATE UNIQUE INDEX company_check_one_open_run ON gateway_v2.company_check_runs(check_id) WHERE completed_at IS NULL;
CREATE INDEX company_check_pending_snapshots ON gateway_v2.company_check_runs(last_checked_at NULLS FIRST,created_at) WHERE completed_at IS NULL AND task_id IS NOT NULL;
CREATE INDEX company_check_history ON gateway_v2.company_check_runs(check_id, completed_at DESC) WHERE completed_at IS NOT NULL;
CREATE TABLE gateway_v2.company_check_reported_signals (
 check_id uuid NOT NULL REFERENCES gateway_v2.company_checks(id) ON DELETE CASCADE,
 signal_id text NOT NULL CHECK (signal_id ~ '^[0-9a-f]{64}$'),
 occurrence text NOT NULL,
 reported_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY (check_id, signal_id),
 FOREIGN KEY (check_id, occurrence) REFERENCES gateway_v2.company_check_runs(check_id, occurrence)
);
ALTER TABLE gateway_v2.company_checks ENABLE ROW LEVEL SECURITY;
ALTER TABLE gateway_v2.company_check_runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE gateway_v2.company_check_reported_signals ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON gateway_v2.company_checks, gateway_v2.company_check_runs, gateway_v2.company_check_reported_signals FROM PUBLIC, anon, authenticated;
GRANT SELECT, INSERT, UPDATE ON gateway_v2.company_checks, gateway_v2.company_check_runs, gateway_v2.company_check_reported_signals TO gateway_v2_runtime;
CREATE POLICY runtime_access ON gateway_v2.company_checks TO gateway_v2_runtime USING (true) WITH CHECK (true);
CREATE POLICY runtime_access ON gateway_v2.company_check_runs TO gateway_v2_runtime USING (true) WITH CHECK (true);
CREATE POLICY runtime_access ON gateway_v2.company_check_reported_signals TO gateway_v2_runtime USING (true) WITH CHECK (true);
COMMENT ON TABLE gateway_v2.company_checks IS 'Gateway-only. Every operation verifies owner, workspace and connection; never exposed through the Data API.';

-- The host sends email. An uncertain send remains blocked until its receipt is
-- reconciled; no timer can turn an ambiguous send into a duplicate delivery.
CREATE TABLE gateway_v2.company_check_deliveries (
 delivery_id uuid PRIMARY KEY,
 check_id uuid NOT NULL REFERENCES gateway_v2.company_checks(id) ON DELETE CASCADE,
 occurrence text NOT NULL,
 payload jsonb NOT NULL,
 status text NOT NULL DEFAULT 'awaiting_confirmation' CHECK(status IN ('awaiting_confirmation','sent')),
 provider text,
 provider_message_id text,
 created_at timestamptz NOT NULL DEFAULT now(),
 confirmed_at timestamptz,
 FOREIGN KEY(check_id,occurrence) REFERENCES gateway_v2.company_check_runs(check_id,occurrence),
 CHECK ((status='sent') = (confirmed_at IS NOT NULL AND provider IS NOT NULL AND provider_message_id IS NOT NULL))
);
CREATE UNIQUE INDEX company_check_one_pending_delivery ON gateway_v2.company_check_deliveries(check_id) WHERE status='awaiting_confirmation';
CREATE UNIQUE INDEX company_check_unique_receipt ON gateway_v2.company_check_deliveries(check_id,provider,provider_message_id) WHERE status='sent';
ALTER TABLE gateway_v2.company_check_deliveries ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON gateway_v2.company_check_deliveries FROM PUBLIC, anon, authenticated;
GRANT SELECT, INSERT, UPDATE ON gateway_v2.company_check_deliveries TO gateway_v2_runtime;
CREATE POLICY runtime_access ON gateway_v2.company_check_deliveries TO gateway_v2_runtime USING (true) WITH CHECK (true);
