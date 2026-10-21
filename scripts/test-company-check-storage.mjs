// Real migration and runtime SQL in embedded PostgreSQL; no sockets, paid
// providers or email. Multi-connection locking still needs native PostgreSQL.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { PGlite } from '@electric-sql/pglite';

const read = path => readFileSync(new URL(path, import.meta.url), 'utf8');
const queries = [...(read('../src/company_checks/store.rs') + read('../src/company_checks/delivery.rs')).matchAll(/sqlx::query(?:_scalar)?\("([^"\n]+)"\)/g)].map(m => m[1]);
const sql = prefix => { const query = queries.find(q => q.startsWith(prefix)); assert.ok(query, `runtime SQL: ${prefix}`); return query; };
const db = new PGlite();
try {
  await db.exec('CREATE SCHEMA gateway_v2; CREATE ROLE anon; CREATE ROLE authenticated; CREATE ROLE gateway_v2_runtime; GRANT USAGE ON SCHEMA gateway_v2 TO gateway_v2_runtime,anon,authenticated;');
  await db.exec(read('../supabase/migrations/20261021114309_company_check_snapshots.sql'));
  const check = randomUUID(), owner = randomUUID(), workspace = randomUUID(), connection = randomUUID();
  const companies = [{name:'Acme B.V.',kvk_number:'01234567'}];
  await db.exec('SET ROLE gateway_v2_runtime');
  await db.query(sql('INSERT INTO gateway_v2.company_checks('), [check, owner, connection, workspace, companies]);
  const get = (actor=owner, scope=workspace, link=connection) => db.query(sql('SELECT companies FROM gateway_v2.company_checks WHERE id=$1 AND owner_id=$2 AND workspace_id IS NOT DISTINCT FROM $3 AND (connection_id'),[check,actor,scope,link]);
  assert.deepEqual((await get()).rows[0].companies,companies);
  assert.equal((await get(randomUUID())).rows.length,0);
  assert.equal((await get(owner,randomUUID())).rows.length,0);
  assert.equal((await get(owner,workspace,randomUUID())).rows.length,0);
  assert.equal((await get(owner,workspace,null)).rows.length,1,'owner can recover their connection check');
  const insertRun=sql('INSERT INTO gateway_v2.company_check_runs(');
  await db.query(insertRun,[check,'2026-10-26',randomUUID()]);
  await assert.rejects(db.query(insertRun,[check,'2026-11-02',randomUUID()]),e=>e.code==='23505','one unfinished occurrence');
  const signal='a'.repeat(64), delivery=randomUUID();
  const snapshot={view:'company_check',signals:[{signal_id:signal}],sources:[]};
  await db.query(sql('UPDATE gateway_v2.company_check_runs SET snapshot=$3,completed_at=now()'),[check,'2026-10-26',snapshot]);
  await db.query(insertRun,[check,'2026-11-02',randomUUID()]);
  await db.query(sql('UPDATE gateway_v2.company_check_runs SET snapshot=$3,completed_at=now()'),[check,'2026-11-02',snapshot]);
  const insert=sql('INSERT INTO gateway_v2.company_check_deliveries(');
  const payload={subject:'One new signal',body:'Original saved source evidence',signal_ids:[signal]};
  await db.query(insert,[delivery,check,'2026-10-26',payload]);
  const pending=sql('SELECT delivery_id,status,payload FROM gateway_v2.company_check_deliveries');
  for (const attempt of [delivery,randomUUID()]) {
    const row=(await db.query(pending,[check,attempt])).rows[0];
    assert.equal(row.delivery_id,delivery,'same and competing handoff recover the original');
    assert.deepEqual(row.payload,payload);
  }
  await assert.rejects(db.query(insert,[randomUUID(),check,'2026-11-02',payload]),e=>e.code==='23505','pending send cannot be bypassed');
  assert.equal((await db.query('SELECT * FROM gateway_v2.company_check_reported_signals')).rows.length,0,'preparing is not delivery');
  const ack=sql('INSERT INTO gateway_v2.company_check_reported_signals(');
  const sent=sql("UPDATE gateway_v2.company_check_deliveries SET status='sent'");
  // Force rollback after acknowledgement but before receipt: no lost signal.
  await db.exec('BEGIN');
  await db.query(ack,[check,signal,'2026-10-26']);
  await db.exec('ROLLBACK');
  assert.equal((await db.query('SELECT * FROM gateway_v2.company_check_reported_signals')).rows.length,0);
  await db.exec('BEGIN');
  await db.query(ack,[check,signal,'2026-10-26']);
  await db.query(sent,[check,delivery,'gmail','message-1']);
  await db.exec('COMMIT');
  await db.query(ack,[check,signal,'2026-10-26']);
  assert.equal((await db.query('SELECT * FROM gateway_v2.company_check_reported_signals')).rows.length,1,'idempotent signal acknowledgement');
  assert.equal((await db.query(pending,[check,delivery])).rows[0].status,'sent','retry of completed handoff never reopens it');
  const next=randomUUID();
  await db.query(insert,[next,check,'2026-11-02',payload]);
  await assert.rejects(db.query(sent,[check,next,'gmail','message-1']),e=>e.code==='23505','one provider receipt cannot confirm a second delivery');
  const rls=(await db.query("SELECT relname,relrowsecurity FROM pg_class WHERE relnamespace='gateway_v2'::regnamespace AND relkind='r'")).rows;
  assert.equal(rls.length,4);assert.ok(rls.every(r=>r.relrowsecurity));
  for (const role of ['anon','authenticated']) {
    await db.exec(`RESET ROLE; SET ROLE ${role}`);
    await assert.rejects(db.query('SELECT * FROM gateway_v2.company_checks'),e=>e.code==='42501');
    await assert.rejects(db.query('SELECT * FROM gateway_v2.company_check_deliveries'),e=>e.code==='42501');
  }
  console.log('Company-check storage passed: migration, runtime grants, RLS, tenant/connection scopes, occurrence exclusion, mail handoff replay, pending-send exclusion, receipt uniqueness and acknowledgement rollback.');
} finally { await db.close(); }
