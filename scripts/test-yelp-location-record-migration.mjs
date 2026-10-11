// Local schema/identity regression only; no provider calls or production writes.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { PGlite } from '../../app/node_modules/@electric-sql/pglite/dist/index.js';
const fixture=JSON.parse(await fs.readFile(new URL('../tests/fixtures/yelp-location-record/reviewed-schema.json',import.meta.url),'utf8'));
const sql=await fs.readFile(new URL('../supabase/migrations/20261011202516_declare_yelp_location_record_scope.sql',import.meta.url),'utf8');
const db=new PGlite();await db.waitReady;
const schema=async()=> (await db.query('SELECT value_schema FROM catalog.fact_types WHERE slug=$1',[fixture.slug])).rows[0].value_schema;
try {
  await db.exec(`CREATE SCHEMA providers; CREATE SCHEMA catalog;
    CREATE TABLE providers.apis(id uuid PRIMARY KEY,slug text);
    CREATE TABLE providers.api_endpoints(id uuid PRIMARY KEY,api_id uuid,method text,path text,active boolean);
    CREATE TABLE catalog.capability_execution_contracts(endpoint_id uuid,status text,output_map jsonb);
    CREATE TABLE catalog.fact_types(slug text PRIMARY KEY,status text,value_schema jsonb);
    INSERT INTO providers.apis VALUES('11111111-1111-4111-8111-111111111111','openwebninja');
    INSERT INTO providers.api_endpoints VALUES('22222222-2222-4222-8222-222222222222','11111111-1111-4111-8111-111111111111','GET','/yelp-business-data/business-search',true);
    INSERT INTO catalog.capability_execution_contracts VALUES('22222222-2222-4222-8222-222222222222','active','{"produces":{"location.yelp_search_results":{"adapter":"document.response","key":"document"}}}');`);
  await db.query('INSERT INTO catalog.fact_types VALUES($1,$2,$3)',[fixture.slug,'active',fixture.before]);
  await db.exec(sql);assert.deepEqual(await schema(),fixture.after);
  await db.exec(sql);assert.deepEqual(await schema(),fixture.after,'idempotent');
  const stripped=structuredClone(fixture.after);delete stripped.properties.document.properties.data.items.$comment;
  assert.deepEqual(stripped,fixture.before,'only the complete record annotation changes');
  for (const [change,undo] of [
    ["UPDATE providers.apis SET slug='other-provider'","UPDATE providers.apis SET slug='openwebninja'"],
    ["UPDATE providers.api_endpoints SET path='/business-details'","UPDATE providers.api_endpoints SET path='/yelp-business-data/business-search'"],
    ["UPDATE providers.api_endpoints SET active=false","UPDATE providers.api_endpoints SET active=true"],
    ["UPDATE catalog.capability_execution_contracts SET output_map='{}'","UPDATE catalog.capability_execution_contracts SET output_map='{\"produces\":{\"location.yelp_search_results\":{}}}'"],
  ]) {
    await db.exec(change);await assert.rejects(db.exec(sql),/contract identity drift/);await db.exec('ROLLBACK');
    assert.deepEqual(await schema(),fixture.after);await db.exec(undo);
  }
  const drift=structuredClone(fixture.before);drift.properties.document.properties.data.items.properties.name.type='integer';
  await db.query('UPDATE catalog.fact_types SET value_schema=$1',[drift]);
  await assert.rejects(db.exec(sql),/schema drift/);await db.exec('ROLLBACK');assert.deepEqual(await schema(),drift);
  console.log('PASS: Yelp record alias preserves exact schema, idempotency and provider/endpoint/contract identity; schema drift is rejected.');
} finally {await db.close();}
