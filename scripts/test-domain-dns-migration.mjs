// Real SQL in isolated PGlite. No credentials, network, account or paid calls.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { PGlite } from '../../app/node_modules/@electric-sql/pglite/dist/index.js';

const read = path => fs.readFile(new URL(path, import.meta.url), 'utf8');
const migration = await read('../supabase/migrations/20261007093340_typed_domain_dns_records.sql');
const fixture = JSON.parse(await read('../tests/fixtures/domain-intelligence/typed/catalog.json'));
const legacy = JSON.parse(await read('../tests/fixtures/domain-intelligence/services.json')).services.find(s => s.path === '/dns/{domain}');
const source = '00000000-0000-4000-8000-000000000001';
const db = new PGlite();
await db.waitReady;
try {
  await db.exec(`
    CREATE SCHEMA providers; CREATE SCHEMA catalog;
    CREATE TABLE providers.apis(id uuid PRIMARY KEY,slug text UNIQUE,is_platform_owned boolean,
      settlement_mode text,active boolean,status text,is_verified boolean,suspended_at timestamptz);
    CREATE TABLE providers.api_endpoints(id uuid PRIMARY KEY,api_id uuid REFERENCES providers.apis(id),method text,path text,
      price numeric,active boolean,payment_required boolean,cache_ttl_seconds integer,description text,query_params jsonb,
      request_body jsonb,response_body jsonb,request_example jsonb,response_example jsonb,tags text[],search_terms text[],
      sectors text[],use_cases text[],input_entities text[],output_entities text[],endpoint_category text,action_type text,
      external_docs_url text,documentation_markdown text,metadata_source text,metadata_confidence numeric,origin_override text,
      task_summary text,contract_documented boolean,UNIQUE(api_id,method,path));
    CREATE TABLE catalog.fact_types(slug text PRIMARY KEY,name text,description text,subject_kind text,value_schema jsonb,
      status text,freshness_seconds integer CHECK(freshness_seconds IS NULL OR freshness_seconds>0));
    CREATE TABLE catalog.capabilities(slug text PRIMARY KEY,name text,description text,category text,input_contract jsonb,
      output_contract jsonb,status text);
    CREATE TABLE catalog.capability_offers(capability_slug text,endpoint_id uuid REFERENCES providers.api_endpoints(id),api_id uuid,
      source text CHECK(source IN ('declared','curated','inferred')),confidence numeric,input_map jsonb,fixed_params jsonb,output_map jsonb,notes text,
      PRIMARY KEY(capability_slug,endpoint_id));
    CREATE TABLE catalog.capability_execution_contracts(id uuid PRIMARY KEY DEFAULT gen_random_uuid(),capability_slug text,
      endpoint_id uuid REFERENCES providers.api_endpoints(id),version integer,status text,requires jsonb,produces jsonb,
      input_map jsonb,fixed_params jsonb,output_map jsonb,cardinality text,identifies text,max_fan_out integer,preconditions jsonb,
      pagination jsonb,effect text,price_model text,freshness_seconds integer CHECK(freshness_seconds IS NULL OR freshness_seconds>0),sample_payload jsonb,validated_at timestamptz,
      validated_note text,notes text,adapter_version text,UNIQUE(capability_slug,endpoint_id,version));
    CREATE UNIQUE INDEX one_active_contract ON catalog.capability_execution_contracts(capability_slug,endpoint_id) WHERE status='active';
  `);
  await db.query("INSERT INTO providers.apis VALUES($1,'domain-intelligence',true,'apiosk_revenue',true,'active',true,NULL)", [source]);
  // Deliberately use a different tariff from the old fixture: migration must inherit it.
  await db.query(`INSERT INTO providers.api_endpoints(id,api_id,method,path,price,active,payment_required,origin_override)
    VALUES($1,$2,'GET','/dns/{domain}',0.012345,true,true,'https://dns.google')`, [legacy.endpoint_id, source]);
  await db.query(`INSERT INTO catalog.capability_execution_contracts(capability_slug,endpoint_id,version,status,effect,price_model,requires)
    VALUES('domain.mail-dns',$1,1,'active','read','per_call',$2)`, [legacy.endpoint_id, legacy.contract.requires]);
  const snapshot = async () => Object.fromEntries(await Promise.all([
    'providers.apis', 'providers.api_endpoints', 'catalog.fact_types', 'catalog.capabilities',
    'catalog.capability_offers', 'catalog.capability_execution_contracts',
  ].map(async table => [table, (await db.query(`SELECT to_jsonb(t) AS row FROM ${table} t ORDER BY to_jsonb(t)::text`)).rows])));
  const original = await snapshot();
  // Trigger a late error after earlier records were inserted; all writes must roll back.
  await db.query("INSERT INTO catalog.capabilities(slug,status,input_contract) VALUES('domain.dns-soa','active','{}')");
  const conflict = await snapshot();
  await assert.rejects(db.exec(migration), /DNS capability drift/);
  await db.exec('ROLLBACK');
  assert.deepEqual(await snapshot(), conflict);
  await db.query("DELETE FROM catalog.capabilities WHERE slug='domain.dns-soa'");
  await db.exec(migration);
  const after = await snapshot();
  assert.deepEqual(after['providers.apis'], original['providers.apis'], 'source identity and approval remain unchanged');
  for (const table of ['providers.api_endpoints', 'catalog.capability_execution_contracts']) {
    for (const old of original[table]) assert.deepEqual(after[table].find(row => row.row.id === old.row.id), old, 'historical MX rows remain unchanged');
  }
  const endpoints = (await db.query("SELECT * FROM providers.api_endpoints WHERE path<>'/dns/{domain}' ORDER BY path")).rows;
  assert.equal(endpoints.length, 8);
  const contracts = (await db.query("SELECT * FROM catalog.capability_execution_contracts WHERE adapter_version='domain-intelligence-v2'")).rows;
  assert.equal(contracts.length, 8);
  for (const expected of fixture.services) {
    const endpoint = endpoints.find(row => row.path === expected.path);
    const contract = contracts.find(row => row.capability_slug === expected.contract.capability_slug);
    assert.equal(Number(endpoint.price), 0.012345);
    assert.equal(endpoint.origin_override, 'https://dns.google');
    assert.equal(endpoint.active, true);
    assert.equal(endpoint.cache_ttl_seconds, 0, 'DNS TTLs must not be replaced by a fixed hour cache');
    assert.equal(contract.endpoint_id, endpoint.id);
    assert.equal(contract.freshness_seconds, 1, 'minimum freshness permitted by catalog constraints');
    assert.deepEqual(contract.requires, expected.contract.requires);
    assert.deepEqual(contract.input_map, expected.contract.input_map);
    assert.deepEqual(contract.output_map, expected.contract.output_map);
    assert.deepEqual(contract.sample_payload, expected.sample_payload);
    assert.deepEqual(endpoint.response_example, expected.sample_payload);
    const output = Object.keys(expected.contract.output_map.produces)[0];
    assert.deepEqual(endpoint.response_body, fixture.vocabulary.find(row => row.slug === output).value_schema);
  }
  await db.exec(migration);
  assert.deepEqual(await snapshot(), after, 'idempotent migration');
  const changed = contracts[0];
  await db.query("UPDATE catalog.capability_execution_contracts SET input_map='{}' WHERE id=$1", [changed.id]);
  const drift = await snapshot();
  await assert.rejects(db.exec(migration), /typed DNS contract drift/);
  await db.exec('ROLLBACK');
  assert.deepEqual(await snapshot(), drift, 'existing drift is never silently repaired');
  console.log('PASS: 8 typed DNS contracts, inherited current price, unchanged legacy/source rows, exact schemas/samples, idempotency and transactional drift refusal.');
} finally {
  await db.close();
}
