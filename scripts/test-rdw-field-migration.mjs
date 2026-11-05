import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { PGlite } from '@electric-sql/pglite';

const sql = await fs.readFile(new URL('../supabase/migrations/20261105210918_rdw_registration_field_coverage.sql', import.meta.url), 'utf8');
const compoundSql = await fs.readFile(new URL('../supabase/migrations/20261105213703_rdw_registration_compound_aliases.sql', import.meta.url), 'utf8');
const apkSql = await fs.readFile(new URL('../supabase/migrations/20261105232047_accept_rdw_apk_iso_date.sql', import.meta.url), 'utf8');
const fixture = JSON.parse(await fs.readFile(new URL('../tests/fixtures/rdw-registration-planning.json', import.meta.url), 'utf8'));
const rdw = fixture.catalog.vocabulary.find(fact => fact.slug === 'vehicle.rdw.technical_page');
for (const schema of ['public', 'catalog']) {
  const db = new PGlite();
  try {
    await db.exec(`CREATE SCHEMA IF NOT EXISTS ${schema}; CREATE TABLE ${schema}.fact_types(slug text PRIMARY KEY,value_schema jsonb NOT NULL);`);
    const before = structuredClone(rdw.value_schema);
    const properties = before.properties.items.items.properties;
    if (schema === 'public') delete before.properties.items.items.properties;
    else {
      for (const field of Object.values(properties)) {
        if (!field.$comment) continue;
        const meta = JSON.parse(field.$comment);
        meta.apiosk_output_aliases_v1 = meta.apiosk_output_aliases_v1.filter(alias => !['model','first_registration','first_registration_date','recall_indicators','make_model'].includes(alias));
        field.$comment = JSON.stringify(meta);
      }
      properties.handelsbenaming.example = 'preserve existing metadata';
    }
    await db.query(`INSERT INTO ${schema}.fact_types VALUES ($1,$2),('unrelated','{"type":"boolean"}')`, [rdw.slug, before]);
    await db.query(`INSERT INTO ${schema}.fact_types VALUES ('vehicle.apk_expiry',$1)`, [{ type: 'string', pattern: '^[0-9]{8}$', description: 'Keep this metadata' }]);
    await db.exec(apkSql);
    const apk = (await db.query(`SELECT value_schema FROM ${schema}.fact_types WHERE slug='vehicle.apk_expiry'`)).rows[0].value_schema;
    assert.equal(apk.description, 'Keep this metadata');
    for (const date of ['2027-10-29', '20271029']) assert(new RegExp(apk.pattern).test(date), date);
    for (const date of ['', '2027', '29-10-2027', '2027-1029', 'unavailable']) assert(!new RegExp(apk.pattern).test(date), date);
    await db.exec(apkSql);
    assert.deepEqual((await db.query(`SELECT value_schema FROM ${schema}.fact_types WHERE slug='vehicle.apk_expiry'`)).rows[0].value_schema, apk);
    await db.exec(sql);
    await db.exec(compoundSql);
    const read = async () => (await db.query(`SELECT value_schema FROM ${schema}.fact_types WHERE slug=$1`, [rdw.slug])).rows[0].value_schema;
    const after = await read();
    const fields = after.properties.items.items.properties;
    const aliases = field => JSON.parse(fields[field].$comment).apiosk_output_aliases_v1;
    assert(aliases('handelsbenaming').includes('model'));
    assert(aliases('handelsbenaming').includes('make_model'));
    assert(aliases('merk').includes('make_model'));
    assert(aliases('datum_eerste_toelating').includes('first_registration'));
    assert(aliases('datum_eerste_tenaamstelling_in_nederland').includes('first_registration'));
    assert(aliases('openstaande_terugroepactie_indicator').includes('recall_indicators'));
    assert(!aliases('openstaande_terugroepactie_indicator').includes('recall_history'));
    if (schema === 'catalog') assert.equal(fields.handelsbenaming.example, 'preserve existing metadata');
    await db.exec(sql);
    await db.exec(compoundSql);
    assert.deepEqual(await read(), after, 'reapplying the migration is a no-op');
    assert.deepEqual((await db.query(`SELECT value_schema FROM ${schema}.fact_types WHERE slug='unrelated'`)).rows[0].value_schema, { type: 'boolean' });
  } finally { await db.close(); }
}
console.log('RDW field migration: legacy and current catalogs, preserved metadata and idempotency passed');
