// Summarizes gateway and browser telemetry (target apiosk_telemetry) into
// p50/p95 and error rates per query class, phase, cache state, device and
// source count; compares them with perf/budgets.json; and reports cost per
// fully answered request with model, provider, fee, customer and
// infrastructure costs kept apart. Reads local log files only.
//
//   node scripts/summarize-telemetry.mjs <log.jsonl...> [--costs rows.json]
//     [--budgets perf/budgets.json] [--enforce] [--markdown]
//
// --costs reads exported gateway_v2.request_costs rows (a JSON array) for
// cost instead of log lines. --enforce exits 1 when a budget with enough
// samples is breached. Missing values stay null: they are never 0.
import fs from 'node:fs/promises'
import { pathToFileURL } from 'node:url'
import { summarize as summarizeLegacy } from './summarize-latency.mjs'

const root = new URL('../', import.meta.url)
const UNKNOWN = 'unknown'
const COLD = new Set(['refresh_wait', 'first_load', 'stale_served'])
const FAILED = new Set(['failed', 'unsupported'])

export const percentile = (values, p) => {
  const sorted = values.filter(Number.isFinite).sort((a, b) => a - b)
  return sorted.length ? sorted[Math.max(0, Math.ceil(p * sorted.length) - 1)] : null
}

/** The telemetry payload of one JSON log row, with its timestamp. */
export function telemetryOf(row) {
  if (row?.target !== 'apiosk_telemetry' || typeof row.fields?.telemetry !== 'string') return null
  try { return { ...JSON.parse(row.fields.telemetry), timestamp: row.timestamp ?? null } } catch { return null }
}

export function parseLines(text) {
  return text.split('\n').filter(line => line.trim()).flatMap(line => {
    try { return [JSON.parse(line)] } catch { return [] }
  })
}

/** Every line of one trace, and the dimensions its budgets are keyed by. */
export function traces(lines) {
  const byTrace = new Map()
  for (const line of lines) {
    if (!line.trace) continue
    const trace = byTrace.get(line.trace) ?? { id: line.trace, lines: [] }
    trace.lines.push(line)
    byTrace.set(line.trace, trace)
  }
  for (const trace of byTrace.values()) {
    const milestone = name => trace.lines.filter(line => line.kind === 'milestone' && line.milestone === name).at(-1)
    const price = milestone('price_ready')
    // A reload continues a trace; its last beacon supersedes an earlier one.
    const client = trace.lines.filter(line => line.kind === 'client').sort((a, b) => String(a.timestamp).localeCompare(String(b.timestamp))).at(-1)
    const answer = milestone('answer_complete')
    const terminal = milestone('terminal')
    trace.query_class = price?.query_class ?? 'unclassified'
    trace.sources = price?.sources ?? UNKNOWN
    trace.cache = price?.catalog_cache ? (COLD.has(price.catalog_cache) ? 'cold' : 'warm') : UNKNOWN
    trace.device = client?.device ?? UNKNOWN
    trace.client = client ?? null
    trace.cap_micro_usd = price?.cap_micro_usd ?? null
    trace.fully_answered = answer?.fully_answered === true
    trace.failed = FAILED.has(terminal?.status) || client?.outcome === 'failed' || answer?.answer_status === 'unavailable'
  }
  return [...byTrace.values()]
}

/** Samples per phase for one trace: browser journeys, server spans, provider wait. */
export function samples(trace, dictionary) {
  const out = []
  const add = (phase, ms, ok = true) => { if (Number.isFinite(ms)) out.push({ phase, ms, ok }) }
  const measured = trace.client?.phases ?? {}
  for (const name of [...Object.keys(dictionary.journey_phases), ...Object.keys(dictionary.client_phases)]) add(name, measured[name])
  if (Number.isFinite(measured.find_to_price) && Number.isFinite(measured.approve_to_answer)) {
    add('find_to_answer_system', measured.find_to_price + measured.approve_to_answer)
  }
  let providerWait = null
  for (const line of trace.lines) {
    if (line.kind === 'span' || line.kind === 'request') add(line.phase, line.ms, line.ok !== false)
    if (line.phase === 'gw.edge_hop' && Number.isFinite(line.provider_ms)) providerWait = (providerWait ?? 0) + line.provider_ms
    if (line.phase === 'gw.cooldown_wait' && Number.isFinite(line.ms)) providerWait = (providerWait ?? 0) + line.ms
  }
  add('provider_wait', providerWait)
  return out
}

