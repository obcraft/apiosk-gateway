-- #11: durable recovery identity for a first request. A lost response or a
-- reload can ask what happened to a request without resending it or
-- re-interpreting the question. Columns are additive and never part of the
-- idempotency hash; an old instance keeps inserting NULLs.
ALTER TABLE gateway_v2.requests
  ADD COLUMN IF NOT EXISTS kind text CHECK (kind IS NULL OR kind ~ '^[a-z_-]{1,32}$'),
  ADD COLUMN IF NOT EXISTS task_id uuid,
  ADD COLUMN IF NOT EXISTS workspace_id uuid,
  ADD COLUMN IF NOT EXISTS connection_id uuid,
  ADD COLUMN IF NOT EXISTS trace_id uuid,
  ADD COLUMN IF NOT EXISTS completed_at timestamptz;
CREATE INDEX IF NOT EXISTS requests_task ON gateway_v2.requests(task_id) WHERE task_id IS NOT NULL;
