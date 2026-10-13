import fs from 'node:fs/promises';
import { PGlite } from '../../../../app/node_modules/@electric-sql/pglite/dist/index.js';
export async function registrationDatabase() {
 const db=new PGlite();
 const snapshot=JSON.parse(await fs.readFile(new URL('../schema-snapshot.json',import.meta.url),'utf8'));
 const tables=snapshot.import_tables;
 const q=s=>'"'+s.replaceAll('"','""')+'"';
 const owner='bf085fe8-41c4-4699-978f-e75e89518bf7';
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
 return db;
}
