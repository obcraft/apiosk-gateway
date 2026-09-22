import fs from 'node:fs/promises'
import { pathToFileURL } from 'node:url'

const stats = values => {
  const sorted = values.filter(Number.isFinite).sort((a, b) => a - b)
  const percentile = p => sorted.length ? sorted[Math.max(0, Math.ceil(p * sorted.length) - 1)] : null
  return { count: sorted.length, p50_ms: percentile(0.5), p95_ms: percentile(0.95) }
}

/** Correlate quote-scoped milestones; incomplete log windows stay unmeasured. */
export function summarize(rows) {
  const stages = { question_to_price: [], approval_to_sources: [], sources_to_answer: [] }
  const quotes = new Map(), models = new Map(), providers = new Map()
  let incomplete = 0, failedAnswers = 0, unpriced = 0
  for (const row of rows) {
    const f = row.fields ?? row
    if (f.stage === 'question_to_price') {
      if (f.price_ready === true) stages.question_to_price.push(f.stage_ms)
      else unpriced++
    }
    if (f.stage === 'sources_to_answer') {
      if (f.success === true) stages.sources_to_answer.push(f.stage_ms)
      else failedAnswers++
    }
    if (f.quote && f.milestone) {
      const key = `${f.task}/${f.quote}`
      const sample = quotes.get(key) ?? { approved: [], sources: [] }
      const timestamp = Date.parse(row.timestamp)
      if (Number.isFinite(timestamp)) {
        if (f.milestone === 'approved') sample.approved.push(timestamp)
        if (f.milestone === 'sources_ready' && f.complete === true) sample.sources.push(timestamp)
      }
      quotes.set(key, sample)
    }
    if (f.message === 'model stage completed') {
      const key = `${f.surface}/${f.model}`
      const sample = models.get(key) ?? { validated: [], failed: 0 }
      if (f.success === true) sample.validated.push(f.model_ms)
      else sample.failed++
      models.set(key, sample)
    }
    if (f.message === 'model response validated' && f.provider) {
      const key = `${f.surface}/${f.model}/${f.provider}`
      const sample = providers.get(key) ?? { validated: [], needed_retry: 0 }
      sample.validated.push(f.model_ms)
      if (f.attempt > 1) sample.needed_retry++
      providers.set(key, sample)
    }
  }
  for (const sample of quotes.values()) {
    const approved = Math.min(...sample.approved)
    const ready = Math.min(...sample.sources.filter(time => time >= approved))
    if (Number.isFinite(approved) && Number.isFinite(ready)) stages.approval_to_sources.push(ready - approved)
    else incomplete++
  }
  return {
    stages: Object.fromEntries(Object.entries(stages).map(([key, values]) => [key, stats(values)])),
    models: Object.fromEntries([...models].map(([key, value]) => [key, { ...stats(value.validated), failed: value.failed }])),
    validated_providers: Object.fromEntries([...providers].map(([key, value]) => [key, { ...stats(value.validated), needed_retry: value.needed_retry }])),
    excluded: { incomplete_quote_windows: incomplete, failed_or_partial_answers: failedAnswers, questions_without_price: unpriced },
    definition: 'Server timings. Approval to sources includes worker scheduling and saved-source reads; sources to answer includes validation, persistence and response assembly. No browser/network delivery time. Model success means contract validation, not an independent semantic quality score.',
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (!process.argv[2] || process.argv[2] === '--help') {
    console.log('Usage: node scripts/summarize-latency.mjs <Gateway JSONL log file>\nReads local logs only; does not send questions, call models or purchase data.')
  } else {
    const lines = (await fs.readFile(process.argv[2], 'utf8')).split('\n').filter(line => line.trim())
    const rows = lines.map(line => JSON.parse(line))
    console.log(JSON.stringify(summarize(rows), null, 2))
  }
}
