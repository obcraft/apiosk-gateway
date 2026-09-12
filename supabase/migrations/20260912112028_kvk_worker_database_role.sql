-- Separate worker login/pool; no access to customer, authorization or ledger data.
DO $$ BEGIN
 IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='kvk_worker_runtime') THEN
  CREATE ROLE kvk_worker_runtime NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
 END IF;
 IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='apiosk_kvk_worker') THEN
  CREATE ROLE apiosk_kvk_worker LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS CONNECTION LIMIT 12;
 END IF;
END $$;
GRANT kvk_worker_runtime TO apiosk_kvk_worker;
GRANT USAGE ON SCHEMA kvk_cache TO kvk_worker_runtime;
GRANT SELECT,INSERT ON kvk_cache.annual_accounts TO kvk_worker_runtime;
GRANT SELECT,INSERT,UPDATE ON kvk_cache.jobs,kvk_cache.rate_limit,kvk_cache.fetch_attempts TO kvk_worker_runtime;
GRANT SELECT,UPDATE ON kvk_cache.worker_slots TO kvk_worker_runtime;
DO $$ DECLARE t text; BEGIN
 FOREACH t IN ARRAY ARRAY['annual_accounts','jobs','worker_slots','rate_limit','fetch_attempts'] LOOP
  EXECUTE format('CREATE POLICY worker_access ON kvk_cache.%I TO kvk_worker_runtime USING (true) WITH CHECK (true)',t);
 END LOOP;
END $$;
