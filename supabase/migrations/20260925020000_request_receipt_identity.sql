-- #11: durable recovery identity for a first request. A lost response or a
-- reload can ask what happened to a request without resending it or
-- re-interpreting the question. Columns are additive and never part of the
-- idempotency hash; an old instance keeps inserting NULLs.
-- Follow the requests table after the domain-schema migration (actions);
-- isolated older installations can still have it in gateway_v2.
DO $migration$
DECLARE request_schema text;
BEGIN
 SELECT n.nspname INTO STRICT request_schema
 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
 WHERE c.relname='requests' AND c.relkind='r'
   AND n.nspname IN ('actions','gateway_v2');
 EXECUTE format($sql$ALTER TABLE %I.requests
  ADD COLUMN IF NOT EXISTS kind text CHECK (kind IS NULL OR kind ~ '^[a-z_-]{1,32}$'),
  ADD COLUMN IF NOT EXISTS task_id uuid,
  ADD COLUMN IF NOT EXISTS workspace_id uuid,
  ADD COLUMN IF NOT EXISTS connection_id uuid,
  ADD COLUMN IF NOT EXISTS trace_id uuid,
  ADD COLUMN IF NOT EXISTS completed_at timestamptz$sql$,request_schema);
 EXECUTE format('CREATE INDEX IF NOT EXISTS requests_task ON %I.requests(task_id) WHERE task_id IS NOT NULL',request_schema);
END
$migration$;
