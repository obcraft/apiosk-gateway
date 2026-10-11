import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import {PGlite} from '../../app/node_modules/@electric-sql/pglite/dist/index.js';
import {tools,capabilities,source} from './crunchbase-mcp-catalog.mjs';
const file='20261011133137_register_crunchbase_official_mcp.sql';
const sql=await fs.readFile(new URL('../supabase/migrations/'+file,import.meta.url),'utf8');
assert.equal(sql,await fs.readFile(new URL('../../app/supabase/migrations/'+file,import.meta.url),'utf8'));
const db=new PGlite();
try{
 await db.exec(`CREATE SCHEMA providers; CREATE SCHEMA catalog;
 CREATE TABLE providers.apis(id uuid PRIMARY KEY DEFAULT gen_random_uuid(),slug text UNIQUE,name text,listing_metadata jsonb);
 CREATE TABLE providers.api_endpoints(id uuid PRIMARY KEY,api_id uuid REFERENCES providers.apis(id),method text,path text,price numeric,payment_required boolean,active boolean,description text,request_body jsonb,request_example jsonb,response_body jsonb,tags text[],sectors text[],endpoint_category text,action_type text,task_summary text,external_docs_url text,metadata_source text,metadata_at timestamptz,origin_override text,UNIQUE(api_id,method,path));
 CREATE TABLE catalog.fact_subject_kinds(slug text PRIMARY KEY,description text);
 CREATE TABLE catalog.fact_types(slug text PRIMARY KEY,name text,description text,subject_kind text REFERENCES catalog.fact_subject_kinds(slug),value_schema jsonb,status text,freshness_seconds int);
 CREATE TABLE catalog.capabilities(slug text PRIMARY KEY,name text,description text,category text,input_contract jsonb,output_contract jsonb,status text);
 CREATE TABLE catalog.capability_execution_contracts(id uuid PRIMARY KEY,capability_slug text REFERENCES catalog.capabilities(slug),endpoint_id uuid REFERENCES providers.api_endpoints(id),version int,status text,adapter_version text,requires jsonb,produces jsonb,input_map jsonb,fixed_params jsonb,output_map jsonb,cardinality text,max_fan_out int,preconditions jsonb,pagination jsonb,effect text,price_model text,freshness_seconds int,notes text,UNIQUE(capability_slug,endpoint_id,version));`);
 await db.query('INSERT INTO providers.apis(slug,name,listing_metadata) VALUES($1,$2,$3)',[source,'Crunchbase',{apimarket:{id:'magicapi/crunchbase'},credential:{configured:true}}]);
 await db.exec(`INSERT INTO providers.api_endpoints(id,api_id,method,path,price,active) SELECT gen_random_uuid(),id,'POST','/autocompletes',0.086956,true FROM providers.apis`);
 await db.exec(sql);await db.exec(sql);
 const endpoints=(await db.query("SELECT * FROM providers.api_endpoints WHERE path LIKE '/mcp/%' ORDER BY path")).rows;
 assert.equal(endpoints.length,4);
 for(const t of tools){const e=endpoints.find(e=>e.path==='/mcp/'+t.tool); assert.ok(e);assert.equal(e.active,false);assert.deepEqual(e.request_body,t.schema);assert.ok(e.tags.includes('crunchbase'));assert.ok(e.endpoint_category);assert.equal(e.origin_override,'https://mcp.crunchbase.com');}
 const contracts=(await db.query('SELECT * FROM catalog.capability_execution_contracts')).rows;
 assert.equal(contracts.length,10);assert.ok(contracts.every(c=>c.status==='draft'&&c.effect==='read'));
 const facts=(await db.query('SELECT * FROM catalog.fact_types')).rows;
 assert.equal(facts.find(f=>f.slug==='crunchbase.entity_autocomplete_query').value_schema.type,'string');
 assert.equal(facts.find(f=>f.slug==='crunchbase.search_query_query').value_schema.type,'array');
 assert.equal((await db.query('SELECT count(*)::int AS n FROM providers.apis')).rows[0].n,1);
 const old=(await db.query("SELECT active,price FROM providers.api_endpoints WHERE path='/autocompletes'")).rows[0];assert.equal(old.active,true);assert.equal(Number(old.price),0.086956);
 assert.deepEqual((await db.query('SELECT listing_metadata FROM providers.apis')).rows[0].listing_metadata.credential,{configured:true});
 assert.deepEqual(contracts.map(c=>c.capability_slug).sort(),capabilities.map(c=>c.slug).sort());
 console.log('PASS: same source, four inactive MCP endpoints, ten typed draft capabilities, preserved API.market route/price, no Jina source, repeatable SQL.');
}finally{await db.close();}
