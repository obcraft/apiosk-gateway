-- Replace the administrative aliases with real tables and original views.
-- Names and OIDs are preserved. No table copies and no compatibility tables in public.
-- Deploy schema-aware API clients and SQL search paths BEFORE this migration.
SET LOCAL lock_timeout = '3s';
SET LOCAL statement_timeout = '120s';
CREATE TEMP TABLE domain_move_map(source_schema text, source_name text, target_schema text, old_alias text, source_oid oid, old_acl aclitem[], old_owner oid, old_rls boolean, old_filenode oid) ON COMMIT DROP;
INSERT INTO domain_move_map(source_schema,source_name,target_schema,old_alias) VALUES
('public','kyb_documents','kyb','documents'),
('public','kyb_events','kyb','events'),
('public','kyb_oauth_sessions','kyb','oauth_sessions'),
('public','kyb_oauth_tokens','kyb','oauth_tokens'),
('public','kyb_persons','kyb','persons'),
('public','kyb_profiles','kyb','profiles'),
('public','kyb_provider_secrets','kyb','provider_secrets'),
('public','kyb_providers','kyb','providers'),
('public','trust_attestations','kyb','trust_attestations'),
('public','admin_api_endpoints','providers','admin_api_endpoints'),
('public','admin_provider_apis','providers','admin_provider_apis'),
('public','admin_provider_users','providers','admin_provider_users'),
('public','api_endpoints','providers','api_endpoints'),
('public','api_monthly_volume','providers','api_monthly_volume'),
('public','api_payout_wallets','providers','api_payout_wallets'),
('public','api_usage_stats','providers','api_usage_stats'),
('public','apis','providers','apis'),
('public','developer_balance_summary','providers','developer_balance_summary'),
('public','developer_balances','providers','developer_balances'),
('public','developer_revenue','providers','developer_revenue'),
('public','endpoint_pricing_rules','providers','endpoint_pricing_rules'),
('public','offramp_mandates','providers','offramp_mandates'),
('public','provider_accounting_mappings','providers','accounting_mappings'),
('public','provider_accounting_settings','providers','accounting_settings'),
('public','provider_api_keys','providers','api_keys'),
('public','provider_api_usage_30d','providers','api_usage_30d'),
('public','provider_company_profiles','providers','company_profiles'),
('public','provider_compliance_profiles','providers','compliance_profiles'),
('public','provider_earnings_ledger','providers','earnings_ledger'),
('public','provider_endpoint_usage_daily','providers','endpoint_usage_daily'),
('public','provider_fiat_payout_accounts','providers','fiat_payout_accounts'),
('public','provider_gateway_request_traces','providers','gateway_request_traces'),
('public','provider_git_oauth_tokens','providers','git_oauth_tokens'),
('public','provider_integrations','providers','integrations'),
('public','provider_mollie_accounts','providers','mollie_accounts'),
('public','provider_notification_prefs','providers','notification_prefs'),
('public','provider_offramp_runs','providers','offramp_runs'),
('public','provider_payout_lines','providers','payout_lines'),
('public','provider_payout_owner_totals','providers','payout_owner_totals'),
('public','provider_payout_schedule','providers','payout_schedule'),
('public','provider_payout_summary','providers','payout_summary'),
('public','provider_profiles','providers','profiles'),
('public','provider_resource_usage_30d','providers','resource_usage_30d'),
('public','provider_sepa_held','providers','sepa_held'),
('public','provider_sepa_payable','providers','sepa_payable'),
('public','provider_settlement_ledger','providers','settlement_ledger'),
('public','provider_team_members','providers','team_members'),
('public','provider_usdc_payouts','providers','usdc_payouts'),
('public','resource_collections','providers','resource_collections'),
('public','resource_files','providers','resource_files'),
('public','resource_usage','providers','resource_usage'),
('public','source_providers','providers','source_providers'),
('public','capabilities','catalog','capabilities'),
('public','capability_execution_contracts','catalog','capability_execution_contracts'),
('public','capability_offers','catalog','capability_offers'),
('public','connector_listings','catalog','connector_listings'),
('public','discovery_agents','catalog','discovery_agents'),
('public','discovery_sources','catalog','discovery_sources'),
('public','fact_subject_kinds','catalog','fact_subject_kinds'),
('public','fact_types','catalog','fact_types'),
('public','federated_execution_readiness','catalog','federated_execution_readiness'),
('public','param_resolvers','catalog','param_resolvers'),
('public','call_runs','actions','call_runs'),
('public','comparisons','actions','comparisons'),
('public','decision_outcomes','actions','decision_outcomes'),
('public','decisions','actions','decisions'),
('public','external_x402_fetches','actions','external_x402_fetches'),
('public','gateway_idempotency_keys','actions','gateway_idempotency_keys'),
('public','user_selected_apis','actions','user_selected_apis'),
('public','x402_host_policy','actions','x402_host_policy'),
('public','x402_proof_nonces','actions','x402_proof_nonces'),
('gateway_v2','authorizations','actions','authorizations'),
('gateway_v2','executions','actions','executions'),
('gateway_v2','requests','actions','requests'),
('gateway_v2','results','actions','results'),
('gateway_v2','tasks','actions','tasks'),
('public','balance_charges','billing','balance_charges'),
('public','balance_grant_policies','billing','balance_grant_policies'),
('public','balance_grants','billing','balance_grants'),
('public','balance_topups','billing','balance_topups'),
('public','commission_tiers','billing','commission_tiers'),
('public','credit_ledger','billing','credit_ledger'),
('public','fiat_customer_profiles','billing','fiat_customer_profiles'),
('public','fiat_daily_spend_counters','billing','fiat_daily_spend_counters'),
('public','fiat_purchase_payments','billing','fiat_purchase_payments'),
('public','fx_rates','billing','fx_rates'),
('public','gateway_payment_attempts','billing','gateway_payment_attempts'),
('public','job_budget_reservations','billing','job_budget_reservations'),
('public','job_charge_allocations','billing','job_charge_allocations'),
('public','payment_intents','billing','payment_intents'),
('public','payment_links','billing','payment_links'),
('public','payout_requests','billing','payout_requests'),
('public','payout_wallets','billing','payout_wallets'),
('public','purchase_approvals','billing','purchase_approvals'),
('public','purchase_policies','billing','purchase_policies'),
('public','purchase_policy_decisions','billing','purchase_policy_decisions'),
('public','reconciliation_daily_summary','billing','reconciliation_daily_summary'),
('public','reconciliation_settlements_without_usage','billing','reconciliation_settlements_without_usage'),
('public','reconciliation_stuck_settlements','billing','reconciliation_stuck_settlements'),
('public','reconciliation_unsettled_paid_calls','billing','reconciliation_unsettled_paid_calls'),
('public','resell_payments','billing','resell_payments'),
('public','sepa_provider_earnings','billing','sepa_provider_earnings'),
('public','sepa_settlement_batches','billing','sepa_settlement_batches'),
('public','sepa_settlement_buyer_balances','billing','sepa_settlement_buyer_balances'),
('public','sepa_settlement_ledger','billing','sepa_settlement_ledger'),
('public','settlement_config','billing','settlement_config'),
('public','settlements','billing','settlements'),
('public','user_balances','billing','user_balances'),
('public','user_credit_balances','billing','user_credit_balances'),
('public','agent_authorizations','agents','authorizations'),
('public','agent_code_attempts','agents','code_attempts'),
('public','agent_connection_intents','agents','connection_intents'),
('public','agent_connection_invites','agents','connection_invites'),
('public','agent_connection_state','agents','connection_state'),
('public','agent_connections','agents','connections'),
('public','agent_intents','agents','intents'),
('public','agent_presence_sessions','agents','presence_sessions'),
('public','agent_tokens','agents','tokens'),
('public','mcp_oauth_events','agents','mcp_oauth_events'),
('public','mcp_sessions','agents','mcp_sessions'),
('public','mcp_settlements','agents','mcp_settlements'),
('public','mcp_tool_calls','agents','mcp_tool_calls'),
('public','agent_wallet_connect_token_wallets','wallets','agent_wallet_connect_token_wallets'),
('public','agent_wallet_connect_tokens','wallets','agent_wallet_connect_tokens'),
('public','agent_wallet_stale_connect_tokens','wallets','agent_wallet_stale_connect_tokens'),
('public','agent_wallet_transactions','wallets','agent_wallet_transactions'),
('public','agent_wallet_user_secrets','wallets','agent_wallet_user_secrets'),
('public','agent_wallets','wallets','agent_wallets'),
('public','network_tokens','wallets','network_tokens'),
('public','network_tokens_view','wallets','network_tokens_view'),
('public','networks','wallets','networks'),
('public','tokens','wallets','tokens'),
('public','wallet_accounts','wallets','wallet_accounts'),
('public','wallet_auth_nonces','wallets','wallet_auth_nonces'),
('public','ask_conversations','research','ask_conversations'),
('public','ask_messages','research','ask_messages'),
('public','research_artifacts','research','artifacts'),
('public','research_job_artifacts','research','job_artifacts'),
('public','research_job_events','research','job_events'),
('public','research_job_facts','research','job_facts'),
('public','research_job_nodes','research','job_nodes'),
('public','research_job_refs','research','job_refs'),
('public','research_jobs','research','jobs'),
('public','research_plan_versions','research','plan_versions'),
('public','research_plans','research','plans'),
('public','connect_api_keys','integrations','api_keys'),
('public','connect_audit_log','integrations','audit_log'),
('public','connect_marketplaces','integrations','marketplaces'),
('public','connect_merchants','integrations','merchants'),
('public','connect_oauth_clients','integrations','oauth_clients'),
('public','connect_oauth_codes','integrations','oauth_codes'),
('public','connect_payments','integrations','payments'),
('public','connect_payouts','integrations','payouts'),
('public','connect_skills','integrations','skills'),
('public','connect_wallets','integrations','wallets'),
('public','github_connections','integrations','github_connections'),
('public','v_connect_marketplace_fee_due','integrations','v_connect_marketplace_fee_due'),
('public','webmcp_sites','integrations','webmcp_sites'),
('public','webshop_integrations','integrations','webshop_integrations'),
('public','admin_gateway_traffic','observability','admin_gateway_traffic'),
('public','admin_gateway_traffic_v2','observability','admin_gateway_traffic_v2'),
('public','dashboard_visits','observability','dashboard_visits'),
('public','discovery_impressions_daily','observability','discovery_impressions_daily'),
('public','discovery_reads_daily','observability','discovery_reads_daily'),
('public','endpoint_health','observability','endpoint_health'),
('public','federated_referral_clicks','observability','federated_referral_clicks'),
('public','federated_traffic_daily','observability','federated_traffic_daily'),
('public','gateway_discovery_events','observability','discovery_events'),
('public','gateway_request_daily_rollups','observability','request_daily_rollups'),
('public','gateway_request_events','observability','request_events'),
('public','gateway_request_logs','observability','request_logs'),
('public','gateway_requests_v2','observability','requests_v2'),
('public','gateway_upstream_attempts','observability','upstream_attempts'),
('public','gateway_usage','observability','usage'),
('public','launch_events','observability','launch_events'),
('public','webmcp_injections_daily','observability','webmcp_injections_daily'),
('public','contact_messages','support','contact_messages'),
('public','dashboard_feedback','support','dashboard_feedback'),
('public','support_thread_messages','support','thread_messages'),
('public','support_threads','support','threads'),
('public','learn_categories','content','categories'),
('public','learn_posts','content','posts'),
('public','api_review_email_outbox','operations','api_review_email_outbox'),
('public','apiosk_alignment_markers','operations','alignment_markers'),
('public','apiosk_endpoint_hide_snapshots','operations','endpoint_hide_snapshots'),
('public','apiosk_hide_snapshots','operations','hide_snapshots'),
('public','platform_admins','identity','platform_admins'),
('public','user_account_lifecycle','identity','account_lifecycle'),
('public','user_preferences','identity','preferences');
UPDATE domain_move_map m SET source_oid=c.oid,old_acl=c.relacl,old_owner=c.relowner,old_rls=c.relrowsecurity,old_filenode=c.relfilenode
FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
WHERE n.nspname=m.source_schema AND c.relname=m.source_name AND c.relkind IN ('r','p','v','m');
DO $migration$
#variable_conflict use_column
DECLARE m record; r record; f record; ddl text; original_ddl text; setting text; reference_pattern text;
BEGIN
  IF EXISTS(SELECT 1 FROM domain_move_map WHERE source_oid IS NULL) THEN
    RAISE EXCEPTION 'Missing canonical source: schema drift; abort before changing anything';
  END IF;
  IF EXISTS(SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND c.relkind IN ('r','p','v','m') AND NOT EXISTS(SELECT 1 FROM domain_move_map m WHERE m.source_oid=c.oid)) THEN
    RAISE EXCEPTION 'Unmapped public relation: update reviewed mapping first';
  END IF;
  -- All 189 aliases must still be invoker views directly dependent on the source.
  FOR m IN SELECT * FROM domain_move_map ORDER BY source_oid LOOP
    IF NOT EXISTS(SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
      WHERE n.nspname=m.target_schema AND c.relname=m.old_alias AND c.relkind='v'
      AND 'security_invoker=true'=ANY(c.reloptions)
      AND EXISTS(SELECT 1 FROM pg_rewrite rw JOIN pg_depend dp ON dp.classid='pg_rewrite'::regclass AND dp.objid=rw.oid
        WHERE rw.ev_class=c.oid AND dp.refclassid='pg_class'::regclass AND dp.refobjid=m.source_oid)) THEN
      RAISE EXCEPTION 'Directory alias changed: %.%',m.target_schema,m.old_alias;
    END IF;
  END LOOP;
  -- No CASCADE: any unexpected dependency aborts the transaction.
  FOR m IN SELECT * FROM domain_move_map ORDER BY source_oid LOOP
    EXECUTE format('DROP VIEW %I.%I',m.target_schema,m.old_alias);
  END LOOP;
  FOR m IN SELECT * FROM domain_move_map ORDER BY source_oid LOOP
    -- Preserve effective source access. No table grants are added or widened.
    FOR r IN SELECT rolname FROM pg_roles WHERE NOT rolsuper
      AND has_schema_privilege(oid,m.source_schema,'USAGE')
      AND has_table_privilege(oid,m.source_oid,'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER') LOOP
      EXECUTE format('GRANT USAGE ON SCHEMA %I TO %I',m.target_schema,r.rolname);
    END LOOP;
    SELECT relkind INTO setting FROM pg_class WHERE oid=m.source_oid;
    EXECUTE format('ALTER %s %I.%I SET SCHEMA %I',CASE WHEN setting='v' THEN 'VIEW' WHEN setting='m' THEN 'MATERIALIZED VIEW' ELSE 'TABLE' END,m.source_schema,m.source_name,m.target_schema);
  END LOOP;
  -- Parsed FK/view/trigger dependencies follow the unchanged relation OIDs.
  -- PL/pgSQL and SQL function text must be rewritten explicitly.
  FOR f IN SELECT p.oid,p.proconfig,n.nspname,pg_get_functiondef(p.oid) AS definition
    FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
    WHERE p.prokind IN ('f','p') AND n.nspname IN ('public','gateway_v2','kvk_cache')
    AND NOT EXISTS(SELECT 1 FROM pg_depend d WHERE d.classid='pg_proc'::regclass AND d.objid=p.oid AND d.deptype='e') LOOP
    ddl:=f.definition;
    FOR m IN SELECT * FROM domain_move_map ORDER BY length(source_name) DESC LOOP
      reference_pattern := '\m' || m.source_schema || '[.]' || m.source_name || '\M';
      ddl:=regexp_replace(ddl,reference_pattern,format('%I.%I',m.target_schema,m.source_name),'g');
      ddl:=replace(ddl,format('"%s"."%s"',m.source_schema,m.source_name),format('%I.%I',m.target_schema,m.source_name));
      -- Three RPC capability probes inspect information_schema by schema/name.
      ddl:=regexp_replace(ddl,'table_schema = '''||m.source_schema||'''(\s+and table_name\s*=\s*'''||m.source_name||''')','table_schema = '''||m.target_schema||'''\1','gi');
    END LOOP;
    IF ddl IS DISTINCT FROM f.definition THEN EXECUTE ddl; END IF;
    IF f.nspname IN ('public','gateway_v2') THEN
      SELECT substring(x FROM length('search_path=')+1) INTO setting FROM unnest(f.proconfig) x WHERE x LIKE 'search_path=%';
      -- Empty search paths remain empty: their references are fully qualified.
      IF setting IS NULL OR setting NOT IN ('','""') THEN
        setting := regexp_replace(coalesce(setting,'public, extensions'),'\mpublic\M','public, gateway_v2, kyb, providers, catalog, actions, billing, agents, wallets, research, integrations, observability, support, content, operations, identity, extensions','g');
        EXECUTE format('ALTER %s %s SET search_path TO %s',CASE WHEN (SELECT prokind FROM pg_proc WHERE oid=f.oid)='p' THEN 'PROCEDURE' ELSE 'FUNCTION' END,f.oid::regprocedure,setting);
      END IF;
    END IF;
  END LOOP;
  IF EXISTS(SELECT 1 FROM domain_move_map m JOIN pg_class c ON c.oid=m.source_oid JOIN pg_namespace n ON n.oid=c.relnamespace
    WHERE n.nspname<>m.target_schema OR c.relname<>m.source_name OR c.relacl IS DISTINCT FROM m.old_acl
      OR c.relowner<>m.old_owner OR c.relrowsecurity<>m.old_rls OR c.relfilenode<>m.old_filenode) THEN
    RAISE EXCEPTION 'Physical identity, grants, owner or RLS changed unexpectedly';
  END IF;
END
$migration$;
COMMENT ON SCHEMA kyb IS 'Bedrijfsverificatie, documenten, personen en KYB-providerkoppelingen. Canonical physical tables and domain views.';
COMMENT ON SCHEMA providers IS 'Providers, API-aanbod, endpoints, providerinstellingen en provideradministratie. Canonical physical tables and domain views.';
COMMENT ON SCHEMA catalog IS 'Capabilities, uitvoercontracten, feiten en discovery. Canonical physical tables and domain views.';
COMMENT ON SCHEMA actions IS 'Selecties, vergelijkingen, beslissingen en bronuitvoering. Canonical physical tables and domain views.';
COMMENT ON SCHEMA billing IS 'Kopersaldo, betalingen, goedkeuringen, afrekening en uitbetalingen. Canonical physical tables and domain views.';
COMMENT ON SCHEMA agents IS 'Agentverbindingen, toestemming, MCP-sessies en toolaanroepen. Canonical physical tables and domain views.';
COMMENT ON SCHEMA wallets IS 'Wallets, wallettransacties, netwerken en tokens. Canonical physical tables and domain views.';
COMMENT ON SCHEMA research IS 'Onderzoeksplannen, jobs, resultaten en gesprekken. Canonical physical tables and domain views.';
COMMENT ON SCHEMA integrations IS 'Marketplace Connect, webshops, GitHub en WebMCP-koppelingen. Canonical physical tables and domain views.';
COMMENT ON SCHEMA observability IS 'Verkeer, requestlogs, health en gebruiksmetingen. Canonical physical tables and domain views.';
COMMENT ON SCHEMA support IS 'Supportgesprekken, contact en feedback. Canonical physical tables and domain views.';
COMMENT ON SCHEMA content IS 'Kennisbankartikelen en categorieën. Canonical physical tables and domain views.';
COMMENT ON SCHEMA operations IS 'Beheerregistraties, snapshots en operationele berichten. Canonical physical tables and domain views.';
COMMENT ON SCHEMA identity IS 'Gebruikerslevenscyclus, voorkeuren en platformbeheerders. Canonical physical tables and domain views.';
ALTER ROLE authenticator SET pgrst.db_schemas = 'public, graphql_public, kyb, providers, catalog, actions, billing, agents, wallets, research, integrations, observability, support, content, operations, identity';
NOTIFY pgrst, 'reload config';
NOTIFY pgrst, 'reload schema';
