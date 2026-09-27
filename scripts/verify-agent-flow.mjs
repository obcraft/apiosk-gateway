// Verify the agent flow on Gateway v2, and that the retired agent-gateway v1
// agent API answers 410 while OAuth, connect and the skill stay up.
//
//   APIOSK_AGENT_TOKEN=apk_access_… node gateway/scripts/verify-agent-flow.mjs \
//     [--question "…"] [--skip-v1] [--skip-v2] [--paid]
//
// Free by default: browse -> endpoints -> discover (plan and price only).
// --paid also approves and follows the task; it needs APIOSK_VERIFY_CONFIRM=yes
// and must not be pointed at production casually. The token is never printed.
//
// Env: APIOSK_GATEWAY_V2_URL (https://gateway.apiosk.com),
//      APIOSK_AGENT_GATEWAY_URL (https://api.apiosk.com/functions/v1/agent-gateway),
//      APIOSK_MCP_URL (https://mcp.apiosk.com), APIOSK_APP_ORIGIN (https://app.apiosk.com)
import { randomUUID } from 'node:crypto';
import { setTimeout as sleep } from 'node:timers/promises';

const argv = process.argv.slice(2);
const flag = name => argv.includes(name);
const option = (name, fallback) => { const i = argv.indexOf(name); return i >= 0 && argv[i + 1] ? argv[i + 1] : fallback; };
const trim = url => url.replace(/\/+$/, '');

const GATEWAY = trim(process.env.APIOSK_GATEWAY_V2_URL || 'https://gateway.apiosk.com');
const AGENT_GATEWAY = trim(process.env.APIOSK_AGENT_GATEWAY_URL || 'https://api.apiosk.com/functions/v1/agent-gateway');
const MCP = trim(process.env.APIOSK_MCP_URL || 'https://mcp.apiosk.com');
const APP_ORIGIN = new URL(process.env.APIOSK_APP_ORIGIN || 'https://app.apiosk.com').origin;
const TOKEN = (process.env.APIOSK_AGENT_TOKEN || '').trim();
const QUESTION = option('--question', 'Company registry profile for Mollie B.V. in the Netherlands');
const PAID = flag('--paid');
const STATUSES = ['ready', 'needs_input', 'needs_selection', 'requires_approval', 'running', 'cancelled', 'succeeded', 'partial', 'unsupported', 'state_conflict', 'failed'];
const TERMINAL = ['succeeded', 'partial', 'failed', 'cancelled', 'unsupported'];

let failures = 0;
const pass = (step, detail = '') => console.log(`PASS  ${step}${detail ? ` — ${detail}` : ''}`);
const fail = (step, detail) => { failures += 1; console.log(`FAIL  ${step} — ${detail}`); };
const info = line => console.log(`      ${line}`);
const usd = micro => `$${(Number(micro) / 1e6).toFixed(6).replace(/0+$/, '').replace(/\.$/, '.00')}`;

async function call(url, { method = 'GET', body, auth = true } = {}) {
  const headers = { accept: 'application/json' };
  if (auth && TOKEN) headers.authorization = `Bearer ${TOKEN}`;
  if (body !== undefined) headers['content-type'] = 'application/json';
  const response = await fetch(url, { method, headers, redirect: 'manual', body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(90_000) });
  const text = await response.text();
  let json = null;
  try { json = text ? JSON.parse(text) : null; } catch { /* not JSON */ }
  return { status: response.status, type: response.headers.get('content-type') || '', text, json };
}

/** Run one step; a thrown error (network, timeout) is a FAIL, never a crash. */
async function step(name, fn) {
  try { return await fn(); } catch (error) { fail(name, error?.message || String(error)); return null; }
}

