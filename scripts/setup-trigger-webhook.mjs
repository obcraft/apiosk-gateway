#!/usr/bin/env node
// One-time Composio delivery setup. Keys are read from the environment, never
// CLI arguments or output. The resulting env file must remain server-side.
import { writeFile, lstat } from 'node:fs/promises'
import { resolve } from 'node:path'

const [destination, output] = process.argv.slice(2)
if (!destination || !output) {
  console.error('Usage: node --env-file=.env scripts/setup-trigger-webhook.mjs <https-gateway/v2/triggers/composio> <new-server-only-env-file>')
  process.exit(1)
}
const url = new URL(destination)
if (url.protocol !== 'https:' || url.username || url.password || url.search || url.hash || url.pathname !== '/v2/triggers/composio') throw new Error('Use the HTTPS gateway URL ending in /v2/triggers/composio.')
const key = process.env.COMPOSIO_API_KEY?.trim()
if (!key) throw new Error('COMPOSIO_API_KEY is required in the server environment.')
const outputPath = resolve(output)
if (await lstat(outputPath).then(() => true, error => { if (error.code === 'ENOENT') return false; throw error })) throw new Error('Choose a new output file. Existing files are never overwritten.')
async function call(path, method = 'GET', body) {
  const response = await fetch(`https://backend.composio.dev/api/v3.1${path}`, {
    method, headers: { 'x-api-key': key, 'content-type': 'application/json' },
    body: body ? JSON.stringify(body) : undefined, signal: AbortSignal.timeout(20_000), redirect: 'error',
  })
  // Provider response bodies can contain secrets; do not put them in errors.
  if (!response.ok) throw new Error(`Composio returned HTTP ${response.status}. Inspect the subscription in its dashboard before retrying.`)
  return response.json()
}
const existing = await call('/webhook_subscriptions')
if (existing.next_cursor) throw new Error('Review existing subscriptions in Composio before configuring delivery.')
let subscription = existing.items?.find(item => item.webhook_url === url.href)
if (!subscription && existing.items?.length) throw new Error('This project already delivers to another URL. Use a dedicated project or explicitly reconcile that destination; it has not been changed.')
if (subscription) {
  if (subscription.version !== 'V3' || !subscription.enabled_events?.includes('composio.trigger.message')) throw new Error('Existing subscription must use V3 and include composio.trigger.message. It has not been changed.')
  if (!subscription.secret) subscription = await call(`/webhook_subscriptions/${encodeURIComponent(subscription.id)}`)
} else {
  subscription = await call('/webhook_subscriptions', 'POST', { webhook_url: url.href, version: 'V3', enabled_events: ['composio.trigger.message'] })
}
if (typeof subscription.secret !== 'string' || !subscription.secret || /[\r\n]/.test(subscription.secret)) throw new Error('The signing secret was missing or invalid. Retrieve it from the Composio dashboard.')
await writeFile(outputPath, `COMPOSIO_WEBHOOK_SECRET=${JSON.stringify(subscription.secret)}\n`, { mode: 0o600, flag: 'wx' })
console.log(`Composio delivery configured for ${url.href}. Signing secret written to the server-only env file with mode 0600; no secret was printed.`)
