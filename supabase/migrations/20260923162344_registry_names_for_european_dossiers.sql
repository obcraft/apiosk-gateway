-- Registered identity must supply the name used by dependent screening.
-- No route, provider price, identifier or source admission is changed.
BEGIN;
UPDATE catalog.capability_execution_contracts
SET output_map = jsonb_set(output_map, '{produces,company.name}', '{"path":"$.company_name"}'::jsonb),
    produces = coalesce(produces, '[]'::jsonb) || CASE WHEN coalesce(produces, '[]'::jsonb) @> '[{"fact_type":"company.name"}]'::jsonb THEN '[]'::jsonb ELSE '[{"fact_type":"company.name","subject_role":"subject","cardinality":"one"}]'::jsonb END
WHERE id='ea445fe2-10d8-5892-bba5-8158cd6b3368'::uuid
  AND endpoint_id='15842cfa-d3f5-4b18-b6c3-45d9722695b9'::uuid
  AND capability_slug='company.registry-profile';
UPDATE catalog.capability_execution_contracts
SET output_map = jsonb_set(output_map, '{produces,company.name}', '{"path":"$.denomination"}'::jsonb),
    produces = coalesce(produces, '[]'::jsonb) || CASE WHEN coalesce(produces, '[]'::jsonb) @> '[{"fact_type":"company.name"}]'::jsonb THEN '[]'::jsonb ELSE '[{"fact_type":"company.name","subject_role":"subject","cardinality":"one"}]'::jsonb END
WHERE id='1e8fa5b7-2909-52e7-aee5-b1f8bb717f45'::uuid
  AND endpoint_id='bd7e87ef-21a9-53d0-93ee-94195d9aa584'::uuid
  AND capability_slug='company.registry-profile';

ALTER TABLE gateway_v2.workflow_runs DROP CONSTRAINT IF EXISTS workflow_runs_workflow_slug_check;
ALTER TABLE gateway_v2.workflow_runs ADD CONSTRAINT workflow_runs_workflow_slug_check CHECK(workflow_slug IN ('supplier-payment-check','prospect-shortlist','customer-onboarding','competitor-comparison','tender-finder','subsidy-finder','property-precheck','company-ownership','company-health','vehicle-precheck','company-news','european-company-dossier','tender-company-dossier'));
COMMIT;
