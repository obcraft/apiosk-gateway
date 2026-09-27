// PGlite check of 20260928001500_coinbase_bazaar_source.sql against a minimal
// replica of the tables, triggers and review RPC it builds on. Rows come from
// the recorded Bazaar pages through scripts/bazaar-sync.mjs, so the RPC is
// exercised with exactly what --apply would send.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { PGlite } from '../../app/node_modules/@electric-sql/pglite/dist/index.js';
import { batchRows, buildCatalog } from './bazaar-sync.mjs';

const migration = await readFile(new URL('../supabase/migrations/20260928001500_coinbase_bazaar_source.sql', import.meta.url), 'utf8');
assert.equal(await readFile(new URL('../../app/supabase/migrations/20260928001500_coinbase_bazaar_source.sql', import.meta.url), 'utf8'), migration,
  'app/ must carry an identical copy: migrations are applied from app/ only');
assert.doesNotMatch(migration.replace(/--.*$/gm, ''), /\bDELETE\s+FROM\b|\bTRUNCATE\b|\bDROP\s+TABLE\b/i, 'the import never deletes');
const logoFix = await readFile(new URL('../supabase/migrations/20260928003000_fix_bazaar_logo_check.sql', import.meta.url), 'utf8');
assert.equal(await readFile(new URL('../../app/supabase/migrations/20260928003000_fix_bazaar_logo_check.sql', import.meta.url), 'utf8'), logoFix,
  'app/ must carry an identical copy of the logo-check fix');
// PostgreSQL caps a regular-expression bound at 255; `{1,2040}` compiled fine
// and failed at the first real batch ("invalid repetition count(s)"). The
// applied migration keeps it, so the check runs on the functions as installed.
const regexBounds = sql => [...sql.matchAll(/\{(\d+)(?:,(\d+))?\}/g)].map(m => Number(m[2] ?? m[1]));

const page = async name => JSON.parse(await readFile(new URL(`../tests/fixtures/bazaar/${name}`, import.meta.url), 'utf8'));
const items = [...(await page('page-offset-0.json')).items, ...(await page('page-offset-6000.json')).items];
const rows = batchRows(buildCatalog(items, { now: Date.parse('2026-09-28T00:00:00Z'), tier: 500 }));
const onesource = rows.filter(r => r.host === 'api.onesource.io');
const withContract = onesource.find(r => r.contract);
const apitoll = rows.find(r => r.host === 'crypto.apitoll.cloud');
assert.ok(withContract && apitoll && onesource.length >= 4);

