// Local PostgreSQL checks for staging, drift guards and internal job row access.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { PGlite } from '../../app/node_modules/@electric-sql/pglite/dist/index.js';
const filename='20261008092408_onboard_brightdata_linkedin.sql';
const migration=await fs.readFile(new URL(`../supabase/migrations/${filename}`,import.meta.url),'utf8');
assert.equal(migration,await fs.readFile(new URL(`../../app/supabase/migrations/${filename}`,import.meta.url),'utf8'));
const admissionFilename='20261008095215_brightdata_v2_admission_access.sql';
const admission=await fs.readFile(new URL(`../supabase/migrations/${admissionFilename}`,import.meta.url),'utf8');
assert.equal(admission,await fs.readFile(new URL(`../../app/supabase/migrations/${admissionFilename}`,import.meta.url),'utf8'));
const db=new PGlite();
try {
await db.exec(`CREATE ROLE anon; CREATE ROLE authenticated; CREATE ROLE apiosk_gateway; CREATE ROLE gateway_v2_runtime; CREATE ROLE service_role BYPASSRLS;
CREATE SCHEMA providers;
CREATE TABLE providers.apis(id uuid primary key,owner_id uuid,slug text UNIQUE,name text,status text,description text,long_description text,category text,docs_url text,website_url text,origin_url text,endpoint_url text,authentication_info jsonb,is_verified boolean,verified_at timestamptz,active boolean,discoverable boolean,listing_type text,listing_metadata jsonb,integration_mode text,settlement_mode text,is_external boolean,is_platform_owned boolean,is_community boolean,source text,requires_payout_wallet boolean);
CREATE TABLE providers.api_endpoints(id uuid primary key,api_id uuid references providers.apis(id),method text,path text,price numeric,active boolean,payment_required boolean,cache_ttl_seconds integer,description text,query_params jsonb,request_body jsonb,response_body jsonb,request_example jsonb,response_example jsonb,tags text[],search_terms text[],sectors text[],use_cases text[],input_entities text[],output_entities text[],endpoint_category text,action_type text,external_docs_url text,documentation_markdown text,metadata_source text,metadata_confidence numeric,metadata_at timestamptz,task_summary text,contract_documented boolean, UNIQUE(api_id,method,path));
CREATE FUNCTION public.auto_verify_owner() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.is_verified:=true; NEW.verified_at:=now(); RETURN NEW; END $$;
CREATE TRIGGER auto_verify_owner BEFORE INSERT ON providers.apis FOR EACH ROW EXECUTE FUNCTION public.auto_verify_owner();
INSERT INTO providers.apis(id,owner_id,slug,is_platform_owned,settlement_mode) VALUES('6b9898a7-55ac-4f98-a51e-a91f7da9345c','37211cf8-c4fd-45d7-b05a-cf958ee317e3','koop',true,'apiosk_revenue');`);
await db.exec(migration);
await db.exec(migration);
await db.exec(admission);
await db.exec(admission);
const source=(await db.query("SELECT * FROM providers.apis WHERE slug='brightdata'")).rows[0];
assert.equal(source.status,'inactive');assert.equal(source.active,false);assert.equal(source.discoverable,false);assert.equal(source.is_verified,false);
assert.equal(source.verified_at,null);assert.match(source.listing_metadata.coverage_notice,/November 13, 2025/);assert.match(source.listing_metadata.coverage_notice,/August 7, 2025/);
assert.equal(source.category,'data');assert.equal(source.settlement_mode,'apiosk_revenue');assert.equal(source.is_platform_owned,true);
assert.equal(source.listing_metadata.credential.env,'BRIGHTDATA_API_KEY');assert.equal(source.listing_metadata.credential.configured,false);
assert.equal(source.listing_metadata.auth_headers,undefined);assert.equal(source.listing_metadata.auth_query,undefined);
const rows=(await db.query("SELECT * FROM providers.api_endpoints ORDER BY path")).rows;
assert.deepEqual(rows.map(e=>e.path),['/linkedin/company','/linkedin/job','/linkedin/post','/linkedin/profile']);
for (const e of rows) {
  assert.equal(e.method,'POST');assert.equal(e.active,false);assert.equal(Number(e.price),0.0015);assert.equal(e.payment_required,true);assert.equal(e.contract_documented,true);assert.equal(e.response_example,null);
  assert.equal(e.endpoint_category,'data');assert.equal(e.action_type,'lookup');
  assert.deepEqual(e.request_body.required,['url']);assert.equal(e.request_body.additionalProperties,false);assert.deepEqual(Object.keys(e.request_body.properties),['url']);assert.equal(e.request_body.properties.url.maxLength,2048);
  assert.equal(e.response_body.properties.data.maxItems,1);assert.equal(e.response_body.properties.coverage.properties.complete.const,false);
  assert.ok(new RegExp(e.request_body.properties.url.pattern).test(e.request_example.url));
  for(const field of ['tags','search_terms','sectors','use_cases','input_entities','output_entities'])assert.ok(e[field].length>0,field);
}
assert.equal((await db.query("SELECT relrowsecurity FROM pg_class WHERE oid='gateway_private.brightdata_linkedin_jobs'::regclass")).rows[0].relrowsecurity,true);
const rowkey='a'.repeat(64);
await db.exec('SET ROLE apiosk_gateway');
await db.query("INSERT INTO gateway_private.brightdata_linkedin_jobs(request_key,kind,source_url,state) VALUES($1,'company','https://www.linkedin.com/company/microsoft','submitting')",[rowkey]);
assert.equal((await db.query("SELECT count(*)::int AS n FROM gateway_private.brightdata_linkedin_jobs")).rows[0].n,1);
await db.query("UPDATE gateway_private.brightdata_linkedin_jobs SET state='running',snapshot_id='s_fixture' WHERE request_key=$1",[rowkey]);
assert.equal((await db.query("SELECT state FROM gateway_private.brightdata_linkedin_jobs")).rows[0].state,'running');
await db.exec('RESET ROLE');
// A staging rerun must preserve ambiguous paid submissions even past cache expiry.
await db.query("UPDATE gateway_private.brightdata_linkedin_jobs SET state='unknown',expires_at=now()-interval '2 hours' WHERE request_key=$1",[rowkey]);
await db.exec(migration);
assert.equal((await db.query("SELECT state FROM gateway_private.brightdata_linkedin_jobs WHERE request_key=$1",[rowkey])).rows[0].state,'unknown');
for(const role of ['anon','authenticated']) {
  assert.equal((await db.query("SELECT has_schema_privilege($1,'gateway_private','USAGE') AS allowed",[role])).rows[0].allowed,false);
  assert.equal((await db.query("SELECT has_table_privilege($1,'gateway_private.brightdata_linkedin_jobs','SELECT') AS allowed",[role])).rows[0].allowed,false);
}
// Approved v2 admission can create/read/update jobs but cannot delete them or create tables.
for(const privilege of ['SELECT','INSERT','UPDATE']) {
  assert.equal((await db.query("SELECT has_table_privilege('gateway_v2_runtime','gateway_private.brightdata_linkedin_jobs',$1) AS allowed",[privilege])).rows[0].allowed,true);
}
for(const privilege of ['DELETE','TRUNCATE','REFERENCES','TRIGGER']) {
  assert.equal((await db.query("SELECT has_table_privilege('gateway_v2_runtime','gateway_private.brightdata_linkedin_jobs',$1) AS allowed",[privilege])).rows[0].allowed,false);
}
assert.equal((await db.query("SELECT has_schema_privilege('gateway_v2_runtime','gateway_private','CREATE') AS allowed")).rows[0].allowed,false);
await db.exec('SET ROLE gateway_v2_runtime');
const admissionKey='d'.repeat(64);
await db.query("INSERT INTO gateway_private.brightdata_linkedin_jobs(request_key,kind,source_url,state) VALUES($1,'profile','https://www.linkedin.com/in/example','submitting')",[admissionKey]);
await db.query("UPDATE gateway_private.brightdata_linkedin_jobs SET state='running',snapshot_id='s_admission' WHERE request_key=$1",[admissionKey]);
assert.equal((await db.query("SELECT state FROM gateway_private.brightdata_linkedin_jobs WHERE request_key=$1",[admissionKey])).rows[0].state,'running');
await assert.rejects(db.query("DELETE FROM gateway_private.brightdata_linkedin_jobs WHERE request_key=$1",[admissionKey]),/permission denied/);
await db.exec('RESET ROLE');
// DELETE remains blocked by RLS even after an accidental future direct grant.
await db.exec('GRANT DELETE ON gateway_private.brightdata_linkedin_jobs TO gateway_v2_runtime; SET ROLE gateway_v2_runtime');
assert.equal((await db.query("DELETE FROM gateway_private.brightdata_linkedin_jobs WHERE request_key=$1 RETURNING request_key",[admissionKey])).rows.length,0);
await db.exec('RESET ROLE');
await db.exec(admission);
assert.equal((await db.query("SELECT state FROM gateway_private.brightdata_linkedin_jobs WHERE request_key=$1",[admissionKey])).rows[0].state,'running');
// Even an accidental future table grant cannot expose rows through the API roles.
await db.exec('GRANT USAGE ON SCHEMA gateway_private TO anon; GRANT SELECT,INSERT ON gateway_private.brightdata_linkedin_jobs TO anon; SET ROLE anon');
assert.equal((await db.query("SELECT count(*)::int AS n FROM gateway_private.brightdata_linkedin_jobs")).rows[0].n,0);
await assert.rejects(db.query("INSERT INTO gateway_private.brightdata_linkedin_jobs(request_key,kind,source_url,state) VALUES($1,'profile','https://www.linkedin.com/in/example','submitting')",['b'.repeat(64)]),/row-level security/);
await db.exec('RESET ROLE');
await assert.rejects(db.query("INSERT INTO gateway_private.brightdata_linkedin_jobs(request_key,kind,source_url,state) VALUES($1,'profile','https://www.linkedin.com/in/example','invalid')",['c'.repeat(64)]),/check constraint/);
await db.exec("UPDATE providers.apis SET status='active',active=true,discoverable=true WHERE slug='brightdata'");
await assert.rejects(db.exec(migration),/conflicts with staged managed source/);await db.exec('ROLLBACK');
assert.equal((await db.query("SELECT status FROM providers.apis WHERE slug='brightdata'")).rows[0].status,'active');
await db.exec("UPDATE providers.apis SET status='inactive',active=false,discoverable=false WHERE slug='brightdata'; UPDATE providers.api_endpoints SET price=1 WHERE path='/linkedin/profile'");
await assert.rejects(db.exec(migration),/endpoint drift/);await db.exec('ROLLBACK');
console.log('PASS Bright Data migration: SQL execution, repeatability, 4 staged classified operations, strict request schemas, no response fabrication or credential storage, private RLS and narrow v2/runtime access, ambiguous job retention, active-source and tariff drift guards, exact mirror.');
} finally {await db.close();}
