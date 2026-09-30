#!/usr/bin/env node
// Verify the public API against production for the Revact/Nomi checklist.
//
//   APIOSK_KEY=apk_org_live_… node scripts/verify-live-api.mjs           # free checks only
//   APIOSK_KEY=apk_org_live_… node scripts/verify-live-api.mjs --paid    # plus a few paid calls (up to $0.75 total)
//
// Free: authentication, balance, the sources for tenders, funding, news and
// KVK, the Pulse Network group and Sirenic, operation variants, and executable
// choices from Ask. Paid (opt-in): one direct call per signal type, checking
// that every result carries its metadata and that usage sums to the cost.
const base = process.env.APIOSK_BASE_URL || 'https://gateway.apiosk.com'
const key = process.env.APIOSK_KEY
const paid = process.argv.includes('--paid')
if (!key?.startsWith('apk_org_live_')) { console.error('Set APIOSK_KEY to an organisation API key.'); process.exit(2) }

let failures = 0
const pass = (m) => console.log(`PASS ${m}`)
const fail = (m, d) => { failures++; console.log(`FAIL ${m}${d ? `\n     ${JSON.stringify(d).slice(0, 400)}` : ''}`) }
async function call(method, path, body, headers = {}) {
  const r = await fetch(base + path, { method, headers: { Authorization: `Bearer ${key}`, 'Content-Type': 'application/json', ...headers }, body: body ? JSON.stringify(body) : undefined })
  let json = null
  try { json = await r.json() } catch { /* empty */ }
  return { status: r.status, json }
}
const micro = (m) => Number(m?.amount_micro_usd ?? NaN)

// ── Authentication and balance ───────────────────────────────────────────
const bal = await call('GET', '/v2/balance')
bal.status === 200 ? pass(`key works; balance ${bal.json.balance?.available?.amount} USD, month left ${bal.json.limits?.month_remaining?.amount} USD`) : fail('GET /v2/balance', bal)
const bad = await fetch(base + '/v2/balance', { headers: { Authorization: 'Bearer apk_org_live_' + '0'.repeat(48) } })
bad.status === 401 ? pass('an unknown key is refused with 401') : fail('unknown key not refused', bad.status)

// ── Sources for each signal ──────────────────────────────────────────────
const wanted = {
  tenders: [['tenderned', 'company_intel.search'], ['ted', 'procurement.ted-kvk-notices'], ['eu-funding-tenders', 'eu_funding.calls.search']],
  funding: [['rvo', 'subsidy.scheme.first-page-search'], ['exa', 'web.search'], ['linkup', 'web.linkup-answer']],
  news: [['reuters', 'news.reuters-search'], ['bloomberg', 'news.bloomberg-latest']],
  kvk: [['kvk-dutch-business-register', 'company_registry.search'], ['kvk-dutch-business-register', 'company.registry-profile'], ['kvk-dutch-business-register', 'company.financial-statements']],
  france: [['sirenic', 'company.sirenic-marches-expirations'], ['sirenic', 'company.registry-profile']],
}
for (const [signal, ops] of Object.entries(wanted)) {
  for (const [source, op] of ops) {
    const r = await call('GET', `/v2/sources/${source}`)
    const found = r.json?.operations?.find((o) => o.id === op)
    found ? pass(`${signal}: ${source} ${op} (${found.pricing.price.amount} USD, inputs ${found.input.map((i) => i.field).join(', ') || 'none'})`) : fail(`${signal}: ${source} ${op}`, r.json?.error || r.status)
  }
}
const kvk = await call('GET', '/v2/sources/kvk-dutch-business-register')
const lookup = kvk.json?.operations?.find((o) => o.id === 'company_registry.lookup')
lookup?.variants?.length > 1 ? pass(`variants listed: company_registry.lookup has ${lookup.variants.length}`) : fail('variants for company_registry.lookup', lookup)
const pulse = await call('GET', '/v2/sources/pulsenetwork')
pulse.status === 200 && pulse.json.sources?.length > 1 ? pass(`pulsenetwork resolves as a group of ${pulse.json.sources.length} sources, ${pulse.json.operations.length} operations`) : fail('pulsenetwork group', pulse.json?.error || pulse.status)
const list = await call('GET', '/v2/sources?search=tender&limit=10')
list.status === 200 && list.json.sources?.length ? pass(`search "tender" finds ${list.json.total} sources`) : fail('search tender', list.json)

// ── Ask choices (free) ─────────────────────────────────────────────────────
for (const question of ['New Dutch government tenders about cybersecurity', 'Latest news about Mollie', 'KVK details for Mollie B.V.']) {
  const r = await call('POST', '/v2/ask', { question })
  const apis = r.json?.apis || []
  const runnable = apis.filter(api => api.executable)
  const valid = runnable.every(api => typeof api.api_id === 'string' && typeof api.capability === 'string'
    && Array.isArray(api.inputs) && (api.outputs === null || Array.isArray(api.outputs))
    && api.input_values && Array.isArray(api.missing_inputs) && /^\d+$/.test(api.buyer_price?.buyer_atomic))
  r.status === 200 && Array.isArray(r.json?.apis) && valid && !r.json.plan && !r.json.id && !r.json.errors?.length
    ? pass(`ask "${question}" → ${apis.length} choices, ${runnable.length} executable contracts`)
    : fail(`ask "${question}"`, r.json)
}

// ── Paid calls (opt-in) ──────────────────────────────────────────────────
if (paid) {
  const runs = [
    ['tenderned', 'company_intel.search', { search: 'cybersecurity' }],
    ['kvk-dutch-business-register', 'company_registry.search', { 'company.name': 'Mollie B.V.' }],
    ['reuters', 'news.reuters-search', { 'news.reuters_search_query': 'Mollie' }],
  ]
  for (const [source, op, input] of runs) {
    const r = await call('POST', `/v1/execute/${source}/${op}?max_spend=0.25`, input, { 'Idempotency-Key': `verify-${source}-${op}-${new Date().toISOString().slice(0, 10)}` })
    const results = r.json?.results || []
    const parts = (r.json?.usage?.sources || []).reduce((sum, s) => sum + micro(s.cost), 0)
    const meta = results[0]?.metadata
    if (r.status !== 200 || r.json.status !== 'completed') { fail(`paid ${source}/${op}`, r.json?.error || r.json); continue }
    parts === micro(r.json.usage.cost) ? pass(`paid ${source}/${op}: ${results.length} result(s), cost ${r.json.usage.cost.amount} USD = sum of sources`) : fail(`usage does not sum for ${source}/${op}`, r.json.usage)
    meta?.source_id && meta?.retrieved_at && meta?.cost ? pass(`metadata: source ${meta.source_id}, retrieved ${meta.retrieved_at}, identifiers ${JSON.stringify(meta.identifiers)}, link ${meta.source_url ?? 'none published'}`) : fail(`metadata missing for ${source}/${op}`, meta)
  }
}

console.log(failures ? `\n${failures} check(s) failed.` : '\nAll checks passed.')
process.exit(failures ? 1 : 0)
