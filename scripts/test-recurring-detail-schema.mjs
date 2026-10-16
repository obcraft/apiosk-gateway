import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { PGlite } from '@electric-sql/pglite';

const source = readFileSync(new URL('../src/recurring.rs', import.meta.url), 'utf8');
const sql = source.split('pub async fn read(')[1].split('sqlx::query(r#"')[1].split('"#)')[0];
const db = new PGlite();
const order = '00000000-0000-0000-0000-000000000001';
const owner = '00000000-0000-0000-0000-000000000002';
const workspace = '00000000-0000-0000-0000-000000000003';
try {
  await db.exec(`create schema billing;
    create table billing.recurring_orders(id uuid,owner_id uuid,workspace_id uuid,state text,question text,
      configuration jsonb,delivery jsonb,dates timestamptz[],billing_mode text,change_only boolean,
      workflow jsonb,provider_per_run bigint,total_per_run bigint,approved_micro_usd bigint,
      reserved_micro_usd bigint,released_micro_usd bigint,expires_at timestamptz,sources jsonb);
    create table billing.recurring_runs(id uuid,order_id uuid);
    create table billing.recurring_calls(id uuid,run_id uuid,allocated_micro_usd bigint);
    create table user_balances(account_id uuid,balance_micro_usd bigint);
    create table balance_charges(user_id uuid,idempotency_key text,status text,charged_micro_usd bigint);
    insert into billing.recurring_orders(id,owner_id,workspace_id,state,question) values('${order}','${owner}','${workspace}','active','Saved order');
    insert into user_balances values('${workspace}',1234);
    prepare stable_detail(uuid,uuid,uuid) as ${sql};
    prepare legacy_detail(uuid,uuid,uuid) as ${sql.replace(/select o\.state,[\s\S]*?o\.sources,/, 'select o.*,')};`);
  const execute = name => db.query(`execute ${name}('${order}','${owner}','${workspace}')`);
  const before = (await execute('stable_detail')).rows;
  assert.equal(before.length, 1);
  assert.equal(before[0].available, '1234');
  await execute('legacy_detail');
  await db.exec('alter table billing.recurring_orders add column delivery_revision integer not null default 1');
  await assert.rejects(execute('legacy_detail'), /cached plan must not change result type/);
  assert.deepEqual((await execute('stable_detail')).rows, before);
  assert.equal((await db.query(`execute stable_detail('${order}','${workspace}','${workspace}')`)).rows.length, 0);
  assert.equal((await db.query(`execute stable_detail('${order}','${owner}',null)`)).rows.length, 0);
  console.log('Recurring detail regression passed: reproduces stale wildcard plan, survives added columns, preserves balances and owner/workspace scope.');
} finally { await db.close(); }
