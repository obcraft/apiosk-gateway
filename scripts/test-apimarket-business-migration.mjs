import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { PGlite } from '../../app/node_modules/@electric-sql/pglite/dist/index.js';

const file='20261008110548_onboard_apimarket_business_sources.sql';
const migration=await readFile(new URL(`../supabase/migrations/${file}`,import.meta.url),'utf8');
assert.equal(await readFile(new URL(`../../app/supabase/migrations/${file}`,import.meta.url),'utf8'),migration);
const manifest=JSON.parse(await readFile(new URL('../tests/fixtures/apimarket/business-sources.json',import.meta.url),'utf8'));
const sourcesSql=await readFile(new URL('../src/sources.sql',import.meta.url),'utf8');
const db=new PGlite();
try {
  await db.exec(`
    CREATE SCHEMA auth; CREATE SCHEMA providers; CREATE SCHEMA catalog;
    CREATE TABLE auth.users(id uuid PRIMARY KEY,instance_id uuid,aud text,role text,email text UNIQUE,
      raw_app_meta_data jsonb,raw_user_meta_data jsonb,created_at timestamptz,updated_at timestamptz,banned_until timestamptz);
    CREATE TABLE providers.provider_profiles(id uuid PRIMARY KEY REFERENCES auth.users(id),username text UNIQUE,
      display_name text,bio text,sector text,onboarded_at timestamptz,avatar_url text);
    CREATE TABLE providers.apis(id uuid PRIMARY KEY,owner_id uuid REFERENCES providers.provider_profiles(id),slug text UNIQUE,
      name text,description text,origin_url text,endpoint_url text,docs_url text,website_url text,category text,source text,
      listing_type text,integration_mode text,settlement_mode text,requires_payout_wallet boolean,is_external boolean,
      commission_rate numeric,status text,active boolean,discoverable boolean,is_verified boolean,verified_at timestamptz,
      approved_at timestamptz,authentication_info jsonb,listing_metadata jsonb,suspended_at timestamptz);
    CREATE TABLE providers.api_endpoints(id uuid PRIMARY KEY,api_id uuid,active boolean,tags text[],endpoint_category text);
    CREATE TABLE catalog.capability_offers(api_id uuid,capability_slug text);
    CREATE TABLE catalog.capability_execution_contracts(id uuid,endpoint_id uuid,status text);
    CREATE FUNCTION public.review_gate() RETURNS trigger LANGUAGE plpgsql AS $$
    BEGIN
      IF NEW.status='active' AND (TG_OP='INSERT' OR OLD.status IS DISTINCT FROM NEW.status)
        AND nullif(current_setting('apiosk.api_review_approved_id',true),'') IS DISTINCT FROM NEW.id::text
      THEN NEW.status:='pending'; END IF;
      NEW.active:=(NEW.status='active'); RETURN NEW;
    END $$;
    CREATE TRIGGER review_gate BEFORE INSERT OR UPDATE ON providers.apis FOR EACH ROW EXECUTE FUNCTION public.review_gate();
    SET search_path=public,providers,catalog;
  `);
  await db.exec(migration);
  await db.exec(migration);
  const rows=(await db.query('SELECT * FROM providers.apis ORDER BY slug')).rows;
  assert.equal(rows.length,37);
  assert.deepEqual(rows.map(row=>row.slug),manifest.map(row=>row.slug).sort());
  for(const row of rows) {
    assert.deepEqual([row.status,row.active,row.discoverable,row.is_verified],['active',true,true,true]);
    assert.equal(row.endpoint_url,null);
    assert.equal(row.authentication_info.configured,false);
    assert.equal(row.listing_metadata.catalog_only,true);
    assert.equal(row.listing_metadata.resell.enabled,false);
    assert.ok(row.listing_metadata.tags.includes('business-apis'));
    assert.ok(row.listing_metadata.apimarket.categories.every(tag=>row.listing_metadata.tags.includes(tag)));
    assert.ok(row.docs_url.startsWith('https://api.market/store/'));
    assert.equal(row.listing_metadata.apimarket.main_category,'business-apis');
  }
  const sourceRows=Object.values((await db.query(sourcesSql)).rows[0])[0];
  assert.equal(sourceRows.length,37,'every registered listing passes real Sources visibility SQL');
  assert.ok(sourceRows.every(row=>row.provider_slug==='api-market' && row.endpoint_count===0 && row.tags.includes('business-apis')));
  assert.equal((await db.query('SELECT count(*)::int AS n FROM providers.api_endpoints')).rows[0].n,0);
  assert.equal((await db.query('SELECT count(*)::int AS n FROM catalog.capability_execution_contracts')).rows[0].n,0);
  const user=(await db.query('SELECT * FROM auth.users')).rows[0];
  assert.equal((await db.query("SELECT banned_until='infinity'::timestamptz AS banned FROM auth.users")).rows[0].banned,true,'system identity cannot sign in');
  assert.equal(user.raw_app_meta_data.apiosk_system_account,'api-market');
  assert.equal((await db.query("SELECT current_setting('apiosk.api_review_approved_id',true) AS gate")).rows[0].gate,'');

  await db.query('UPDATE providers.apis SET discoverable=false,suspended_at=now() WHERE id=$1',[rows[0].id]);
  await db.exec(migration);
  const hidden=(await db.query('SELECT discoverable,suspended_at FROM providers.apis WHERE id=$1',[rows[0].id])).rows[0];
  assert.equal(hidden.discoverable,false); assert.ok(hidden.suspended_at);
  assert.equal(Object.values((await db.query(sourcesSql)).rows[0])[0].length,36,'rerunning does not undo curation');

  await db.query("UPDATE providers.apis SET listing_metadata=jsonb_set(listing_metadata,'{apimarket,id}','\"different/product\"') WHERE id=$1",[rows[1].id]);
  await assert.rejects(db.exec(migration),/identity collision/);
  await db.exec('ROLLBACK');
  assert.equal((await db.query('SELECT count(*)::int AS n FROM providers.apis')).rows[0].n,37,'collision never duplicates or replaces a listing');
  await db.exec('CREATE ROLE authenticated; CREATE SCHEMA apiosk_private');
  const curation=await readFile(new URL('../supabase/migrations/20261008115557_show_only_crunchbase_apimarket.sql',import.meta.url),'utf8');
  await db.exec(curation);
  await db.exec(curation);
  const visible=Object.values((await db.query(sourcesSql)).rows[0])[0];
  assert.equal(visible.length,1);
  assert.equal(visible[0].name,'Crunchbase');
  assert.equal(visible[0].logo_url,'https://www.crunchbase.com/favicon.ico');
  assert.equal((await db.query('SELECT count(*)::int AS n FROM providers.apis WHERE discoverable=false')).rows[0].n,36);
  assert.equal((await db.query('SELECT count(*)::int AS n FROM providers.apis')).rows[0].n,37);
  console.log('PASS API.market onboarding and curation: 37 retained, 36 hidden, only Crunchbase visible with official logo, no executable offers');
} finally { await db.close(); }
