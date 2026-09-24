import { test } from 'node:test'
import assert from 'node:assert/strict'
import fs from 'node:fs/promises'
import { budgetGuard, compareBudgets, costReport, rowsAsLines, summarize, telemetryOf, traces } from './summarize-telemetry.mjs'

const read = async path => JSON.parse(await fs.readFile(new URL(`../${path}`, import.meta.url), 'utf8'))
const dictionary = await read('perf/phases.json')
const committedBudgets = await read('perf/budgets.json')

let second = 0
const line = payload => ({ timestamp: `2026-09-25T01:00:${String(second++).padStart(2, '0')}Z`, level: 'INFO', target: 'apiosk_telemetry',
  fields: { message: 'telemetry', telemetry: JSON.stringify({ schema: 1, surface: 'app', ...payload }) } })

// A: answered simple lookup. B: failed before a price. C: a reload sent two beacons.
const fixture = [
  line({ kind: 'milestone', trace: 'a', milestone: 'price_ready', query_class: 'simple_lookup', sources: 'one', catalog_cache: 'hit', cap_micro_usd: 138 }),
  line({ kind: 'request', trace: 'a', phase: 'gw.request', ms: 40, ok: true, db_calls: 3, db_ms: 12, bytes_in: 200, bytes_out: 900 }),
  line({ kind: 'span', trace: 'a', phase: 'gw.model', ms: 800, ok: true }),
  line({ kind: 'span', trace: 'a', phase: 'gw.edge_hop', ms: 900, provider_ms: 700, ok: true }),
  line({ kind: 'span', trace: 'a', phase: 'gw.cooldown_wait', ms: 300, ok: true }),
  line({ kind: 'cost', trace: 'a', segment: 'question', model_calls: 1, prompt_tokens: 1200, cost_nanousd: null, cost_complete: false }),
  line({ kind: 'cost', trace: 'a', segment: 'execution', model_calls: 1, prompt_tokens: 900, cost_nanousd: 210000, cost_complete: true,
    source_calls: 2, customer_charged_micro_usd: 138, provider_micro_usd: 120, fee_micro_usd: 18, db_calls: 9 }),
  line({ kind: 'milestone', trace: 'a', milestone: 'answer_complete', answer_status: 'completed', fully_answered: true }),
  line({ kind: 'client', trace: 'a', device: 'desktop', outcome: 'answered',
    phases: { find_to_price: 2000, approval_wait: 50000, approve_to_answer: 6000, 'ui.answer_render': 20 }, counts: {} }),
  line({ kind: 'span', trace: 'b', phase: 'gw.model', ms: 25000, ok: false, error_class: 'upstream_5xx' }),
  line({ kind: 'cost', trace: 'b', segment: 'question', model_calls: 1, model_retries: 1, prompt_tokens: null, cost_nanousd: null, cost_complete: false }),
  line({ kind: 'milestone', trace: 'b', milestone: 'terminal', status: 'failed' }),
  line({ kind: 'milestone', trace: 'c', milestone: 'price_ready', query_class: 'comparison', sources: 'multiple', catalog_cache: 'refresh_wait' }),
  line({ kind: 'client', trace: 'c', device: 'mobile', outcome: 'abandoned', phases: { find_to_price: 9000 }, counts: {} }),
  line({ kind: 'client', trace: 'c', device: 'mobile', outcome: 'answered', phases: { find_to_price: 9000, approve_to_answer: 4000 }, counts: { reloads: 1 } }),
  { fields: { message: 'model stage completed', surface: 'chat', model: 'fixture', success: true, model_ms: 800 } },
]
const logs = fixture

test('joins browser and server lines per trace and keeps approval wait out of system time', () => {
  const summary = summarize(logs, { dictionary, budgets: committedBudgets })
  assert.equal(summary.traces, 3)
  const row = (phase, queryClass = 'simple_lookup') => summary.phases.find(item => item.phase === phase && item.query_class === queryClass)
  assert.deepEqual([row('find_to_price').cache, row('find_to_price').device, row('find_to_price').sources], ['warm', 'desktop', 'one'])
  assert.equal(row('find_to_answer_system').p50_ms, 8000, 'find to price plus approve to answer, not the 50 s approval wait')
  assert.equal(row('approval_wait').p50_ms, 50000)
  assert.equal(row('provider_wait').p50_ms, 1000, 'provider time plus cooldown')
  assert.equal(row('approve_to_answer', 'comparison').p50_ms, 4000, 'the last beacon of a reloaded trace wins')
  assert.equal(row('find_to_price', 'comparison').cache, 'cold')
  const failed = summary.phases.filter(item => item.query_class === 'unclassified')
  assert.equal(failed.find(item => item.phase === 'gw.model').error_rate, 1)
  assert.equal(failed.find(item => item.phase === 'find_to_price').errors, 1, 'a question that failed before a price is an error of that journey')
  assert.equal(summary.legacy.models['chat/fixture'].count, 1, 'the legacy latency summary still reads the same logs')
})

