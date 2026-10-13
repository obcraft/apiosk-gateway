import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { PGlite } from '../../../../app/node_modules/@electric-sql/pglite/dist/index.js';
const here = new URL('./',import.meta.url);
const snapshot=JSON.parse(await fs.readFile(new URL('../schema-snapshot.json',here),'utf8'));
const migration=await fs.readFile(new URL('../../../supabase/migrations/20261013172106_onboard_nl_benefits_sources.sql',here),'utf8');
const db=new PGlite();
const q=s=>'"'+s.replaceAll('"','""')+'"';
const owner='bf085fe8-41c4-4699-978f-e75e89518bf7';
const tables=snapshot.import_tables;
try {
 await db.exec(`CREATE SCHEMA providers; CREATE SCHEMA catalog; CREATE SCHEMA auth; CREATE SCHEMA wallets; CREATE SCHEMA billing; CREATE SCHEMA vault;
 CREATE TYPE public.listing_type AS ENUM ('api','skill','product','dataset','service','connector','federated');
 CREATE TABLE auth.users(id uuid PRIMARY KEY); INSERT INTO auth.users VALUES('${owner}');
 CREATE TABLE wallets.network_tokens(id uuid PRIMARY KEY); CREATE TABLE billing.commission_tiers(id uuid PRIMARY KEY);
 CREATE TABLE vault.secrets(id uuid PRIMARY KEY DEFAULT gen_random_uuid(),name text UNIQUE,secret text);`);
 for(const table of tables) {
  const columns=snapshot.columns.filter(c=>`${c.table_schema}.${c.table_name}`===table);
  const defs=columns.map(c=>`${q(c.column_name)} ${c.data_type==='ARRAY'?'text[]':c.data_type==='USER-DEFINED'?`${q(c.udt_schema)}.${q(c.udt_name)}`:c.data_type}${c.column_default?` DEFAULT ${c.column_default}`:''}${c.is_nullable==='NO'?' NOT NULL':''}`);
  await db.exec(`CREATE TABLE ${table} (${defs.join(',')})`);
 }
 for(const c of snapshot.constraints.filter(c=>tables.includes(`${c.schema}.${c.table_name}`)).sort((a,b)=>Number(a.definition.startsWith('FOREIGN KEY'))-Number(b.definition.startsWith('FOREIGN KEY'))))
  await db.exec(`ALTER TABLE ${c.schema}.${c.table_name} ADD CONSTRAINT ${q(c.conname)} ${c.definition}`);
 const hooks=['apiosk_auto_verify_owner_apis()','apiosk_normalize_api_category()','apiosk_apis_review_gate()','apiosk_sync_apis_active_with_status()','require_payout_wallet_to_publish()'];
 for(const f of snapshot.trigger_functions.filter(f=>hooks.includes(f.signature))) await db.exec(f.definition);
 for(const t of snapshot.triggers.filter(t=>hooks.includes(t.function))) await db.exec(t.definition);
 await db.exec(`INSERT INTO providers.apis(owner_id,slug,name,description,origin_url,status,is_platform_owned,settlement_mode,requires_payout_wallet,listing_metadata)
 VALUES('${owner}','koop','Existing KOOP','Existing description','https://repository.overheid.nl','inactive',true,'apiosk_revenue',false,'{"unrelated_metadata":"preserve"}');
 INSERT INTO providers.api_endpoints(api_id,method,path,price,active,tags) SELECT id,'GET','/sru',0.085066,true,ARRAY['existing-tag'] FROM providers.apis WHERE slug='koop';
 INSERT INTO providers.api_endpoints(api_id,method,path,price,active,tags) SELECT id,'GET','/sru/Search',0.085066,true,ARRAY['existing-tag'] FROM providers.apis WHERE slug='koop';`);
 const before=(await db.query("SELECT status,active,discoverable,is_verified,listing_metadata FROM providers.apis WHERE slug='koop'")).rows[0];
 await db.exec(migration);
 const count=async table=>(await db.query(`SELECT count(*)::int n FROM ${table}`)).rows[0].n;
 assert.equal(await count('providers.apis'),4);
 assert.equal(await count('providers.api_endpoints'),13);
 assert.equal(await count('catalog.capabilities'),11);
 assert.equal(await count('catalog.capability_execution_contracts'),11);
 assert.equal(await count('catalog.capability_offers'),11);
 assert.equal((await db.query("SELECT count(*)::int n FROM providers.apis WHERE slug<>'koop' AND (active OR discoverable OR is_verified OR status<>'inactive')")).rows[0].n,0);
 assert.equal((await db.query("SELECT count(*)::int n FROM catalog.capability_execution_contracts WHERE status<>'draft' OR validated_at IS NOT NULL")).rows[0].n,0);
 assert.equal((await db.query("SELECT count(*)::int n FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id WHERE a.slug<>'koop' AND (e.active OR e.payment_required OR cardinality(e.tags)=0 OR e.endpoint_category IS NULL OR e.task_summary IS NULL)")).rows[0].n,0);
 const after=(await db.query("SELECT status,active,discoverable,is_verified,listing_metadata FROM providers.apis WHERE slug='koop'")).rows[0];
 for(const k of ['status','active','discoverable','is_verified'])assert.deepEqual(after[k],before[k]);
 assert.equal(after.listing_metadata.unrelated_metadata,'preserve');
 assert.equal((await db.query("SELECT count(*)::int n FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id WHERE a.slug='koop' AND e.price=0.085066 AND e.active AND 'existing-tag'=ANY(e.tags)")).rows[0].n,2);
 assert.equal((await db.query("SELECT count(*)::int n FROM catalog.capabilities WHERE slug='nl.benefits.eligibility'")).rows[0].n,0);
 await assert.rejects(db.exec(migration),/slug already exists/); await db.exec('ROLLBACK');
 assert.equal(await count('providers.apis'),4,'duplicate attempt is atomic');
 // A late failure must roll back all source registrations and KOOP enrichment.
 await db.exec("DELETE FROM catalog.capability_offers; DELETE FROM catalog.capability_execution_contracts; DELETE FROM providers.api_endpoints WHERE api_id IN (SELECT id FROM providers.apis WHERE slug<>'koop'); DELETE FROM providers.apis WHERE slug<>'koop';");
 await assert.rejects(db.exec(migration.replace(/COMMIT;\s*$/,"SELECT 1/0; COMMIT;")),/division by zero/); await db.exec('ROLLBACK');
 assert.equal(await count('providers.apis'),1);
 console.log('PASS: 3 sources, 11 endpoints/capabilities/contracts/offers; inactive and unpriced; KOOP state/price/tags preserved; duplicate protection and rollback.');
} catch(error) { console.error(error.message, error.where??''); process.exitCode=1; } finally {await db.close();}
