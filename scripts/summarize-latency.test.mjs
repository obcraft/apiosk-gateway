import { test } from 'node:test'
import assert from 'node:assert/strict'
import { summarize } from './summarize-latency.mjs'

test('pairs only complete quote windows and never reports missing timings as zero', () => {
  const milestone = (quote, name, second) => ({ timestamp: `2026-09-20T12:00:0${second}Z`, fields: { task: 'task', quote, milestone: name, complete: true } })
  const result = summarize([
    milestone('first', 'sources_ready', 5), milestone('first', 'approved', 1),
    milestone('first', 'approved', 2), milestone('first', 'sources_ready', 6),
    milestone('missing', 'sources_ready', 8),
    { fields: { stage: 'question_to_price', price_ready: true, stage_ms: 100 } },
    { fields: { stage: 'question_to_price', price_ready: false, stage_ms: 200 } },
    { fields: { stage: 'sources_to_answer', success: false, stage_ms: 65000 } },
    { fields: { message: 'model stage completed', surface: 'chat', model: 'fixture', success: true, model_ms: 90 } },
    { fields: { message: 'model stage completed', surface: 'chat', model: 'fixture', success: false, model_ms: 25000 } },
  ])
  assert.deepEqual(result.stages.approval_to_sources, { count: 1, p50_ms: 4000, p95_ms: 4000 })
  assert.deepEqual(result.stages.sources_to_answer, { count: 0, p50_ms: null, p95_ms: null })
  assert.equal(result.stages.question_to_price.count, 1)
  assert.equal(result.models['chat/fixture'].failed, 1)
  assert.deepEqual(result.excluded, { incomplete_quote_windows: 1, failed_or_partial_answers: 1, questions_without_price: 1 })
})
