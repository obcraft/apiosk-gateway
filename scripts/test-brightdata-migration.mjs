// Local PostgreSQL checks for staging, drift guards and internal job row access.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { PGlite } from '../../app/node_modules/@electric-sql/pglite/dist/index.js';
import { buyerChargeMicro, feeMicro, usdToMicro, WORKFLOW_FEE_BPS } from '../../app/supabase/functions/_shared/pricing.ts';
import { formatSourcePrice, formatQuoteCeiling } from '../../app/src/lib/ui/source-price.ts';
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
CREATE TABLE providers.apis(id uuid primary key,owner_id uuid,slug text UNIQUE,name text,status text,description text,long_description text,category text,docs_url text,website_url text,origin_url text,endpoint_url text,authentication_info jsonb,is_verified boolean,verified_at timestamptz,approved_at timestamptz,suspended_at timestamptz,active boolean,discoverable boolean,listing_type text,listing_metadata jsonb,integration_mode text,settlement_mode text,is_external boolean,is_platform_owned boolean,is_community boolean,source text,requires_payout_wallet boolean);
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
// Publication requires current successful jobs for the exact four public URLs.
const activationFilename='20261008104542_activate_brightdata_linkedin.sql';
const activationSource=await fs.readFile(new URL(`../supabase/migrations/${activationFilename}`,import.meta.url),'utf8');
assert.equal(activationSource,await fs.readFile(new URL(`../../app/supabase/migrations/${activationFilename}`,import.meta.url),'utf8'));
// Keep historical migration testing repeatable after its fixed live probe date.
// Only the fixture date changes; the released SQL always requires 2026-10-08.
const today=(await db.query("SELECT (now() AT TIME ZONE 'Europe/Lisbon')::date::text AS today")).rows[0].today;
const activation=activationSource.replace("DATE '2026-10-08'",`DATE '${today}'`);
const probes=JSON.parse(activationSource.match(/\$probes\$([\s\S]*?)\$probes\$/)[1]);
assert.deepEqual(probes.map(p=>p.kind),['profile','company','job','post']);
assert.equal(probes.find(p=>p.kind==='job').url,'https://www.linkedin.com/jobs/view/4476198009');
for(const probe of probes)assert.equal(probe.request_key,createHash('sha256').update(`${probe.kind}\n${probe.url}`).digest('hex'));
await db.exec("UPDATE providers.api_endpoints SET price=0.0015 WHERE path='/linkedin/profile'");
await db.exec(`CREATE FUNCTION public.review_gate() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
  IF NEW.status='active' AND OLD.status IS DISTINCT FROM NEW.status
    AND nullif(current_setting('apiosk.api_review_approved_id',true),'') IS DISTINCT FROM NEW.id::text
  THEN NEW.status:='pending'; NEW.active:=false; END IF;
  RETURN NEW; END $$;
CREATE TRIGGER review_gate BEFORE UPDATE ON providers.apis FOR EACH ROW EXECUTE FUNCTION public.review_gate();`);
async function activationRejected(pattern) {
  await assert.rejects(db.exec(activation),pattern);await db.exec('ROLLBACK');
  const state=(await db.query("SELECT status,active,is_verified,discoverable FROM providers.apis WHERE slug='brightdata'")).rows[0];
  assert.deepEqual(state,{status:'inactive',active:false,is_verified:false,discoverable:false});
  assert.equal((await db.query('SELECT count(*)::int AS n FROM providers.api_endpoints WHERE active')).rows[0].n,0);
}
await activationRejected(/live probe is missing/);
for(const probe of probes) await db.query("INSERT INTO gateway_private.brightdata_linkedin_jobs(request_key,kind,source_url,state,snapshot_id,result) VALUES($1,$2,$3,'ready','sd_test_fixture',$4)",[probe.request_key,probe.kind,probe.url,JSON.stringify([{fixture:'synthetic local PostgreSQL validation only',url:probe.url}])]);
for(const state of ['running','failed','unknown']) {
 await db.query("UPDATE gateway_private.brightdata_linkedin_jobs SET state=$1 WHERE request_key=$2",[state,probes[0].request_key]);
 await activationRejected(/not a fresh successful single record/);
}
await db.query("UPDATE gateway_private.brightdata_linkedin_jobs SET state='ready',updated_at=now()-interval '2 days' WHERE request_key=$1",[probes[0].request_key]);
await activationRejected(/not a fresh successful single record/);
await db.query("UPDATE gateway_private.brightdata_linkedin_jobs SET updated_at=now(),result='[]'::jsonb WHERE request_key=$1",[probes[0].request_key]);
await activationRejected(/not a fresh successful single record/);
await db.query("UPDATE gateway_private.brightdata_linkedin_jobs SET result='[{\"fixture\":true}]'::jsonb WHERE request_key=$1",[probes[0].request_key]);
await db.query("UPDATE gateway_private.brightdata_linkedin_jobs SET result='[{\"error\":\"fixture provider failure\"}]'::jsonb WHERE request_key=$1",[probes[0].request_key]);
await activationRejected(/contains a provider error/);
await db.query("UPDATE gateway_private.brightdata_linkedin_jobs SET result='[{\"fixture\":true}]'::jsonb WHERE request_key=$1",[probes[0].request_key]);
await db.exec("UPDATE providers.api_endpoints SET price=0.002 WHERE path='/linkedin/profile'");
await activationRejected(/tariff or request contract drift/);
await db.exec("UPDATE providers.api_endpoints SET price=0.0015 WHERE path='/linkedin/profile'");
const jobsBefore=(await db.query('SELECT request_key,state,result FROM gateway_private.brightdata_linkedin_jobs ORDER BY request_key')).rows;
await db.exec(activation);await db.exec(activation);
const published=(await db.query("SELECT * FROM providers.apis WHERE slug='brightdata'")).rows[0];
assert.equal(published.status,'active');assert.equal(published.active,true);assert.equal(published.is_verified,true);assert.equal(published.discoverable,true);assert.ok(published.approved_at);assert.ok(published.verified_at);
assert.equal(published.listing_metadata.catalog_state,'published');assert.equal(published.listing_metadata.credential.configured,true);assert.equal(published.listing_metadata.staging_reason,undefined);
assert.deepEqual(published.listing_metadata.production_probe,{adapter:'brightdata-linkedin-v1',probed_on:today,verified_kinds:4});
assert.ok(!JSON.stringify(published.listing_metadata).includes('synthetic local PostgreSQL'));
assert.equal((await db.query("SELECT coalesce(current_setting('apiosk.api_review_approved_id',true),'') AS approval")).rows[0].approval,'');
const publishedEndpoints=(await db.query('SELECT active,path,request_example,response_example FROM providers.api_endpoints ORDER BY path')).rows;
assert.equal(publishedEndpoints.filter(e=>e.active).length,4);
for(const endpoint of publishedEndpoints) {assert.equal(endpoint.request_example.url,probes.find(p=>endpoint.path===`/linkedin/${p.kind}`).url);assert.equal(endpoint.response_example,null);}
assert.deepEqual((await db.query('SELECT request_key,state,result FROM gateway_private.brightdata_linkedin_jobs ORDER BY request_key')).rows,jobsBefore);
const pricingFilename='20261008105950_brightdata_five_cent_prices_and_examples.sql';
const pricing=await fs.readFile(new URL(`../supabase/migrations/${pricingFilename}`,import.meta.url),'utf8');
assert.equal(pricing,await fs.readFile(new URL(`../../app/supabase/migrations/${pricingFilename}`,import.meta.url),'utf8'));
// An unexpected tariff must abort without altering any of the four operations.
await db.exec("UPDATE providers.api_endpoints SET price=0.002 WHERE path='/linkedin/profile'");
await assert.rejects(db.exec(pricing),/tariff or request contract drift/);await db.exec('ROLLBACK');
assert.equal((await db.query("SELECT count(*)::int AS n FROM providers.api_endpoints WHERE price=0.0015")).rows[0].n,3);
await db.exec("UPDATE providers.api_endpoints SET price=0.0015 WHERE path='/linkedin/profile'");
await db.exec(pricing);
const firstPriced=(await db.query('SELECT path,price,use_cases,documentation_markdown FROM providers.api_endpoints ORDER BY path')).rows;
await db.exec(pricing);
assert.deepEqual((await db.query('SELECT path,price,use_cases,documentation_markdown FROM providers.api_endpoints ORDER BY path')).rows,firstPriced);
const pricedSource=(await db.query("SELECT * FROM providers.apis WHERE slug='brightdata'")).rows[0];
assert.equal(pricedSource.listing_metadata.logo,'https://brightdata.com/wp-content/themes/brightdata/assets/images/favicon.png');
assert.deepEqual(pricedSource.listing_metadata.production_probe,published.listing_metadata.production_probe);
assert.deepEqual(pricedSource.listing_metadata.credential,published.listing_metadata.credential);
assert.equal(pricedSource.listing_metadata.billing.list_micro_usd,47258);
assert.equal(pricedSource.listing_metadata.billing.buyer_total_micro_usd,54347);
assert.equal(pricedSource.listing_metadata.billing.conversion_rate,0.92);
assert.equal(pricedSource.listing_metadata.billing.conversion_date,'2026-08-21');
assert.equal(pricedSource.listing_metadata.billing.target_total_eur,0.05);
assert.equal(pricedSource.listing_metadata.example_questions.length,4);
for(const endpoint of firstPriced) {
  const micro=usdToMicro(String(endpoint.price));
  assert.equal(micro,47258);assert.equal(feeMicro(micro),7089);assert.equal(buyerChargeMicro(micro),54347);
  assert.equal(formatSourcePrice(`${buyerChargeMicro(micro)/1_000_000} USD`,'EUR',0.92),'€0.05');
  assert.equal(formatQuoteCeiling(String(buyerChargeMicro(micro)),'EUR',0.92),'€0.05');
  // Existing workflow pricing is deliberately unchanged and is not the standalone target.
  assert.equal(buyerChargeMicro(micro,WORKFLOW_FEE_BPS),56710);
  const example=pricedSource.listing_metadata.example_questions.find(e=>e.path===endpoint.path);
  assert.equal(example.inputs.url,probes.find(p=>endpoint.path===`/linkedin/${p.kind}`).url);
  assert.ok(endpoint.use_cases.includes(example.question));assert.ok(endpoint.documentation_markdown.includes(example.question));
  assert.ok(!endpoint.documentation_markdown.includes('USD 0.0015 before'));
}
assert.deepEqual((await db.query('SELECT request_key,state,result FROM gateway_private.brightdata_linkedin_jobs ORDER BY request_key')).rows,jobsBefore);
console.log('PASS Bright Data migration: SQL execution and repeatability; staged and live-probe publication guards; private RLS and narrow runtime access; preserved jobs; exact mirrored migrations; tariff drift rejection; canonical standalone/workflow fees; €0.05 source and approval displays; four documented URL questions; official logo.');
} finally {await db.close();}
