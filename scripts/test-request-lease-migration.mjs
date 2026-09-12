import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { PGlite } from '../../app/node_modules/@electric-sql/pglite/dist/index.js';

const migration = await readFile(new URL('../supabase/migrations/20260912181441_request_execution_lease.sql', import.meta.url), 'utf8');
for (const schema of ['gateway_v2', 'actions']) {
  const db = new PGlite();
  try {
    await db.exec(`CREATE ROLE anon; CREATE ROLE authenticated; CREATE ROLE gateway_v2_runtime;
      CREATE SCHEMA ${schema}; CREATE TABLE ${schema}.tasks(id uuid PRIMARY KEY);`);
    await db.exec(migration);
    const { rows: [access] } = await db.query(`SELECT relrowsecurity AS rls,
      has_table_privilege('anon',oid,'SELECT') AS anon_read,
      has_table_privilege('authenticated',oid,'SELECT') AS authenticated_read,
      has_table_privilege('gateway_v2_runtime',oid,'INSERT') AS runtime_write
      FROM pg_class WHERE oid='${schema}.execution_leases'::regclass`);
    assert.deepEqual(access, { rls: true, anon_read: false, authenticated_read: false, runtime_write: true });
    const id = '11111111-1111-4111-8111-111111111111';
    await assert.rejects(db.exec(`INSERT INTO ${schema}.execution_leases VALUES ('${id}','${id}',now())`));
    await db.exec(`INSERT INTO ${schema}.tasks VALUES ('${id}');
      INSERT INTO ${schema}.execution_leases VALUES ('${id}','${id}',now());
      DELETE FROM ${schema}.tasks WHERE id='${id}';`);
    assert.equal((await db.query(`SELECT count(*)::int AS n FROM ${schema}.execution_leases`)).rows[0].n, 0);
    console.log(`${schema}: migration, private access, foreign key and cascade passed`);
  } finally {
    await db.close();
  }
}
