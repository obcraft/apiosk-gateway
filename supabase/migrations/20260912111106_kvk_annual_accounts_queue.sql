-- Private shared source data, independent of buyer executions and retention.
CREATE SCHEMA kvk_cache;
REVOKE ALL ON SCHEMA kvk_cache FROM PUBLIC, anon, authenticated;
CREATE TABLE kvk_cache.annual_accounts (
  kvk_number text NOT NULL CHECK (kvk_number ~ '^[0-9]{8}$'),
  financial_year integer NOT NULL CHECK (financial_year BETWEEN 1900 AND 2200),
  document_type text NOT NULL DEFAULT 'kvk-hvds-xbrl-v1',
  version text NOT NULL CHECK (version ~ '^[0-9a-f]{64}$'),
  payload jsonb NOT NULL CHECK (jsonb_typeof(payload)='object'),
  source_url text NOT NULL,
  fetched_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (kvk_number,financial_year,document_type,version)
);
-- This endpoint accepts only a KVK number. 'latest' is a refreshable lookup,
-- never an invented financial year or a replacement for immutable documents.
CREATE TABLE kvk_cache.jobs (
  kvk_number text PRIMARY KEY CHECK (kvk_number ~ '^[0-9]{8}$'),
  status text NOT NULL CHECK (status IN ('queued','running','succeeded','failed')),
  available_at timestamptz NOT NULL DEFAULT now(),
  lease_token uuid,
  lease_until timestamptz,
  attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
  financial_year integer,
  document_type text,
  version text,
  error_code text,
  fresh_until timestamptz,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  FOREIGN KEY (kvk_number,financial_year,document_type,version)
    REFERENCES kvk_cache.annual_accounts(kvk_number,financial_year,document_type,version),
  CHECK ((status='running') = (lease_token IS NOT NULL AND lease_until IS NOT NULL)),
  CHECK (status!='succeeded' OR (financial_year IS NOT NULL AND document_type IS NOT NULL AND version IS NOT NULL AND fresh_until IS NOT NULL))
);
CREATE INDEX jobs_ready ON kvk_cache.jobs(available_at,created_at) WHERE status IN ('queued','running');
CREATE TABLE kvk_cache.worker_slots (
  slot integer PRIMARY KEY CHECK (slot BETWEEN 1 AND 5),
  lease_token uuid,
  lease_until timestamptz
);
INSERT INTO kvk_cache.worker_slots(slot) SELECT generate_series(1,5);
-- One budget per verified fixed egress IP, shared across all replicas.
CREATE TABLE kvk_cache.rate_limit (
  egress_key text PRIMARY KEY,
  next_allowed_at timestamptz NOT NULL DEFAULT now(),
  active_until timestamptz,
  lease_token uuid
);

CREATE TABLE kvk_cache.fetch_attempts (
  id uuid PRIMARY KEY,
  kvk_number text NOT NULL REFERENCES kvk_cache.jobs(kvk_number),
  egress_key text NOT NULL,
  attempt integer NOT NULL CHECK (attempt > 0),
  started_at timestamptz NOT NULL DEFAULT now(),
  completed_at timestamptz,
  status text NOT NULL CHECK (status IN ('running','succeeded','failed','abandoned')),
  error_code text
);
CREATE INDEX fetch_attempts_lookup ON kvk_cache.fetch_attempts(kvk_number,started_at);
GRANT USAGE ON SCHEMA kvk_cache TO gateway_v2_runtime, service_role;
GRANT SELECT,INSERT ON kvk_cache.annual_accounts TO gateway_v2_runtime, service_role;
GRANT SELECT,INSERT,UPDATE ON kvk_cache.jobs TO gateway_v2_runtime, service_role;
GRANT SELECT,UPDATE ON kvk_cache.worker_slots TO gateway_v2_runtime, service_role;
GRANT SELECT,INSERT,UPDATE ON kvk_cache.rate_limit,kvk_cache.fetch_attempts TO gateway_v2_runtime, service_role;
DO $$ DECLARE t text; BEGIN
  FOREACH t IN ARRAY ARRAY['annual_accounts','jobs','worker_slots','rate_limit','fetch_attempts'] LOOP
    EXECUTE format('ALTER TABLE kvk_cache.%I ENABLE ROW LEVEL SECURITY', t);
    EXECUTE format('REVOKE ALL ON kvk_cache.%I FROM PUBLIC,anon,authenticated', t);
    EXECUTE format('CREATE POLICY server_access ON kvk_cache.%I TO gateway_v2_runtime,service_role USING (true) WITH CHECK (true)',t);
  END LOOP;
END $$;
