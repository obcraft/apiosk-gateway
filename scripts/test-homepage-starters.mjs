#!/usr/bin/env node
// Cross-repository release gate. Unlike `cargo test`, this command requires the
// actual published app prompts and fails if the app checkout is unavailable.
import { readFileSync, writeFileSync } from 'node:fs'
import { randomUUID } from 'node:crypto'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'
import { spawnSync } from 'node:child_process'
import assert from 'node:assert/strict'

const gateway = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const args = process.argv.slice(2)
let appSource = resolve(gateway, '../app/src/lib/ui/ask-examples.ts')
let checkOnly = false
let live = false
let reportPath
for (let index = 0; index < args.length; index += 1) {
  if (args[index] === '--app-source' && args[index + 1]) appSource = resolve(args[++index])
  else if (args[index] === '--check-only') checkOnly = true
  else if (args[index] === '--live') live = true
  else if (args[index] === '--report' && args[index + 1]) reportPath = resolve(args[++index])
  else throw new Error(`Unknown argument ${args[index]}; use --app-source PATH, --check-only, --live or --report PATH`)
}

const manifest = JSON.parse(readFileSync(resolve(gateway, 'tests/fixtures/homepage-starters.json'), 'utf8'))
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
assert.equal(new Set(manifest.map((entry) => entry.id)).size, manifest.length, 'Every acceptance case needs a distinct id')
assert.deepEqual(prompts, manifest.map((entry) => entry.question),
  'Published starter questions drifted from the gateway acceptance cases; update and verify both before release')
console.log(`All ${prompts.length} published app prompts exactly match the gateway acceptance cases.`)

// Exercise the deployed interpretation, discovery and final coverage gate.
// Preview is ephemeral: never approve, persist its token or buy source data.
if (live) {
  const report = { checked_at: new Date().toISOString(), endpoint: 'https://apiosk-gateway-v2.fly.dev/v2/ask-v2/guest/preview', results: [] }
  for (const question of prompts) {
    let result
    try {
      const response = await fetch(report.endpoint, {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ request_id: randomUUID(), question }), signal: AbortSignal.timeout(100_000),
      })
      const body = await response.json()
      const ready = response.ok && body.status === 'ready' && typeof body.preview_token === 'string'
        && body.preview_token.length > 0 && Number(body.total_atomic) > 0 && body.sources?.length > 0
      result = { question, ready: Boolean(ready), http_status: response.status, status: body.status, total_atomic: body.total_atomic,
        sources: body.sources?.map(source => ({ name: source.name, endpoint_id: source.endpoint_id })), message: body.message ?? body.error }
    } catch (error) {
      result = { question, ready: false, error: error.message }
    }
    report.results.push(result)
    console.log(`${result.ready ? 'PASS' : 'FAIL'}: ${question}`)
    if (reportPath) writeFileSync(reportPath, `${JSON.stringify(report, null, 2)}\n`)
  }
  assert(report.results.every(result => result.ready), 'Remove or repair every failing example before publishing. See live planning report.')
}

if (!checkOnly) {
  const result = spawnSync('cargo', ['test', '--lib', '--offline', 'homepage_starter_tests', '--', '--test-threads=4'],
    { cwd: gateway, stdio: 'inherit' })
  if (result.error) throw result.error
  process.exitCode = result.status ?? 1
}
