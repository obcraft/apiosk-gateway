import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { PGlite } from '../../../../app/node_modules/@electric-sql/pglite/dist/index.js';
const here = new URL('./',import.meta.url);
const snapshot=JSON.parse(await fs.readFile(new URL('../schema-snapshot.json',here),'utf8'));
const migration=await fs.readFile(new URL('../../../supabase/migrations/20261013181301_onboard_nl_benefits_runtime.sql',here),'utf8');
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
 await db.exec(migration);
 const count=async table=>(await db.query(`SELECT count(*)::int n FROM ${table}`)).rows[0].n;
 for(const [table,expected] of [['providers.apis',2],['providers.api_endpoints',5],['catalog.capabilities',5],['catalog.capability_execution_contracts',5],['catalog.capability_offers',5]]) assert.equal(await count(table),expected,table);
 assert.equal((await db.query("SELECT count(*)::int n FROM providers.apis WHERE active OR discoverable OR is_verified OR status<>'inactive'")).rows[0].n,0);
 assert.equal((await db.query("SELECT count(*)::int n FROM catalog.capability_execution_contracts WHERE status<>'draft' OR sample_payload IS NULL OR adapter_version<>'nl-benefits-v1'")).rows[0].n,0);
 assert.equal((await db.query("SELECT count(*)::int n FROM providers.api_endpoints WHERE active OR NOT payment_required OR cache_ttl_seconds<>0 OR cardinality(tags)=0 OR endpoint_category<>'government' OR request_body IS NULL OR response_example IS NULL")).rows[0].n,0);
 await assert.rejects(db.exec(migration),/slug already exists/); await db.exec('ROLLBACK');
 assert.equal(await count('providers.apis'),2);
 console.log('PASS: two sources, five priced POST endpoints and complete draft contracts; inactive, private uncached results, samples, tags, category, duplicate rollback.');
} catch(error) { console.error(error.message,error.where??'');process.exitCode=1; } finally {await db.close();}
