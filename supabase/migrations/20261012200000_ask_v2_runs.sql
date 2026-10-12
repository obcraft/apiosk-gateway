-- Ask V2 runs (gateway/contracts/ask-v2-run.md): question → source plan →
-- one approval → background execution → validated evidence → cited answer.
-- Private gateway state like tasks and workflow_runs: no Data API grants;
-- every query binds owner_id and the workspace scope. Execution, approval,
-- reservations, idempotency and the ledger stay in the existing
-- actions.tasks/authorizations/executions/results and gateway_v2.execution_leases plus
-- public.balance_charges: each run owns exactly one task, and each plan
-- revision is that task's quote (quote_ref, plan_hash, cap_atomic).
--
-- Index exception: every index below is on a table created in this same
-- transaction, so it is empty and nothing else can hold a lock on it. CREATE
-- INDEX CONCURRENTLY is not allowed inside BEGIN/COMMIT and adds nothing here
-- (same pattern as 20260915160000_workflow_runs_and_document_uploads.sql).
-- Any later index on these tables, once they hold data, must use CONCURRENTLY
-- in its own non-transactional migration.
BEGIN;

-- The domain migration 20260912130830 moved the physical runtime tables to
-- actions. The isolated/pre-domain database still keeps them in gateway_v2.
-- Resolve only these trusted schemas, preferring the canonical actions tables;
-- PostgreSQL binds foreign keys to their physical relation OIDs at creation.
SET LOCAL search_path TO actions, gateway_v2;
DO $$
DECLARE dependency text;
BEGIN
 FOREACH dependency IN ARRAY ARRAY['tasks', 'executions', 'results'] LOOP
  IF to_regclass(dependency) IS NULL THEN
   RAISE EXCEPTION 'Ask V2 prerequisite missing: %. Check the target project and apply the existing Gateway runtime migrations first.', dependency;
  END IF;
 END LOOP;
END $$;

CREATE TABLE IF NOT EXISTS gateway_v2.ask_runs (
 id uuid PRIMARY KEY,
 owner_id uuid NOT NULL,
 workspace_id uuid,
 -- Client idempotency key: the same request_id and question return this run.
 request_id uuid NOT NULL,
 question text NOT NULL CHECK (length(question) BETWEEN 1 AND 16000),
 question_sha256 text NOT NULL CHECK (question_sha256 ~ '^[0-9a-f]{64}$'),
 task_id uuid NOT NULL UNIQUE REFERENCES tasks(id) ON DELETE CASCADE,
 stage text NOT NULL CHECK (stage IN ('understanding','validating_intent','needs_clarification','finding_sources',
  'filtering_sources','ranking_sources','building_plan','awaiting_approval','executing','validating_results',
  'summarizing','verifying_answer','completed','partial','no_qualifying_sources','failed','cancelled')),
 question_revision integer NOT NULL DEFAULT 1 CHECK (question_revision >= 1),
 plan_revision integer CHECK (plan_revision IS NULL OR plan_revision >= 1),
 planning_passes integer NOT NULL DEFAULT 0 CHECK (planning_passes >= 0),
 -- Gapless event cursor: incremented in the same transaction as each event.
 last_sequence bigint NOT NULL DEFAULT 0 CHECK (last_sequence >= 0),
 -- Server-produced grounded intent (schema_version 2) and its validated
 -- planner input for the current question revision. Never client JSON.
 intent jsonb CHECK (intent IS NULL OR (jsonb_typeof(intent) = 'object' AND octet_length(intent::text) <= 262144)),
 planner_input jsonb CHECK (planner_input IS NULL OR (jsonb_typeof(planner_input) = 'object' AND octet_length(planner_input::text) <= 262144)),
 clarification jsonb CHECK (clarification IS NULL OR (jsonb_typeof(clarification) = 'object' AND octet_length(clarification::text) <= 16384)),
 gaps jsonb NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(gaps) = 'array' AND octet_length(gaps::text) <= 65536),
 -- Extraction/synthesis/repair attempts and provider usage; unknown is null, never zero.
 model_calls jsonb NOT NULL DEFAULT '{"extraction":0,"synthesis":0,"repair":0}'::jsonb
  CHECK (jsonb_typeof(model_calls) = 'object' AND octet_length(model_calls::text) <= 1024),
 model_usage jsonb CHECK (model_usage IS NULL OR (jsonb_typeof(model_usage) = 'object' AND octet_length(model_usage::text) <= 4096)),
 error jsonb CHECK (error IS NULL OR (jsonb_typeof(error) = 'object' AND octet_length(error::text) <= 2048)),
 -- Background planning/synthesis owner. Source execution keeps using
 -- gateway_v2.execution_leases for the task.
 lease_id uuid,
 lease_expires_at timestamptz,
 stage_started_at timestamptz NOT NULL DEFAULT now(),
 created_at timestamptz NOT NULL DEFAULT now(),
 updated_at timestamptz NOT NULL DEFAULT now(),
 expires_at timestamptz NOT NULL DEFAULT now() + interval '30 days',
 UNIQUE (owner_id, request_id)
);
CREATE INDEX IF NOT EXISTS ask_runs_owner_recent ON gateway_v2.ask_runs (owner_id, workspace_id, created_at DESC);
CREATE INDEX IF NOT EXISTS ask_runs_expiry ON gateway_v2.ask_runs (expires_at);
CREATE INDEX IF NOT EXISTS ask_runs_recovery ON gateway_v2.ask_runs (stage, lease_expires_at)
 WHERE stage NOT IN ('needs_clarification','awaiting_approval','completed','partial','no_qualifying_sources','failed','cancelled');

