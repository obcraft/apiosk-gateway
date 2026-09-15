-- Workflows: saved runs of directly runnable flows (first: supplier payment
-- check) and temporary document uploads for reading that needs the original
-- file. Private gateway state: no Data API grants; queries bind owner_id and
-- the workspace scope.
BEGIN;

CREATE TABLE IF NOT EXISTS gateway_v2.workflow_runs (
 id uuid PRIMARY KEY,
 owner_id uuid NOT NULL,
 workspace_id uuid,
 request_id uuid,
 workflow_slug text NOT NULL CHECK (workflow_slug IN ('supplier-payment-check')),
 surface text NOT NULL CHECK (surface IN ('workflows','ask','connected_agent')),
 status text NOT NULL CHECK (status IN ('quoted','needs_input','needs_selection','unsupported','running','completed','failed','cancelled')),
 task_id uuid,
 quote_ref uuid,
 document jsonb CHECK (document IS NULL OR (jsonb_typeof(document)='object' AND octet_length(document::text) <= 4096)),
 input jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(input)='object' AND octet_length(input::text) <= 16384),
 question text NOT NULL DEFAULT '' CHECK (length(question) <= 4000),
 result jsonb CHECK (result IS NULL OR octet_length(result::text) <= 262144),
 created_at timestamptz NOT NULL DEFAULT now(),
 updated_at timestamptz NOT NULL DEFAULT now(),
 expires_at timestamptz NOT NULL DEFAULT now() + interval '13 months',
 UNIQUE (owner_id, request_id),
 UNIQUE (task_id, quote_ref)
);
CREATE INDEX IF NOT EXISTS workflow_runs_owner_recent ON gateway_v2.workflow_runs (owner_id, workspace_id, created_at DESC);
CREATE INDEX IF NOT EXISTS workflow_runs_task ON gateway_v2.workflow_runs (task_id);
CREATE INDEX IF NOT EXISTS workflow_runs_expiry ON gateway_v2.workflow_runs (expires_at);

-- Original files are kept only while reading needs them: removed after a
-- successful read, and never longer than one hour.
CREATE TABLE IF NOT EXISTS gateway_v2.document_uploads (
 id uuid PRIMARY KEY,
 owner_id uuid NOT NULL,
 workspace_id uuid,
 purpose text NOT NULL CHECK (purpose IN ('invoice')),
 file_name text NOT NULL CHECK (length(file_name) BETWEEN 1 AND 255),
 mime_type text NOT NULL CHECK (mime_type IN ('application/pdf','image/png','image/jpeg')),
 sha256 text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
 byte_size integer NOT NULL CHECK (byte_size BETWEEN 1 AND 10485760),
 content bytea NOT NULL,
 created_at timestamptz NOT NULL DEFAULT now(),
 expires_at timestamptz NOT NULL DEFAULT now() + interval '1 hour' CHECK (expires_at <= created_at + interval '1 hour')
);
CREATE INDEX IF NOT EXISTS document_uploads_expiry ON gateway_v2.document_uploads (expires_at);

ALTER TABLE gateway_v2.workflow_runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE gateway_v2.document_uploads ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON gateway_v2.workflow_runs, gateway_v2.document_uploads FROM PUBLIC, anon, authenticated;
DO $$ BEGIN
 IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname='gateway_v2_runtime') THEN
  GRANT SELECT, INSERT, UPDATE, DELETE ON gateway_v2.workflow_runs, gateway_v2.document_uploads TO gateway_v2_runtime;
  IF NOT EXISTS (SELECT 1 FROM pg_policies WHERE schemaname='gateway_v2' AND tablename='workflow_runs' AND policyname='runtime_access') THEN
   CREATE POLICY runtime_access ON gateway_v2.workflow_runs TO gateway_v2_runtime USING (true) WITH CHECK (true);
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_policies WHERE schemaname='gateway_v2' AND tablename='document_uploads' AND policyname='runtime_access') THEN
   CREATE POLICY runtime_access ON gateway_v2.document_uploads TO gateway_v2_runtime USING (true) WITH CHECK (true);
  END IF;
 END IF;
END $$;
COMMIT;
