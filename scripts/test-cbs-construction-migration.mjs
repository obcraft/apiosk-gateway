import fs from 'node:fs/promises';
import assert from 'node:assert/strict';
import {PGlite} from '@electric-sql/pglite';
const fixture=JSON.parse(await fs.readFile(new URL('../tests/fixtures/cbs-construction.json',import.meta.url),'utf8'));
const sql=await fs.readFile(new URL('../supabase/migrations/20260913134913_cbs_construction_annual_source.sql',import.meta.url),'utf8');
const db=new PGlite();await db.waitReady;
try{
await db.exec(`CREATE SCHEMA providers;CREATE SCHEMA catalog;
CREATE TABLE providers.apis(id uuid primary key,slug text,status text,is_verified boolean,discoverable boolean);
CREATE TABLE providers.api_endpoints(id uuid primary key,api_id uuid references providers.apis(id),path text,method text,price numeric,active boolean,description text,query_params jsonb,response_body jsonb,response_example jsonb);
CREATE TABLE catalog.fact_types(slug text primary key,name text,description text,subject_kind text,value_schema jsonb,status text,freshness_seconds int);
CREATE TABLE catalog.capabilities(slug text primary key,name text,description text,category text,input_contract jsonb,output_contract jsonb,status text);
CREATE TABLE catalog.capability_execution_contracts(id uuid primary key,capability_slug text references catalog.capabilities(slug),endpoint_id uuid references providers.api_endpoints(id),version int,status text,requires jsonb,produces jsonb,input_map jsonb,fixed_params jsonb,output_map jsonb,cardinality text,identifies text,max_fan_out int,preconditions jsonb,pagination jsonb,effect text,price_model text,freshness_seconds int,sample_payload jsonb,validated_at timestamptz,validated_note text);`);
await db.exec(`INSERT INTO providers.apis VALUES('00000000-0000-4000-8000-000000000001','cbs-statline','active',true,true);
INSERT INTO providers.api_endpoints VALUES('00000000-0000-4000-8000-000000000002','00000000-0000-4000-8000-000000000001','/odata/v1/CBS/86414NED/Observations','GET',0.047259,true,'Original observations','{}','{}','{}'),('00000000-0000-4000-8000-000000000003','00000000-0000-4000-8000-000000000001','/odata/v1/CBS/{tableId}/MeasureCodes','GET',0.017391,true,'Original codebook','{"type":"object","required":["tableId"],"properties":{"tableId":{"type":"string"}}}','{}','{}');`);
const before=(await db.query('SELECT * FROM providers.api_endpoints ORDER BY id')).rows;
await db.exec(sql);await db.exec(sql);
assert.equal((await db.query('SELECT count(*)::int AS n FROM catalog.capability_execution_contracts')).rows[0].n,6);
assert.deepEqual((await db.query("SELECT * FROM providers.api_endpoints WHERE path NOT LIKE '%85809NED%' ORDER BY id")).rows,before);
for(const s of fixture.services){const r=(await db.query('SELECT e.price,e.path,e.query_params,c.fixed_params,c.sample_payload FROM providers.api_endpoints e JOIN catalog.capability_execution_contracts c ON c.endpoint_id=e.id WHERE c.id=$1',[s.id])).rows[0];assert.equal(Number(r.price)*1e6,s.list_atomic);assert.equal(r.path,s.path);assert.deepEqual(r.fixed_params,s.fixed_params);assert.deepEqual(r.sample_payload,s.sample_payload);assert.equal(r.query_params.properties?.tableId,undefined);}
console.log('PASS: six source contracts, migration replay, exact tariffs and old source preservation');
}finally{await db.close()}
