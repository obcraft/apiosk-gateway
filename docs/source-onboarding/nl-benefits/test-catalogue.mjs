import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { registrationDatabase } from './test-database.mjs';

const directory = new URL('../../../supabase/migrations/',import.meta.url);
const read = name => fs.readFile(new URL(name,directory),'utf8');
const publicationName = (await fs.readdir(directory)).find(name => name.endsWith('_brand_nl_benefits_sources.sql'));
assert.ok(publicationName, 'tracked catalogue migration');
const publication = await read(publicationName);
const db = await registrationDatabase();
const rows = async sql => (await db.query(sql)).rows;
const affected = ['dienst-toeslagen','samenwerkende-catalogi','nl-toeslagen-rekenservice','nl-gemeentelijke-regelingen'];
try {
  await db.exec(`INSERT INTO providers.apis(owner_id,slug,name,description,origin_url,status,is_platform_owned,settlement_mode,requires_payout_wallet)
    VALUES('bf085fe8-41c4-4699-978f-e75e89518bf7','koop','Existing KOOP','Existing description','https://repository.overheid.nl','inactive',true,'apiosk_revenue',false);
    INSERT INTO providers.api_endpoints(api_id,method,path,price,active,tags)
    SELECT id,'GET',path,0.085066,true,ARRAY['existing-tag'] FROM providers.apis CROSS JOIN (VALUES('/sru'),('/sru/Search')) paths(path) WHERE slug='koop';`);
  await db.exec(await read('20261013172106_onboard_nl_benefits_sources.sql'));
  await db.exec(await read('20261013181301_onboard_nl_benefits_runtime.sql'));
  const before = await rows('SELECT * FROM providers.apis ORDER BY slug');
  const endpointsBefore = await rows('SELECT * FROM providers.api_endpoints ORDER BY id');
  const contractsBefore = await rows('SELECT * FROM catalog.capability_execution_contracts ORDER BY id');

  // A late failure must not expose half of the catalogue.
  await assert.rejects(db.exec(publication.replace(/COMMIT;\s*$/, 'SELECT 1/0; COMMIT;')), /division by zero/);
  await db.exec('ROLLBACK');
  assert.deepEqual(await rows('SELECT * FROM providers.apis ORDER BY slug'), before);
  assert.deepEqual(await rows('SELECT * FROM providers.api_endpoints ORDER BY id'), endpointsBefore);

  await db.exec(publication);
  const after = await rows('SELECT * FROM providers.apis ORDER BY slug');
  for (const source of after) {
    if (!affected.includes(source.slug)) {
      assert.deepEqual(source, before.find(row => row.id===source.id), `${source.slug} unchanged`);
      continue;
    }
    assert.equal(source.status,'active');
    assert.ok(source.active && source.discoverable && source.is_verified && !source.suspended_at);
    assert.match(source.listing_metadata.logo,/^https:\/\//);
    assert.equal(source.listing_metadata.catalog_state,'browse_only');
    assert.equal(source.listing_metadata.execution_enabled,false);
    assert.ok(source.listing_metadata.planner_block_reason);
  }
  assert.deepEqual(await rows('SELECT * FROM catalog.capability_execution_contracts ORDER BY id'),contractsBefore,'publication cannot approve execution contracts');
  for (const endpoint of await rows('SELECT * FROM providers.api_endpoints ORDER BY id')) {
    const source = after.find(row => row.id===endpoint.api_id);
    const original = endpointsBefore.find(row => row.id===endpoint.id);
    assert.deepEqual(endpoint, affected.includes(source.slug) ? {...original,active:true} : original,'preserve schemas, tags, category, price and source relationship');
  }
  const counts = await rows(`SELECT a.slug,count(e.id)::int total FROM providers.apis a JOIN providers.api_endpoints e ON e.api_id=a.id
    WHERE a.listing_metadata->>'catalog_state'='browse_only' AND e.active GROUP BY a.slug ORDER BY a.slug`);
  assert.deepEqual(counts,[{slug:'dienst-toeslagen',total:5},{slug:'nl-gemeentelijke-regelingen',total:1},{slug:'nl-toeslagen-rekenservice',total:4},{slug:'samenwerkende-catalogi',total:1}]);
  const openapi = JSON.parse(await fs.readFile(new URL('runtime.openapi.json',import.meta.url),'utf8'));
  assert.deepEqual(openapi.servers,[{url:'https://gateway.apiosk.com'}],'use the public executor origin, not the Supabase API origin');
  for (const endpoint of await rows("SELECT a.slug,e.method,e.path FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id WHERE a.slug IN ('nl-toeslagen-rekenservice','nl-gemeentelijke-regelingen')")) {
    assert.ok(openapi.paths[`/${endpoint.slug}${endpoint.path}`]?.[endpoint.method.toLowerCase()], 'each native endpoint has a documented public route');
  }
  await assert.rejects(db.exec(publication),/must be an unsuspended staged source/);
  await db.exec('ROLLBACK');
  console.log('PASS: 4 visible sources / 11 nested endpoints with logos; execution disabled; all contracts, prices, schemas and metadata preserved; Nibud and KOOP untouched; late failure rolls back publication.');
} finally { await db.close(); }
