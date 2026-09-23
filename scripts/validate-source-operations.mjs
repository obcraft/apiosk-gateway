// Live validation of staged source-operation contracts (source-document-v1).
//
//   node scripts/validate-source-operations.mjs <staged.json> <samples-dir> <out-dir>            # free dry runs
//   node scripts/validate-source-operations.mjs <staged.json> <samples-dir> <out-dir> --pay --budget-usd=50
//
// Each call goes through the same /v1/do treasury path as use-selection: an
// operator procurement test, never a debit of a customer's balance. Dry runs
// are free and prove the inputs bind and the price matches. Paid runs go
// cheapest first under a hard budget, counted at the buyer cap (list + 15%)
// of every attempt already on disk, whatever its outcome. An attempt is
// written before its request and is never sent again. Only a response the
// document adapter accepts counts as evidence.
import fs from 'node:fs/promises';
import {execFileSync} from 'node:child_process';
import {randomUUID} from 'node:crypto';
import path from 'node:path';

const [stagedPath, samplesDir, outDir] = process.argv.slice(2);
if (!stagedPath || !samplesDir || !outDir) throw Error('Usage: validate-source-operations.mjs staged.json samples-dir out-dir [--pay] [--budget-usd=N] [--only=id] [--limit=N]');
const flag = (name) => process.argv.find((a) => a.startsWith(`--${name}=`))?.split('=')[1];
const paid = process.argv.includes('--pay');
const budget = Math.round(Number(flag('budget-usd') ?? 0) * 1e6);
const only = flag('only');
const limit = Number(flag('limit') ?? Infinity);
// Sources whose payment never confirms (no receipt) only burn held budget.
const skipSources = new Set((flag('skip-sources') ?? '').split(',').filter(Boolean));
if (paid && !(budget > 0)) throw Error('--pay needs --budget-usd');

const staged = JSON.parse(await fs.readFile(stagedPath, 'utf8'));
const samples = {};
for (const file of (await fs.readdir(samplesDir)).filter((f) => /^samples-\d+\.json$/.test(f))) {
  Object.assign(samples, JSON.parse(await fs.readFile(path.join(samplesDir, file), 'utf8')));
}
const dryDir = path.join(outDir, 'dry'), paidDir = path.join(outDir, 'paid');
await fs.mkdir(dryDir, {recursive: true});
await fs.mkdir(paidDir, {recursive: true});