const OWNER = '146d4387-50a1-4481-a9bf-2d69914ff780';
const ADMIN = '99999999-9999-4999-8999-999999999999';
const bootstrap = `
CREATE ROLE anon; CREATE ROLE authenticated; CREATE ROLE service_role BYPASSRLS; CREATE ROLE gateway_v2_runtime;
CREATE SCHEMA auth; CREATE SCHEMA providers; CREATE SCHEMA catalog; CREATE SCHEMA actions; CREATE SCHEMA wallets; CREATE SCHEMA identity;
GRANT USAGE ON SCHEMA providers TO service_role, authenticated, anon;
CREATE TABLE auth.users (instance_id uuid, id uuid PRIMARY KEY, aud text, role text, email text UNIQUE, raw_app_meta_data jsonb,
  raw_user_meta_data jsonb, created_at timestamptz, updated_at timestamptz, banned_until timestamptz, email_confirmed_at timestamptz);
CREATE FUNCTION auth.uid() RETURNS uuid LANGUAGE sql STABLE AS $$ SELECT nullif(nullif(current_setting('request.jwt.claims', true), '')::jsonb->>'sub', '')::uuid $$;
CREATE TABLE identity.platform_admins (user_id uuid PRIMARY KEY, active boolean NOT NULL DEFAULT true);
CREATE FUNCTION public.is_active_platform_admin() RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path = ''
  AS $$ SELECT EXISTS (SELECT 1 FROM identity.platform_admins WHERE user_id = auth.uid() AND active) $$;
CREATE TABLE providers.provider_profiles (id uuid PRIMARY KEY REFERENCES auth.users(id), username text UNIQUE, display_name text, bio text,
  avatar_url text, sector text, onboarded_at timestamptz, created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now());
CREATE TYPE public.listing_type AS ENUM ('api', 'federated');
CREATE TABLE providers.apis (id uuid PRIMARY KEY DEFAULT gen_random_uuid(), owner_id uuid NOT NULL, slug text NOT NULL UNIQUE, name text NOT NULL,
  origin_url text NOT NULL, endpoint_url text, wallet_address text, status text NOT NULL DEFAULT 'active', created_at timestamptz NOT NULL DEFAULT now(),
  description text, long_description text, source text DEFAULT 'manual', commission_rate numeric NOT NULL DEFAULT 0.2174, category text, docs_url text,
  website_url text, is_verified boolean DEFAULT false, verified_at timestamptz, active boolean DEFAULT true,
  listing_type public.listing_type NOT NULL DEFAULT 'api', listing_metadata jsonb NOT NULL DEFAULT '{}', is_external boolean NOT NULL DEFAULT false,
  discoverable boolean NOT NULL DEFAULT true, suspended_at timestamptz, submitted_for_review_at timestamptz, approved_at timestamptz, reviewed_by uuid);
-- Production triggers on apis (legacy 023, app 20260717020000), fired in name order.
CREATE FUNCTION public.apiosk_auto_verify_owner_apis() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN IF NEW.owner_id IS DISTINCT FROM '00000000-0000-0000-0000-000000000001'::uuid THEN NEW.is_verified := true; NEW.verified_at := coalesce(NEW.verified_at, now()); END IF; RETURN NEW; END $$;
CREATE TRIGGER trg_apis_auto_verify_owner BEFORE INSERT ON providers.apis FOR EACH ROW EXECUTE FUNCTION public.apiosk_auto_verify_owner_apis();
CREATE FUNCTION public.apiosk_apis_review_gate() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path = public AS $$
DECLARE v_status_changed boolean; v_approved_api_id text;
BEGIN
  v_status_changed := tg_op = 'INSERT';
  IF tg_op = 'UPDATE' THEN v_status_changed := old.status IS DISTINCT FROM new.status; END IF;
  v_approved_api_id := nullif(current_setting('apiosk.api_review_approved_id', true), '');
  IF new.status = 'active' AND v_status_changed AND v_approved_api_id IS DISTINCT FROM new.id::text THEN new.status := 'pending'; END IF;
  IF new.status = 'pending' AND v_status_changed THEN new.submitted_for_review_at := now(); new.reviewed_by := NULL; END IF;
  IF new.status = 'active' AND new.approved_at IS NULL THEN new.approved_at := now(); END IF;
  RETURN new;
END $$;
CREATE TRIGGER trg_apis_review_gate BEFORE INSERT OR UPDATE ON providers.apis FOR EACH ROW EXECUTE FUNCTION public.apiosk_apis_review_gate();
CREATE FUNCTION public.apiosk_sync_apis_active_with_status() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.active := (NEW.status = 'active'); RETURN NEW; END $$;
CREATE TRIGGER trg_apis_sync_active BEFORE INSERT OR UPDATE ON providers.apis FOR EACH ROW EXECUTE FUNCTION public.apiosk_sync_apis_active_with_status();
CREATE FUNCTION public.admin_review_api(p_api_id uuid, p_decision text) RETURNS TABLE (id uuid, status text) LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = public, providers AS $$
BEGIN
  IF NOT public.is_active_platform_admin() THEN RAISE EXCEPTION 'not_authorized' USING errcode = '42501'; END IF;
  IF p_decision NOT IN ('approve', 'reject') THEN RAISE EXCEPTION 'invalid_decision' USING errcode = '22023'; END IF;
  IF p_decision = 'approve' THEN PERFORM set_config('apiosk.api_review_approved_id', p_api_id::text, true); END IF;
  RETURN QUERY UPDATE providers.apis a SET status = CASE WHEN p_decision = 'approve' THEN 'active' ELSE 'inactive' END,
    approved_at = CASE WHEN p_decision = 'approve' THEN coalesce(a.approved_at, now()) ELSE a.approved_at END, reviewed_by = auth.uid()
   WHERE a.id = p_api_id AND a.status = 'pending' RETURNING a.id, a.status;
  IF p_decision = 'approve' THEN PERFORM set_config('apiosk.api_review_approved_id', '', true); END IF;
END $$;
CREATE TABLE wallets.network_tokens (id uuid PRIMARY KEY);
INSERT INTO wallets.network_tokens VALUES ('26adabc5-92ec-44f3-9540-3bd636eeb1f4');
CREATE TABLE providers.api_endpoints (id uuid PRIMARY KEY DEFAULT gen_random_uuid(), api_id uuid NOT NULL REFERENCES providers.apis(id), method text NOT NULL,
  path text NOT NULL, payment_required boolean NOT NULL DEFAULT true, price numeric NOT NULL, created_at timestamptz NOT NULL DEFAULT now(),
  network_token_id uuid REFERENCES wallets.network_tokens(id), description text, request_body jsonb, response_example jsonb, query_params jsonb,
  active boolean NOT NULL DEFAULT true, tags text[], endpoint_category text, action_type text, metadata_source text, metadata_at timestamptz,
  contract_documented boolean NOT NULL DEFAULT true, UNIQUE (api_id, method, path));
CREATE TABLE actions.x402_host_policy (host text PRIMARY KEY, policy text NOT NULL, max_price_usdc numeric);
CREATE TABLE catalog.fact_subject_kinds (slug text PRIMARY KEY, description text NOT NULL, created_at timestamptz NOT NULL DEFAULT now());
CREATE TABLE catalog.fact_types (slug text PRIMARY KEY, name text NOT NULL, description text NOT NULL, subject_kind text NOT NULL REFERENCES catalog.fact_subject_kinds(slug),
  value_schema jsonb NOT NULL, match_hints jsonb NOT NULL DEFAULT '{}', status text NOT NULL DEFAULT 'draft', created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now());
CREATE TABLE catalog.capabilities (slug text PRIMARY KEY, name text NOT NULL, description text NOT NULL, category text, input_contract jsonb NOT NULL,
  output_contract jsonb NOT NULL, status text NOT NULL DEFAULT 'active', created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now());
CREATE TABLE catalog.capability_offers (capability_slug text REFERENCES catalog.capabilities(slug), endpoint_id uuid REFERENCES providers.api_endpoints(id),
  api_id uuid NOT NULL REFERENCES providers.apis(id), source text NOT NULL CHECK (source IN ('declared', 'curated', 'inferred', 'auto')),
  confidence numeric NOT NULL DEFAULT 1, input_map jsonb NOT NULL, fixed_params jsonb NOT NULL, notes text, created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (capability_slug, endpoint_id));
CREATE TABLE catalog.capability_execution_contracts (id uuid PRIMARY KEY DEFAULT gen_random_uuid(), capability_slug text NOT NULL REFERENCES catalog.capabilities(slug),
  endpoint_id uuid NOT NULL REFERENCES providers.api_endpoints(id), version integer NOT NULL, status text NOT NULL DEFAULT 'draft', requires jsonb NOT NULL,
  produces jsonb NOT NULL, input_map jsonb NOT NULL, fixed_params jsonb NOT NULL, output_map jsonb NOT NULL, cardinality text NOT NULL DEFAULT 'one',
  identifies text, max_fan_out integer NOT NULL DEFAULT 1, preconditions jsonb NOT NULL, pagination jsonb NOT NULL, effect text NOT NULL DEFAULT 'read',
  price_model text NOT NULL DEFAULT 'per_call', freshness_seconds integer, sample_payload jsonb, validated_at timestamptz, validated_note text, notes text,
  adapter_version text NOT NULL DEFAULT 'v1', created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now());
CREATE TABLE catalog.source_operation_failures (contract_id uuid, failed_at timestamptz NOT NULL DEFAULT now());
-- An existing Apiosk listing whose host the Bazaar also lists.
INSERT INTO auth.users (id, email) VALUES ('11111111-1111-4111-8111-111111111111', 'other@example.net');
INSERT INTO providers.apis (owner_id, slug, name, origin_url, status, listing_metadata)
VALUES ('11111111-1111-4111-8111-111111111111', 'apitoll-prices', 'ApiToll', 'https://crypto.apitoll.cloud', 'active', '{}');
INSERT INTO providers.apis (owner_id, slug, name, origin_url, status, listing_metadata)
VALUES ('11111111-1111-4111-8111-111111111111', 'cbz-squatter-io', 'Squatter', 'https://squatter.io', 'active', '{}');
INSERT INTO identity.platform_admins VALUES ('${ADMIN}', true);
`;

