// Reviewed record aliases only; no provider calls or production writes.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { PGlite } from '@electric-sql/pglite';
const fixture=JSON.parse(await fs.readFile(new URL('../tests/fixtures/uk-registry-profile/reviewed-schema.json',import.meta.url),'utf8'));
const sql=await fs.readFile(new URL('../supabase/migrations/20261011210614_declare_uk_registry_profile_scope.sql',import.meta.url),'utf8');
const db=new PGlite();await db.waitReady;
const schema=async()=> (await db.query('SELECT value_schema FROM catalog.fact_types WHERE slug=$1',[fixture.slug])).rows[0].value_schema;
try {
  await db.exec(`CREATE SCHEMA providers; CREATE SCHEMA catalog;
    CREATE TABLE providers.apis(id uuid PRIMARY KEY,slug text);
    CREATE TABLE providers.api_endpoints(id uuid PRIMARY KEY,api_id uuid,method text,path text,active boolean);
    CREATE TABLE catalog.capability_execution_contracts(endpoint_id uuid,status text,capability_slug text,input_map jsonb,output_map jsonb);
    CREATE TABLE catalog.fact_types(slug text PRIMARY KEY,status text,value_schema jsonb);
    INSERT INTO providers.apis VALUES('11111111-1111-4111-8111-111111111111','companies-house');
    INSERT INTO providers.api_endpoints VALUES('22222222-2222-4222-8222-222222222222','11111111-1111-4111-8111-111111111111','GET','/company',true);`);
  await db.query('INSERT INTO catalog.capability_execution_contracts VALUES($1,$2,$3,$4,$5)',
    ['22222222-2222-4222-8222-222222222222','active','company.registry-profile',
      {'company.registration.gb.companies_house':{in:'path',name:'company_number'}},
      {base:'$',produces:{'company.uk_registry_record':{path:'$'}}}]);
  await db.query('INSERT INTO catalog.fact_types VALUES($1,$2,$3)',[fixture.slug,'active',fixture.before]);
  await db.exec(sql);assert.deepEqual(await schema(),fixture.after);
  await db.exec(sql);assert.deepEqual(await schema(),fixture.after,'idempotent');
  const stripped=structuredClone(fixture.after);delete stripped.$comment;
  assert.deepEqual(stripped,fixture.before,'validation is identical');
  for (const [change,undo] of [
    ["UPDATE providers.apis SET slug='other-provider'","UPDATE providers.apis SET slug='companies-house'"],
    ["UPDATE providers.api_endpoints SET path='/other'","UPDATE providers.api_endpoints SET path='/company'"],
    ["UPDATE catalog.capability_execution_contracts SET output_map='{}'","UPDATE catalog.capability_execution_contracts SET output_map='{\"base\":\"$\",\"produces\":{\"company.uk_registry_record\":{\"path\":\"$\"}}}'"],
  ]) {
    await db.exec(change);await assert.rejects(db.exec(sql),/contract identity drift/);await db.exec('ROLLBACK');
    assert.deepEqual(await schema(),fixture.after);await db.exec(undo);
  }
  const drift={...fixture.before,additionalProperties:false};
  await db.query('UPDATE catalog.fact_types SET value_schema=$1',[drift]);
  await assert.rejects(db.exec(sql),/output schema drift/);await db.exec('ROLLBACK');assert.deepEqual(await schema(),drift);
  console.log('PASS: UK registry profile aliases preserve validation, idempotency and exact contract identity; schema drift is rejected.');
} finally {await db.close();}