-- Durable progress log. sequence is gapless per run, starting at 1. A
-- dedupe_key makes derived events (e.g. a source row reaching a status)
-- idempotent across gateway instances and recovery.
CREATE TABLE IF NOT EXISTS gateway_v2.ask_run_events (
 run_id uuid NOT NULL REFERENCES gateway_v2.ask_runs(id) ON DELETE CASCADE,
 sequence bigint NOT NULL CHECK (sequence >= 1),
 owner_id uuid NOT NULL,
 question_revision integer NOT NULL CHECK (question_revision >= 1),
 plan_revision integer CHECK (plan_revision IS NULL OR plan_revision >= 1),
 stage text NOT NULL,
 dedupe_key text CHECK (dedupe_key IS NULL OR length(dedupe_key) BETWEEN 1 AND 300),
 event jsonb NOT NULL CHECK (jsonb_typeof(event) = 'object' AND octet_length(event::text) <= 16384),
 created_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY (run_id, sequence),
 UNIQUE (run_id, dedupe_key)
);

-- Immutable plan revisions. Each is exactly the resolver::Plan saved as the
-- run task's proposal; approval is the existing authorizations row for
-- (task_id, quote_ref), never a copy here.
CREATE TABLE IF NOT EXISTS gateway_v2.ask_run_plans (
 run_id uuid NOT NULL REFERENCES gateway_v2.ask_runs(id) ON DELETE CASCADE,
 plan_revision integer NOT NULL CHECK (plan_revision >= 1),
 question_revision integer NOT NULL CHECK (question_revision >= 1),
 plan_id uuid NOT NULL UNIQUE,
 owner_id uuid NOT NULL,
 task_id uuid NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
 quote_ref uuid NOT NULL,
 plan_hash text NOT NULL,
 cap_atomic bigint NOT NULL CHECK (cap_atomic >= 0),
 currency text NOT NULL CHECK (currency = 'USD'),
 status text NOT NULL CHECK (status IN ('complete','partial')),
 score_version text NOT NULL CHECK (length(score_version) BETWEEN 1 AND 100),
 -- The user-selected floor: no plan can be saved under a lower threshold.
 threshold double precision NOT NULL CHECK (threshold >= 90 AND threshold <= 100),
 catalog_version text NOT NULL,
 expires_at timestamptz NOT NULL,
 source_plan jsonb NOT NULL CHECK (jsonb_typeof(source_plan) = 'object' AND octet_length(source_plan::text) <= 524288),
 planner_input jsonb NOT NULL CHECK (jsonb_typeof(planner_input) = 'object' AND octet_length(planner_input::text) <= 262144),
 superseded_at timestamptz,
 created_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY (run_id, plan_revision),
 UNIQUE (task_id, quote_ref)
);

-- Score diagnostics for every candidate, including rejected ones (never shown
-- as recommended sources). Shadow-policy scores are kept for evaluation only.
CREATE TABLE IF NOT EXISTS gateway_v2.ask_run_scores (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 run_id uuid NOT NULL REFERENCES gateway_v2.ask_runs(id) ON DELETE CASCADE,
 owner_id uuid NOT NULL,
 planning_pass integer NOT NULL CHECK (planning_pass >= 1),
 question_revision integer NOT NULL CHECK (question_revision >= 1),
 plan_revision integer CHECK (plan_revision IS NULL OR plan_revision >= 1),
 candidate_id text NOT NULL CHECK (length(candidate_id) BETWEEN 1 AND 400),
 request_id text NOT NULL,
 contract_id uuid NOT NULL,
 endpoint_id uuid NOT NULL,
 source_id text NOT NULL,
 -- PostgreSQL orders NaN above every number, so the upper bound rejects it;
 -- the explicit check documents the rule.
 fit_score double precision CHECK (fit_score IS NULL OR (fit_score <> 'NaN'::double precision AND fit_score >= 0 AND fit_score <= 100)),
 score_version text NOT NULL,
 calibration_status text NOT NULL CHECK (calibration_status IN ('uncalibrated_heuristic','calibrated')),
 mode text NOT NULL CHECK (mode IN ('enforced','shadow')),
 threshold double precision NOT NULL CHECK (threshold >= 90 AND threshold <= 100),
 decision text NOT NULL CHECK (decision IN ('qualifies','below_threshold','unscorable','gate_failed')),
 record jsonb NOT NULL CHECK (jsonb_typeof(record) = 'object' AND octet_length(record::text) <= 65536),
 created_at timestamptz NOT NULL DEFAULT now(),
 -- Unrounded comparison: a stored qualifying decision always meets its threshold.
 CHECK (decision <> 'qualifies' OR (fit_score IS NOT NULL AND fit_score >= threshold)),
 UNIQUE (run_id, planning_pass, score_version, candidate_id)
);

