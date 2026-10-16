import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { PGlite } from '@electric-sql/pglite';

// Exercise the actual claim SQL without a network or any provider execution.
const sql = readFileSync(new URL('../src/recurring/manual_claim.sql', import.meta.url), 'utf8');
const db = new PGlite();
try {
  await db.exec(`create schema billing;
    create table billing.recurring_orders(id uuid primary key,owner_id uuid,workspace_id uuid,state text,approved_at timestamptz);
    create table billing.recurring_runs(id uuid primary key,order_id uuid,state text,scheduled_at timestamptz,finished_at timestamptz,lease_id uuid,lease_until timestamptz);`);
  const owner = randomUUID(), workspace = randomUUID(), order = randomUUID(), first = randomUUID(), second = randomUUID();
  await db.query("insert into billing.recurring_orders values($1,$2,$3,'active',now())", [order, owner, workspace]);
  await db.query("insert into billing.recurring_runs(id,order_id,state,scheduled_at) values($1,$3,'queued',now()+interval '1 day'),($2,$3,'queued',now()+interval '2 days')", [first, second, order]);
  const claim = (run = first, actor = owner, scope = workspace, id = order) => db.query(sql, [run, id, actor, scope, randomUUID()]);
  const original = (await db.query('select id,scheduled_at from billing.recurring_runs order by scheduled_at')).rows;
  assert.equal((await claim(first, randomUUID())).rows.length, 0, 'another owner');
  assert.equal((await claim(first, owner, randomUUID())).rows.length, 0, 'another workspace');
  assert.equal((await claim(first, owner, null)).rows.length, 0, 'personal scope');
  assert.equal((await claim(first, owner, workspace, randomUUID())).rows.length, 0, 'another order');
  assert.equal((await claim(second)).rows.length, 0, 'cannot skip earlier unfinished run');
  for (const state of ['paused', 'cancelled', 'attention', 'completed', 'draft']) {
    await db.query('update billing.recurring_orders set state=$1', [state]);
    assert.equal((await claim()).rows.length, 0, state);
  }
  await db.exec("update billing.recurring_orders set state='active',approved_at=null");
  assert.equal((await claim()).rows.length, 0, 'approval required');
  await db.exec('update billing.recurring_orders set approved_at=now()');
  assert.equal((await claim()).rows.length, 1, 'future occurrence starts early');
  assert.equal((await claim()).rows.length, 0, 'replay cannot start it twice');
  assert.equal((await claim(second)).rows.length, 0, 'earlier running occurrence blocks next');
  assert.deepEqual((await db.query('select id,scheduled_at from billing.recurring_runs order by scheduled_at')).rows, original, 'dates and occurrence count preserved');
  await db.query("update billing.recurring_runs set state='succeeded',finished_at=now() where id=$1", [first]);
  assert.equal((await claim()).rows.length, 0, 'completed occurrence cannot rerun');
  assert.equal((await claim(second)).rows.length, 1, 'next occurrence available after completion');
  await db.query("update billing.recurring_runs set state='queued',scheduled_at=now()-interval '2 hours' where id=$1", [second]);
  assert.equal((await claim(second)).rows.length, 0, 'missed occurrence is not resurrected');
  console.log('Manual trigger SQL passed: scope, states, approval, ordering, replay, schedule preservation and missed-run protection.');
} finally { await db.close(); }
