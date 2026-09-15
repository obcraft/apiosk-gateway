-- Supplier payment check history: the identifiers of each checked invoice, so
-- the next invoice from the same supplier can be compared ("IBAN changed").
-- Private gateway state: no Data API grants; every query binds owner_id and
-- the workspace scope. Retained for 13 months, independent of 30-day tasks.
BEGIN;
CREATE TABLE IF NOT EXISTS gateway_v2.supplier_invoice_checks (
 id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
 owner_id uuid NOT NULL,
 workspace_id uuid,
 task_id uuid NOT NULL,
 quote_ref uuid NOT NULL,
 supplier_keys text[] NOT NULL CHECK (cardinality(supplier_keys) BETWEEN 1 AND 8),
 record jsonb NOT NULL CHECK (jsonb_typeof(record)='object' AND octet_length(record::text) <= 8192),
 decision text CHECK (decision IN ('hold','review','ok')),
 observed_at timestamptz NOT NULL DEFAULT now(),
 expires_at timestamptz NOT NULL DEFAULT now() + interval '13 months',
 UNIQUE (task_id, quote_ref)
);
CREATE INDEX IF NOT EXISTS supplier_invoice_checks_owner_keys ON gateway_v2.supplier_invoice_checks USING gin (supplier_keys);
CREATE INDEX IF NOT EXISTS supplier_invoice_checks_owner_recent ON gateway_v2.supplier_invoice_checks (owner_id, observed_at DESC);
CREATE INDEX IF NOT EXISTS supplier_invoice_checks_expiry ON gateway_v2.supplier_invoice_checks (expires_at);
ALTER TABLE gateway_v2.supplier_invoice_checks ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON gateway_v2.supplier_invoice_checks FROM PUBLIC, anon, authenticated;
DO $$ BEGIN
 IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname='gateway_v2_runtime') THEN
  GRANT SELECT, INSERT, UPDATE, DELETE ON gateway_v2.supplier_invoice_checks TO gateway_v2_runtime;
  DROP POLICY IF EXISTS runtime_access ON gateway_v2.supplier_invoice_checks;
  CREATE POLICY runtime_access ON gateway_v2.supplier_invoice_checks TO gateway_v2_runtime USING (true) WITH CHECK (true);
 END IF;
END $$;
COMMIT;
