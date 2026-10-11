import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import {PGlite} from '../../app/node_modules/@electric-sql/pglite/dist/index.js';
import {tools,capabilities,source,pricing,capabilityOutputSchema} from './crunchbase-mcp-catalog.mjs';
import {formatSourcePrice,formatQuoteCeiling} from '../../app/src/lib/ui/source-price.ts';
const originalPricing={...pricing,listAtomic:47_259,buyerAtomic:54_348};
const coverageFile=(await fs.readdir(new URL('../supabase/migrations/',import.meta.url))).find(name=>name.endsWith('_declare_crunchbase_profile_output_coverage.sql'));
const coverageSql=await fs.readFile(new URL('../supabase/migrations/'+coverageFile,import.meta.url),'utf8');
assert.equal(coverageSql,await fs.readFile(new URL('../../app/supabase/migrations/'+coverageFile,import.meta.url),'utf8'));
const refinementFile='20261011154810_refine_crunchbase_profile_discovery_and_price.sql';
const refinementSql=await fs.readFile(new URL('../supabase/migrations/'+refinementFile,import.meta.url),'utf8');
assert.equal(refinementSql,await fs.readFile(new URL('../../app/supabase/migrations/'+refinementFile,import.meta.url),'utf8'));
const profileInputsFile='20261011155719_constrain_crunchbase_profile_planning_inputs.sql';
const profileInputsSql=await fs.readFile(new URL('../supabase/migrations/'+profileInputsFile,import.meta.url),'utf8');
assert.equal(profileInputsSql,await fs.readFile(new URL('../../app/supabase/migrations/'+profileInputsFile,import.meta.url),'utf8'));
const file='20261011133137_register_crunchbase_official_mcp.sql';
const sql=await fs.readFile(new URL('../supabase/migrations/'+file,import.meta.url),'utf8');
assert.equal(sql,await fs.readFile(new URL('../../app/supabase/migrations/'+file,import.meta.url),'utf8'));
const priceFile='20261011142816_price_crunchbase_mcp_tools.sql';
const priceSql=await fs.readFile(new URL('../supabase/migrations/'+priceFile,import.meta.url),'utf8');
assert.equal(priceSql,await fs.readFile(new URL('../../app/supabase/migrations/'+priceFile,import.meta.url),'utf8'));
const accountFile='20261011145243_refine_crunchbase_mcp_account_schema.sql';
const accountSql=await fs.readFile(new URL('../supabase/migrations/'+accountFile,import.meta.url),'utf8');
assert.equal(accountSql,await fs.readFile(new URL('../../app/supabase/migrations/'+accountFile,import.meta.url),'utf8'));
const activationFile='20261011152332_activate_crunchbase_official_mcp.sql';
const activationSql=await fs.readFile(new URL('../supabase/migrations/'+activationFile,import.meta.url),'utf8');
assert.equal(activationSql,await fs.readFile(new URL('../../app/supabase/migrations/'+activationFile,import.meta.url),'utf8'));
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
 await db.exec(sql);await db.exec(sql);await db.exec(accountSql);await db.exec(accountSql);
 const endpoints=(await db.query("SELECT * FROM providers.api_endpoints WHERE path LIKE '/mcp/%' ORDER BY path")).rows;
 assert.equal(endpoints.length,4);
 for(const t of tools){const e=endpoints.find(e=>e.path==='/mcp/'+t.tool); assert.ok(e);assert.equal(e.active,false);assert.deepEqual(e.request_body,t.schema);assert.ok(e.tags.includes('crunchbase'));assert.ok(e.endpoint_category);assert.equal(e.origin_override,'https://mcp.crunchbase.com');}
 const contracts=(await db.query('SELECT * FROM catalog.capability_execution_contracts')).rows;
 assert.equal(contracts.length,10);assert.ok(contracts.every(c=>c.status==='draft'&&c.effect==='read'));
 const facts=(await db.query('SELECT * FROM catalog.fact_types')).rows;
 assert.equal(facts.find(f=>f.slug==='crunchbase.entity_autocomplete_query').value_schema.type,'string');
 assert.equal(facts.find(f=>f.slug==='crunchbase.search_query_query').value_schema.type,'array');
 assert.equal(facts.find(f=>f.slug==='crunchbase.search_query_limit').value_schema.maximum,15);
 assert.ok(contracts.every(c=>!c.requires.some(r=>r.fact_type==='crunchbase.search_query_query')));
 assert.equal((await db.query('SELECT count(*)::int AS n FROM providers.apis')).rows[0].n,1);
 const old=(await db.query("SELECT active,price FROM providers.api_endpoints WHERE path='/autocompletes'")).rows[0];assert.equal(old.active,true);assert.equal(Number(old.price),0.086956);
 assert.deepEqual((await db.query('SELECT listing_metadata FROM providers.apis')).rows[0].listing_metadata.credential,{configured:true});
 assert.deepEqual(contracts.map(c=>c.capability_slug).sort(),capabilities.map(c=>c.slug).sort());
 await db.exec(priceSql);await db.exec(priceSql);await db.exec(sql);
 const priced=(await db.query("SELECT price,payment_required,active FROM providers.api_endpoints WHERE path LIKE '/mcp/%'")).rows;
 assert.equal(priced.length,4);
 for(const e of priced){
  const list=Math.round(Number(e.price)*1_000_000);
  assert.equal(list,originalPricing.listAtomic);
  assert.equal(list+Math.ceil(list*pricing.buyerMarkupBps/10_000),originalPricing.buyerAtomic);
  assert.equal(e.payment_required,true);assert.equal(e.active,false);
 }
 assert.deepEqual((await db.query('SELECT listing_metadata FROM providers.apis')).rows[0].listing_metadata.official_mcp.pricing,originalPricing);
 assert.ok((await db.query('SELECT status FROM catalog.capability_execution_contracts')).rows.every(c=>c.status==='draft'));
 assert.equal(Number((await db.query("SELECT price FROM providers.api_endpoints WHERE path='/autocompletes'")).rows[0].price),0.086956);
 await db.exec(`UPDATE providers.apis SET listing_metadata=jsonb_set(listing_metadata,'{official_mcp,status}','"verified"')`);
 await db.exec(priceSql);await db.exec(sql);
 assert.equal((await db.query('SELECT listing_metadata FROM providers.apis')).rows[0].listing_metadata.official_mcp.status,'verified');
 await db.exec(`CREATE SCHEMA vault; CREATE TABLE vault.secrets(name text);
 ALTER TABLE catalog.capability_execution_contracts ADD COLUMN sample_payload jsonb,ADD COLUMN validated_at timestamptz,ADD COLUMN validated_note text,ADD COLUMN updated_at timestamptz;`);
 await assert.rejects(db.exec(activationSql),/OAuth not connected/);await db.exec('ROLLBACK');
 await db.exec("INSERT INTO vault.secrets(name) VALUES('CRUNCHBASE_MCP_OAUTH')");
 await db.exec(activationSql);await db.exec(activationSql);
 assert.equal((await db.query("SELECT count(*)::int n FROM providers.api_endpoints WHERE path LIKE '/mcp/%' AND active AND price=0.047259 AND payment_required")).rows[0].n,4);
 const active=(await db.query('SELECT * FROM catalog.capability_execution_contracts')).rows;
 assert.equal(active.length,10);assert.ok(active.every(c=>c.status==='active'&&c.sample_payload.data.request_id&&c.validated_note.includes('Buyer charge 0')));
 assert.equal(active.find(c=>c.capability_slug==='company.crunchbase-filter').fixed_params.collection_id,'organizations');
 assert.equal((await db.query('SELECT listing_metadata FROM providers.apis')).rows[0].listing_metadata.official_mcp.status,'active');
 await db.exec(refinementSql);await db.exec(refinementSql);
 for(const e of (await db.query("SELECT price FROM providers.api_endpoints WHERE path LIKE '/mcp/%'")).rows){
  const list=Math.round(Number(e.price)*1_000_000);
  const total=list+Math.ceil(list*pricing.buyerMarkupBps/10_000);
  assert.equal(total,pricing.buyerAtomic);
  assert.equal(formatSourcePrice(`${total/1_000_000} USDC`,'EUR',pricing.displayRate),'€0.05');
  assert.equal(formatQuoteCeiling(String(total),'EUR',pricing.displayRate),'€0.05');
 }
 const profile=(await db.query("SELECT description FROM catalog.capabilities WHERE slug='company.crunchbase-profile'")).rows[0];
 assert.ok(profile.description.startsWith('Fetch company profile, founders, employees, board members, funding rounds and investors.'));
 await db.exec(profileInputsSql);await db.exec(profileInputsSql);
 for(const c of (await db.query("SELECT requires,input_map FROM catalog.capability_execution_contracts WHERE capability_slug IN ('company.crunchbase-profile','person.crunchbase-profile','funding.crunchbase-profile')")).rows){
  assert.equal(c.requires.length,1);assert.equal(c.requires[0].optional,false);
  assert.deepEqual(Object.keys(c.input_map),[c.requires[0].fact_type]);
 }
 assert.deepEqual((await db.query('SELECT listing_metadata FROM providers.apis')).rows[0].listing_metadata.official_mcp.pricing,pricing);
 await db.exec(coverageSql);await db.exec(coverageSql);
 const arraysFile=(await fs.readdir(new URL('../supabase/migrations/',import.meta.url))).find(name=>name.endsWith('_refine_crunchbase_profile_array_contracts.sql'));
 const arraysSql=await fs.readFile(new URL('../supabase/migrations/'+arraysFile,import.meta.url),'utf8');
 assert.equal(arraysSql,await fs.readFile(new URL('../../app/supabase/migrations/'+arraysFile,import.meta.url),'utf8'));
 await db.exec(arraysSql);await db.exec(arraysSql);
 for(const c of capabilities.filter(c=>c.tool==='cb_entity_get')){
  const actual=(await db.query('SELECT value_schema FROM catalog.fact_types WHERE slug=$1',[c.slug.replaceAll('-','_')+'_result'])).rows[0].value_schema;
  assert.deepEqual(actual,capabilityOutputSchema(c));
  const contract=(await db.query('SELECT output_contract FROM catalog.capabilities WHERE slug=$1',[c.slug])).rows[0].output_contract;
  assert.deepEqual(contract,actual);
 }
 console.log('PASS: registration, pricing, account refinement and guarded activation; four tools, ten real-sample contracts, unchanged API.market price, no Jina surcharge.');
}finally{await db.close();}