function group(entries) {
  const count = entries.filter(entry => entry.ok).length
  const errors = entries.length - count
  const values = entries.filter(entry => entry.ok).map(entry => entry.ms)
  return { count, errors, error_rate: entries.length ? errors / entries.length : null, p50_ms: percentile(values, 0.5), p95_ms: percentile(values, 0.95) }
}

/** p50/p95 and errors per (query class, phase, cache, device, sources). */
export function phaseTable(allTraces, dictionary) {
  const rows = new Map()
  for (const trace of allTraces) {
    const traceSamples = samples(trace, dictionary)
    // A failed question that never reached a browser journey phase still
    // counts against that phase's error rate.
    if (trace.failed) {
      for (const phase of ['find_to_price', 'approve_to_answer']) {
        if (!traceSamples.some(sample => sample.phase === phase)) traceSamples.push({ phase, ms: null, ok: false })
      }
    }
    for (const sample of traceSamples) {
      const key = [trace.query_class, sample.phase, trace.cache, trace.device, trace.sources].join('|')
      const row = rows.get(key) ?? { query_class: trace.query_class, phase: sample.phase, cache: trace.cache, device: trace.device, sources: trace.sources, entries: [] }
      row.entries.push(sample)
      rows.set(key, row)
    }
  }
  return [...rows.values()].map(({ entries, ...row }) => ({ ...row, ...group(entries) }))
    .sort((a, b) => [a.query_class, a.phase, a.cache, a.device, a.sources].join('|').localeCompare([b.query_class, b.phase, b.cache, b.device, b.sources].join('|')))
}

/** pending (no target), insufficient_samples, ok or breach, per budget. */
export function compareBudgets(allTraces, dictionary, budgets) {
  const collect = (predicate, phase) => {
    const entries = []
    for (const trace of allTraces.filter(predicate)) {
      const traceSamples = samples(trace, dictionary).filter(sample => sample.phase === phase)
      if (!traceSamples.length && trace.failed && ['find_to_price', 'approve_to_answer'].includes(phase)) traceSamples.push({ phase, ms: null, ok: false })
      entries.push(...traceSamples)
    }
    return group(entries)
  }
  const judge = (measured, target, maxErrorRate) => {
    const targets = [target?.p50_ms, target?.p95_ms, maxErrorRate]
    if (targets.every(value => value === null || value === undefined)) return 'pending'
    if (measured.count < budgets.min_samples) return 'insufficient_samples'
    const breach = (Number.isFinite(target?.p50_ms) && measured.p50_ms > target.p50_ms)
      || (Number.isFinite(target?.p95_ms) && measured.p95_ms > target.p95_ms)
      || (Number.isFinite(maxErrorRate) && measured.error_rate > maxErrorRate)
    return breach ? 'breach' : 'ok'
  }
  const results = []
  for (const [queryClass, phases] of Object.entries(budgets.budgets)) {
    for (const [phase, budget] of Object.entries(phases)) {
      for (const cache of budgets.dimensions.cache) {
        const measured = collect(trace => trace.query_class === queryClass && trace.cache === cache, phase)
        results.push({ query_class: queryClass, phase, cache, target: budget[cache], max_error_rate: budget.max_error_rate, measured, status: judge(measured, budget[cache], budget.max_error_rate) })
      }
    }
  }
  for (const override of budgets.overrides) {
    const measured = collect(trace => trace.query_class === override.query_class && trace.device === override.device && trace.sources === override.sources, override.phase)
    const target = { p50_ms: override.p50_ms, p95_ms: override.p95_ms }
    results.push({ ...override, target, measured, status: judge(measured, target, null) })
  }
  return results
}

const COST_KEYS = ['model_calls', 'model_attempts', 'model_retries', 'model_failures', 'prompt_tokens', 'completion_tokens', 'cached_tokens',
  'reasoning_tokens', 'cost_nanousd', 'source_calls', 'source_retries', 'source_refunds', 'source_unresolved', 'refunded_micro_usd',
  'provider_micro_usd', 'fee_micro_usd', 'customer_charged_micro_usd', 'db_calls', 'db_ms', 'edge_bytes', 'sse_bytes', 'bytes_in', 'bytes_out']
