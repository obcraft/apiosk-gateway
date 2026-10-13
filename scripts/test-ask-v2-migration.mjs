// Regression: the runtime tables moved from gateway_v2 to actions in September.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { PGlite } from '@electric-sql/pglite';
const migration = await fs.readFile(new URL('../supabase/migrations/20261012200000_ask_v2_runs.sql', import.meta.url), 'utf8');
const base = await fs.readFile(new URL('../supabase/migrations/20260907094100_gateway_v2_state.sql', import.meta.url), 'utf8');
for (const layout of ['legacy', 'actions', 'both', 'missing']) {
 const db = new PGlite();
 try {
  await db.exec('CREATE ROLE anon; CREATE ROLE authenticated; CREATE ROLE gateway_v2_runtime;');
  if (layout !== 'missing') await db.exec(base);
  if (layout === 'actions' || layout === 'both') {
   await db.exec('CREATE SCHEMA actions;');
   for (const table of ['tasks', 'requests', 'authorizations', 'executions', 'results']) {
    await db.exec(`ALTER TABLE gateway_v2.${table} SET SCHEMA actions;`);
   }
   if (layout === 'both') await db.exec(base);
  }
  if (layout === 'missing') {
   await assert.rejects(db.exec(migration), /Ask V2 prerequisite missing: tasks/);
   await db.exec('ROLLBACK');
   assert.equal((await db.query("SELECT to_regclass('gateway_v2.ask_runs') AS relation")).rows[0].relation, null);
   console.log('PASS: missing runtime fails before creating Ask tables');
   continue;
  }
  await db.exec(migration);
  await db.exec(migration); // safe retry after a completed SQL-editor submission
  const expected = layout === 'legacy' ? 'gateway_v2' : 'actions';
  const { rows } = await db.query(`SELECT n.nspname AS schema, c.relname AS target FROM pg_constraint f JOIN pg_class c ON c.oid=f.confrelid JOIN pg_namespace n ON n.oid=c.relnamespace JOIN pg_class child ON child.oid=f.conrelid WHERE f.contype='f' AND child.relname LIKE 'ask_%' AND c.relname IN ('tasks','executions','results')`);
  assert.equal(rows.length, 4);
  assert.ok(rows.every(row => row.schema === expected), JSON.stringify(rows));
  const tables = (await db.query("SELECT relrowsecurity AS rls FROM pg_class JOIN pg_namespace n ON n.oid=relnamespace WHERE n.nspname='gateway_v2' AND relkind='r' AND relname LIKE 'ask_%'")).rows;
  assert.equal(tables.length, 6);
  assert.ok(tables.every(t => t.rls));
  for (const role of ['anon','authenticated']) {
   assert.equal((await db.query(`SELECT has_table_privilege('${role}', 'gateway_v2.ask_runs', 'SELECT') AS allowed`)).rows[0].allowed, false);
  }
  console.log(`PASS: ${layout} layout, four foreign keys reference ${expected}, repeatable, six private RLS tables`);
 } finally { await db.close(); }
}
