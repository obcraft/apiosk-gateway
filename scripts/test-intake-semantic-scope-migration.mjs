// Isolated metadata regression: no production reads, writes or provider calls.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { PGlite } from '../../app/node_modules/@electric-sql/pglite/dist/index.js';

const fixture = JSON.parse(await fs.readFile(new URL('../tests/fixtures/semantic-intake-aliases/reviewed-schemas.json', import.meta.url), 'utf8'));
const sql = await fs.readFile(new URL('../supabase/migrations/20261011194903_reviewed_intake_semantic_scope.sql', import.meta.url), 'utf8');
const endpoint = '11111111-1111-4111-8111-111111111111';
const db = new PGlite();
await db.waitReady;
const normalize = value => {
  if (Array.isArray(value)) return value.map(normalize);
  if (!value || typeof value !== 'object') return value;
  return Object.fromEntries(Object.entries(value).map(([key, child]) => [key, key === '$comment' ? JSON.parse(child) : normalize(child)]));
};
try {
  await db.exec(`CREATE SCHEMA catalog; CREATE SCHEMA providers;
    CREATE TABLE catalog.fact_types(slug text PRIMARY KEY,status text,value_schema jsonb);
    CREATE TABLE providers.apis(id uuid PRIMARY KEY,slug text);
    CREATE TABLE providers.api_endpoints(id uuid PRIMARY KEY,api_id uuid,method text,path text,active boolean);
    INSERT INTO providers.apis VALUES ('22222222-2222-4222-8222-222222222222','frankfurter');
    INSERT INTO providers.api_endpoints VALUES ('${endpoint}','22222222-2222-4222-8222-222222222222','GET','/v2/providers/ecb/rate/EUR/{quote}',true);`);
  for (const fact of fixture.before) await db.query('INSERT INTO catalog.fact_types VALUES ($1,$2,$3)', [fact.slug, 'active', fact.value_schema]);
  const schemas = async () => (await db.query('SELECT slug,value_schema FROM catalog.fact_types ORDER BY slug')).rows;
  const expected = fixture.after.map(({ slug, value_schema }) => ({ slug, value_schema: structuredClone(value_schema) })).sort((a,b)=>a.slug < b.slug ? -1 : a.slug > b.slug ? 1 : 0);
  const ecb = expected.find(fact=>fact.slug==='fx.ecb.reference_rate');
  const annotation = JSON.parse(ecb.value_schema.$comment);
  annotation.apiosk_fixed_scope_v1[0].endpoint_id = endpoint;
  ecb.value_schema.$comment = JSON.stringify(annotation);
  await db.exec(sql);
  assert.deepEqual(normalize(await schemas()), normalize(expected));
  await db.exec(sql);
  assert.deepEqual(normalize(await schemas()), normalize(expected), 'idempotent');
  const splitFixture = JSON.parse(await fs.readFile(new URL('../tests/fixtures/ecb-split-currency/reviewed-schemas.json', import.meta.url), 'utf8'));
  const splitSql = await fs.readFile(new URL('../supabase/migrations/20261011195845_extend_ecb_currency_input_scope.sql', import.meta.url), 'utf8');
  for (const fact of splitFixture.before) await db.query('INSERT INTO catalog.fact_types VALUES ($1,$2,$3) ON CONFLICT (slug) DO NOTHING', [fact.slug, 'active', fact.value_schema]);
  await db.exec(splitSql);
  for (const fact of splitFixture.after) {
    const expectedSchema = structuredClone(fact.value_schema);
    if (fact.slug === 'fx.ecb.reference_rate') {
      const metadata = JSON.parse(expectedSchema.$comment);
      metadata.apiosk_fixed_scope_v1[0].endpoint_id = endpoint;
      expectedSchema.$comment = JSON.stringify(metadata);
    }
    assert.deepEqual(normalize((await schemas()).find(row => row.slug === fact.slug).value_schema), normalize(expectedSchema));
  }
  const beforeTime = (await schemas()).find(fact => fact.slug === 'fx.ecb.reference_rate').value_schema;
  const timeSql = await fs.readFile(new URL('../supabase/migrations/20261011200710_extend_ecb_latest_time_scope.sql', import.meta.url), 'utf8');
  await db.exec(timeSql);
  const updated = (await schemas()).find(fact => fact.slug === 'fx.ecb.reference_rate').value_schema;
  const timeScope = JSON.parse(updated.$comment).apiosk_fixed_scope_v1[0];
  for (const key of ['period', 'as_of', 'date', 'time_period']) {
    assert.deepEqual(timeScope.inputs[key], ['latest', 'most recent', 'laatste', 'meest recente']);
    assert.ok(!timeScope.inputs[key].includes('2025-01-01'), 'historical dates remain unsupported');
  }
  assert.deepEqual({...updated, $comment: undefined}, {...beforeTime, $comment: undefined}, 'only reviewed scope metadata changes');
  await db.exec(timeSql);
  assert.deepEqual((await schemas()).find(fact => fact.slug === 'fx.ecb.reference_rate').value_schema, updated, 'temporal migration is idempotent');
  const invalidScope = structuredClone(updated);
  const invalidAnnotation = JSON.parse(invalidScope.$comment);
  invalidAnnotation.apiosk_fixed_scope_v1[0].inputs.period = ['all dates'];
  invalidScope.$comment = JSON.stringify(invalidAnnotation);
  await db.query("UPDATE catalog.fact_types SET value_schema=$1 WHERE slug='fx.ecb.reference_rate'", [invalidScope]);
  await assert.rejects(db.exec(timeSql), /different scope/);
  await db.exec('ROLLBACK');
  await db.query("UPDATE catalog.fact_types SET value_schema=$1 WHERE slug='fx.ecb.reference_rate'", [updated]);
  const qualifierSql = await fs.readFile(new URL('../supabase/migrations/20261011201034_extend_ecb_qualifier_vocabulary.sql', import.meta.url), 'utf8');
  await db.exec(qualifierSql);
  const qualifiers = (await schemas()).find(fact => fact.slug === 'fx.ecb.reference_rate').value_schema;
  const qualifierScope = JSON.parse(qualifiers.$comment).apiosk_fixed_scope_v1[0];
  for (const key of ['recency', 'time_scope']) {
    assert.deepEqual(qualifierScope.inputs[key], timeScope.inputs.observation_date);
    assert.ok(!qualifierScope.inputs[key].includes('2025-01-01'), 'aliases never permit historical dates');
  }
  for (const key of ['authority', 'reference_authority']) {
    assert.deepEqual(qualifierScope.inputs[key], ['ECB']);
    assert.ok(!qualifierScope.inputs[key].includes('Federal Reserve'), 'aliases never permit another authority');
  }
  const originalMetadata = JSON.parse(updated.$comment);
  const restoredMetadata = JSON.parse(qualifiers.$comment);
  for (const key of ['recency', 'time_scope', 'authority', 'reference_authority']) delete restoredMetadata.apiosk_fixed_scope_v1[0].inputs[key];
  assert.deepEqual(restoredMetadata, originalMetadata, 'all existing endpoint and qualifier metadata is preserved');
  assert.deepEqual({...qualifiers, $comment: undefined}, {...updated, $comment: undefined}, 'no value validation or output alias changes');
  await db.exec(qualifierSql);
  assert.deepEqual((await schemas()).find(fact => fact.slug === 'fx.ecb.reference_rate').value_schema, qualifiers, 'qualifier migration is idempotent');
  for (const [name, mutate, expectedError] of [
    ['alias scope', scope => { scope.inputs.recency = ['all dates']; }, /different scope/],
    ['canonical date scope', scope => { scope.inputs.observation_date = ['all dates']; }, /canonical qualifier values changed/],
    ['canonical authority scope', scope => { scope.inputs.reference_source = ['Federal Reserve']; }, /canonical qualifier values changed/],
    ['endpoint identity', scope => { scope.endpoint_id = '33333333-3333-4333-8333-333333333333'; }, /identity changed/],
    ['route identity', scope => { scope.path = '/historical'; }, /identity changed/],
    ['input scope', scope => { scope.input_facts.push('observation_date'); }, /identity changed/],
  ]) {
    const invalid = structuredClone(qualifiers);
    const metadata = JSON.parse(invalid.$comment);
    mutate(metadata.apiosk_fixed_scope_v1[0]);
    invalid.$comment = JSON.stringify(metadata);
    await db.query("UPDATE catalog.fact_types SET value_schema=$1 WHERE slug='fx.ecb.reference_rate'", [invalid]);
    await assert.rejects(db.exec(qualifierSql), expectedError, name);
    await db.exec('ROLLBACK');
    assert.deepEqual((await schemas()).find(fact => fact.slug === 'fx.ecb.reference_rate').value_schema, invalid, 'failed migration leaves schema intact');
  }
  await db.query("UPDATE catalog.fact_types SET value_schema=$1 WHERE slug='fx.ecb.reference_rate'", [qualifiers]);
  await db.query("UPDATE catalog.fact_types SET value_schema=$1 WHERE slug='company.profile'", [{type:'object',properties:{unreviewed:{type:'string'}}}]);
  await assert.rejects(db.exec(sql), /Reviewed intake schema drift/);
  await db.exec('ROLLBACK');
  assert.deepEqual((await schemas()).find(f=>f.slug==='company.profile').value_schema, {type:'object',properties:{unreviewed:{type:'string'}}});
  console.log('PASS: reviewed semantic, currency, temporal and qualifier migrations preserve exact scope, endpoint identity, idempotency and drift rejection.');
} finally { await db.close(); }
