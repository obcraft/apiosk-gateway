// Local development ONLY. Imports catalog metadata, never production balances
// or tokens. The real Supabase Auth service still verifies App sessions.
import fs from 'node:fs/promises';
import { execFileSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const socket=path.join(root,'.local/socket');
const admin={...process.env,PGHOST:socket,PGPORT:'55432',PGUSER:'apiosk_local',PGDATABASE:'apiosk_v2_dev'};
const psql='/opt/homebrew/Cellar/postgresql@17/17.11/bin/psql';
const sql=(text,env=admin)=>execFileSync(psql,['-X','-v','ON_ERROR_STOP=1','-q'],{env,input:text,stdio:['pipe','pipe','pipe']}).toString();
const literal=v=>`'${String(v).replaceAll("'","''")}'`;
const exists=sql("SELECT 1 FROM pg_database WHERE datname='apiosk_v2_dev';",{...admin,PGDATABASE:'postgres'}).includes('(1 row)');
if(!exists)sql('CREATE DATABASE apiosk_v2_dev;',{...admin,PGDATABASE:'postgres'});
const initialized=sql("SELECT 1 FROM pg_namespace WHERE nspname='gateway_v2';").includes('(1 row)');
if(!initialized){
 sql("CREATE ROLE anon; CREATE ROLE authenticated;");
 for(const f of (await fs.readdir(path.join(root,'supabase/migrations'))).filter(f=>!f.endsWith('_runtime_access.sql')).sort())sql(await fs.readFile(path.join(root,'supabase/migrations',f),'utf8'));
 // Keep the development schema in sync with the wire integration database.
 // Only the DDL segment is reused; no fixture data or fake identity is loaded.
 const support=await fs.readFile(path.join(root,'tests/support-db.mjs'),'utf8');
 const ddl=support.match(/await db.exec\(`(CREATE TABLE public[\s\S]*?)`\);/)[1];
 sql(ddl);
 sql(`CREATE FUNCTION public.use_agent_token(text) RETURNS TABLE(status text,owner_id uuid,connection_id uuid) LANGUAGE sql AS $$ SELECT NULL::text,NULL::uuid,NULL::uuid WHERE false $$;
 CREATE FUNCTION public.use_agent_connection(text) RETURNS TABLE(owner_id uuid,connection_id uuid) LANGUAGE sql AS $$ SELECT NULL::uuid,NULL::uuid WHERE false $$;`);
}
sql(await fs.readFile(path.join(root,'supabase/runtime-role.sql'),'utf8'));
const snapshot=JSON.parse(await fs.readFile(path.join(root,'test-artifacts/local-catalog.json'),'utf8'));
let statements=['BEGIN;'];
for(const f of snapshot.vocabulary)statements.push(`INSERT INTO public.fact_types(slug,status,value_schema) VALUES(${literal(f.slug)},'active',${literal(JSON.stringify(f.value_schema))}) ON CONFLICT(slug) DO UPDATE SET value_schema=excluded.value_schema;`);
for(const s of snapshot.services){
 const c=s.contract;
 statements.push(`INSERT INTO public.capabilities VALUES(${literal(c.capability_slug)},'active') ON CONFLICT DO NOTHING;`);
 statements.push(`INSERT INTO public.apis VALUES(${literal(s.id)},${literal(s.api_slug)},${literal(s.name)},${literal(s.listing_type)},'active',true,true,true,NULL,${s.source_url?literal(s.source_url):'NULL'},NULL) ON CONFLICT(id) DO NOTHING;`);
 statements.push(`INSERT INTO public.api_endpoints VALUES(${literal(s.endpoint_id)},${literal(s.id)},${literal(s.method)},${literal(s.path)},${s.list_atomic}/1000000.0,true) ON CONFLICT(id) DO NOTHING;`);
 statements.push(`INSERT INTO public.capability_execution_contracts VALUES(${literal(s.id)},${literal(s.endpoint_id)},${literal(c.capability_slug)},${c.version},'active',${literal(JSON.stringify(c.requires))},${literal(JSON.stringify(c.input_map))},${literal(JSON.stringify(c.output_map))},${c.identifies?literal(c.identifies):'NULL'},${literal(c.cardinality)},${c.max_fan_out},${literal(JSON.stringify(s.fixed_params))},${literal(JSON.stringify(s.preconditions))},${literal(JSON.stringify(s.pagination))},${s.freshness_seconds??3600},${literal(JSON.stringify(s.sample_payload))},now(),'read','per_call') ON CONFLICT(id) DO NOTHING;`);
}
statements.push('COMMIT;');sql(statements.join('\n'));
const envFile=path.join(root,'.env');
let env=await fs.readFile(envFile,'utf8').catch(()=> '');
if(!/^DATABASE_URL=/m.test(env)){
 const password=randomBytes(32).toString('hex');
 sql(`CREATE ROLE apiosk_v2_server LOGIN PASSWORD ${literal(password)}; GRANT gateway_v2_runtime TO apiosk_v2_server;`);
 env=`DATABASE_URL=postgresql://apiosk_v2_server:${password}@127.0.0.1:55432/apiosk_v2_dev?sslmode=disable\nSTATE_SIGNING_KEY=${randomBytes(48).toString('hex')}\n${env}`;
 await fs.writeFile(envFile,env,{mode:0o600});await fs.chmod(envFile,0o600);
}
console.log(`Local database prepared: ${snapshot.services.length} catalog contracts. No production balances or tokens copied.`);