-- Provenance for every validated value. Evidence ids are deterministic per
-- run ("ev-<n>"), so recovery re-validation inserts nothing new.
CREATE TABLE IF NOT EXISTS gateway_v2.ask_run_evidence (
 run_id uuid NOT NULL REFERENCES gateway_v2.ask_runs(id) ON DELETE CASCADE,
 evidence_id text NOT NULL CHECK (evidence_id ~ '^ev-[1-9][0-9]{0,8}$'),
 owner_id uuid NOT NULL,
 plan_revision integer NOT NULL CHECK (plan_revision >= 1),
 request_id text NOT NULL,
 subject_id text NOT NULL,
 field_id text NOT NULL CHECK (field_id LIKE '%#%'),
 step_id text NOT NULL,
 execution_id uuid NOT NULL REFERENCES executions(id) ON DELETE CASCADE,
 response_ref uuid NOT NULL REFERENCES results(id) ON DELETE CASCADE,
 response_pointer text NOT NULL CHECK (response_pointer = '' OR response_pointer LIKE '/%'),
 validation_status text NOT NULL CHECK (validation_status IN ('accepted','excluded')),
 outcome text NOT NULL CHECK (outcome IN ('supported','missing','ambiguous','conflicting','stale','invalid','source_failed')),
 exclusion_reason text CHECK (exclusion_reason IS NULL OR length(exclusion_reason) BETWEEN 1 AND 200),
 record jsonb NOT NULL CHECK (jsonb_typeof(record) = 'object' AND octet_length(record::text) <= 65536),
 created_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY (run_id, evidence_id),
 CHECK (validation_status = 'accepted' OR exclusion_reason IS NOT NULL)
);
CREATE INDEX IF NOT EXISTS ask_run_evidence_result ON gateway_v2.ask_run_evidence (response_ref);

-- The published answer of an executed plan revision. Reloads, presentation
-- changes and exports read this row; they never call a model or provider.
CREATE TABLE IF NOT EXISTS gateway_v2.ask_run_answers (
 run_id uuid NOT NULL REFERENCES gateway_v2.ask_runs(id) ON DELETE CASCADE,
 plan_revision integer NOT NULL CHECK (plan_revision >= 1),
 owner_id uuid NOT NULL,
 status text NOT NULL CHECK (status IN ('verified','repaired','deterministic_fallback')),
 model text NOT NULL,
 synthesis_calls integer NOT NULL CHECK (synthesis_calls >= 0),
 repair_calls integer NOT NULL CHECK (repair_calls BETWEEN 0 AND 1),
 answer jsonb NOT NULL CHECK (jsonb_typeof(answer) = 'object' AND octet_length(answer::text) <= 524288),
 field_outcomes jsonb NOT NULL CHECK (jsonb_typeof(field_outcomes) = 'array' AND octet_length(field_outcomes::text) <= 262144),
 usage jsonb CHECK (usage IS NULL OR (jsonb_typeof(usage) = 'object' AND octet_length(usage::text) <= 4096)),
 created_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY (run_id, plan_revision)
);

ALTER TABLE gateway_v2.ask_runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE gateway_v2.ask_run_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE gateway_v2.ask_run_plans ENABLE ROW LEVEL SECURITY;
ALTER TABLE gateway_v2.ask_run_scores ENABLE ROW LEVEL SECURITY;
ALTER TABLE gateway_v2.ask_run_evidence ENABLE ROW LEVEL SECURITY;
ALTER TABLE gateway_v2.ask_run_answers ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON gateway_v2.ask_runs, gateway_v2.ask_run_events, gateway_v2.ask_run_plans,
 gateway_v2.ask_run_scores, gateway_v2.ask_run_evidence, gateway_v2.ask_run_answers FROM PUBLIC, anon, authenticated;
DO $$ DECLARE t text; BEGIN
 IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'gateway_v2_runtime') THEN
  GRANT SELECT, INSERT, UPDATE, DELETE ON gateway_v2.ask_runs, gateway_v2.ask_run_events, gateway_v2.ask_run_plans,
   gateway_v2.ask_run_scores, gateway_v2.ask_run_evidence, gateway_v2.ask_run_answers TO gateway_v2_runtime;
  FOREACH t IN ARRAY ARRAY['ask_runs','ask_run_events','ask_run_plans','ask_run_scores','ask_run_evidence','ask_run_answers'] LOOP
   IF NOT EXISTS (SELECT 1 FROM pg_policies WHERE schemaname = 'gateway_v2' AND tablename = t AND policyname = 'runtime_access') THEN
    EXECUTE format('CREATE POLICY runtime_access ON gateway_v2.%I TO gateway_v2_runtime USING (true) WITH CHECK (true)', t);
   END IF;
  END LOOP;
 END IF;
END $$;
COMMIT;