let key = process.env.APIOSK_APP_SERVICE_KEY;
try {
  key ||= execFileSync('fly', ['ssh', 'console', '-a', 'apiosk-gateway-v2', '-q', '-C', `sh -c 'printf "%s" "$APIOSK_APP_SERVICE_KEY"'`], {encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe']}).trim();
} catch { throw Error('Treasury service credential unavailable'); }
if (!(key.length > 20) || key.includes('\n')) throw Error('Treasury service credential unavailable');

const buyerCap = (list) => list + Math.ceil(list * 0.15);
// Mirrors catalog-core facts::document::response: an upstream error, a pending
// job or an empty body is not source evidence.
export function usable(body) {
  if (body == null || typeof body === 'boolean' || typeof body === 'number') return false;
  if (typeof body === 'string') return body.trim() !== '';
  if (Array.isArray(body)) return body.length > 0;
  if (Object.keys(body).length === 0) return false;
  if (body.error != null && body.error !== false && body.error !== '') return false;
  if (body.errors != null && !(Array.isArray(body.errors) && body.errors.length === 0)) return false;
  if (body.success === false || body.ok === false) return false;
  if (typeof body.status === 'string' && ['error', 'failed', 'pending', 'queued', 'processing'].includes(body.status.toLowerCase())) return false;
  if (Number(body.statusCode) >= 400) return false;
  return true;
}
const exists = (p) => fs.stat(p).then(() => true, () => false);

const delayMs = Number(flag('delay-ms') ?? 1200);
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
async function doCall(c, inputs, dryRun, idempotency) {
  // /v1/do rate-limits per service key. The same idempotency key makes a
  // retried paid request the same request, never a second payment.
  for (let attempt = 0; ; attempt++) {
    await sleep(delayMs * 2 ** attempt);
    const r = await send(c, inputs, dryRun, idempotency);
    if (r.status !== 429 || attempt >= 4) return r;
  }
}
async function send(c, inputs, dryRun, idempotency) {
  const total = buyerCap(c.list_atomic) / 1e6;
  const response = await fetch('https://apiosk-gateway-v2.fly.dev/v1/do', {
    method: 'POST',
    headers: {'content-type': 'application/json', 'X-Apiosk-Service-Key': key, 'Idempotency-Key': idempotency},
    body: JSON.stringify({candidate_id: c.endpoint_id, job: 'Source operation contract verification', inputs, deterministic: true, parse: false,
      max_price: total, max_price_usdc: total, dry_run: dryRun, timeout_ms: 60000}),
    signal: AbortSignal.timeout(75000),
  });
  let body; try { body = await response.json(); } catch { body = null; }
  return {status: response.status, body};
}

// Budget already committed by earlier paid attempts, whatever their outcome.
let spent = 0;
for (const f of await fs.readdir(paidDir)) spent += JSON.parse(await fs.readFile(path.join(paidDir, f), 'utf8')).buyer_cap;

const summary = {dry_ready: 0, dry_not_ready: 0, no_sample: 0, paid_usable: 0, paid_unusable: 0, skipped_budget: 0, already_attempted: 0};
let n = 0;
for (const c of [...staged].sort((a, b) => a.list_atomic - b.list_atomic || a.id.localeCompare(b.id))) {
  if (only && c.id !== only) continue;
  if (skipSources.has(c.source)) continue;
  if (n >= limit) break;
  const inputs = samples[c.id];
  if (!inputs) { summary.no_sample++; continue; }
  n++;
  const dryFile = path.join(dryDir, `${c.id}.json`);
  let dry = await exists(dryFile) ? JSON.parse(await fs.readFile(dryFile, 'utf8')) : null;
  if (!dry) {
    const r = await doCall(c, inputs, true, randomUUID());
    const req = r.body?.request ?? {};
    dry = {id: c.id, source: c.source, path: c.path, inputs, http_status: r.status,
      ready: r.status === 200 && req.ready === true && !(req.missing_required?.length) && !(req.ignored_inputs?.length)
        && Math.round((r.body?.decision?.selected?.price_usd ?? -1) * 1e6) === c.list_atomic,
      missing: req.missing_required ?? null, ignored: req.ignored_inputs ?? null,
      price_usd: r.body?.decision?.selected?.price_usd ?? null, error: r.body?.error ?? null, at: new Date().toISOString()};
    // Still rate-limited after backoff: not an answer, so the next run asks again.
    if (r.status !== 429) await fs.writeFile(dryFile, JSON.stringify(dry, null, 2) + '\n');
  }
  dry.ready ? summary.dry_ready++ : summary.dry_not_ready++;
  if (!paid || !dry.ready) continue;

  const paidFile = path.join(paidDir, `${c.id}.json`);
  if (await exists(paidFile)) { summary.already_attempted++; continue; }
  const cap = buyerCap(c.list_atomic);
  if (spent + cap > budget) { summary.skipped_budget++; continue; }
  const idempotency = randomUUID();
  const record = {id: c.id, endpoint_id: c.endpoint_id, source: c.source, path: c.path, inputs, idempotency, provider_cap: c.list_atomic, buyer_cap: cap, started_at: new Date().toISOString(), status: 'dispatching'};
  await fs.writeFile(paidFile, JSON.stringify(record, null, 2) + '\n', {flag: 'wx'});
  spent += cap;
  let r;
  try { r = await doCall(c, inputs, false, idempotency); } catch (e) { r = {status: 0, body: {error: String(e)}}; }
  // Refused at service authentication (e.g. the database is restarting): the
  // request never reached payment. Forget the attempt and stop the run; the
  // endpoint is tried again next time. Any other outcome stays recorded.
  // The treasury token's own rules (e.g. 200 purchases per day) refuse before
  // anything is paid or called: not the source's failure either.
  const policyDenied = r.body?.did !== true && JSON.stringify(r.body?.execution?.body ?? r.body ?? '').includes('policy_denied');
  if ((r.status === 401 && r.body?.did !== true && !r.body?.execution) || policyDenied) {
    await fs.rm(paidFile);
    spent -= cap;
    console.log(JSON.stringify({stopped: 'gateway refused service authentication', error: r.body?.error ?? null, source: c.source, path: c.path}));
    break;
  }
  const execution = r.body?.execution ?? {};
  record.status = 'response';
  record.http_status = r.status;
  record.did = r.body?.did ?? null;
  record.paid = execution.paid ?? null;
  record.charged_usdc = execution.charged_usdc ?? null;
  record.request_id = execution.request_id ?? null;
  record.usable = r.status === 200 && r.body?.did === true && usable(execution.body);
  record.error = r.body?.error ?? null;
  record.body = execution.body ?? null;
  await fs.writeFile(paidFile, JSON.stringify(record, null, 2) + '\n');
  record.usable ? summary.paid_usable++ : summary.paid_unusable++;
  console.log(JSON.stringify({source: c.source, path: c.path, status: r.status, usable: record.usable, charged: record.charged_usdc, error: typeof record.error === 'string' ? record.error.slice(0, 120) : record.error}));
}
console.log(JSON.stringify({mode: paid ? 'treasury procurement' : 'dry-run', ...summary, budget_committed_micro_usd: spent, customer_balance_debited: false}));
