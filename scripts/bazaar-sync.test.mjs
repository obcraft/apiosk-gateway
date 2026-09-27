import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import {
  admitItem, atomicToUsd, batchRows, batches, BASE_USDC, buildCatalog, checkedSchema, contentHash, fetchAll, missedKeys,
  pathShape, scoreListing, serviceSlug, sqlChunk, uuid,
} from './bazaar-sync.mjs';

// Two real pages of the public discovery API, recorded 2026-09-28 (offsets 0
// and 6000, 20 items each). Admission is judged as of that day.
const page = name => JSON.parse(readFileSync(new URL(`../tests/fixtures/bazaar/${name}`, import.meta.url), 'utf8'));
const recorded = [...page('page-offset-0.json').items, ...page('page-offset-6000.json').items];
const now = Date.parse('2026-09-28T00:00:00Z');
const good = () => structuredClone(recorded.find(i => i.resource === 'https://api.onesource.io/api/chain/erc20-balance'));
const reason = (item, ctx = {}) => { const r = admitItem(item, { now, ...ctx }); return r.ok ? 'admitted' : r.reason; };

test('the recorded pages admit only Base USDC exact offers and record a reason for every exclusion', () => {
  const catalog = buildCatalog(recorded, { now, tier: 500 });
  assert.equal(catalog.admitted.length + catalog.excluded.length, recorded.length);
  assert.deepEqual(catalog.admitted.map(c => c.resource).sort(), [
    'https://api.onesource.io/api/chain/chain-id', 'https://api.onesource.io/api/chain/erc20-balance',
    'https://api.onesource.io/api/chain/erc721-tokens', 'https://api.onesource.io/api/chain/live-balance',
    'https://api.onesource.io/api/chain/network-info', 'https://api.onesource.io/api/chain/nft-metadata',
    'https://crypto.apitoll.cloud/v1/crypto/price',
  ]);
  for (const x of catalog.excluded) assert.match(x.reason, /^[a-z_]+$/, JSON.stringify(x));
  const reasons = Object.groupBy(catalog.excluded, x => x.reason);
  assert.equal(reasons.path_placeholder_trailing.length, 6, 'express-style :params are not published in the pilot');
  assert.equal(reasons.ambiguous_offer[0].resource, 'https://api.exa.ai/search');
  for (const c of catalog.admitted) {
    assert.equal(c.network, 'eip155:8453');
    assert.ok(BigInt(c.price_atomic) > 0n && BigInt(c.price_atomic) <= 1_000_000n);
    assert.match(c.pay_to, /^0x[0-9a-f]{40}$/);
  }
});