const COMPLETE_KEYS = ['tokens_complete', 'cost_complete', 'charged_complete', 'provider_complete', 'fee_complete']
const ROW_KEYS = { cached_prompt_tokens: 'cached_tokens', model_cost_nanousd: 'cost_nanousd', model_cost_complete: 'cost_complete' }

/** One trace's costs: a sum per field, null when no line measured it. */
export function traceCost(lines) {
  const cost = Object.fromEntries(COST_KEYS.map(key => [key, null]))
  const complete = Object.fromEntries(COMPLETE_KEYS.map(key => [key, true]))
  for (const line of lines) {
    for (const key of COST_KEYS) if (Number.isFinite(line[key])) cost[key] = (cost[key] ?? 0) + line[key]
    for (const key of COMPLETE_KEYS) if (line[key] === false) complete[key] = false
  }
  return { ...cost, ...complete }
}

/** Log lines equivalent to exported request_costs rows, for the cost report. */
export function rowsAsLines(rows) {
  return rows.map(row => {
    const line = { kind: 'cost', trace: row.trace_id, segment: row.segment }
    for (const [key, value] of Object.entries(row)) line[ROW_KEYS[key] ?? key] = value
    return line
  })
}

/** Totals over every trace, failed ones included, per fully answered request. */
export function costReport(allTraces, rows) {
  const costLines = rows ? rowsAsLines(rows) : null
  const perTrace = allTraces.map(trace => {
    const lines = costLines ? costLines.filter(line => line.trace === trace.id) : trace.lines.filter(line => line.kind === 'cost' || line.kind === 'request')
    const fully = costLines ? costLines.some(line => line.trace === trace.id && line.fully_answered === true) || trace.fully_answered : trace.fully_answered
    return { trace: trace.id, failed: trace.failed, fully_answered: fully, cap_micro_usd: trace.cap_micro_usd, ...traceCost(lines) }
  })
  const total = traceCost(perTrace)
  const answered = perTrace.filter(trace => trace.fully_answered).length
  const failed = perTrace.filter(trace => trace.failed)
  const per = value => (value === null || answered === 0 ? null : value / answered)
  return {
    source: rows ? 'request_costs rows' : 'log lines',
    traces: perTrace.length,
    fully_answered: answered,
    failed: failed.length,
    failure_rate: perTrace.length ? failed.length / perTrace.length : null,
    totals: total,
    per_fully_answered_request: {
      model_cost_usd: per(total.cost_nanousd === null ? null : total.cost_nanousd / 1e9),
      model_cost_complete: total.cost_complete,
      provider_usd: per(total.provider_micro_usd === null ? null : total.provider_micro_usd / 1e6),
      fee_usd: per(total.fee_micro_usd === null ? null : total.fee_micro_usd / 1e6),
      customer_charged_usd: per(total.customer_charged_micro_usd === null ? null : total.customer_charged_micro_usd / 1e6),
      refunded_usd: per(total.refunded_micro_usd === null ? null : total.refunded_micro_usd / 1e6),
      prompt_tokens: per(total.prompt_tokens), cached_tokens: per(total.cached_tokens),
      model_retries: per(total.model_retries), source_calls: per(total.source_calls), source_retries: per(total.source_retries),
      db_calls: per(total.db_calls), bytes_out: per(total.bytes_out), sse_bytes: per(total.sse_bytes), edge_bytes: per(total.edge_bytes),
    },
    failed_traces_cost: traceCost(failed),
  }
}

/**
 * A budget may carry a number only when a committed production baseline
 * backs it: `baseline_ref` names perf/baselines/<date>.json and that file
 * holds at least `min_samples` samples. Returns the problems found.
 */
export async function budgetGuard(budgets, readBaseline) {
  const numbers = []
  const visit = (value, path) => {
    if (typeof value === 'number') numbers.push(path)
    else if (value && typeof value === 'object') for (const [key, inner] of Object.entries(value)) visit(inner, `${path}.${key}`)
  }
  visit(budgets.budgets, 'budgets')
  for (const [index, override] of budgets.overrides.entries()) {
    for (const key of ['p50_ms', 'p95_ms']) if (typeof override[key] === 'number') numbers.push(`overrides.${index}.${key}`)
  }
  if (!numbers.length) return []
  const problems = []
  if (!/^perf\/baselines\/\d{4}-\d{2}-\d{2}\.json$/.test(budgets.baseline_ref ?? '')) {
    return [`${numbers.join(', ')} set without a baseline_ref of the form perf/baselines/<date>.json`]
  }
  const baseline = await readBaseline(budgets.baseline_ref).catch(() => null)
  if (!baseline) problems.push(`${budgets.baseline_ref} is not committed`)
  else if (!(baseline.sample_count >= budgets.min_samples)) problems.push(`${budgets.baseline_ref} has fewer than min_samples (${budgets.min_samples}) samples`)
  if (budgets.status !== 'baselined') problems.push('a budget with numbers must have status "baselined"')
  return problems
}