// --- Gateway v2: browse, endpoints, discover ---------------------------------
async function verifyV2() {
  if (!TOKEN) { fail('token', 'set APIOSK_AGENT_TOKEN to an apk_access_… or apk_live_… agent token (or pass --skip-v2)'); return null; }
  if (TOKEN.startsWith('apk_org_live_')) { fail('token', 'organisation keys are refused on the agent protocol routes; they use the public API (POST /v2/ask). Use an agent token.'); return null; }
  if (!/^apk_(access|live)_/.test(TOKEN)) { fail('token', 'expected an apk_access_… or apk_live_… agent token'); return null; }
  pass('token', `${TOKEN.startsWith('apk_access_') ? 'OAuth access token' : 'agent key'} (not printed)`);

  const slug = await step('GET /v2/sources', async () => {
    const r = await call(`${GATEWAY}/v2/sources?limit=5`);
    const ok = r.status === 200 && r.json?.protocol_version === '2' && Array.isArray(r.json.sources) && r.json.sources.length > 0 && r.json.total >= 1;
    if (!ok) return fail('GET /v2/sources', `HTTP ${r.status} ${r.text.slice(0, 200)}`);
    pass('GET /v2/sources', `${r.json.total} sources, first: ${r.json.sources[0].slug}`);
    return r.json.sources[0].slug;
  });

  if (slug) await step('GET /v2/sources/{id}/endpoints', async () => {
    const r = await call(`${GATEWAY}/v2/sources/${encodeURIComponent(slug)}/endpoints?limit=5`);
    if (r.status !== 200 || !Array.isArray(r.json?.items)) return fail('GET /v2/sources/{id}/endpoints', `HTTP ${r.status} ${r.text.slice(0, 200)}`);
    pass('GET /v2/sources/{id}/endpoints', `${r.json.items.length} of ${r.json.total ?? '?'} endpoints for ${slug}`);
  });

  return step('POST /v2/discover', async () => {
    const r = await call(`${GATEWAY}/v2/discover`, { method: 'POST', body: { request_id: randomUUID(), question: QUESTION } });
    const task = r.json;
    if (r.status !== 200 || task?.protocol_version !== '2') return fail('POST /v2/discover', `HTTP ${r.status} ${r.text.slice(0, 300)}`);
    if (!STATUSES.includes(task.status)) return fail('POST /v2/discover', `unknown status ${task.status}`);
    const mode = task.context_view?.approval_mode;
    if (!['chatbot', 'app'].includes(mode)) return fail('POST /v2/discover', `context_view.approval_mode is ${JSON.stringify(mode)}`);
    pass('POST /v2/discover', `status ${task.status}, approval_mode ${mode}`);
    if (task.errors?.length) info(`errors: ${JSON.stringify(task.errors).slice(0, 300)}`);
    const proposal = task.proposal;
    if (!proposal) { info('no proposal (nothing to price for this status)'); return task; }
    let approval;
    try { approval = new URL(proposal.approval_url); } catch { fail('proposal.approval_url', `not a URL: ${proposal.approval_url}`); return task; }
    if (approval.origin !== APP_ORIGIN || approval.pathname !== '/gateway-v2' || approval.searchParams.get('task') !== task.state?.state_ref) {
      fail('proposal.approval_url', `${approval.href} is not ${APP_ORIGIN}/gateway-v2?task=${task.state?.state_ref}`);
    } else pass('proposal.approval_url', approval.href);
    if (!/^\d+$/.test(String(proposal.max_total_atomic))) fail('proposal.max_total_atomic', `not a micro-USD integer string: ${proposal.max_total_atomic}`);
    else pass('proposal.max_total_atomic', `price ceiling ${usd(proposal.max_total_atomic)} (quote ${proposal.quote_ref}, expires ${proposal.expires_at})`);
    return task;
  });
}

// --- --paid: approve and follow ----------------------------------------------
async function verifyPaid(task) {
  console.log('\n!!!!  --paid: this approves a REAL purchase from the connected account and follows it.  !!!!');
  if (process.env.APIOSK_VERIFY_CONFIRM !== 'yes') { fail('paid', 'set APIOSK_VERIFY_CONFIRM=yes to allow spending'); return; }
  if (!task?.state?.state_ref) { fail('paid', 'discover returned no task state to approve'); return; }
  const id = task.state.state_ref;
  if (task.status === 'requires_approval' || task.status === 'ready') {
    if (task.context_view.approval_mode === 'chatbot' && task.proposal) {
      const { quote_ref, max_total_atomic } = task.proposal;
      const r = await call(`${GATEWAY}/v2/approve`, { method: 'POST', body: { request_id: randomUUID(), state: task.state, quote_ref, max_total_atomic } });
      if (r.status !== 200) { fail('POST /v2/approve', `HTTP ${r.status} ${r.text.slice(0, 300)}`); return; }
      pass('POST /v2/approve', `approved ${usd(max_total_atomic)}, status ${r.json?.status}`);
    } else {
      info(`approval_mode app: open ${task.proposal?.approval_url} and approve; waiting…`);
    }
  }
  // Gentle polling: Supabase is small, so back off from 5 s to 20 s, ~3 min total.
  let delay = 5_000;
  for (let waited = 0; waited < 180_000; waited += delay, delay = Math.min(delay * 1.5, 20_000)) {
    await sleep(delay);
    const r = await call(`${GATEWAY}/v2/tasks/${id}`);
    if (r.status !== 200) { info(`GET /v2/tasks/${id}: HTTP ${r.status}`); continue; }
    info(`task ${id}: ${r.json.status}`);
    if (TERMINAL.includes(r.json.status)) {
      const charged = r.json.billing?.total_charged;
      const report = r.json.context_view?.report?.url || r.json.context_view?.report?.path;
      (r.json.status === 'failed' ? fail : pass)('GET /v2/tasks/{id}', `${r.json.status}; charged ${charged != null ? usd(charged) : 'unknown'}${report ? `; report ${report}` : ''}`);
      return;
    }
  }
  fail('GET /v2/tasks/{id}', 'no terminal status within ~3 minutes (check the task in the app; do not buy again)');
}

