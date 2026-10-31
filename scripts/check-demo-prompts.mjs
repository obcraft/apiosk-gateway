#!/usr/bin/env node
// Pre-demo check for the showcase run sheet. Each question is planned and priced
// by the live guest preview, the same call the public Ask page makes. Preview
// is ephemeral: this never approves, never stores the token and buys no data.
//
//   node gateway/scripts/check-demo-prompts.mjs            # 2 rounds, all scenes
//   node gateway/scripts/check-demo-prompts.mjs --rounds 3 scene-1 scene-3
import { randomUUID } from 'node:crypto'

const endpoint = 'https://apiosk-gateway-v2.fly.dev/v2/ask-v2/guest/preview'
// Keep in sync with the run sheet. `backup` scenes may fail without failing the run.
const scenes = [
  { id: 'scene-1', title: 'KYB check (Mollie)', question: 'KYB check for Mollie B.V. (KVK number 30204462): show the registered company profile and screen the company name against sanctions lists.' },
  { id: 'scene-2', title: 'UK company with owners (Monzo)', question: 'KYB check for MONZO BANK LIMITED, company number 09446231: show its Companies House profile and persons with significant control, and screen the company name against sanctions lists.' },
  { id: 'scene-3', title: 'Pay per call, chosen providers', question: 'Screen the company name Mollie B.V. against sanctions lists with CompliancePulse and Sirenic, paying per call.' },
  { id: 'scene-4', title: 'Weekly sanctions briefing', question: 'Weekly sanctions briefing for Mollie B.V.: screen the company name against sanctions lists with OpenSanctions and CompliancePulse.' },
  { id: 'scene-6', title: 'Vehicle registration', question: 'Use Overheid.io to look up the vehicle registered with Dutch plate RZ874R.' },
  { id: 'backup-coolblue', backup: true, title: 'Company search and screen', question: 'Search the Dutch business register for companies named Coolblue and screen the company name against sanctions lists.' },
  { id: 'backup-iban', backup: true, title: 'IBAN and EUR/USD rate', question: 'Validate IBAN NL91ABNA0417164300 with IBAN Check and show the latest EUR/USD reference rate from Frankfurter.' },
  { id: 'backup-monthly', backup: true, title: 'Monthly briefing (Monzo)', question: 'Monthly compliance briefing for MONZO BANK LIMITED, company number 09446231: Companies House filing history and a sanctions screen of the company name.' },
]

const args = process.argv.slice(2)
let rounds = 2
const only = []
for (let index = 0; index < args.length; index += 1) {
  if (args[index] === '--rounds' && args[index + 1]) rounds = Number(args[++index])
  else if (scenes.some(scene => scene.id === args[index])) only.push(args[index])
  else throw new Error(`Unknown argument ${args[index]}; use --rounds N or a scene id: ${scenes.map(scene => scene.id).join(', ')}`)
}
if (!Number.isInteger(rounds) || rounds < 1 || rounds > 5) throw new Error('--rounds must be between 1 and 5')
const selected = only.length ? scenes.filter(scene => only.includes(scene.id)) : scenes

// Sequential on purpose: parallel previews hit "could not be priced" (503) more often.
async function preview(question) {
  const response = await fetch(endpoint, {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ request_id: randomUUID(), question }), signal: AbortSignal.timeout(110_000),
  })
  const body = await response.json().catch(() => ({}))
  return { http: response.status, status: body.status ?? body.error ?? 'no_status', total: body.total_atomic, currency: body.currency,
    sources: [...new Set((body.sources ?? []).map(source => source.source_slug).filter(Boolean))] }
}

let failed = false
for (const scene of selected) {
  const tries = []
  for (let round = 0; round < rounds; round += 1) {
    try { tries.push(await preview(scene.question)) } catch (error) { tries.push({ status: error.name === 'TimeoutError' ? 'timeout' : 'network_error', sources: [] }) }
  }
  const ready = tries.filter(attempt => attempt.status === 'ready')
  const prices = [...new Set(ready.map(attempt => (Number(attempt.total) / 1e6).toFixed(3)))]
  const verdict = ready.length === rounds ? 'PASS' : ready.length > 0 ? 'FLAKY' : 'FAIL'
  if (verdict === 'FAIL' && !scene.backup) failed = true
  console.log(`${verdict.padEnd(5)} ${scene.id.padEnd(15)} ${ready.length}/${rounds} ready  ${prices.length ? `${prices.join(' / ')} ${ready[0].currency ?? ''}`.trim() : '-'}  ${[...new Set(ready.flatMap(attempt => attempt.sources))].join(', ') || tries.map(attempt => attempt.status).join(', ')}`)
}
console.log('\nPreview only: nothing was approved or bought. Sources and prices can differ per run; the app shows the exact total before approval.')
process.exitCode = failed ? 1 : 0
