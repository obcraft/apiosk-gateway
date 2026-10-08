-- Gateway v2 admits approved Bright Data requests before reserving buyer funds.
-- Share the durable provider job with the legacy fulfillment adapter; no public
-- API role can access the job or its upstream snapshot identifier.
BEGIN;
SELECT pg_advisory_xact_lock(hashtext('apiosk-brightdata-linkedin-v1'));

GRANT USAGE ON SCHEMA gateway_private TO gateway_v2_runtime;
REVOKE CREATE ON SCHEMA gateway_private FROM gateway_v2_runtime;
GRANT SELECT,INSERT,UPDATE ON gateway_private.brightdata_linkedin_jobs TO gateway_v2_runtime;
REVOKE DELETE,TRUNCATE,REFERENCES,TRIGGER ON gateway_private.brightdata_linkedin_jobs FROM gateway_v2_runtime;

-- Explicit operation policies keep deletion denied even if someone later adds
-- an accidental DELETE grant. Ambiguous paid submissions must be retained.
DROP POLICY IF EXISTS brightdata_v2_admission_read ON gateway_private.brightdata_linkedin_jobs;
CREATE POLICY brightdata_v2_admission_read ON gateway_private.brightdata_linkedin_jobs
  FOR SELECT TO gateway_v2_runtime USING (true);
DROP POLICY IF EXISTS brightdata_v2_admission_insert ON gateway_private.brightdata_linkedin_jobs;
CREATE POLICY brightdata_v2_admission_insert ON gateway_private.brightdata_linkedin_jobs
  FOR INSERT TO gateway_v2_runtime WITH CHECK (true);
DROP POLICY IF EXISTS brightdata_v2_admission_update ON gateway_private.brightdata_linkedin_jobs;
CREATE POLICY brightdata_v2_admission_update ON gateway_private.brightdata_linkedin_jobs
  FOR UPDATE TO gateway_v2_runtime USING (true) WITH CHECK (true);

COMMIT;