const db = new PGlite();
const one = async (sql, params) => (await db.query(sql, params)).rows[0];
const call = async (fn, arg) => (await one(`SELECT providers.${fn} AS r`, arg)).r;
const upsert = batch => one('SELECT providers.upsert_bazaar_batch($1::jsonb) AS r', [JSON.stringify(batch)]).then(x => x.r);
try {
  await db.exec(bootstrap);
  await db.exec(migration);
  await db.exec(migration); // idempotent
  await db.exec(logoFix);
  for (const { proname, src } of (await db.query(`SELECT p.proname, p.prosrc AS src FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
      WHERE n.nspname = 'providers' AND p.proname LIKE '%bazaar%'`)).rows) {
    assert.ok(regexBounds(src).every(n => n <= 255), `providers.${proname}: a regex bound above 255 fails at run time`);
  }

  const identity = await one(`SELECT p.username, p.display_name, u.email, u.email_confirmed_at, u.banned_until = 'infinity' AS banned
    FROM providers.provider_profiles p JOIN auth.users u ON u.id = p.id WHERE p.id = '${OWNER}'`);
  assert.deepEqual(identity, { username: 'coinbase-bazaar', display_name: 'Coinbase Bazaar', email: 'system+coinbase-bazaar@apiosk.com', email_confirmed_at: null, banned: true });

  // Every RPC is a definer with an empty search path; only the right roles may call it.
  const fns = (await db.query(`SELECT p.proname, p.prosecdef, p.proconfig FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'providers' AND p.proname IN ('begin_bazaar_sync_run','upsert_bazaar_batch','finish_bazaar_sync_run','approve_bazaar_services','bazaar_owner') ORDER BY 1`)).rows;
  assert.equal(fns.length, 5);
  for (const f of fns) { assert.equal(f.prosecdef, true, f.proname); assert.deepEqual(f.proconfig, ['search_path=""'], f.proname); }
  const can = async (role, fn) => (await one(`SELECT has_function_privilege('${role}', '${fn}', 'EXECUTE') AS ok`)).ok;
  for (const fn of ['providers.begin_bazaar_sync_run(integer,text)', 'providers.upsert_bazaar_batch(jsonb)', 'providers.finish_bazaar_sync_run(uuid,jsonb)']) {
    assert.equal(await can('anon', fn), false); assert.equal(await can('authenticated', fn), false); assert.equal(await can('service_role', fn), true);
  }
  assert.equal(await can('anon', 'providers.approve_bazaar_services(uuid[])'), false);
  assert.equal(await can('authenticated', 'providers.approve_bazaar_services(uuid[])'), true);
  const tables = (await db.query(`SELECT c.relname, c.relrowsecurity AS rls, has_table_privilege('anon', c.oid, 'SELECT') AS anon,
    has_table_privilege('authenticated', c.oid, 'SELECT') AS authenticated, has_table_privilege('service_role', c.oid, 'SELECT') AS service
    FROM pg_class c WHERE c.oid IN ('providers.bazaar_sync_control'::regclass, 'providers.bazaar_sync_runs'::regclass, 'providers.bazaar_resources'::regclass)`)).rows;
  for (const t of tables) assert.deepEqual([t.rls, t.anon, t.authenticated, t.service], [true, false, false, true], t.relname);

  // Kill switch: off by default, every RPC refuses.
  assert.deepEqual(await one('SELECT enabled, max_tier FROM providers.bazaar_sync_control'), { enabled: false, max_tier: 500 });
  await assert.rejects(call('begin_bazaar_sync_run(3, $1)', ['t']), /bazaar_sync_disabled/);
  await assert.rejects(upsert({ rows: [] }), /bazaar_sync_disabled/);
  await db.exec(`UPDATE providers.bazaar_sync_control SET enabled = true, max_tier = 3`);
  await assert.rejects(call('begin_bazaar_sync_run(4, $1)', ['t']), /bazaar_tier_not_approved/);
  const run = await call('begin_bazaar_sync_run(3, $1)', ['pilot test']);
  await assert.rejects(call('begin_bazaar_sync_run(3, $1)', ['again']), /bazaar_run_in_progress/);

  // First batch: two OneSource resources, one host that is already an Apiosk
  // source, one host whose cbz- slug another provider holds, one invalid row.
  const [a, b] = [withContract, onesource.find(r => r !== withContract)];
  const squat = { ...a, host: 'squatter.io', service: { ...a.service, slug: 'cbz-squatter-io', origin_url: 'https://squatter.io' } };
  const invalid = { ...b, path: '/pricey', endpoint: { ...b.endpoint, price_usd: '2' } };
  // The ceiling counts every new key a batch carries, skipped or not.
  await db.exec(`UPDATE providers.bazaar_sync_control SET max_tier = 5`);
  const first = await upsert({ run_id: run, rows: [a, b, apitoll, squat, invalid] });
  await db.exec(`UPDATE providers.bazaar_sync_control SET max_tier = 3`);
  assert.equal(first.inserted, 2);
  assert.deepEqual(first.skipped.map(s => s.reason).sort(), ['host_already_listed', 'invalid_row', 'slug_owned_by_another_provider']);
  assert.equal(first.staged_contracts, [a, b].filter(r => r.contract).length);
  const api = await one(`SELECT * FROM providers.apis WHERE slug = 'cbz-api-onesource-io'`);
  assert.equal(api.owner_id, OWNER);
  assert.deepEqual([api.status, api.active, api.discoverable, api.is_verified, api.listing_type, api.is_external, Number(api.commission_rate)],
    ['pending', false, false, true, 'federated', true, 0], 'new services wait for review, invisible');
  assert.deepEqual(api.listing_metadata.resell, { enabled: true, source: 'bazaar-sync' });
  assert.equal(api.listing_metadata.provider_network, 'Coinbase Bazaar');
  assert.deepEqual(api.listing_metadata.bazaar.pay_to, [...new Set([a.endpoint.pay_to, b.endpoint.pay_to])].sort());
  const firstSeen = api.listing_metadata.bazaar.first_seen;
  const endpoint = await one(`SELECT * FROM providers.api_endpoints WHERE id = $1`, [a.endpoint.id]);
  assert.equal(String(endpoint.price), a.endpoint.price_usd);
  assert.deepEqual([endpoint.payment_required, endpoint.network_token_id, endpoint.metadata_source, endpoint.active], [true, '26adabc5-92ec-44f3-9540-3bd636eeb1f4', 'declared', true]);
  assert.deepEqual(endpoint.query_params, a.endpoint.query_params);
  const contract = await one(`SELECT status, adapter_version, capability_slug FROM catalog.capability_execution_contracts WHERE id = $1`, [a.contract.id]);
  assert.deepEqual(contract, { status: 'validated', adapter_version: 'source-document-v1', capability_slug: a.contract.capability_slug });
  assert.equal((await one(`SELECT source FROM catalog.capability_offers WHERE endpoint_id = $1`, [a.endpoint.id])).source, 'declared');
  assert.equal((await one(`SELECT count(*)::int AS n FROM providers.bazaar_resources WHERE last_seen_run = $1`, [run])).n, 2);

  // The approved ceiling holds whatever the caller sends; policy is enforced server-side.
  const extra = onesource.filter(r => r !== a && r !== b).slice(0, 2);
  await assert.rejects(upsert({ rows: extra }), /bazaar_tier_exceeded/);
  await db.exec(`INSERT INTO actions.x402_host_policy VALUES ('${extra[0].host}', 'deny', NULL)`);
  assert.deepEqual((await upsert({ rows: [extra[0]] })).skipped.map(s => s.reason), ['host_policy_deny']);
  await db.exec(`DELETE FROM actions.x402_host_policy`);
  await assert.rejects(upsert({ rows: Array(201).fill(a) }), /bazaar_batch_too_large/);

  // Merge, never replace: operator keys and a deliberate resell stop survive a content change.
  await db.exec(`UPDATE providers.apis SET listing_metadata = listing_metadata || '{"planner_block_reason":"held","resell":{"enabled":false,"source":"operator"}}' WHERE slug = 'cbz-api-onesource-io'`);
  const changed = { ...a, content_hash: 'f'.repeat(64), endpoint: { ...a.endpoint, description: `${a.endpoint.description} Now with totals.` } };
  const second = await upsert({ rows: [changed] });
  assert.deepEqual([second.inserted, second.updated, second.unchanged], [0, 1, 0]);
  const merged = (await one(`SELECT listing_metadata FROM providers.apis WHERE slug = 'cbz-api-onesource-io'`)).listing_metadata;
  assert.equal(merged.planner_block_reason, 'held');
  assert.deepEqual(merged.resell, { enabled: false, source: 'bazaar-sync' });
  assert.equal(merged.bazaar.first_seen, firstSeen);
  assert.equal((await upsert({ rows: [changed] })).unchanged, 1);
  assert.equal((await upsert({ rows: [], touch: [{ host: b.host, method: b.method, path: b.path }] })).touched, 1);

  // Retirement: missed by two complete runs, never deleted.
  const missed = [{ host: b.host, method: b.method, path: b.path }];
  let finish = await call('finish_bazaar_sync_run($1, $2::jsonb)', [run, JSON.stringify({ complete: true, missed })]);
  assert.deepEqual([finish.missed, finish.retired], [0, 0], 'a resource seen in this run is not missed');
  const run2 = await call('begin_bazaar_sync_run(3, $1)', ['second']);
  finish = await call('finish_bazaar_sync_run($1, $2::jsonb)', [run2, JSON.stringify({ complete: true, missed })]);
  assert.deepEqual([finish.missed, finish.retired], [1, 0]);
  const run3 = await call('begin_bazaar_sync_run(3, $1)', ['third']);
  finish = await call('finish_bazaar_sync_run($1, $2::jsonb)', [run3, JSON.stringify({ complete: true, missed })]);
  assert.deepEqual([finish.missed, finish.retired], [1, 1]);
  assert.equal((await one(`SELECT active FROM providers.api_endpoints WHERE id = $1`, [b.endpoint.id])).active, false);
  assert.equal((await one(`SELECT count(*)::int AS n FROM providers.bazaar_resources`)).n, 2, 'rows are kept');
  const run4 = await call('begin_bazaar_sync_run(3, $1)', ['incomplete']);
  finish = await call('finish_bazaar_sync_run($1, $2::jsonb)', [run4, JSON.stringify({ complete: false, missed: [{ host: a.host, method: a.method, path: a.path }] })]);
  assert.equal(finish.retired, 0);
  assert.equal((await one(`SELECT status FROM providers.bazaar_sync_runs WHERE id = $1`, [run4])).status, 'aborted');
  // A mass miss (a listing outage) retires nothing.
  await db.exec(`INSERT INTO providers.bazaar_resources (host, method, path, resource, content_hash, pay_to, price_usd)
    SELECT 'mass.io', 'GET', '/r' || n, 'https://mass.io/r' || n, repeat('a', 64), '0x${'1'.repeat(40)}', 0.01 FROM generate_series(1, 40) n`);
  const run5 = await call('begin_bazaar_sync_run(3, $1)', ['outage']);
  finish = await call('finish_bazaar_sync_run($1, $2::jsonb)', [run5, JSON.stringify({ complete: true,
    missed: Array.from({ length: 40 }, (_, n) => ({ host: 'mass.io', method: 'GET', path: `/r${n + 1}` })) })]);
  assert.deepEqual([finish.missed, finish.retired], [40, 0]);
  assert.match(finish.note, /retirement skipped/);

  // Approval: platform admins only, through admin_review_api, then discoverable.
  const apiId = api.id;
  await assert.rejects(one(`SELECT * FROM providers.approve_bazaar_services(ARRAY[$1]::uuid[])`, [apiId]), /not_authorized/);
  await db.exec(`SELECT set_config('request.jwt.claims', '{"sub":"${ADMIN}","role":"authenticated"}', false)`);
  await assert.rejects(one(`SELECT * FROM providers.approve_bazaar_services(ARRAY[(SELECT id FROM providers.apis WHERE slug = 'apitoll-prices')])`), /not a Coinbase Bazaar service/);
  const approved = (await db.query(`SELECT * FROM providers.approve_bazaar_services()`)).rows;
  assert.deepEqual(approved.map(r => [r.api_slug, r.api_status, r.is_discoverable]), [['cbz-api-onesource-io', 'active', true]]);
  const live = await one(`SELECT status, active, discoverable, is_verified, approved_at IS NOT NULL AS approved FROM providers.apis WHERE id = $1`, [apiId]);
  assert.deepEqual(live, { status: 'active', active: true, discoverable: true, is_verified: true, approved: true });
  console.log(`coinbase_bazaar_source: identity, grants, kill switch, tier ceiling, merge, policy, retirement and approval passed (${rows.length} fixture rows)`);
} finally {
  await db.close();
}
