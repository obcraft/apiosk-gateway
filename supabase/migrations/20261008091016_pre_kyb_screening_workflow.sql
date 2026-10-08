-- Persist Pre-KYB Screening runs alongside the existing workflow catalog.
BEGIN;

ALTER TABLE gateway_v2.workflow_runs
  DROP CONSTRAINT IF EXISTS workflow_runs_workflow_slug_check;
ALTER TABLE gateway_v2.workflow_runs
  ADD CONSTRAINT workflow_runs_workflow_slug_check CHECK (workflow_slug IN (
    'supplier-payment-check',
    'prospect-shortlist',
    'customer-onboarding',
    'competitor-comparison',
    'tender-finder',
    'subsidy-finder',
    'property-precheck',
    'company-ownership',
    'company-health',
    'vehicle-precheck',
    'company-news',
    'european-company-dossier',
    'tender-company-dossier',
    'pre-kyb-screening'
  ));

COMMIT;
