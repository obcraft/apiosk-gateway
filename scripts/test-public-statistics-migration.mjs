import fs from 'node:fs/promises';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { PGlite } from '@electric-sql/pglite';
const fixture=JSON.parse(await fs.readFile(new URL('../tests/fixtures/public-statistics/catalog.json',import.meta.url),'utf8'));
const sql=await fs.readFile(new URL('../supabase/migrations/20260909093944_connect_public_statistics_planner.sql',import.meta.url),'utf8');
const db=new PGlite(); await db.waitReady;
try {
await db.exec(`CREATE TABLE fact_subject_kinds(slug text primary key,description text);
CREATE TABLE fact_types(slug text primary key,name text,description text,subject_kind text references fact_subject_kinds(slug),jurisdiction text,value_schema jsonb,status text,freshness_seconds int);
CREATE TABLE capabilities(slug text primary key,name text,description text,category text,input_contract jsonb,output_contract jsonb,status text);
CREATE TABLE apis(id uuid primary key,slug text unique,name text,status text,active boolean default true,is_verified boolean,discoverable boolean,suspended_at timestamptz,listing_type text default 'api',listing_metadata jsonb default '{}',docs_url text,website_url text);
CREATE TABLE api_endpoints(id uuid primary key,api_id uuid references apis(id),path text,method text,price numeric,active boolean,query_params jsonb);
CREATE TABLE capability_execution_contracts(id uuid primary key,capability_slug text references capabilities(slug),endpoint_id uuid references api_endpoints(id),version int,status text,requires jsonb,produces jsonb,input_map jsonb,fixed_params jsonb,output_map jsonb,cardinality text,identifies text,max_fan_out int,preconditions jsonb,pagination jsonb,effect text,price_model text,freshness_seconds int,sample_payload jsonb,validated_at timestamptz,validated_note text,notes text);`);
const ids=new Map();
for (const s of fixture.services) {
 if (!ids.has(s.api_slug)){const id=randomUUID();ids.set(s.api_slug,id);await db.query("INSERT INTO apis(id,slug,name,status,is_verified,discoverable) VALUES($1,$2,$3,'active',true,true)",[id,s.api_slug,s.name]);}
 await db.query("INSERT INTO api_endpoints VALUES($1,$2,$3,$4,$5,true,'{}') ON CONFLICT(id) DO NOTHING",[s.endpoint_id,ids.get(s.api_slug),s.path,s.method,s.list_atomic/1e6]);
}
const eodId=randomUUID();await db.query("INSERT INTO apis(id,slug,name,status,is_verified,discoverable) VALUES($1,'eodhd','EODHD Market Data','active',true,true)",[eodId]);
await db.query("INSERT INTO api_endpoints VALUES($1,$2,'/api/eod/{symbol}','GET',0.02,true,'{\"x-apiosk-sellable\":false}')",[randomUUID(),eodId]);
const before=await db.query('SELECT id,price FROM api_endpoints ORDER BY id');
await db.exec(sql); await db.exec(sql);
assert.equal((await db.query('SELECT count(*)::int AS n FROM capability_execution_contracts')).rows[0].n,fixture.services.length);
assert.deepEqual((await db.query('SELECT id,price FROM api_endpoints ORDER BY id')).rows,before.rows);
assert.equal((await db.query("SELECT count(*)::int AS n FROM capability_execution_contracts c JOIN api_endpoints e ON e.id=c.endpoint_id JOIN apis a ON a.id=e.api_id WHERE a.slug='eodhd'")).rows[0].n,0);
assert.match((await db.query("SELECT listing_metadata->>'planner_block_reason' AS reason FROM apis WHERE slug='eodhd'")).rows[0].reason,/resale/);
// This historical migration predates the schema split. Present its actual rows
// through the current runtime schemas so the real catalog query stays covered.
await db.exec(`
CREATE SCHEMA actions; CREATE SCHEMA providers; CREATE SCHEMA catalog;
CREATE TABLE actions.executions(selection_id uuid, status text, created_at timestamptz);
CREATE TABLE actions.user_selected_apis(id uuid, api_slug text);
CREATE TABLE providers.provider_profiles(id uuid, username text);
CREATE VIEW providers.apis WITH (security_invoker=true) AS
 SELECT a.*, NULL::uuid AS owner_id, ''::text AS description, ''::text AS category FROM public.apis a;
CREATE VIEW providers.api_endpoints WITH (security_invoker=true) AS
 SELECT e.*, NULL::text AS origin_override, NULL::text AS external_docs_url,
 NULL::text AS description, ARRAY[]::text[] AS tags, NULL::text AS endpoint_category,
 '{}'::jsonb AS request_body, NULL::text AS metadata_source FROM public.api_endpoints e;
CREATE VIEW catalog.capability_execution_contracts WITH (security_invoker=true) AS
 SELECT c.*, 'v1'::text AS adapter_version FROM public.capability_execution_contracts c;
CREATE VIEW catalog.capabilities WITH (security_invoker=true) AS SELECT * FROM public.capabilities;
CREATE VIEW catalog.fact_types WITH (security_invoker=true) AS SELECT * FROM public.fact_types;
`);
const loaded=(await db.query(await fs.readFile(new URL('../src/catalog.sql',import.meta.url),'utf8'))).rows[0].jsonb_build_object;
assert.equal(loaded.services.length,fixture.services.length);assert.equal(loaded.known_sources.length,4);
console.log('PASS migration twice; 15 contracts loaded; prices preserved; EODHD visible and blocked.');
} finally {await db.close();}
