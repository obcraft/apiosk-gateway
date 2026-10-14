#!/usr/bin/env node
// Cross-repository release gate. Unlike `cargo test`, this command requires the
// actual published app prompts and fails if the app checkout is unavailable.
import { readFileSync, writeFileSync } from 'node:fs'
import { randomUUID } from 'node:crypto'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'
import { spawnSync } from 'node:child_process'
import assert from 'node:assert/strict'
import { assessPreview, validateCases } from './planning-release-validation.mjs'

const gateway = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const args = process.argv.slice(2)
let appSource = resolve(gateway, '../app/src/lib/ui/ask-examples.ts')
let checkOnly = false
let live = false
let reportPath
let casesPath = resolve(gateway, 'tests/fixtures/planning-release-cases.json')
for (let index = 0; index < args.length; index += 1) {
  if (args[index] === '--app-source' && args[index + 1]) appSource = resolve(args[++index])
  else if (args[index] === '--check-only') checkOnly = true
  else if (args[index] === '--live') live = true
  else if (args[index] === '--report' && args[index + 1]) reportPath = resolve(args[++index])
  else if (args[index] === '--cases' && args[index + 1]) casesPath = resolve(args[++index])
  else throw new Error(`Unknown argument ${args[index]}; use --app-source PATH, --cases PATH, --check-only, --live or --report PATH`)
}

// The UI publishes literal one-line strings. Reject unsupported syntax instead
// of evaluating arbitrary TS or silently skipping questions in the app file.
const source = readFileSync(appSource, 'utf8')
const declarations = source.match(/^\s*prompt\s*:/gm) ?? []
const prompts = [...source.matchAll(/^\s*prompt\s*:\s*'((?:\\.|[^'\\])*)',\s*$/gm)].map((match) => {
  assert(!match[1].includes('\\'), 'Review the starter gate string parser before adding escaped prompt literals')
  return match[1]
})
assert.equal(prompts.length, declarations.length, 'Every published prompt must be a readable literal')
assert(prompts.length > 0, 'Publish only examples that pass planning; do not pad the list to a fixed count')
// Published starters retain the requested two-source minimum. An arbitrary
// question may explicitly declare a different minimum in its acceptance case.
const extraCases = validateCases(JSON.parse(readFileSync(casesPath, 'utf8')))
const cases = validateCases(prompts.map((question, index) => {
  const declared = extraCases.find(item => item.question === question)
  return { ...declared, id: declared?.id ?? `homepage-${index + 1}`, question, expected_status: 'ready',
    minimum_sources: Math.max(2, declared?.minimum_sources ?? 2) }
}).concat(extraCases.filter(item => !prompts.includes(item.question))))
console.log(`SYNTAX CHECK: read ${prompts.length} app questions and ${cases.length} acceptance cases. This does not verify planning.`)

// Exercise the deployed interpretation, discovery and final coverage gate.
// Preview is ephemeral: never approve, persist its token or buy source data.
if (live) {
  const report = { checked_at: new Date().toISOString(), scope: 'live_preview_acceptance', endpoint: 'https://apiosk-gateway-v2.fly.dev/v2/ask-v2/guest/preview', results: [] }
  for (const item of cases) {
    const { question } = item
    const started = performance.now()
    let result
    try {
      const response = await fetch(report.endpoint, {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ request_id: randomUUID(), question }), signal: AbortSignal.timeout(100_000),
      })
      const body = await response.json()
      const assessment = assessPreview(item, body, { httpOk: response.ok })
      result = { id: item.id, question, expected_status: item.expected_status, minimum_sources: item.minimum_sources,
        ...assessment, elapsed_ms: Math.round(performance.now() - started), http_status: response.status, status: body.status, total_atomic: body.total_atomic,
        sources: body.sources?.map(source => ({ name: source.name, source_slug: source.source_slug, endpoint_id: source.endpoint_id })), message: body.message ?? body.error }
    } catch (error) {
      result = { id: item.id, question, accepted: false, elapsed_ms: Math.round(performance.now() - started), error: error.message }
    }
    report.results.push(result)
    console.log(`${result.accepted ? 'PASS' : 'FAIL'}: ${question}${result.issues?.length ? ` (${result.issues.join(', ')})` : ''}`)
    if (reportPath) writeFileSync(reportPath, `${JSON.stringify(report, null, 2)}\n`)
  }
  assert(report.results.every(result => result.accepted), 'Every acceptance case must pass before publishing. Repair the planner; do not narrow a question to conceal lost requirements. See live planning report.')
}

if (!checkOnly) {
  const result = spawnSync('cargo', ['test', '--lib', '--offline', 'homepage_starter_tests', '--', '--test-threads=4'],
    { cwd: gateway, stdio: 'inherit' })
  if (result.error) throw result.error
  process.exitCode = result.status ?? 1
}
if (!live) console.log('NOT A RELEASE ACCEPTANCE: no current model, catalog or deployed planner was verified. Run --live before publication.')
