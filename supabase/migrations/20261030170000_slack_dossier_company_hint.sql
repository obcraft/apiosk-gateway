-- The company a Slack screening request names, detected from its text to
-- prefill the dossier's company search. A hint only, never a verified identity.
ALTER TABLE gateway_v2.slack_dossiers
  ADD COLUMN IF NOT EXISTS company_hint text CHECK (length(company_hint) <= 160);
