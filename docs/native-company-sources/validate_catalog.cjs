#!/usr/bin/env node
// Offline artifact validation. Rebuild exports with: python3 build_catalog.py
// Uses Ajv from this environment or the adjacent Apiosk app's dependencies.
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const assert = require('assert/strict');
const root = __dirname;
const Ajv = require(require.resolve('ajv', {paths: [root, path.resolve(root, '../../../app')]}));
const ajv = new Ajv({allErrors: true});
const read = file => JSON.parse(fs.readFileSync(path.join(root, file), 'utf8'));
const cat = read('catalog.json');
let checks = 0;
function check(condition, message) { assert(condition, message); checks++; }
for (const file of ['catalog.schema.json', 'company-record.schema.json']) {
  const schema = read(file);
  check(ajv.validateSchema(schema), file + ': ' + ajv.errorsText());
}
const valid = ajv.compile(read('catalog.schema.json'));
check(valid(cat), 'Catalog schema: ' + ajv.errorsText(valid.errors));
const ids = new Set(cat.sources.map(s => s.source_id));
check(ids.size === cat.sources.length, 'Duplicate source IDs');
check(cat.sources.length === cat.counts.source_records, 'Source count mismatch');
check(cat.countries.length === cat.counts.countries, 'Country count mismatch');
check(cat.countries.filter(c => c.region === 'EU27').length === 27, 'EU coverage');
check(new Set(cat.countries.map(c => c.country_code)).size === 42, 'Country uniqueness');
assert.deepEqual([...cat.scope.requested_countries].sort(), cat.countries.map(c => c.country_code).sort()); checks++;
assert.deepEqual([...cat.scope.eu_members].sort(), cat.countries.filter(c => c.region === 'EU27').map(c => c.country_code).sort()); checks++;
const sources = new Map(cat.sources.map(s => [s.source_id, s]));
const plans = read('implementation.json').source_plans;
check(plans.length === ids.size, 'Implementation plan count');
check(new Set(plans.map(p => p.source_id)).size === ids.size, 'Implementation plan uniqueness');
assert.deepEqual(cat.catalog_sections.map(s => s.id), ['countries', 'other', 'integration', 'evidence']); checks++;
for (const plan of plans) {
  const s = sources.get(plan.source_id);
  check(Boolean(s), 'Implementation references unknown source');
  check(plan.new_activation_proven === false, 'Implementation overstates activation');
  check(plan.stages.length === 7, 'Incomplete implementation stages');
  assert.deepEqual(plan.required_work, s.required_work); checks++;
  for (const fixture of plan.evidence.fixtures) check(fs.existsSync(path.join(root, fixture)), 'Missing implementation evidence');
}
const authorities = read('evidence/gleif-registration-authorities.json').rows;
const native = new Set();
for (const country of cat.countries) {
  check(country.native_source_ids.length > 0, country.country_code + ': missing native source');
  for (const id of country.native_source_ids) {
    const s = sources.get(id);
    check(s && s.country_codes.includes(country.country_code), 'Country/source mismatch: ' + id);
    check(s.source_class === 'national_registry' || s.source_class.startsWith('official_'), 'Not a national publisher: ' + id);
    native.add(id);
  }
  const exp = read('countries/' + country.country_code.toLowerCase() + '.json');
  assert.deepEqual(exp.country, country); checks++;
  assert.deepEqual(exp.sources, country.native_source_ids.map(id => sources.get(id))); checks++;
  assert.deepEqual(exp.implementation_plans, country.native_source_ids.map(id => plans.find(p => p.source_id === id))); checks++;
  assert.deepEqual(exp.capability_contracts, cat.capability_contracts.filter(op => op.country_code === country.country_code)); checks++;
}
check(native.size === cat.counts.national_source_records, 'Native-source count');
for (const s of cat.sources) {
  check(s.access.apiosk_list_price_micro_usd === null, s.source_id + ': unexpected price');
  check(s.readiness.activation_authorized_by_this_file === false, s.source_id + ': activation flag');
  check(s.identifiers.every(id => id.value_type === 'string'), s.source_id + ': identifiers must preserve source representation');
  for (const p of s.proposed_capabilities) {
    check(Boolean(cat.shared_capability_contracts[p.capability_template]), s.source_id + ': capability template');
    check(p.inputs.length > 0 && p.outputs.length > 0, s.source_id + ': missing capability inputs/outputs');
  }
  for (const ref of s.references) check(new URL(ref.url).protocol === 'https:', s.source_id + ': invalid reference');
  for (const ra of s.registration_authority_codes) {
    check(authorities.some(row => row['Registration Authority Code'] === ra && (!s.country_codes.length || s.country_codes.includes(row['Country Code']))), s.source_id + ': authority mismatch ' + ra);
  }
}
for (const wave of cat.priorities) for (const id of wave.sources) check(ids.has(id), 'Unknown priority source: ' + id);
check(new Set(cat.capability_contracts.map(op => op.spec_id)).size === cat.counts.detailed_operation_specs, 'Contract IDs/count');
for (const op of cat.capability_contracts) {
  const s = sources.get(op.source_id);
  check(Boolean(s), 'Unknown contract source');
  check(s.proposed_capabilities.some(p => p.public_name === op.public_capability), op.spec_id + ': public capability missing');
  check(op.path.startsWith('/') && new URL(op.origin_url).protocol === 'https:', op.spec_id + ': invalid route');
  check(ajv.validateSchema(op.input_schema), op.spec_id + ': invalid input schema');
  check(op.max_fan_out === 1 && op.list_price_micro_usd === null, op.spec_id + ': billing assumptions');
  check(op.status === 'draft_not_registered' && !op.verification.apiosk_execution_tested, op.spec_id + ': evidence overclaim');
  for (const key of op.input_schema.required) check(Boolean(op.input_map[key]), op.spec_id + ': unbound required input ' + key);
  if (op.verification.fixture) check(fs.existsSync(path.join(root, op.verification.fixture)), op.spec_id + ': missing fixture');
}
const manifest = read('evidence/probe-manifest.json');
check(manifest.length === 7 && manifest.every(p => p.status === 200 && !p.error), 'Probe manifest success counts');
for (const p of manifest) {
  const body = fs.readFileSync(path.join(root, 'evidence', p.body_file));
  check(crypto.createHash('sha256').update(body).digest('hex') === p.saved_fixture_sha256, p.id + ': saved fixture hash');
}
check(read('evidence/no-brreg-profile.json').organisasjonsnummer === '923609016', 'NO identity');
check(read('evidence/fi-prh-profile.json').companies[0].businessId.value === '0112038-9', 'FI identity');
check(read('evidence/cz-ares-profile.json').ico === '00177041', 'CZ identity, including leading zeroes');
check(read('evidence/pl-krs-profile.json').odpis.naglowekA.numerKRS === '0000006865', 'PL identity, including leading zeroes');
check(read('evidence/fr-dinum-search.json').results[0].siren === '552032534', 'FR identity');
check(read('evidence/sk-ruz-identity.json').id.includes(read('evidence/sk-ruz-profile.json').id), 'SK unit linkage');
check(read('evidence/sk-ruz-profile.json').ico === '00603481', 'SK identity, including leading zeroes');
const ares = read('evidence/ares-openapi.json');
check(Boolean(ares.components.schemas.StrankovaniARazeni.properties.start && ares.components.schemas.StrankovaniARazeni.properties.pocet), 'ARES pagination fields');
const html = fs.readFileSync(path.join(root, 'index.html'), 'utf8');
check(!html.includes('@@'), 'Unexpanded HTML token');
check((html.match(/class="country"/g) || []).length === 42, 'Rendered country count');
check(html.includes('<option value="EU27">'), 'EU filter option missing');
check((html.match(/<button role="tab"/g) || []).length === 4, 'Expected exactly four core tabs');
check((html.match(/class="source implementation-source"/g) || []).length === ids.size, 'Rendered implementation plans');
for (const match of html.matchAll(/href="([^"]+)"/g)) {
  const href = match[1];
  if (!href.startsWith('https://') && !href.startsWith('#')) check(fs.existsSync(path.resolve(root, href)), 'Broken local link: ' + href);
}
console.log(JSON.stringify({status: 'passed', checks, countries: cat.counts.countries, sources: ids.size,
  operation_specs: cat.counts.detailed_operation_specs, public_api_requests: manifest.length,
  scope: 'Offline catalog/schema/export/reference/fixture integrity. No adapter or paid end-to-end test.'}, null, 2));
