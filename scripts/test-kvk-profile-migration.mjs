// Isolated local PostgreSQL test. No credentials, provider calls or production writes.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { PGlite } from '../../app/node_modules/@electric-sql/pglite/dist/index.js';

const fixture = JSON.parse(await fs.readFile(new URL('../tests/fixtures/research-report-catalog.json', import.meta.url), 'utf8'));
const service = fixture.services.find(s => s.path === '/api/v1/basisprofielen');
const migration = await fs.readFile(new URL('../supabase/migrations/20261003203509_support_kvk_sole_trader_profiles.sql', import.meta.url), 'utf8');
const db = new PGlite();
await db.waitReady;
try {
  await db.exec(`
    CREATE SCHEMA providers;
    CREATE SCHEMA catalog;
    CREATE TABLE providers.apis(id uuid PRIMARY KEY,slug text);
    CREATE TABLE providers.api_endpoints(id uuid PRIMARY KEY,api_id uuid,path text,method text,active boolean,price numeric);
    CREATE TABLE catalog.capability_execution_contracts(
      id uuid PRIMARY KEY DEFAULT gen_random_uuid(),capability_slug text,endpoint_id uuid,version int,status text,
      requires jsonb,produces jsonb,input_map jsonb,fixed_params jsonb,output_map jsonb,cardinality text,identifies text,
      max_fan_out int,preconditions jsonb,pagination jsonb,effect text,price_model text,freshness_seconds int,
      sample_payload jsonb,validated_at timestamptz,validated_note text,notes text,adapter_version text NOT NULL DEFAULT 'v1',created_at timestamptz DEFAULT now(),updated_at timestamptz DEFAULT now(),
      UNIQUE(capability_slug,endpoint_id,version));
    CREATE UNIQUE INDEX one_active_contract ON catalog.capability_execution_contracts(capability_slug,endpoint_id) WHERE status='active';
    INSERT INTO providers.apis VALUES('00000000-0000-4000-8000-000000000001','kvk-dutch-business-register');
  `);
  await db.query('INSERT INTO providers.api_endpoints VALUES($1,$2,$3,$4,true,$5)', [service.endpoint_id, '00000000-0000-4000-8000-000000000001', service.path, service.method, service.list_atomic / 1e6]);
  const produces = Object.keys(service.contract.output_map.produces).map(fact_type => ({
    fact_type, subject_role: 'subject', cardinality: 'one', optional: service.optional_outputs.includes(fact_type),
  }));
  for (const capability of ['company.registry-profile', 'company_registry.lookup']) {
    await db.query(`INSERT INTO catalog.capability_execution_contracts(
      capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,output_map,
      cardinality,identifies,max_fan_out,preconditions,pagination,effect,price_model,sample_payload,validated_note,notes,adapter_version)
      VALUES($1,$2,7,'active',$3,$4,$5,$6,$7,'one','company_registry.kvknummer',1,'{}','{}','read','per_call',$8,'prior validation','prior notes','reviewed-fixture-version')`,
    [capability, service.endpoint_id, service.contract.requires, produces, service.contract.input_map,
      service.fixed_params, service.contract.output_map, service.sample_payload]);
  }
  const rows = async () => (await db.query('SELECT * FROM catalog.capability_execution_contracts ORDER BY capability_slug,version')).rows;
  const original = await rows();
  const endpoints = (await db.query('SELECT * FROM providers.api_endpoints')).rows;
  await db.exec(migration);
  const repaired = await rows();
  assert.equal(repaired.length, 4);
  assert.deepEqual((await db.query('SELECT * FROM providers.api_endpoints')).rows, endpoints, 'endpoint price and wire settings are unchanged');
  for (const old of original) {
    const prior = repaired.find(r => r.id === old.id);
    assert.deepEqual({ ...prior, status: old.status, updated_at: old.updated_at }, old, 'only retire the old contract');
    const next = repaired.find(r => r.capability_slug === old.capability_slug && r.version === 8);
    assert.equal(next.status, 'active');
    assert.notEqual(next.id, old.id);
    const expectedOutput = structuredClone(old.output_map);
    expectedOutput.produces['company.name'] = { path: '$.naam' };
    assert.deepEqual(next.output_map, expectedOutput);
    assert.deepEqual(next.produces, old.produces.map(p => p.fact_type === 'company.profile' ? { ...p, optional: true } : p));
    const mutable = new Set(['id', 'status', 'version', 'produces', 'output_map', 'validated_at', 'validated_note', 'notes', 'created_at', 'updated_at']);
    for (const key of Object.keys(old).filter(k => !mutable.has(k))) assert.deepEqual(next[key], old[key], `${key} is preserved`);
  }
  await db.exec(migration);
  assert.deepEqual(await rows(), repaired, 'idempotent: no extra versions or writes');

  for (const [version, field, value] of [
    [7, 'requires', []],
    [7, 'output_map', {}],
    [7, 'produces', produces.map(p => p.fact_type === 'company.name' ? { ...p, optional: true } : p)],
    [8, 'output_map', {}],
    [8, 'fixed_params', { kvkNummer: '99999999' }],
  ]) {
    const originalRow = repaired.find(r => r.version === version);
    await db.query(`UPDATE catalog.capability_execution_contracts SET ${field}=$1 WHERE id=$2`, [value, originalRow.id]);
    await assert.rejects(db.exec(migration), /KVK v[78] contract drift/);
    await db.exec('ROLLBACK');
    assert.deepEqual((await rows()).find(r => r.id === originalRow.id)[field], value, 'drift rejected without overwrite');
    await db.query(`UPDATE catalog.capability_execution_contracts SET ${field}=$1 WHERE id=$2`, [originalRow[field], originalRow.id]);
  }
  console.log('PASS: both KVK contracts versioned; historical payloads, wire settings and prices preserved; migration idempotent and drift rejected.');
} finally {
  await db.close();
}
