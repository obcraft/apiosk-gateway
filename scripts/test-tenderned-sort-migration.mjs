// Exercises only an isolated local PostgreSQL fixture; no production writes.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { PGlite } from '@electric-sql/pglite';

const fixture = JSON.parse(await fs.readFile(new URL('../crates/commerce-runtime/src/fixtures/tenderned-wire-binding.json', import.meta.url), 'utf8'));
const migration = await fs.readFile(new URL('../supabase/migrations/20261003130359_preserve_tenderned_publication_sort.sql', import.meta.url), 'utf8');
const original = structuredClone(fixture.query_params);
delete original.sort;
const db = new PGlite();
await db.waitReady;
try {
  await db.exec(`
    CREATE SCHEMA providers;
    CREATE TABLE providers.apis(id uuid PRIMARY KEY,slug text,origin_url text,endpoint_url text);
    CREATE TABLE providers.api_endpoints(id uuid PRIMARY KEY,api_id uuid,path text,method text,active boolean,query_params jsonb,price numeric);
    INSERT INTO providers.apis VALUES('00000000-0000-4000-8000-000000000001','tenderned','https://www.tenderned.nl/papi/tenderned-rs-tns','https://www.tenderned.nl/papi/tenderned-rs-tns');
  `);
  for (const [id, method] of [
    ['00000000-0000-4000-8000-000000000002', 'GET'],
    ['00000000-0000-4000-8000-000000000003', 'POST'],
  ]) {
    await db.query(`INSERT INTO providers.api_endpoints VALUES($1,'00000000-0000-4000-8000-000000000001','/v2/publicaties',$2,true,$3,0.047259)`, [id, method, original]);
  }
  const rows = async () => (await db.query('SELECT * FROM providers.api_endpoints ORDER BY id')).rows;
  const before = await rows();
  await db.exec(migration);
  const expected = structuredClone(before);
  expected[0].query_params = fixture.query_params;
  assert.deepEqual(await rows(), expected, 'only the targeted query sort declaration changes');
  await db.exec(migration);
  assert.deepEqual(await rows(), expected, 'migration is idempotent');

  // A provider-schema change or a previously different sort declaration needs
  // a new review, not an overwrite that quietly invalidates an agreed request.
  for (const drifted of [
    { ...original, unexpected: { type: 'string' } },
    { ...original, sort: { type: 'integer' } },
  ]) {
    await db.query('UPDATE providers.api_endpoints SET query_params=$1 WHERE method=$2', [drifted, 'GET']);
    await assert.rejects(db.exec(migration), /TenderNed query schema drift/);
    await db.exec('ROLLBACK');
    assert.deepEqual((await rows())[0].query_params, drifted, 'rejected drift remains untouched');
  }
  console.log('PASS: TenderNed sort migration preserves other parameters and tariff; idempotent; schema drift rejected.');
} finally {
  await db.close();
}
