-- Private, short-lived ownership of an approved execution. No credentials.
-- Follow the task table after the domain-schema migration; isolated older
-- installations can still have their task table in gateway_v2.
DO $migration$
DECLARE task_schema text;
BEGIN
 SELECT n.nspname INTO STRICT task_schema
 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
 WHERE c.relname='tasks' AND c.relkind='r'
   AND n.nspname IN ('actions','gateway_v2');
 EXECUTE format('CREATE TABLE %I.execution_leases (
  task_id uuid PRIMARY KEY REFERENCES %I.tasks(id) ON DELETE CASCADE,
  worker_id uuid NOT NULL,
  expires_at timestamptz NOT NULL
 )',task_schema,task_schema);
 EXECUTE format('ALTER TABLE %I.execution_leases ENABLE ROW LEVEL SECURITY',task_schema);
 EXECUTE format('REVOKE ALL ON %I.execution_leases FROM PUBLIC, anon, authenticated',task_schema);
 EXECUTE format('GRANT SELECT, INSERT, UPDATE, DELETE ON %I.execution_leases TO gateway_v2_runtime',task_schema);
 EXECUTE format('CREATE POLICY runtime_access ON %I.execution_leases TO gateway_v2_runtime USING (true) WITH CHECK (true)',task_schema);
END
$migration$;
