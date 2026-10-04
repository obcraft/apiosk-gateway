// Isolated PostgreSQL metadata regression: no credentials, network or provider calls.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { PGlite } from '../../app/node_modules/@electric-sql/pglite/dist/index.js';

const read = path => fs.readFile(new URL(path, import.meta.url), 'utf8');
const kvk = JSON.parse(await read('../tests/fixtures/research-report-catalog.json')).services.find(s => s.path === '/api/v1/basisprofielen');
const linkup = JSON.parse(await read('../tests/fixtures/search-source-repairs/catalog.json')).services.find(s => s.api_slug === 'linkup' && s.path === '/v1/search');
const v8 = await read('../supabase/migrations/20261003203509_support_kvk_sole_trader_profiles.sql');
const migration = await read('../supabase/migrations/20261004182009_kvk_registered_website_bridge.sql');
const db = new PGlite();
await db.waitReady;
try {
  await db.exec(`
    CREATE SCHEMA providers;
    CREATE SCHEMA catalog;
    CREATE TABLE providers.apis(id uuid PRIMARY KEY,slug text);
    CREATE TABLE providers.api_endpoints(id uuid PRIMARY KEY,api_id uuid,path text,method text,active boolean,price numeric);
    CREATE TABLE catalog.fact_types(slug text PRIMARY KEY,name text NOT NULL,description text NOT NULL,
      subject_kind text NOT NULL,value_schema jsonb NOT NULL,status text NOT NULL);
    CREATE TABLE catalog.capabilities(slug text PRIMARY KEY,name text NOT NULL,description text NOT NULL,
      category text NOT NULL,input_contract jsonb NOT NULL,output_contract jsonb NOT NULL,status text NOT NULL);
    CREATE TABLE catalog.capability_offers(capability_slug text NOT NULL,endpoint_id uuid NOT NULL,api_id uuid NOT NULL,
      source text NOT NULL,confidence numeric DEFAULT 1.00,input_map jsonb NOT NULL DEFAULT '{}',
      fixed_params jsonb NOT NULL DEFAULT '{}',notes text,created_at timestamptz DEFAULT now(),
      updated_at timestamptz DEFAULT now(),output_map jsonb,PRIMARY KEY(capability_slug,endpoint_id));
    CREATE TABLE catalog.capability_execution_contracts(
      id uuid PRIMARY KEY DEFAULT gen_random_uuid(),capability_slug text,endpoint_id uuid,version int,status text,
      requires jsonb,produces jsonb,input_map jsonb,fixed_params jsonb,output_map jsonb,cardinality text,identifies text,
      max_fan_out int,preconditions jsonb,pagination jsonb,effect text,price_model text,freshness_seconds int,
      sample_payload jsonb,validated_at timestamptz,validated_note text,notes text,
      adapter_version text NOT NULL DEFAULT 'v1',created_at timestamptz DEFAULT now(),updated_at timestamptz DEFAULT now(),
      UNIQUE(capability_slug,endpoint_id,version));
    CREATE UNIQUE INDEX one_active_contract ON catalog.capability_execution_contracts(capability_slug,endpoint_id) WHERE status='active';
  `);
  for (const [service, apiId, aliases, version] of [
    [kvk, '00000000-0000-4000-8000-000000000001', ['company.registry-profile', 'company_registry.lookup'], 7],
    [linkup, '00000000-0000-4000-8000-000000000002', ['web.linkup-answer'], 1],
  ]) {
    await db.query('INSERT INTO providers.apis VALUES($1,$2)', [apiId, service.api_slug]);
    await db.query('INSERT INTO providers.api_endpoints VALUES($1,$2,$3,$4,true,$5)',
      [service.endpoint_id, apiId, service.path, service.method, service.list_atomic / 1e6]);
    const produces = Object.keys(service.contract.output_map.produces).map(fact_type => ({
      fact_type, subject_role: 'subject', cardinality: 'one', optional: (service.optional_outputs ?? []).includes(fact_type),
    }));
    for (const capability of aliases) {
      await db.query(`INSERT INTO catalog.capability_execution_contracts(
        capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,output_map,
        cardinality,identifies,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,
        sample_payload,validated_note,notes,adapter_version)
        VALUES($1,$2,$3,'active',$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,'read','per_call',$14,$15,'prior validation','prior notes','reviewed-fixture-version')`,
      [capability, service.endpoint_id, version, service.contract.requires, produces, service.contract.input_map,
        service.fixed_params, service.contract.output_map, service.contract.cardinality, service.contract.identifies,
        service.contract.max_fan_out, service.preconditions ?? {}, service.pagination ?? {}, service.freshness_seconds,
        service.sample_payload]);
      await db.query(`INSERT INTO catalog.capability_offers(capability_slug,endpoint_id,api_id,source,input_map,fixed_params,notes,output_map)
        VALUES($1,$2,$3,'reviewed_fixture',$4,$5,'original offer',$6)`,
      [capability, service.endpoint_id, apiId, service.contract.input_map, service.fixed_params, service.contract.output_map]);
    }
  }
  const rows = async () => (await db.query('SELECT * FROM catalog.capability_execution_contracts ORDER BY capability_slug,version')).rows;
  const snapshot = async () => Object.fromEntries(await Promise.all([
    'providers.apis', 'providers.api_endpoints', 'catalog.fact_types', 'catalog.capabilities',
    'catalog.capability_offers', 'catalog.capability_execution_contracts',
  ].map(async table => [table, (await db.query(`SELECT to_jsonb(t) AS row FROM ${table} t ORDER BY to_jsonb(t)::text`)).rows])));
  await db.exec(v8);
  const original = await rows();
  const endpoints = (await db.query('SELECT * FROM providers.api_endpoints ORDER BY id')).rows;
  const offers = (await db.query('SELECT * FROM catalog.capability_offers ORDER BY capability_slug')).rows;

  // Late failure occurs after both KVK upgrades and vocabulary inserts. The
  // transaction must roll every partial write back, including new catalog rows.
  const baseLinkup = original.find(r => r.capability_slug === 'web.linkup-answer');
  await db.query('UPDATE catalog.capability_execution_contracts SET fixed_params=$1 WHERE id=$2',
    [{ depth: 'deep', outputType: 'sourcedAnswer' }, baseLinkup.id]);
  const beforeFailure = await snapshot();
  await assert.rejects(db.exec(migration), /Linkup sourced-answer v1 drift/);
  await db.exec('ROLLBACK');
  assert.deepEqual(await snapshot(), beforeFailure, 'late drift rolls back every migration write');
  await db.query('UPDATE catalog.capability_execution_contracts SET fixed_params=$1 WHERE id=$2',
    [baseLinkup.fixed_params, baseLinkup.id]);

  await db.exec(migration);
  const repaired = await rows();
  assert.equal(repaired.length, 8);
  assert.deepEqual((await db.query('SELECT * FROM providers.api_endpoints ORDER BY id')).rows, endpoints,
    'provider identity, endpoint wire settings and prices remain byte-for-byte unchanged');
  for (const old of original) {
    const prior = repaired.find(r => r.id === old.id);
    const isReplaced = old.endpoint_id === kvk.endpoint_id && old.version === 8;
    assert.deepEqual(isReplaced ? { ...prior, status: old.status, updated_at: old.updated_at } : prior, old,
      'historical rows remain intact; only active v8 contracts retire');
    if (!isReplaced) continue;
    assert.equal(prior.status, 'retired');
    const next = repaired.find(r => r.capability_slug === old.capability_slug && r.version === 9);
    assert.equal(next.status, 'active');
    assert.notEqual(next.id, old.id);
    const expectedOutput = structuredClone(old.output_map);
    expectedOutput.produces['web.company_website_query'] = {
      adapter: 'record.website-query',
      key: '{"name":"/naam","identifier":"/kvkNummer","registry":"KVK","pattern":"^[0-9]{8}$"}',
    };
    assert.deepEqual(next.output_map, expectedOutput);
    assert.deepEqual(next.produces, [...old.produces, {
      fact_type: 'web.company_website_query', subject_role: 'subject', cardinality: 'one', optional: true,
    }], 'auxiliary query absence does not break ordinary profile requests');
    const changed = new Set(['id', 'status', 'version', 'produces', 'output_map', 'validated_at', 'validated_note', 'notes', 'created_at', 'updated_at']);
    for (const key of Object.keys(old).filter(k => !changed.has(k))) assert.deepEqual(next[key], old[key], `${key} preserved in KVK v9`);
  }
  const alias = repaired.find(r => r.capability_slug === 'web.company-website-search');
  assert.deepEqual(alias.requires, [{ fact_type: 'web.company_website_query', subject_role: 'subject', optional: false }]);
  assert.deepEqual(alias.input_map, { 'web.company_website_query': { name: 'q', location: 'body' } });
  const aliasChanged = new Set(['id', 'capability_slug', 'requires', 'input_map', 'validated_at', 'validated_note', 'notes', 'created_at', 'updated_at']);
  for (const key of Object.keys(baseLinkup).filter(k => !aliasChanged.has(k))) assert.deepEqual(alias[key], baseLinkup[key], `${key} preserved in Linkup alias`);
  const finalOffers = (await db.query('SELECT * FROM catalog.capability_offers ORDER BY capability_slug')).rows;
  for (const offer of offers) assert.deepEqual(finalOffers.find(o => o.capability_slug === offer.capability_slug), offer);
  const aliasOffer = finalOffers.find(o => o.capability_slug === alias.capability_slug);
  assert.equal(aliasOffer.endpoint_id, linkup.endpoint_id);
  assert.deepEqual(aliasOffer.input_map, alias.input_map);
  assert.deepEqual(aliasOffer.fixed_params, baseLinkup.fixed_params);
  assert.deepEqual(aliasOffer.output_map, baseLinkup.output_map);
  assert.deepEqual((await db.query("SELECT value_schema FROM catalog.fact_types WHERE slug='web.company_website_query'")).rows[0].value_schema,
    { type: 'string', minLength: 1, maxLength: 1000 });

  const stable = await snapshot();
  await db.exec(migration);
  assert.deepEqual(await snapshot(), stable, 'idempotent across contracts, offers and vocabulary');
  for (const [row, field, value] of [
    [original.find(r => r.version === 8), 'requires', []],
    [repaired.find(r => r.version === 9), 'output_map', {}],
    [alias, 'input_map', { 'web.source_search_query': { name: 'q', location: 'body' } }],
  ]) {
    await db.query(`UPDATE catalog.capability_execution_contracts SET ${field}=$1 WHERE id=$2`, [value, row.id]);
    const before = await snapshot();
    await assert.rejects(db.exec(migration), /contract drift|search v1 drift/);
    await db.exec('ROLLBACK');
    assert.deepEqual(await snapshot(), before, 'existing drift is never overwritten');
    const clean = repaired.find(r => r.id === row.id);
    await db.query(`UPDATE catalog.capability_execution_contracts SET ${field}=$1 WHERE id=$2`, [clean[field], row.id]);
  }
  console.log('PASS: KVK v8→v9 and registry-bound Linkup alias; optional query; unchanged prices, wire settings, samples and historical rows; idempotency and complete rollback on drift.');
} finally {
  await db.close();
}
