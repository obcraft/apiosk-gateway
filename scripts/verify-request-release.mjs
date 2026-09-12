// Read-only production checks. Optional REQUEST_FLOW_EVENTS_PATH is a
// short-lived capability returned for your own task; never log its signature.
import assert from 'node:assert/strict';
import { APIO_V2_CARD_URI } from '../../mcp/src/gateway-v2-card.mjs';
const gateway = 'https://apiosk-gateway-v2.fly.dev';
const checked = {};
const get = (url, options = {}) => fetch(url, { ...options, signal: AbortSignal.timeout(15_000) });
const health = await get(`${gateway}/health`);
assert.equal(health.status, 200);
assert.equal((await health.json()).payment_executor, 'ready');
assert.equal((await get(`${gateway}/v2/tasks`)).status, 401);
checked.gateway = 'ready; task reads require authentication';
const card = await (await get('https://mcp.apiosk.com/.well-known/mcp/server-card.json')).json();
assert.deepEqual(card.tools.map(t => t.name).sort(), ['apiosk_approve', 'apiosk_discover', 'apiosk_execute', 'apiosk_sources', 'apiosk_status']);
assert.ok(card.tools.filter(t => t.name !== 'apiosk_approve').every(t => t._meta.ui.resourceUri === APIO_V2_CARD_URI));
assert.deepEqual(card.tools.find(t => t.name === 'apiosk_approve')._meta.ui.visibility, ['app']);
assert.equal(card.authentication.required, true);
checked.mcp = `five tools; ${APIO_V2_CARD_URI}; app-only approval`;
const origin = 'https://embedded-host.example';
const preflight = await get(`${gateway}/v2/approve`, { method: 'OPTIONS', headers: { origin, 'access-control-request-method': 'POST', 'access-control-request-headers': 'authorization,content-type' } });
assert.notEqual(preflight.headers.get('access-control-allow-origin'), origin);
assert.notEqual(preflight.headers.get('access-control-allow-origin'), '*');
checked.approval_cors = 'App-only';
const path = process.env.REQUEST_FLOW_EVENTS_PATH;
if (path) {
  assert.match(path, /^\/v2\/tasks\/[0-9a-f-]+\/events\?/);
  const url = new URL(path, gateway);
  const invalid = new URL(url); invalid.searchParams.set('signature', '00');
  assert.equal((await get(invalid, { headers: { origin } })).status, 403);
  const stream = await get(url, { headers: { origin } });
  assert.equal(stream.status, 200);
  assert.equal(stream.headers.get('access-control-allow-origin'), '*');
  assert.equal(stream.headers.get('access-control-allow-credentials'), null);
  assert.match(stream.headers.get('content-type'), /text\/event-stream/);
  const reader = stream.body.getReader();
  const decoder = new TextDecoder(); let buffer = '';
  try {
    while (!buffer.includes('\n\n')) {
      const part = await reader.read(); assert.equal(part.done, false);
      buffer += decoder.decode(part.value);
    }
  } finally { await reader.cancel(); }
  const task = JSON.parse(buffer.split('\n').find(line => line.startsWith('data:')).slice(5));
  assert.equal(task.context_view.execution_mode, 'server');
  assert.equal(url.pathname, `/v2/tasks/${task.state.state_ref}/events`);
  checked.events = { status: task.status, revision: task.state.revision, worker_active: task.context_view.worker_active, authorization_active: task.billing?.authorization_active, cors: '*' };
}
console.log(JSON.stringify(checked, null, 2));
