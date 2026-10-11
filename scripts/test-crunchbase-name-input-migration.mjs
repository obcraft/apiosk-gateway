// Reviewed semantic metadata only; no provider calls or production writes.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { PGlite } from '../../app/node_modules/@electric-sql/pglite/dist/index.js';
const fixture=JSON.parse(await fs.readFile(new URL('../tests/fixtures/crunchbase/typed-name-input.json',import.meta.url),'utf8'));
const sql=await fs.readFile(new URL('../supabase/migrations/20261011205913_declare_crunchbase_typed_name_inputs.sql',import.meta.url),'utf8');
const db=new PGlite();await db.waitReady;
const schema=async()=> (await db.query('SELECT value_schema FROM catalog.fact_types WHERE slug=$1',[fixture.slug])).rows[0].value_schema;
try {
  await db.exec(`CREATE SCHEMA providers; CREATE SCHEMA catalog;
    CREATE TABLE providers.apis(id uuid PRIMARY KEY,slug text);
    CREATE TABLE providers.api_endpoints(id uuid PRIMARY KEY,api_id uuid,method text,path text,active boolean);
    CREATE TABLE catalog.capability_execution_contracts(endpoint_id uuid,status text,capability_slug text,input_map jsonb);
    CREATE TABLE catalog.fact_types(slug text PRIMARY KEY,status text,value_schema jsonb);
    INSERT INTO providers.apis VALUES('11111111-1111-4111-8111-111111111111','apimarket-magicapi-crunchbase-bbcca0a6');
    INSERT INTO providers.api_endpoints VALUES('22222222-2222-4222-8222-222222222222','11111111-1111-4111-8111-111111111111','POST','/mcp/cb_entity_autocomplete',true);`);
  for (const capability of ['company.crunchbase-official-search','person.crunchbase-search','research.crunchbase-identifiers']) {
    await db.query('INSERT INTO catalog.capability_execution_contracts VALUES($1,$2,$3,$4)',
      ['22222222-2222-4222-8222-222222222222','active',capability,{'crunchbase.entity_autocomplete_query':{in:'body',name:'query'}}]);
  }
  await db.query('INSERT INTO catalog.fact_types VALUES($1,$2,$3)',[fixture.slug,'active',fixture.before]);
  await db.exec(sql);assert.deepEqual(await schema(),fixture.after);
  await db.exec(sql);assert.deepEqual(await schema(),fixture.after,'idempotent');
  const stripped=structuredClone(fixture.after);delete stripped.$comment;
  assert.deepEqual(stripped,fixture.before,'validation is identical');
  for (const [change,undo] of [
    ["UPDATE providers.apis SET slug='other-provider'","UPDATE providers.apis SET slug='apimarket-magicapi-crunchbase-bbcca0a6'"],
    ["UPDATE providers.api_endpoints SET path='/mcp/other'","UPDATE providers.api_endpoints SET path='/mcp/cb_entity_autocomplete'"],
    ["UPDATE catalog.capability_execution_contracts SET input_map='{}'","UPDATE catalog.capability_execution_contracts SET input_map='{\"crunchbase.entity_autocomplete_query\":{\"in\":\"body\",\"name\":\"query\"}}'"],
  ]) {
    await db.exec(change);await assert.rejects(db.exec(sql),/contract identity drift/);await db.exec('ROLLBACK');
    assert.deepEqual(await schema(),fixture.after);await db.exec(undo);
  }
  const drift={...fixture.before,maxLength:400};
  await db.query('UPDATE catalog.fact_types SET value_schema=$1',[drift]);
  await assert.rejects(db.exec(sql),/input schema drift/);await db.exec('ROLLBACK');assert.deepEqual(await schema(),drift);
  console.log('PASS: official name aliases preserve validation, idempotency and exact contract identity; unexpected schema drift is rejected.');
} finally {await db.close();}