export function summarize(logRows, { dictionary, budgets, rows = null }) {
  const lines = logRows.map(telemetryOf).filter(Boolean)
  const allTraces = traces(lines)
  const table = phaseTable(allTraces, dictionary)
  const comparisons = compareBudgets(allTraces, dictionary, budgets)
  return {
    traces: allTraces.length,
    phases: table,
    budgets: { status: budgets.status, baseline_ref: budgets.baseline_ref, min_samples: budgets.min_samples, results: comparisons },
    costs: costReport(allTraces, rows),
    legacy: summarizeLegacy(logRows),
    definition: 'Percentiles are nearest-rank over successful samples; error_rate counts failed samples (and failed questions that never reached a browser journey phase). approval_wait is human time and provider_wait is provider time: neither is app-budgeted. cache is cold when the catalog had to load (refresh_wait, first_load, stale_served). Cost per fully answered request divides the cost of every trace, failed ones included, by the number of fully answered requests. Null means unmeasured, never 0.',
  }
}

function markdown(summary) {
  const cell = value => value === null || value === undefined ? '-' : typeof value === 'number' && !Number.isInteger(value) ? value.toFixed(3) : String(value)
  const lines = ['# Telemetry summary', '', `Traces: ${summary.traces}. Budgets: ${summary.budgets.status}.`, '',
    '| class | phase | cache | device | sources | n | errors | error rate | p50 ms | p95 ms |', '|---|---|---|---|---|---|---|---|---|---|']
  for (const row of summary.phases) lines.push(`| ${[row.query_class, row.phase, row.cache, row.device, row.sources, row.count, row.errors, row.error_rate, row.p50_ms, row.p95_ms].map(cell).join(' | ')} |`)
  lines.push('', '| budget | cache | status | n | p50 ms | p95 ms |', '|---|---|---|---|---|---|')
  for (const result of summary.budgets.results) lines.push(`| ${[`${result.query_class} ${result.phase}${result.device ? ` ${result.device}/${result.sources}` : ''}`, result.cache ?? '-', result.status, result.measured.count, result.measured.p50_ms, result.measured.p95_ms].map(cell).join(' | ')} |`)
  lines.push('', '## Cost per fully answered request', '', `Source: ${summary.costs.source}. Traces ${summary.costs.traces}, fully answered ${summary.costs.fully_answered}, failed ${summary.costs.failed}.`, '')
  for (const [key, value] of Object.entries(summary.costs.per_fully_answered_request)) lines.push(`- ${key}: ${cell(value)}`)
  return lines.join('\n')
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2)
  const option = name => { const index = args.indexOf(name); return index >= 0 ? args.splice(index, 2)[1] : null }
  const flag = name => { const index = args.indexOf(name); return index >= 0 && Boolean(args.splice(index, 1)) }
  const costs = option('--costs'), budgetsPath = option('--budgets')
  const enforce = flag('--enforce'), asMarkdown = flag('--markdown')
  if (!args.length || args.includes('--help')) {
    console.log('Usage: node scripts/summarize-telemetry.mjs <log.jsonl...> [--costs rows.json] [--budgets perf/budgets.json] [--enforce] [--markdown]\nReads local files only; sends nothing and calls no model or provider.')
  } else {
    const read = async path => JSON.parse(await fs.readFile(path, 'utf8'))
    const dictionary = await read(new URL('perf/phases.json', root))
    const budgets = await read(budgetsPath ?? new URL('perf/budgets.json', root))
    const logRows = (await Promise.all(args.map(path => fs.readFile(path, 'utf8')))).flatMap(parseLines)
    const summary = summarize(logRows, { dictionary, budgets, rows: costs ? await read(costs) : null })
    console.log(asMarkdown ? markdown(summary) : JSON.stringify(summary, null, 2))
    if (enforce && summary.budgets.results.some(result => result.status === 'breach')) process.exitCode = 1
  }
}
