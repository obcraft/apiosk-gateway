-- Cost per question segment, for cost per fully answered request (#3).
-- One row per (trace, segment, task, quote), written only when the gateway
-- runs with TELEMETRY_PERSIST=1. Metrics are nullable: null is unmeasured,
-- never 0. No owner, workspace, question, answer or error text is stored;
-- the provider cost, Apiosk fee, customer charge and quote cap stay separate.
-- Rows expire after 90 days (gateway maintenance prunes them).
BEGIN;

CREATE TABLE IF NOT EXISTS gateway_v2.request_costs (
 id uuid PRIMARY KEY,
 trace_id uuid NOT NULL,
 segment text NOT NULL CHECK (segment IN ('question','execution','retry_analysis','follow_up')),
 task_id uuid NOT NULL,
 quote_ref uuid,
 surface text NOT NULL CHECK (surface IN ('app','connected_agent','public_api','unknown')),
 query_class text NOT NULL CHECK (query_class IN ('simple_lookup','search_list','comparison','dossier','unclassified')),
 source_count smallint CHECK (source_count >= 0),
 cache_state text NOT NULL CHECK (cache_state IN ('warm','cold','unknown')),
 outcome text NOT NULL CHECK (outcome IN ('price_ready','needs_input','unsupported','failed','answered','partial','unavailable','cancelled')),
 fully_answered boolean NOT NULL DEFAULT false,
 duration_ms integer CHECK (duration_ms >= 0),
 model_calls integer CHECK (model_calls >= 0),
 model_attempts integer CHECK (model_attempts >= 0),
 prompt_tokens bigint CHECK (prompt_tokens >= 0),
 completion_tokens bigint CHECK (completion_tokens >= 0),
 cached_prompt_tokens bigint CHECK (cached_prompt_tokens >= 0),
 reasoning_tokens bigint CHECK (reasoning_tokens >= 0),
 model_cost_nanousd bigint CHECK (model_cost_nanousd >= 0),
 model_cost_complete boolean,
 source_calls integer CHECK (source_calls >= 0),
 source_retries integer CHECK (source_retries >= 0),
 source_refunds integer CHECK (source_refunds >= 0),
 source_unresolved integer CHECK (source_unresolved >= 0),
 provider_micro_usd bigint CHECK (provider_micro_usd >= 0),
 fee_micro_usd bigint CHECK (fee_micro_usd >= 0),
 customer_charged_micro_usd bigint CHECK (customer_charged_micro_usd >= 0),
 customer_cap_micro_usd bigint CHECK (customer_cap_micro_usd >= 0),
 refunded_micro_usd bigint CHECK (refunded_micro_usd >= 0),
 db_calls integer CHECK (db_calls >= 0),
 db_ms integer CHECK (db_ms >= 0),
 bytes_in bigint CHECK (bytes_in >= 0),
 bytes_out bigint CHECK (bytes_out >= 0),
 model_bytes bigint CHECK (model_bytes >= 0),
 edge_bytes bigint CHECK (edge_bytes >= 0),
 sse_bytes bigint CHECK (sse_bytes >= 0),
 schema_version smallint NOT NULL DEFAULT 1,
 created_at timestamptz NOT NULL DEFAULT now(),
 expires_at timestamptz NOT NULL DEFAULT now() + interval '90 days',
 UNIQUE NULLS NOT DISTINCT (trace_id, segment, task_id, quote_ref)
);
CREATE INDEX IF NOT EXISTS request_costs_created ON gateway_v2.request_costs (created_at);
CREATE INDEX IF NOT EXISTS request_costs_expiry ON gateway_v2.request_costs (expires_at);
CREATE INDEX IF NOT EXISTS request_costs_class ON gateway_v2.request_costs (query_class, created_at);

ALTER TABLE gateway_v2.request_costs ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON gateway_v2.request_costs FROM PUBLIC, anon, authenticated;
DO $$ BEGIN
 IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname='gateway_v2_runtime') THEN
  -- Insert-once rows and retention only: the runtime never rewrites a row.
  GRANT SELECT, INSERT, DELETE ON gateway_v2.request_costs TO gateway_v2_runtime;
  IF NOT EXISTS (SELECT 1 FROM pg_policies WHERE schemaname='gateway_v2' AND tablename='request_costs' AND policyname='runtime_access') THEN
   CREATE POLICY runtime_access ON gateway_v2.request_costs TO gateway_v2_runtime USING (true) WITH CHECK (true);
  END IF;
 END IF;
END $$;
COMMIT;