test('every admission rule refuses with its own reason', () => {
  const cases = [
    [i => { i.accepts = i.accepts.map(a => ({ ...a, network: 'solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp' })); }, 'no_base_network'],
    [i => { i.accepts = i.accepts.map(a => ({ ...a, asset: '0x0000000000000000000000000000000000000001' })); }, 'no_base_usdc_asset'],
    [i => { i.accepts = i.accepts.map(a => ({ ...a, scheme: 'batch-settlement' })); }, 'no_exact_scheme'],
    [i => { i.accepts = [{ ...i.accepts[0], amount: '1000001', maxAmountRequired: '1000001' }]; }, 'price_above_cap'],
    [i => { i.accepts = [{ ...i.accepts[0], amount: '0', maxAmountRequired: '0' }]; }, 'unpriced'],
    [i => { i.accepts = [{ ...i.accepts[0], amount: '3000', maxAmountRequired: '4000' }]; }, 'ambiguous_amount'],
    [i => { i.accepts = [i.accepts[0], { ...i.accepts[0], payTo: '0x1111111111111111111111111111111111111111' }]; }, 'ambiguous_offer'],
    [i => { i.resource += '?network=base'; }, 'resource_has_query'],
    [i => { i.resource = 'https://gateway.apiosk.com/api/chain/erc20-balance'; }, 'apiosk_host'],
    [i => { i.resource = 'https://127.0.0.1/api'; }, 'unsafe_host'],
    [i => { i.resource = 'https://abc.ngrok-free.app/api'; }, 'unsafe_host'],
    [i => { i.resource = 'http://api.onesource.io/api/chain/erc20-balance'; }, 'unsafe_resource_url'],
    [i => { i.resource = 'https://demo.onrender.com/api/chain/erc20-balance'; delete i.serviceName; }, 'anonymous_paas_host'],
    [i => { i.quality.l30DaysTotalCalls = 0; }, 'no_recent_calls'],
    [i => { i.lastUpdated = '2026-08-01T00:00:00Z'; }, 'stale_listing'],
    [i => { i.resource = 'https://api.onesource.io/api/:chain/balance'; }, 'path_placeholder_mid'],
    [i => { i.resource = 'https://api.onesource.io/api/balance/{address}'; }, 'path_placeholder_trailing'],
    [i => { i.resource = 'https://api.onesource.io/api/send'; }, 'effect_review_required'],
    [i => { i.description = 'Exploit finder for any contract, with an attack surface map of the wallet'; }, 'specialized_review_required'],
    [i => { i.description += ' Returns a pending job id; the result is asynchronous.'; }, 'asynchronous_result'],
    [i => { i.description = 'Roll the dice for your agent and read the dice result as a token balance'; }, 'toy'],
    [i => { i.description = 'test'; }, 'junk_description'],
    [i => { i.extensions.bazaar.info.input.method = 'DELETE'; }, 'method_unsupported'],
    [i => { i.extensions.bazaar.schema.properties.input.properties.queryParams.properties.address = { oneOf: [{ type: 'string' }] }; }, 'schema_unsupported'],
    [i => { i.extensions.bazaar.schema.properties.input.properties.headers = { type: 'object', properties: { 'x-api-key': { type: 'string' } }, required: ['x-api-key'] }; }, 'required_headers'],
    [i => { delete i.extensions.bazaar.schema; }, 'missing_input_schema'],
  ];
  for (const [mutate, expected] of cases) { const item = good(); mutate(item); assert.equal(reason(item), expected, expected); }
  assert.equal(reason(good()), 'admitted');
  // A PaaS host that names its service is a supplier, not an anonymous demo.
  const named = good(); named.resource = 'https://demo.onrender.com/api/chain/erc20-balance'; named.serviceName = 'Demo Chain';
  assert.equal(reason(named), 'admitted');
  // The price ceiling is inclusive.
  const dollar = good(); dollar.accepts = [{ ...dollar.accepts[0], amount: '1000000', maxAmountRequired: '1000000' }];
  assert.equal(reason(dollar), 'admitted');
  assert.equal(reason(good(), { existingHosts: new Set(['api.onesource.io']) }), 'host_already_listed');
  assert.equal(reason(good(), { deniedHosts: new Set(['api.onesource.io']) }), 'host_policy_deny');
  assert.equal(reason(good(), { hostMaxAtomic: new Map([['api.onesource.io', 1000n]]) }), 'host_policy_price_cap');
  assert.equal(reason(good(), { minScore: 11 }), 'low_listing_score');
});

test('the declared input schema is unwrapped and held to the Ask v2 external rules', () => {
  const admitted = admitItem(good(), { now });
  assert.equal(admitted.ok, true);
  assert.deepEqual(admitted.candidate.query_params.required, ['address', 'contract']);
  assert.equal(admitted.candidate.request_body, null);
  assert.equal(admitted.candidate.query_params.properties.address.pattern, '^0x[a-fA-F0-9]{40}$');
  assert.deepEqual(checkedSchema({ type: 'string', const: 'http', format: 'uri', example: 'x' }), { type: 'string', enum: ['http'] });
  for (const bad of [{ type: 'string', pattern: '(?=a)b' }, { type: 'object', additionalProperties: { type: 'string' } },
    { type: 'object', properties: { a: { type: 'string' } }, required: ['b'] }, { description: 'untyped' }, { type: ['string', 'null'] },
    { type: 'array', maxItems: 3 }, { type: 'string', enum: [] }]) {
    assert.throws(() => checkedSchema(bad), JSON.stringify(bad));
  }
});

