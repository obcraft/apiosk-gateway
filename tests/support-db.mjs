// Local PostgreSQL wire adapter for SQLx integration tests. PGlite is the
// existing App test dependency. One connection only: production concurrency
// must additionally be exercised against a real multi-connection Postgres.
import net from 'node:net';
import fs from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { PGlite } from '../../app/node_modules/@electric-sql/pglite/dist/index.js';
const db=new PGlite();await db.waitReady;
await db.exec('CREATE ROLE anon; CREATE ROLE authenticated;');
// The durable-flow fixture needs only the v2 runtime schema. Provider-onboarding
// migrations target the complete production catalogue and deliberately assert
// that production-owned provider rows already exist; replaying those into this
// minimal database would make an unrelated new source break the core flow test.
await db.exec(await fs.readFile(new URL('../supabase/migrations/20260907094100_gateway_v2_state.sql',import.meta.url),'utf8'));
await db.exec(`CREATE TABLE public.user_selected_apis(id uuid primary key,user_id uuid,connection_id uuid,prompt text,kind text,source text,name text,capability text,api_slug text,candidate_id uuid,resource_url text,method text,provider_price_micro_usd bigint,charged_micro_usd bigint,metadata jsonb);
CREATE TABLE public.balance_charges(id uuid primary key,user_id uuid,idempotency_key text,status text,charged_micro_usd bigint,actual_provider_micro_usd bigint,provider_price_micro_usd bigint,fee_micro_usd bigint,metadata jsonb,created_at timestamptz default now(),job_id uuid);
CREATE TABLE public.agent_connections(id uuid primary key,user_id uuid,revoked_at timestamptz,per_call_limit_micro_usd bigint,daily_limit_micro_usd bigint);
CREATE TABLE public.job_budget_reservations(user_id uuid,connection_id uuid,spent_micro_usd bigint,reserved_micro_usd bigint,created_at timestamptz,status text);
CREATE TABLE public.user_balances(user_id uuid primary key,balance_micro_usd bigint,account_id uuid GENERATED ALWAYS AS (user_id) STORED);
CREATE TABLE public.call_runs(id uuid primary key,charge_id uuid,user_id uuid,status text,output jsonb,output_truncated boolean,started_at timestamptz default now());
CREATE TABLE public.fact_types(slug text primary key,status text,value_schema jsonb,jurisdiction text);
CREATE TABLE public.capabilities(slug text primary key,status text);
CREATE TABLE public.provider_profiles(id uuid primary key, username text, avatar_url text);
CREATE TABLE public.apis(id uuid primary key,owner_id uuid,slug text,name text,listing_type text,status text,active boolean,is_verified boolean,discoverable boolean,suspended_at timestamptz,docs_url text,website_url text,description text DEFAULT 'Company filings',category text DEFAULT 'finance',listing_metadata jsonb DEFAULT '{"tags":["registry"]}');
CREATE TABLE public.api_endpoints(id uuid primary key,api_id uuid,method text,path text,price numeric,active boolean);
CREATE TABLE public.capability_execution_contracts(id uuid primary key,endpoint_id uuid,capability_slug text,version int,status text,requires jsonb,input_map jsonb,output_map jsonb,identifies text,cardinality text,max_fan_out int,fixed_params jsonb,preconditions jsonb,pagination jsonb,freshness_seconds int,sample_payload jsonb,validated_at timestamptz,effect text,price_model text);`);
// Scope checks for isolated purchase tests; the real tenancy and billing RPCs
// are intentionally not replaced in production or called by this fixture.
await db.exec(`CREATE TABLE public.fixture_workspace_access(actor_id uuid,workspace_id uuid,can_spend boolean,primary key(actor_id,workspace_id));
CREATE FUNCTION public.workspace_gateway_context(actor uuid,connection uuid,workspace uuid) RETURNS jsonb LANGUAGE plpgsql AS $$
DECLARE allowed boolean; BEGIN
 SELECT can_spend INTO allowed FROM public.fixture_workspace_access WHERE actor_id=actor AND workspace_id=workspace;
 IF NOT FOUND THEN RAISE EXCEPTION 'forbidden' USING ERRCODE='42501'; END IF;
 RETURN jsonb_build_object('workspace_id',workspace,'can_spend',allowed);
END $$;`);
const fixtureHash=createHash('sha256').update('apk_access_local_fixture').digest('hex');
await db.exec(`CREATE FUNCTION public.use_agent_token(text) RETURNS TABLE(status text,owner_id uuid,connection_id uuid) LANGUAGE sql AS $$ SELECT 'ok'::text,'00000000-0000-4000-8000-000000000001'::uuid,'00000000-0000-4000-8000-000000000002'::uuid WHERE $1='${fixtureHash}' $$;
CREATE FUNCTION public.use_agent_connection(text) RETURNS TABLE(owner_id uuid,connection_id uuid) LANGUAGE sql AS $$ SELECT NULL::uuid,NULL::uuid WHERE false $$;`);
// Organisation API keys (App migration 20260922160000): a full-scope key, a
// read-only key and an expired key, each a connection of account ...0001.
const sha=value=>createHash('sha256').update(value).digest('hex');
const orgKey=sha('apk_org_live_'+'a'.repeat(48)),readKey=sha('apk_org_live_'+'b'.repeat(48)),expiredKey=sha('apk_org_live_'+'c'.repeat(48));
await db.exec(`CREATE FUNCTION public.use_organisation_api_key(text) RETURNS TABLE(status text,key_id uuid,organisation_id uuid,owner_id uuid,connection_id uuid,name text,provider text,scopes text[],per_call_limit_micro_usd bigint,daily_limit_micro_usd bigint,monthly_limit_micro_usd bigint,expires_at timestamptz) LANGUAGE sql AS $$
 SELECT 'ok'::text,'00000000-0000-4000-8000-000000000011'::uuid,'00000000-0000-4000-8000-000000000012'::uuid,'00000000-0000-4000-8000-000000000001'::uuid,'00000000-0000-4000-8000-000000000003'::uuid,'Nomi Production'::text,'apiosk-api'::text,ARRAY['sources:read','sources:execute','ask:create','ask:execute','billing:read'],1000000::bigint,2000000::bigint,2000000::bigint,now()+interval '1 day' WHERE $1='${orgKey}'
 UNION ALL SELECT 'ok','00000000-0000-4000-8000-000000000013','00000000-0000-4000-8000-000000000012','00000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000004','Read only','apiosk-api',ARRAY['sources:read'],1000000,2000000,2000000,now()+interval '1 day' WHERE $1='${readKey}'
 UNION ALL SELECT 'expired','00000000-0000-4000-8000-000000000014',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,now()-interval '1 day' WHERE $1='${expiredKey}' $$;
CREATE FUNCTION public.organisation_api_key_usage(uuid,integer) RETURNS jsonb LANGUAGE sql AS $$ SELECT jsonb_build_object('key_id',$1,'balance_micro_usd',(SELECT balance_micro_usd::text FROM public.user_balances WHERE user_id='00000000-0000-4000-8000-000000000001'),'month_start',date_trunc('month',now()),
 'month_spent_micro_usd',(SELECT coalesce(sum(charged_micro_usd),0)::text FROM public.balance_charges WHERE metadata->>'connection_id'='00000000-0000-4000-8000-000000000003' AND status<>'refunded'),'charges','[]'::jsonb) $$;`);
