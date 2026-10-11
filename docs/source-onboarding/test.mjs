import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { PGlite } from '../../../app/node_modules/@electric-sql/pglite/dist/index.js';
import { render } from './render.mjs';

const snapshot = JSON.parse(await fs.readFile(new URL('schema-snapshot.json', import.meta.url)));
const example = JSON.parse(await fs.readFile(new URL('manifest.example.json', import.meta.url)));
const db = new PGlite();
const tables = ['providers.apis','providers.api_endpoints','catalog.fact_subject_kinds',
  'catalog.fact_types','catalog.capabilities','catalog.capability_execution_contracts','catalog.capability_offers'];
const q = name => `"${name.replaceAll('"','""')}"`;
const count = async table => (await db.query(`SELECT count(*)::int AS n FROM ${table}`)).rows[0].n;
const clone = () => structuredClone(example);
async function fails(bundle, pattern) {
  const before = await Promise.all(tables.map(count));
  await assert.rejects(db.exec(await render(bundle)), pattern);
  await db.exec('ROLLBACK');
  assert.deepEqual(await Promise.all(tables.map(count)), before, 'failure must roll back all catalog writes');
}
try {
  await db.exec(`CREATE SCHEMA providers; CREATE SCHEMA catalog; CREATE SCHEMA auth;
    CREATE SCHEMA wallets; CREATE SCHEMA billing; CREATE SCHEMA vault;
    CREATE TYPE public.listing_type AS ENUM ('api','skill','product','dataset','service','connector','federated');
    CREATE TABLE auth.users(id uuid PRIMARY KEY);
    INSERT INTO auth.users VALUES ('00000000-0000-0000-0000-000000000000');
    CREATE TABLE wallets.network_tokens(id uuid PRIMARY KEY);
    CREATE TABLE billing.commission_tiers(id uuid PRIMARY KEY);
    CREATE TABLE vault.secrets(id uuid PRIMARY KEY DEFAULT gen_random_uuid(),name text UNIQUE,secret text);
    CREATE FUNCTION vault.create_secret(secret text,name text,description text) RETURNS uuid LANGUAGE sql AS
      $$ INSERT INTO vault.secrets(name,secret) VALUES(name,secret) RETURNING id $$;`);
  // Actual read-only production column/default/constraint snapshot. The Vault
  // fixture tests transaction semantics only, not Supabase encryption.
  for (const table of tables) {
    const cols = snapshot.columns.filter(c => `${c.table_schema}.${c.table_name}` === table);
    const defs = cols.map(c => `${q(c.column_name)} ${c.data_type === 'ARRAY' ? 'text[]' : c.data_type === 'USER-DEFINED' ? `${q(c.udt_schema)}.${q(c.udt_name)}` : c.data_type}${c.column_default ? ` DEFAULT ${c.column_default}` : ''}${c.is_nullable === 'NO' ? ' NOT NULL' : ''}`);
    await db.exec(`CREATE TABLE ${table} (${defs.join(',')})`);
  }
  const constraints = snapshot.constraints.filter(c => tables.includes(`${c.schema}.${c.table_name}`))
    .sort((a,b) => Number(a.definition.startsWith('FOREIGN KEY')) - Number(b.definition.startsWith('FOREIGN KEY')));
  for (const c of constraints) {
    await db.exec(`ALTER TABLE ${c.schema}.${c.table_name} ADD CONSTRAINT ${q(c.conname)} ${c.definition}`);
  }
  const hooks = ['apiosk_auto_verify_owner_apis()','apiosk_normalize_api_category()',
    'apiosk_apis_review_gate()','apiosk_sync_apis_active_with_status()','require_payout_wallet_to_publish()'];
  for (const f of snapshot.trigger_functions.filter(f => hooks.includes(f.signature))) await db.exec(f.definition);
  for (const t of snapshot.triggers.filter(t => hooks.includes(t.function))) await db.exec(t.definition);

  await db.exec(await render(example));
  assert.equal(await count('providers.apis'),1);
  const state = (await db.query(`SELECT a.status,a.active,a.discoverable,a.is_verified,a.verified_at,
    a.settlement_mode,a.requires_payout_wallet,e.active AS endpoint_active,e.price,c.status AS contract_status,
    c.validated_at,c.requires,c.output_map FROM providers.apis a JOIN providers.api_endpoints e ON e.api_id=a.id
    JOIN catalog.capability_execution_contracts c ON c.endpoint_id=e.id`)).rows[0];
  assert.equal(state.status,'inactive');
  for (const k of ['active','discoverable','is_verified','endpoint_active','requires_payout_wallet']) assert.equal(state[k],false);
  assert.equal(state.verified_at,null); assert.equal(state.validated_at,null);
  assert.equal(state.contract_status,'draft'); assert.equal(state.settlement_mode,'apiosk_revenue');
  assert.equal(Number(state.price),0.05);
  assert.deepEqual(state.requires,example.contracts[0].requires);
  assert.deepEqual(state.output_map,example.contracts[0].output_map);
  await fails(example,/slug already exists/);

  const broken = clone(); broken.source.slug='broken-relationship'; broken.contracts[0].requires[0].fact_type='company.missing';
  await fails(broken,/Unknown fact/);
  const badOwner = clone(); badOwner.source.slug='bad-owner'; badOwner.source.owner_id='11111111-1111-1111-1111-111111111111';
  await fails(badOwner,/existing auth.users/);
  const drift = clone(); drift.source.slug='vocabulary-drift'; drift.fact_types[0].value_schema.type='number';
  await fails(drift,/Shared fact definition differs/);
  const missingMap = clone(); missingMap.source.slug='missing-mapping'; missingMap.contracts[0].input_map={};
  await fails(missingMap,/requires a mapping/);
  const duplicate = clone(); duplicate.source.slug='duplicate-endpoint'; duplicate.endpoints.push(structuredClone(duplicate.endpoints[0]));
  await fails(duplicate,/unique bundle-local key/);
  const unsafe = clone(); unsafe.source.static_headers=[{name:'Accept',value:'application/json\r\nX-Test: injected'}]; unsafe.source.slug='unsafe-header';
  await fails(unsafe,/Only public/);
  const price = clone(); price.source.slug='bad-price'; price.endpoints[0].price=0.0000001;
  await fails(price,/micro-USD precision/);
  const unknown = clone(); unknown.endpoints[0].active=true;
  await assert.rejects(render(unknown),/unknown field/);
  const injection = clone(); injection.source.slug='quoted-provider';
  injection.source.name="Provider '$manifest$ $onboard$ $$; DROP TABLE providers.apis; --";
  await db.exec(await render(injection));
  assert.equal(await count('providers.apis'),2);
  assert.equal((await db.query("SELECT name FROM providers.apis WHERE slug='quoted-provider'")).rows[0].name,injection.source.name);

  const multi = clone(); multi.source.slug='multiple-endpoints';
  const secondEndpoint = structuredClone(multi.endpoints[0]); secondEndpoint.key='company-profile-v2'; secondEndpoint.path='/v2/companies/{id}';
  multi.endpoints.push(secondEndpoint);
  const secondContract = structuredClone(multi.contracts[0]); secondContract.endpoint_key='company-profile-v2';
  multi.contracts.push(secondContract);
  await db.exec(await render(multi));
  const links = (await db.query(`SELECT count(DISTINCT e.id)::int AS endpoints,count(c.id)::int AS contracts
    FROM providers.apis a JOIN providers.api_endpoints e ON e.api_id=a.id
    JOIN catalog.capability_execution_contracts c ON c.endpoint_id=e.id WHERE a.slug='multiple-endpoints'`)).rows[0];
  assert.deepEqual(links,{endpoints:2,contracts:2});
  const metadata = clone(); metadata.source.slug='null-metadata'; metadata.source.listing_metadata=null;
  await fails(metadata,/Source metadata/);

  const keyBundle = clone(); keyBundle.source.slug='vault-example';
  const sql = await render(keyBundle);
  assert.ok(!sql.includes('DUMMY_LOCAL_TEST_SECRET'));
  await db.query("SELECT set_config('apiosk.onboarding_secrets',$1,false)",[JSON.stringify({APIOSK_EXAMPLE_COMPANY_KEY:'DUMMY_LOCAL_TEST_SECRET'})]);
  await db.exec(sql);
  assert.equal(await count('vault.secrets'),1);
  assert.equal((await db.query("SELECT count(*)::int AS n FROM providers.apis WHERE listing_metadata::text LIKE '%DUMMY_LOCAL_TEST_SECRET%'")).rows[0].n,0);
  // Session-local input must be explicitly cleared by a pooled backend after commit.
  await db.query("SELECT set_config('apiosk.onboarding_secrets',$1,false)",['{}']);
  const conflict = clone(); conflict.source.slug='key-conflict';
  await db.query("SELECT set_config('apiosk.onboarding_secrets',$1,false)",[JSON.stringify({APIOSK_EXAMPLE_COMPANY_KEY:'DUMMY_REPLACEMENT'})]);
  await fails(conflict,/Vault name already exists/);
  assert.equal((await db.query('SELECT secret FROM vault.secrets')).rows[0].secret,'DUMMY_LOCAL_TEST_SECRET');
  await db.query("SELECT set_config('apiosk.onboarding_secrets',$1,false)",['{}']);

  const vaultRollback = clone(); vaultRollback.source.slug='vault-rollback';
  vaultRollback.credential_requirements[0].vault_name='NEW_UNUSED_TEST_KEY';
  await db.query("SELECT set_config('apiosk.onboarding_secrets',$1,false)",[JSON.stringify({NEW_UNUSED_TEST_KEY:'DUMMY_LOCAL',UNDECLARED_KEY:'DUMMY_EXTRA'})]);
  await fails(vaultRollback,/undeclared Vault name/);
  assert.equal(await count('vault.secrets'),1,'late validation failure rolls back newly provisioned key');
  console.log('PASS: production-shaped SQL, staging triggers, relationships, shared vocabulary, duplicate import, invalid input, rollback, safe SQL quoting, Vault binding/no-overwrite/no-catalog-secret. Local only; no production writes or API calls.');
} catch (error) {
  console.error(error.message, error.where ?? '');
  process.exitCode = 1;
} finally { await db.close(); }