test('usage can only cap a listing score, as in the legacy index', () => {
  const item = good();
  const accept = item.accepts[0];
  assert.ok(scoreListing(item, accept, now).score >= 8);
  item.quality = { l30DaysTotalCalls: 3, l30DaysUniquePayers: 1, lastCalledAt: '2026-09-27T00:00:00Z' };
  assert.equal(scoreListing(item, accept, now).score, 7);
  item.quality.l30DaysUniquePayers = 0;
  assert.equal(scoreListing(item, accept, now).score, 4);
  delete item.quality;
  assert.equal(scoreListing(item, accept, now).capped, null, 'no usage data is never held against a listing');
});

test('the tier ranks by 30-day calls, caps each host and groups services by host', () => {
  const items = Array.from({ length: 6 }, (_, n) => {
    const item = good();
    item.resource = `https://${n < 4 ? 'a.example-data.io' : 'b.example-data.io'}/v1/r${'abcdef'[n]}`;
    item.quality = { ...item.quality, l30DaysTotalCalls: 100 - n };
    item.serviceName = n < 4 ? 'Alpha Data' : 'Beta Data';
    return item;
  });
  const catalog = buildCatalog(items, { now, tier: 4, maxPerHost: 3 });
  assert.deepEqual(catalog.tier.map(c => c.resource.slice(-2)), ['ra', 'rb', 'rc', 're']);
  assert.equal(catalog.beyondHostCap.length, 1);
  assert.deepEqual(catalog.services.map(s => [s.slug, s.name, s.endpoints.length]), [['cbz-a-example-data-io', 'Alpha Data', 3], ['cbz-b-example-data-io', 'Beta Data', 1]]);
  assert.equal(catalog.services[0].id, uuid(['bazaar-api-v1', 'a.example-data.io']));
  const dup = buildCatalog([good(), good()], { now });
  assert.equal(dup.admitted.length, 1);
  assert.equal(dup.excluded[0].reason, 'duplicate_resource');
});

test('one listing per concrete call is an example of a template, not an endpoint', () => {
  assert.equal(pathShape('/api/flightaware/flights/UA123'), '/api/flightaware/flights/*');
  assert.equal(pathShape('/api/chain/erc20-balance'), '/api/chain/erc20-balance');
  const items = ['UA123', 'AA95'].map(id => { const i = good(); i.resource = `https://api.onesource.io/flights/${id}`; return i; });
  const catalog = buildCatalog([...items, good()], { now });
  assert.deepEqual(catalog.admitted.map(c => c.path), ['/api/chain/erc20-balance']);
  assert.deepEqual(catalog.excluded.map(x => x.reason), ['concrete_instance_path', 'concrete_instance_path']);
});

test('contracts are staged through the provider-neutral document adapter only when an input is required', () => {
  const catalog = buildCatalog(recorded, { now, tier: 500 });
  const staged = catalog.contracts.services;
  assert.ok(staged.length > 0);
  for (const s of staged) {
    assert.equal(s.unverified, true);
    assert.equal(s.id, uuid(['source-document-v1', s.endpoint_id]));
    assert.match(s.contract.capability_slug, /^operation\.cbz-[a-z0-9-]+\.(get|post)-/);
    assert.ok(s.contract.requires.some(r => !r.optional));
  }
  const blocked = catalog.contracts.report.filter(r => r.blockers.length);
  assert.ok(blocked.every(r => r.blockers.includes('required_input_alternatives_need_review') || r.blockers.includes('unsupported_required_input')));
});

