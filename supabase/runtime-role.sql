-- Run once by a DBA AFTER the migration. No passwords or application deploys.
-- Grant this NOLOGIN role to a dedicated server login managed in your secrets.
DO $$ BEGIN
 IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='gateway_v2_runtime') THEN
  CREATE ROLE gateway_v2_runtime NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
 END IF;
END $$;
GRANT USAGE ON SCHEMA gateway_v2,public TO gateway_v2_runtime;
GRANT SELECT,INSERT,UPDATE,DELETE ON ALL TABLES IN SCHEMA gateway_v2 TO gateway_v2_runtime;
-- The trusted service serves many accounts; HTTP principals and every query
-- enforce ownership. This role is never exposed through the Data API/client.
DO $$ DECLARE t text; BEGIN
 FOREACH t IN ARRAY ARRAY['tasks','requests','authorizations','executions','results'] LOOP
  EXECUTE format('DROP POLICY IF EXISTS runtime_access ON gateway_v2.%I',t);
  EXECUTE format('CREATE POLICY runtime_access ON gateway_v2.%I TO gateway_v2_runtime USING (true) WITH CHECK (true)',t);
 END LOOP;
END $$;
REVOKE SELECT ON public.fact_types,public.capabilities,public.capability_execution_contracts,
 public.apis,public.api_endpoints,public.balance_charges,public.call_runs FROM gateway_v2_runtime;
GRANT SELECT(slug,status,value_schema,jurisdiction) ON public.fact_types TO gateway_v2_runtime;
GRANT SELECT(slug,status) ON public.capabilities TO gateway_v2_runtime;
GRANT SELECT(id,endpoint_id,capability_slug,version,status,requires,input_map,output_map,identifies,
 cardinality,max_fan_out,fixed_params,preconditions,pagination,freshness_seconds,sample_payload,
 validated_at,effect,price_model) ON public.capability_execution_contracts TO gateway_v2_runtime;
GRANT SELECT(id,slug,name,listing_type,status,active,is_verified,discoverable,suspended_at,docs_url,website_url,
 description,category,listing_metadata,owner_id)
 ON public.apis TO gateway_v2_runtime;
GRANT SELECT(id,username,avatar_url) ON public.provider_profiles TO gateway_v2_runtime;
DROP POLICY IF EXISTS gateway_v2_public_provider_handles ON public.provider_profiles;
CREATE POLICY gateway_v2_public_provider_handles ON public.provider_profiles FOR SELECT TO gateway_v2_runtime
USING (EXISTS (SELECT 1 FROM public.apis a WHERE a.owner_id=provider_profiles.id
 AND a.status='active' AND coalesce(a.active,true) AND a.is_verified=true
 AND coalesce(a.discoverable,true) AND a.suspended_at IS NULL));
GRANT SELECT(id,api_id,method,path,price,active) ON public.api_endpoints TO gateway_v2_runtime;
GRANT SELECT(id,user_id,idempotency_key,status,charged_micro_usd,actual_provider_micro_usd,provider_price_micro_usd,fee_micro_usd) ON public.balance_charges TO gateway_v2_runtime;
GRANT SELECT(user_id,balance_micro_usd) ON public.user_balances TO gateway_v2_runtime;
DROP POLICY IF EXISTS gateway_v2_runtime_balance_read ON public.user_balances;
CREATE POLICY gateway_v2_runtime_balance_read ON public.user_balances FOR SELECT TO gateway_v2_runtime USING (true);
GRANT SELECT(charge_id,user_id,status,output,output_truncated,started_at) ON public.call_runs TO gateway_v2_runtime;
DO $$ DECLARE t text; BEGIN
 FOREACH t IN ARRAY ARRAY['fact_types','capabilities','capability_execution_contracts','apis','api_endpoints','balance_charges','call_runs'] LOOP
  EXECUTE format('DROP POLICY IF EXISTS gateway_v2_runtime_read ON public.%I',t);
  EXECUTE format('CREATE POLICY gateway_v2_runtime_read ON public.%I FOR SELECT TO gateway_v2_runtime USING (true)',t);
 END LOOP;
END $$;
GRANT INSERT(id,user_id,connection_id,prompt,kind,source,name,capability,api_slug,
 candidate_id,method,provider_price_micro_usd,charged_micro_usd,metadata)
 ON public.user_selected_apis TO gateway_v2_runtime;
DROP POLICY IF EXISTS gateway_v2_runtime_selection ON public.user_selected_apis;
CREATE POLICY gateway_v2_runtime_selection ON public.user_selected_apis
 FOR INSERT TO gateway_v2_runtime WITH CHECK (true);
-- Pacing joins the execution journal to the selected endpoint identity only.
-- No input metadata, prompts, credentials or user balances become readable.
GRANT SELECT(id,candidate_id) ON public.user_selected_apis TO gateway_v2_runtime;
DROP POLICY IF EXISTS gateway_v2_runtime_selection_identity ON public.user_selected_apis;
CREATE POLICY gateway_v2_runtime_selection_identity ON public.user_selected_apis
 FOR SELECT TO gateway_v2_runtime USING (true);
GRANT EXECUTE ON FUNCTION public.use_agent_token(text),public.use_agent_connection(text) TO gateway_v2_runtime;
-- Deliberately no UPDATE on user_balances, no balance reservation RPC grants,
-- no treasury access and no access to auth.users or stored provider secrets.
