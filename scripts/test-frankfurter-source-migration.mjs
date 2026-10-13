// Validate local staging, repeatability and existing-listing guards in PostgreSQL.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { PGlite } from '@electric-sql/pglite';
const migration=await fs.readFile(new URL('../supabase/migrations/20261002084852_frankfurter_ecb_reference_rates_staged_prod_only.sql',import.meta.url),'utf8');
const fixture=JSON.parse(await fs.readFile(new URL('../tests/fixtures/frankfurter/catalog.json',import.meta.url),'utf8'));
const db=new PGlite();
try {
await db.exec(`CREATE SCHEMA providers; CREATE SCHEMA catalog;
CREATE TABLE providers.apis(id uuid primary key,slug text,origin_url text,status text,active boolean,discoverable boolean,suspended_at timestamptz,listing_metadata jsonb);
CREATE TABLE providers.api_endpoints(id uuid primary key,api_id uuid references providers.apis(id),method text,path text,price numeric,active boolean,payment_required boolean,cache_ttl_seconds integer,description text,query_params jsonb,request_body jsonb,response_body jsonb,request_example jsonb,response_example jsonb,tags text[],search_terms text[],endpoint_category text,action_type text,external_docs_url text,documentation_markdown text,metadata_source text,metadata_confidence numeric,origin_override text,auth_override jsonb,task_summary text,contract_documented boolean);
CREATE TABLE catalog.fact_subject_kinds(slug text primary key,description text);
CREATE TABLE catalog.fact_types(slug text primary key,name text,description text,subject_kind text references catalog.fact_subject_kinds(slug),value_schema jsonb,status text,freshness_seconds integer);
CREATE TABLE catalog.capabilities(slug text primary key,name text,description text,category text,input_contract jsonb,output_contract jsonb,status text);
CREATE TABLE catalog.capability_execution_contracts(id uuid primary key,capability_slug text references catalog.capabilities(slug),endpoint_id uuid references providers.api_endpoints(id),version integer,status text,requires jsonb,produces jsonb,input_map jsonb,fixed_params jsonb,output_map jsonb,cardinality text,identifies text,max_fan_out integer,preconditions jsonb,pagination jsonb,effect text,price_model text,freshness_seconds integer,sample_payload jsonb,validated_at timestamptz,validated_note text,notes text,adapter_version text);
INSERT INTO providers.apis VALUES('0e2ef2d1-1fc0-4a5b-a88d-eec3f629a067','frankfurter','https://api.frankfurter.dev/v1','active',true,false,NULL,'{}');
INSERT INTO providers.api_endpoints(id,api_id,method,path,price,active) VALUES('be7a01a6-a21b-4228-aa66-fa4a1e64d710','0e2ef2d1-1fc0-4a5b-a88d-eec3f629a067','GET','/latest',0.05,true);`);
await db.exec(migration);await db.exec(migration);
assert.equal((await db.query('SELECT count(*)::int AS n FROM catalog.capability_execution_contracts')).rows[0].n,1);
const row=(await db.query(`SELECT a.discoverable,e.active,e.price,e.path,e.origin_override,c.status,c.requires,c.input_map,c.output_map,c.sample_payload FROM catalog.capability_execution_contracts c JOIN providers.api_endpoints e ON e.id=c.endpoint_id JOIN providers.apis a ON a.id=e.api_id`)).rows[0];
assert.equal(row.discoverable,false);assert.equal(row.active,false);assert.equal(row.status,'validated');
assert.equal(Number(row.price),0.05);assert.equal(row.origin_override,'https://api.frankfurter.dev');
const service=fixture.services[0];assert.equal(row.path,service.path);assert.deepEqual(row.requires,service.contract.requires);assert.deepEqual(row.input_map,service.contract.input_map);assert.deepEqual(row.output_map,service.contract.output_map);assert.deepEqual(row.sample_payload,service.sample_payload);
await db.exec("UPDATE providers.api_endpoints SET price=0.5 WHERE path='/latest'");
await assert.rejects(db.exec(migration),/listing or tariff drift/);await db.exec('ROLLBACK;');
await db.exec("UPDATE providers.api_endpoints SET price=0.05 WHERE path='/latest'; UPDATE providers.apis SET listing_metadata='{\"planner_block_reason\":\"Do not publish\"}'");
await assert.rejects(db.exec(migration),/listing or tariff drift/);await db.exec('ROLLBACK;');
assert.equal((await db.query('SELECT discoverable FROM providers.apis')).rows[0].discoverable,false);
console.log('Frankfurter staging migration: PostgreSQL execution, idempotence, exact contract/fixture parity, hidden source, inactive endpoint, tariff drift and block preservation passed. No production changes.');
} finally {await db.close();}
