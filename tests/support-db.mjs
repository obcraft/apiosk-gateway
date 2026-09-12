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
await db.exec(`CREATE TABLE public.user_selected_apis(id uuid primary key,user_id uuid,connection_id uuid,prompt text,kind text,source text,name text,capability text,api_slug text,candidate_id uuid,method text,provider_price_micro_usd bigint,charged_micro_usd bigint,metadata jsonb);
CREATE TABLE public.balance_charges(id uuid primary key,user_id uuid,idempotency_key text,status text,charged_micro_usd bigint,actual_provider_micro_usd bigint,provider_price_micro_usd bigint,fee_micro_usd bigint,metadata jsonb,created_at timestamptz default now(),job_id uuid);
CREATE TABLE public.agent_connections(id uuid primary key,user_id uuid,revoked_at timestamptz,per_call_limit_micro_usd bigint,daily_limit_micro_usd bigint);
CREATE TABLE public.job_budget_reservations(user_id uuid,connection_id uuid,spent_micro_usd bigint,reserved_micro_usd bigint,created_at timestamptz,status text);
CREATE TABLE public.user_balances(user_id uuid primary key,balance_micro_usd bigint);
CREATE TABLE public.call_runs(id uuid primary key,charge_id uuid,user_id uuid,status text,output jsonb,output_truncated boolean,started_at timestamptz default now());
CREATE TABLE public.fact_types(slug text primary key,status text,value_schema jsonb,jurisdiction text);
CREATE TABLE public.capabilities(slug text primary key,status text);
CREATE TABLE public.provider_profiles(id uuid primary key, username text, avatar_url text);
CREATE TABLE public.apis(id uuid primary key,owner_id uuid,slug text,name text,listing_type text,status text,active boolean,is_verified boolean,discoverable boolean,suspended_at timestamptz,docs_url text,website_url text,description text DEFAULT 'Company filings',category text DEFAULT 'finance',listing_metadata jsonb DEFAULT '{"tags":["registry"]}');
CREATE TABLE public.api_endpoints(id uuid primary key,api_id uuid,method text,path text,price numeric,active boolean);
CREATE TABLE public.capability_execution_contracts(id uuid primary key,endpoint_id uuid,capability_slug text,version int,status text,requires jsonb,input_map jsonb,output_map jsonb,identifies text,cardinality text,max_fan_out int,fixed_params jsonb,preconditions jsonb,pagination jsonb,freshness_seconds int,sample_payload jsonb,validated_at timestamptz,effect text,price_model text);`);
const fixtureHash=createHash('sha256').update('apk_access_local_fixture').digest('hex');
await db.exec(`CREATE FUNCTION public.use_agent_token(text) RETURNS TABLE(status text,owner_id uuid,connection_id uuid) LANGUAGE sql AS $$ SELECT 'ok'::text,'00000000-0000-4000-8000-000000000001'::uuid,'00000000-0000-4000-8000-000000000002'::uuid WHERE $1='${fixtureHash}' $$;
CREATE FUNCTION public.use_agent_connection(text) RETURNS TABLE(owner_id uuid,connection_id uuid) LANGUAGE sql AS $$ SELECT NULL::uuid,NULL::uuid WHERE false $$;`);
await db.exec(await fs.readFile(new URL('../supabase/runtime-role.sql',import.meta.url),'utf8'));
await db.exec(await fs.readFile(new URL('../supabase/migrations/20260908104136_gateway_v2_connection_approval.sql',import.meta.url),'utf8'));
await db.exec("INSERT INTO agent_connections VALUES('00000000-0000-4000-8000-000000000002','00000000-0000-4000-8000-000000000001',NULL,1000000,2000000); INSERT INTO user_balances VALUES('00000000-0000-4000-8000-000000000001',2000000);");
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
 ADD COLUMN request_body jsonb;
 ALTER TABLE public.capabilities ADD COLUMN description text DEFAULT 'Fixture capability';
`);
await db.exec(await fs.readFile(new URL("../supabase/migrations/20260911113528_source_readiness_audit_access.sql",import.meta.url),"utf8"));

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
await db.exec(`ALTER TABLE public.capability_execution_contracts ADD COLUMN produces jsonb NOT NULL DEFAULT '[]'::jsonb; GRANT SELECT(produces) ON public.capability_execution_contracts TO gateway_v2_runtime;`);
server.listen(port,'127.0.0.1',()=>console.log(`Test PostgreSQL on 127.0.0.1:${port} (one connection)`));
process.on('SIGTERM',()=>{server.close();db.close().finally(()=>process.exit(0));});
