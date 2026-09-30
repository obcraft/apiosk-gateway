#!/usr/bin/env node
// Default: free Ask discovery. Execution requires an explicit selection, cap
// and stable retry key. Credentials are read from the environment, never logged.
import assert from 'node:assert/strict'

const base = process.env.APIOSK_BASE_URL || 'https://gateway.apiosk.com'
const key = process.env.APIOSK_KEY
const question = process.env.APIOSK_QUESTION
const execute = process.argv.includes('--execute')
assert(key && question, 'Set APIOSK_KEY and APIOSK_QUESTION.')
const selectedId = process.env.APIOSK_API_ID
const retryKey = process.env.APIOSK_IDEMPOTENCY_KEY
const cap = process.env.APIOSK_MAX_SPEND
if (execute) {
  assert(selectedId && retryKey, 'Set APIOSK_API_ID from discovery and a stable APIOSK_IDEMPOTENCY_KEY.')
  assert(cap && /^\d+(\.\d{1,6})?$/.test(cap), 'Set APIOSK_MAX_SPEND to the total USD amount you approve.')
}

async function call(method, path, body, idempotency) {
  const response = await fetch(new URL(path, base), {
    method, signal: AbortSignal.timeout(60_000),
    headers: { Authorization: `Bearer ${key}`, 'Content-Type': 'application/json', ...(idempotency ? { 'Idempotency-Key': idempotency } : {}) },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  const value = await response.json()
  assert([200, 202].includes(response.status), `${path}: HTTP ${response.status}, ${value.error?.code || value.status || 'unexpected response'}`)
  return value
}

const choices = await call('POST', '/v2/ask', { question })
assert(Array.isArray(choices.apis) && !choices.plan && !choices.id, 'Expected the choice contract; this deployment may still run the previous API.')
assert(!choices.errors?.length, 'One or more discovery origins failed; inspect Ask before executing.')
for (const api of choices.apis.filter(api => api.executable)) {
  assert(api.api_id && api.capability && Array.isArray(api.inputs) && Array.isArray(api.missing_inputs), 'Incomplete executable contract.')
  console.log(JSON.stringify({ api_id: api.api_id, capability: api.capability, source: api.source_slug || api.url,
    inputs: api.inputs, input_values: api.input_values, missing_inputs: api.missing_inputs,
    outputs: api.outputs, buyer_price: api.buyer_price }))
}
console.log(`PASS discovery: ${choices.apis.length} choices; no execution requested.`)

if (execute) {
  const api = choices.apis.find(api => api.api_id === selectedId && api.executable)
  assert(api, 'The selected API is absent or no longer executable in this discovery response.')
  const overrides = JSON.parse(process.env.APIOSK_INPUT_JSON || '{}')
  assert(overrides && typeof overrides === 'object' && !Array.isArray(overrides), 'APIOSK_INPUT_JSON must be an object.')
  const input = { ...api.input_values, ...overrides }
  assert(api.missing_inputs.every(field => Object.hasOwn(input, field)), 'Fill the missing fields through APIOSK_INPUT_JSON.')
  const body = { api_id: api.api_id, capability: api.capability, input, max_spend: cap, wait_seconds: 25 }
  let saved = await call('POST', '/v2/execute', body, retryKey)
  assert(saved.id, 'Execution did not return a durable run ID.')
  const id = saved.id
  const deadline = Date.now() + 60_000
  while (saved.status === 'running' || (saved.status === 'succeeded' && saved.result_present === false)) {
    assert(Date.now() < deadline, `Run ${id} remains pending. Recover with GET /v2/runs/${id}; do not start a new purchase.`)
    await new Promise(resolve => setTimeout(resolve, 2000))
    saved = await call('GET', `/v2/runs/${encodeURIComponent(id)}`)
  }
  assert(['completed', 'succeeded'].includes(saved.status), `Run ${id}: ${saved.status}; inspect the saved result and billing.`)
  const output = value => value.status === 'succeeded' ? value.result : value.results
  assert(saved.status === 'succeeded' ? saved.result_present === true : saved.results?.length, 'No source output was returned.')
  const billing = value => value.billing || value.usage
  const replay = await call('POST', '/v2/execute', body, retryKey)
  assert.equal(replay.id, id)
  assert.deepEqual(output(replay), output(saved))
  assert.deepEqual(billing(replay), billing(saved))
  console.log(`PASS selected execution: ${id}; saved output and billing unchanged on replay.`)
  console.log('Check the production ledger for this run to confirm the single charge. This script does not inspect the database.')
}