await db.exec(await fs.readFile(new URL('../supabase/runtime-role.sql',import.meta.url),'utf8'));
await db.exec('GRANT SELECT(account_id) ON public.user_balances TO gateway_v2_runtime');
await db.exec(await fs.readFile(new URL('../supabase/migrations/20260908104136_gateway_v2_connection_approval.sql',import.meta.url),'utf8'));
await db.exec("INSERT INTO agent_connections VALUES('00000000-0000-4000-8000-000000000002','00000000-0000-4000-8000-000000000001',NULL,1000000,2000000),('00000000-0000-4000-8000-000000000003','00000000-0000-4000-8000-000000000001',NULL,1000000,2000000),('00000000-0000-4000-8000-000000000004','00000000-0000-4000-8000-000000000001',NULL,1000000,2000000); INSERT INTO user_balances(user_id,balance_micro_usd) VALUES('00000000-0000-4000-8000-000000000001',2000000);");
await db.exec(await fs.readFile(new URL('../supabase/migrations/20260907124538_gateway_v2_source_catalog.sql',import.meta.url),'utf8'));
await db.exec(await fs.readFile(new URL('../supabase/migrations/20260907125545_gateway_v2_source_identity.sql',import.meta.url),'utf8'));
const snapshot=JSON.parse(await fs.readFile(new URL('./fixtures/integration-catalog.json',import.meta.url),'utf8'));
for(const f of snapshot.vocabulary) await db.query("INSERT INTO fact_types VALUES($1,'active',$2,NULL)",[f.slug,JSON.stringify(f.value_schema)]);
for(const s of snapshot.services) {
 await db.query("INSERT INTO capabilities VALUES($1,'active') ON CONFLICT DO NOTHING",[s.contract.capability_slug]);
 await db.query("INSERT INTO apis(id,slug,name,listing_type,status,active,is_verified,discoverable,suspended_at,docs_url,website_url) VALUES($1,$2,$3,'api','active',true,true,true,NULL,NULL,NULL) ON CONFLICT DO NOTHING",[s.id,s.api_slug,s.name]);
 await db.query("INSERT INTO api_endpoints VALUES($1,$2,$3,$4,$5,true)",[s.endpoint_id,s.id,s.method,s.path,s.list_atomic/1000000]);
 await db.query("INSERT INTO capability_execution_contracts VALUES($1,$2,$3,$4,'active',$5,$6,$7,$8,$9,$10,$11,'{}','{}',3600,$12,now(),'read','per_call')",[s.id,s.endpoint_id,s.contract.capability_slug,s.contract.version,JSON.stringify(s.contract.requires),JSON.stringify(s.contract.input_map),JSON.stringify(s.contract.output_map),s.contract.identifies??null,s.contract.cardinality,s.contract.max_fan_out,JSON.stringify(s.fixed_params),JSON.stringify(s.sample_payload)]);
}
await db.exec('ALTER TABLE public.apis ADD COLUMN provider_api_key_encrypted text;');
// Production catalog metadata added since the original flow fixture.
await db.exec(`ALTER TABLE public.api_endpoints
 ADD COLUMN description text DEFAULT 'Documented fixture endpoint',
 ADD COLUMN tags text[] DEFAULT ARRAY['registry'],
 ADD COLUMN endpoint_category text DEFAULT 'finance',
 ADD COLUMN metadata_source text DEFAULT 'curated',
 ADD COLUMN query_params jsonb DEFAULT '{"type":"object","properties":{}}',
 ADD COLUMN request_body jsonb,
 ADD COLUMN origin_override text,
 ADD COLUMN external_docs_url text;
 ALTER TABLE public.capabilities ADD COLUMN description text DEFAULT 'Fixture capability';
`);
await db.exec(await fs.readFile(new URL("../supabase/migrations/20260911113528_source_readiness_audit_access.sql",import.meta.url),"utf8"));
// These public endpoint metadata columns are included in the production
// runtime grant. Include them before creating the restricted provider view.
await db.exec('GRANT SELECT(origin_override,external_docs_url) ON public.api_endpoints TO gateway_v2_runtime');