// --- agent-gateway: what stays, and what is gone ---------------------------------
const REMOVED = [
  ['GET', '/v1/ask?q=x'], ['GET', '/v1/discover?q=x'], ['POST', '/v1/select'], ['GET', '/v1/sources'],
  ['GET', '/v1/providers'], ['GET', '/v1/apis'], ['POST', '/v1/quote'], ['POST', '/v1/plans'], ['POST', '/v1/jobs'],
  ['GET', '/v1/jobs/00000000-0000-0000-0000-000000000000'], ['GET', '/v1/approvals/x'], ['GET', '/v1/balance'], ['GET', '/v1/history'],
];

async function verifyV1() {
  const expect = async (name, url, check, options = {}) => step(name, async () => {
    const r = await call(url, options);
    const problem = check(r);
    return problem ? fail(name, `${problem} (HTTP ${r.status})`) : pass(name, `HTTP ${r.status}`);
  });
  await expect('agent-gateway GET /', `${AGENT_GATEWAY}/`, r => (r.status !== 200 || !r.json) && 'expected 200 JSON', { auth: false });
  await expect('GET /v1/skill/public', `${AGENT_GATEWAY}/v1/skill/public`, r =>
    r.status !== 200 ? 'expected 200' : !r.type.includes('markdown') ? `content-type ${r.type}` : !r.text.includes('/v2/discover') && 'skill does not teach /v2/discover', { auth: false });
  await expect('GET /v1/connect', `${AGENT_GATEWAY}/v1/connect`, r => r.status !== 200 && 'expected 200', { auth: false });
  await expect('MCP OAuth authorization-server metadata', `${MCP}/.well-known/oauth-authorization-server`, r => (r.status !== 200 || !r.json) && 'expected 200 JSON', { auth: false });
  await expect('MCP OAuth protected-resource metadata', `${MCP}/.well-known/oauth-protected-resource/mcp`, r => (r.status !== 200 || !r.json) && 'expected 200 JSON', { auth: false });
  await expect('POST /v1/run without a token', `${AGENT_GATEWAY}/v1/run`, r => r.status !== 401 && 'expected 401 (route alive, gateway execution depends on it)', { method: 'POST', body: {}, auth: false });
  for (const [method, path] of REMOVED) {
    await expect(`${method} ${path} is gone`, `${AGENT_GATEWAY}${path}`, r => {
      if (r.status !== 410) return 'expected 410';
      const b = r.json || {};
      if (b.error !== 'moved_to_gateway_v2') return `error is ${JSON.stringify(b.error)}`;
      if (trim(b.gateway || '') !== 'https://gateway.apiosk.com') return `gateway is ${JSON.stringify(b.gateway)}`;
      if (!b.replacements || typeof b.replacements !== 'object') return 'no replacements map';
      if (typeof b.replacement !== 'string' || !b.message || !b.docs) return 'missing replacement, message or docs';
      return null;
    }, { method, body: method === 'POST' ? {} : undefined });
  }
}

console.log(`Gateway v2: ${GATEWAY}\nAgent gateway: ${AGENT_GATEWAY}\nMCP: ${MCP}\nApp origin: ${APP_ORIGIN}\n`);
if (!flag('--skip-v2')) {
  const task = await verifyV2();
  if (PAID) await verifyPaid(task);
}
if (!flag('--skip-v1')) { console.log(''); await verifyV1(); }
console.log(`\n${failures ? `${failures} check(s) FAILED` : 'All checks passed'}`);
process.exit(failures ? 1 : 0);
