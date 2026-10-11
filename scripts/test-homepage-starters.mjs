#!/usr/bin/env node
// Cross-repository release gate. Unlike `cargo test`, this command requires the
// actual published app prompts and fails if the app checkout is unavailable.
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'
import { spawnSync } from 'node:child_process'
import assert from 'node:assert/strict'

const gateway = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const args = process.argv.slice(2)
let appSource = resolve(gateway, '../app/src/lib/ui/ask-examples.ts')
let checkOnly = false
for (let index = 0; index < args.length; index += 1) {
  if (args[index] === '--app-source' && args[index + 1]) appSource = resolve(args[++index])
  else if (args[index] === '--check-only') checkOnly = true
  else throw new Error(`Unknown argument ${args[index]}; use --app-source PATH or --check-only`)
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
assert.equal(prompts.length, 6, 'All six homepage questions must be covered by the planning gate')
assert.equal(new Set(manifest.map((entry) => entry.id)).size, 6, 'Every acceptance case needs a distinct id')
assert.deepEqual(prompts, manifest.map((entry) => entry.question),
  'Published starter questions drifted from the gateway acceptance cases; update and verify both before release')
console.log('All 6 published app prompts exactly match the gateway acceptance cases.')

if (!checkOnly) {
  const result = spawnSync('cargo', ['test', '--lib', '--offline', 'homepage_starter_tests', '--', '--test-threads=4'],
    { cwd: gateway, stdio: 'inherit' })
  if (result.error) throw result.error
  process.exitCode = result.status ?? 1
}