test('batches stay within 200 rows and 280 KB, and each SQL chunk is one RPC call', () => {
  const catalog = buildCatalog(recorded, { now, tier: 500 });
  const rows = batchRows(catalog);
  assert.equal(rows.length, catalog.tier.length);
  for (const row of rows) {
    assert.match(row.content_hash, /^[0-9a-f]{64}$/);
    assert.match(row.service.slug, /^cbz-[a-z0-9-]+$/);
    assert.equal(row.service.origin_url, `https://${row.host}`);
    assert.deepEqual(row.service.pay_to, [...new Set(row.service.pay_to)].sort());
  }
  const many = Array.from({ length: 450 }, (_, n) => ({ ...rows[0], path: `/p${n}` }));
  const split = batches(many);
  assert.ok(split.every(b => b.length <= 200 && Buffer.byteLength(JSON.stringify({ run_id: null, rows: b })) <= 300_000));
  assert.equal(split.flat().length, 450);
  assert.throws(() => batches([{ big: 'x'.repeat(300_000) }]));
  const sql = sqlChunk(split[0], 0, split.length);
  assert.equal(sql.match(/^SELECT /gm).length, 1);
  assert.ok(Buffer.byteLength(sql) <= 300_000);
  assert.doesNotMatch(sql, /\bDELETE\b|\bDROP\b|\bTRUNCATE\b/);
  const tricky = sqlChunk([{ ...rows[0], endpoint: { ...rows[0].endpoint, description: 'costs $bazaar$ nothing' } }], 0, 1);
  assert.match(tricky, /\$bazaarx\$/);
});

test('ids, slugs, amounts and content hashes are deterministic', () => {
  assert.equal(atomicToUsd(3000n), '0.003');
  assert.equal(atomicToUsd(1_000_000n), '1');
  assert.equal(atomicToUsd(1_250_001n), '1.250001');
  assert.equal(serviceSlug('api.onesource.io'), 'cbz-api-onesource-io');
  const long = serviceSlug(`${'a'.repeat(120)}.example.com`);
  assert.ok(long.length <= 96 && /^cbz-[a-z0-9-]+$/.test(long));
  const c = admitItem(good(), { now }).candidate;
  const service = { name: 'OneSource', logo: null };
  assert.equal(contentHash(c, service), contentHash(structuredClone(c), service));
  assert.notEqual(contentHash(c, service), contentHash({ ...c, price_atomic: '4000' }, service));
});

test('only tracked, live resources that no longer pass admission count as missed', () => {
  const admitted = [{ host: 'a.io', method: 'GET', path: '/x' }];
  const tracked = [{ host: 'a.io', method: 'GET', path: '/x' }, { host: 'a.io', method: 'GET', path: '/gone' }, { host: 'a.io', method: 'GET', path: '/old', retired_at: '2026-09-01' }];
  assert.deepEqual(missedKeys(tracked, admitted), [{ host: 'a.io', method: 'GET', path: '/gone' }]);
});

test('paging is polite, retries a rate limit and reports an incomplete read', async () => {
  const calls = [];
  const pages = [{ items: [1, 2], pagination: { total: 3 } }, { items: [3], pagination: { total: 3 } }];
  let limited = false;
  const fetchImpl = async url => {
    calls.push(String(url));
    if (!limited) { limited = true; return new Response('slow down', { status: 429 }); }
    return Response.json(pages.shift());
  };
  const read = await fetchAll({ fetchImpl, gapMs: 0, retryMs: 0 });
  assert.deepEqual(read.items, [1, 2, 3]);
  assert.equal(read.complete, true);
  assert.match(calls[0], /limit=1000&offset=0$/);
  assert.match(calls.at(-1), /offset=2$/);
  await assert.rejects(fetchAll({ fetchImpl: async () => new Response('no', { status: 400 }), gapMs: 0 }), /HTTP 400/);
  const short = await fetchAll({ fetchImpl: async () => Response.json({ items: [], pagination: { total: 5 } }), gapMs: 0 });
  assert.equal(short.complete, false);
});