test('cost per fully answered request counts failed traces and keeps unknown cost null', () => {
  const report = costReport(traces(logs.map(telemetryOf).filter(Boolean)), null)
  assert.equal(report.fully_answered, 1)
  assert.equal(report.failed, 1)
  assert.equal(report.totals.model_calls, 3, 'the failed trace model call is in the numerator')
  assert.equal(report.totals.prompt_tokens, 2100)
  assert.equal(report.totals.cost_complete, false)
  assert.equal(report.per_fully_answered_request.model_cost_usd, 0.00021)
  assert.equal(report.per_fully_answered_request.customer_charged_usd, 0.000138)
  assert.equal(report.per_fully_answered_request.provider_usd, 0.00012)
  assert.equal(report.per_fully_answered_request.fee_usd, 0.000018)
  assert.equal(report.per_fully_answered_request.db_calls, 12)
  assert.equal(report.failed_traces_cost.prompt_tokens, null, 'a failed trace without token usage is unmeasured, not 0')
  assert.equal(report.failed_traces_cost.cost_nanousd, null)
})

test('persisted rows can replace log lines as the cost source', () => {
  const rows = [{ trace_id: 'a', segment: 'execution', fully_answered: true, model_cost_nanousd: 5, model_cost_complete: true, cached_prompt_tokens: 7, customer_charged_micro_usd: 138 }]
  assert.deepEqual(rowsAsLines(rows)[0], { kind: 'cost', trace: 'a', segment: 'execution', trace_id: 'a', fully_answered: true, cost_nanousd: 5, cost_complete: true, cached_tokens: 7, customer_charged_micro_usd: 138 })
  const report = costReport(traces(logs.map(telemetryOf).filter(Boolean)), rows)
  assert.equal(report.source, 'request_costs rows')
  assert.equal(report.totals.cost_nanousd, 5)
})

test('a budget without a number is pending, and a number needs enough samples', () => {
  const allTraces = traces(logs.map(telemetryOf).filter(Boolean))
  const pending = compareBudgets(allTraces, dictionary, committedBudgets)
  assert.ok(pending.length > 0)
  assert.ok(pending.every(result => result.status === 'pending'))
  const withTarget = (p95, minSamples) => ({ ...committedBudgets, min_samples: minSamples,
    budgets: { simple_lookup: { find_to_price: { warm: { p50_ms: null, p95_ms: p95 }, cold: { p50_ms: null, p95_ms: null }, max_error_rate: null } } }, overrides: [] })
  const status = budgets => compareBudgets(allTraces, dictionary, budgets).find(result => result.cache === 'warm').status
  assert.equal(status(withTarget(1000, 30)), 'insufficient_samples')
  assert.equal(status(withTarget(1000, 1)), 'breach')
  assert.equal(status(withTarget(3000, 1)), 'ok')
})

test('no budget carries a number without a committed production baseline', async () => {
  const committed = await budgetGuard(committedBudgets, path => read(path))
  assert.deepEqual(committed, [], 'perf/budgets.json')
  const numbered = { ...committedBudgets, budgets: { simple_lookup: { find_to_price: { warm: { p50_ms: 1500, p95_ms: null } } } } }
  assert.equal((await budgetGuard(numbered, async () => null)).length, 1, 'no baseline_ref')
  const missing = { ...numbered, status: 'baselined', baseline_ref: 'perf/baselines/2026-10-01.json' }
  assert.match((await budgetGuard(missing, async () => { throw new Error('ENOENT') }))[0], /not committed/)
  assert.match((await budgetGuard(missing, async () => ({ sample_count: 3 })))[0], /fewer than min_samples/)
  assert.deepEqual(await budgetGuard(missing, async () => ({ sample_count: 120 })), [])
  assert.equal((await budgetGuard({ ...missing, status: 'pending_baseline' }, async () => ({ sample_count: 120 }))).length, 1)
})