function msg(type,body=Buffer.alloc(0)){const header=Buffer.alloc(5);header[0]=type.charCodeAt(0);header.writeUInt32BE(body.length+4,1);return Buffer.concat([header,body]);}
const status=(k,v)=>msg('S',Buffer.from(k+'\0'+v+'\0'));
let occupied=false;
const server=net.createServer(socket=>{
 if(occupied){socket.destroy();return;}occupied=true;
 let buffer=Buffer.alloc(0),started=false,chain=Promise.resolve();
 socket.on('error',()=>{});socket.on('close',()=>{chain.finally(async()=>{await db.exec('ROLLBACK');await db.exec('DISCARD ALL');occupied=false;}).catch(e=>{console.error(e.message);occupied=false;});});
 socket.on('data',chunk=>{buffer=Buffer.concat([buffer,chunk]);drain();});
 function drain(){
  if(!started){if(buffer.length<8)return;const n=buffer.readUInt32BE(0);if(buffer.length<n)return;const code=buffer.readUInt32BE(4);buffer=buffer.subarray(n);if(code===80877103){socket.write('N');drain();return;}
   started=true;socket.write(Buffer.concat([msg('R',Buffer.alloc(4)),status('server_version','17.5'),status('client_encoding','UTF8'),status('DateStyle','ISO, MDY'),status('integer_datetimes','on'),status('standard_conforming_strings','on'),msg('K',Buffer.alloc(8)),msg('Z',Buffer.from('I'))]));
  }
  while(buffer.length>=5){const n=buffer.readUInt32BE(1)+1;if(buffer.length<n)return;const packet=Buffer.from(buffer.subarray(0,n));buffer=buffer.subarray(n);if(packet[0]===88){socket.end();return;}
   chain=chain.then(async()=>{const bytes=await db.execProtocolRaw(packet);if(bytes.length)socket.write(Buffer.from(bytes));}).catch(e=>{console.error(e.message);socket.destroy();});
  }
 }
});
const port=Number(process.env.PGTEST_PORT || 54329);
await db.exec(await fs.readFile(new URL('../supabase/migrations/20260912181441_request_execution_lease.sql',import.meta.url),'utf8'));
await db.exec(`ALTER TABLE public.capability_execution_contracts ADD COLUMN produces jsonb NOT NULL DEFAULT '[]'::jsonb; GRANT SELECT(produces) ON public.capability_execution_contracts TO gateway_v2_runtime;`);
// sources.sql lists each source's offered capabilities, as production does.
await db.exec(`CREATE SCHEMA IF NOT EXISTS catalog; CREATE TABLE catalog.capability_offers(api_id uuid, endpoint_id uuid, capability_slug text, source text); GRANT USAGE ON SCHEMA catalog TO gateway_v2_runtime; GRANT SELECT(api_id, capability_slug) ON catalog.capability_offers TO gateway_v2_runtime;
CREATE TABLE catalog.discovery_sources(id text PRIMARY KEY,fetch_kind text,shape text,url text,enabled boolean);
INSERT INTO catalog.discovery_sources VALUES('coinbase-x402-bazaar','queried','x402-items','https://api.cdp.coinbase.com/platform/v2/x402/discovery/search?limit=20&query={query}',false);`);
// catalog.sql also loads staged source-document contracts by adapter version.
await db.exec(`ALTER TABLE public.capability_execution_contracts ADD COLUMN adapter_version text; GRANT SELECT(adapter_version) ON public.capability_execution_contracts TO gateway_v2_runtime;`);
await db.exec(await fs.readFile(new URL('../supabase/migrations/20260915150000_supplier_invoice_checks.sql',import.meta.url),'utf8'));
await db.exec(await fs.readFile(new URL('../supabase/migrations/20260915160000_workflow_runs_and_document_uploads.sql',import.meta.url),'utf8'));
// Runtime migrations from 2026-09-25 on run here in timestamp order. The
// Eurostat data migration requires the full production catalog, which this
// isolated runtime fixture deliberately does not load.
const migrations=new URL('../supabase/migrations/',import.meta.url);
for(const name of (await fs.readdir(migrations)).filter(name=>/^\d{14}_.+\.sql$/.test(name)&&name.slice(0,14)>='20260925000000'&&!name.endsWith('_prod_only.sql')&&!name.includes('eurostat_unemployment_by_country')
  // Coinbase Bazaar import/review migrations need its production provider
  // schema; scripts/test-bazaar-source-migration.mjs covers them together.
  &&!/_(coinbase_bazaar_source|fix_bazaar_logo_check|publish_bazaar_on_admin_approval)\.sql$/.test(name)
  // The API.market metadata import has its own full provider-identity fixture.
  &&!/_((stage_company_reputation_sources)|(activate_company_reputation_sources)|(clarify_glassdoor_company_name_input)|(declare_company_reputation_response_fields)|(bind_company_reputation_typed_results))\.sql$/.test(name)
  &&!name.endsWith('_allow_ask_analysis_compute.sql')
  &&!name.endsWith('_expand_ask_analysis_model_prices.sql')
  &&!name.endsWith('_declare_yelp_location_record_scope.sql')
  &&!name.endsWith('_onboard_apimarket_business_sources.sql')
  &&!name.endsWith('_admin_apimarket_credentials.sql')
  &&!name.endsWith('_show_only_crunchbase_apimarket.sql')
  &&!name.endsWith('_register_crunchbase_endpoints.sql')
  &&!name.endsWith('_price_crunchbase_calls.sql')
  // Official Crunchbase onboarding and follow-up source contracts require
  // production provider rows, OAuth evidence and complete catalog schemas.
  // Their recorded source-contract suites run independently of task storage.
  &&!/_(register_crunchbase_official_mcp|price_crunchbase_mcp_tools|refine_crunchbase_mcp_account_schema|activate_crunchbase_official_mcp|allow_gateway_crunchbase_endpoint_docs|refine_crunchbase_profile_discovery_and_price|constrain_crunchbase_profile_planning_inputs|declare_crunchbase_profile_output_coverage|refine_crunchbase_profile_array_contracts|accept_documented_crunchbase_nullable_fields)\.sql$/.test(name)
  // Reviewed source metadata asserts existing production provider/catalog
  // rows. The source-flow suites seed their reviewed catalog fixtures instead.
  &&!/_(describe_pdok_record_coverage|cbs_bankruptcy_sector_comparison|preserve_tenderned_publication_sort|support_kvk_sole_trader_profiles|kvk_registered_website_bridge|typed_domain_dns_records)\.sql$/.test(name)
  &&!/_(reviewed_intake_semantic_scope|extend_ecb_currency_input_scope|extend_ecb_latest_time_scope|extend_ecb_qualifier_vocabulary)\.sql$/.test(name)
  // Bright Data owns a private provider cache and is covered by its dedicated
  // migration/admission tests; its grants depend on the omitted onboarding.
  // Operator publication additionally requires production probe records.
  &&!/_((onboard_brightdata_linkedin)|(brightdata_v2_admission_access)|(activate_brightdata_linkedin)|(brightdata_five_cent_prices_and_examples))\.sql$/.test(name)
  // Recurring orders require the App's complete billing and tenancy schema,
  // which this isolated task/selection fixture does not install.
  &&!name.endsWith('_recurring_orders.sql')&&!name.endsWith('_locked_recurring_workflows.sql')&&!name.endsWith('_recurring_self_email.sql')).sort()) {
 await db.exec(await fs.readFile(new URL(name,migrations),'utf8'));
}
// Production uses providers/catalog/actions schemas. Keep the legacy fixture
// writers intact while exercising the same qualified runtime SELECTs and grants.
await db.exec('CREATE SCHEMA IF NOT EXISTS providers; CREATE SCHEMA IF NOT EXISTS actions; GRANT USAGE ON SCHEMA providers,actions TO gateway_v2_runtime;');
for (const [target, source] of [
 ['providers.apis','public.apis'], ['providers.api_endpoints','public.api_endpoints'],
 ['providers.provider_profiles','public.provider_profiles'], ['catalog.fact_types','public.fact_types'],
 ['catalog.capabilities','public.capabilities'], ['catalog.capability_execution_contracts','public.capability_execution_contracts'],
 ['actions.executions','gateway_v2.executions'], ['actions.user_selected_apis','public.user_selected_apis'],
]) {
 // A SELECT * view would require access to private fixture columns (e.g.
 // provider credentials) even when the outer query does not select them.
 const {rows}=await db.query("SELECT attname FROM pg_attribute WHERE attrelid=$1::regclass AND attnum>0 AND NOT attisdropped AND has_column_privilege('gateway_v2_runtime',$1,attname,'SELECT') ORDER BY attnum",[source]);
 const columns=rows.map(row=>'"'+row.attname.replaceAll('"','""')+'"').join(',');
 await db.exec(`CREATE VIEW ${target} WITH (security_invoker=true) AS SELECT ${columns} FROM ${source}; GRANT SELECT ON ${target} TO gateway_v2_runtime;`);
}
server.listen(port,'127.0.0.1',()=>console.log(`Test PostgreSQL on 127.0.0.1:${port} (one connection)`));
process.on('SIGTERM',()=>{server.close();db.close().finally(()=>process.exit(0));});
