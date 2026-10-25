#!/usr/bin/env node
/** Local App + MCP + Gateway/API launcher. Supabase stays in the configured project. */
import { spawn, spawnSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { existsSync, mkdirSync, openSync, closeSync, readFileSync, writeFileSync } from 'node:fs';
import net from 'node:net';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { parseEnv } from 'node:util';
import { setTimeout as delay } from 'node:timers/promises';
import { superviseLauncher } from './dev-local-supervisor.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const runtime = path.join(root, 'gateway/.local/dev-stack');
const appOrigin = 'http://localhost:5276';
const gatewayOrigin = 'http://127.0.0.1:8082';
const mcpOrigin = 'http://127.0.0.1:3000';

function envFile(file) {
  return existsSync(file) ? parseEnv(readFileSync(file, 'utf8')) : {};
}

export function localConfiguration(workspace = root, inherited = process.env) {
  const gateway = { ...envFile(path.join(workspace, 'gateway/.env')), ...inherited };
  const app = { ...envFile(path.join(workspace, 'app/.env')), ...envFile(path.join(workspace, 'app/.env.local')), ...inherited };
  const mcp = { ...envFile(path.join(workspace, 'mcp/.env')), ...envFile(path.join(workspace, 'mcp/.env.v2.local')), ...inherited };
  const modelKey = gateway.OPENROUTER_API_KEY || gateway.INTENT_API_KEY;
  const required = ['DATABASE_URL', 'SUPABASE_URL', 'SUPABASE_PUBLISHABLE_KEY', 'STATE_SIGNING_KEY'];
  for (const key of required) {
    if (!gateway[key]?.trim()) throw new Error(`Missing ${key}. Set it in gateway/.env.`);
  }
  if (Buffer.byteLength(gateway.STATE_SIGNING_KEY) < 32) throw new Error('STATE_SIGNING_KEY must contain at least 32 bytes.');
  if (!modelKey?.trim()) throw new Error('Set OPENROUTER_API_KEY or INTENT_API_KEY in gateway/.env for Ask.');
  const supabase = gateway.SUPABASE_URL.replace(/\/+$/, '');
  const agentApi = mcp.APIOSK_GATEWAY_URL || app.VITE_AGENT_API_URL || `${supabase}/functions/v1/agent-gateway`;
  Object.assign(gateway, {
    APP_ORIGIN: appOrigin, BIND_ADDRESS: '127.0.0.1:8082',
    INTENT_API_KEY: gateway.INTENT_API_KEY || modelKey, OPENROUTER_API_KEY: modelKey,
    DATABASE_POOL_SIZE: gateway.DATABASE_POOL_SIZE || '2',
    RUST_LOG: gateway.RUST_LOG || 'apiosk_gateway_v2=info',
    V2_EXECUTION_ENABLED: '0', KVK_WORKERS_ENABLED: '0', RECURRING_ORDERS_ENABLED: '0',
    RECURRING_MANUAL_TRIGGER_ENABLED: '0', RECURRING_LOCAL_MANAGEMENT_ENABLED: '1',
    WORKSPACE_CONTEXT_ENABLED: 'true',
    ASK_V2_TRANSLATION: '0',
  });
  Object.assign(app, {
    VITE_SUPABASE_URL: supabase, VITE_SUPABASE_PUBLISHABLE_KEY: gateway.SUPABASE_PUBLISHABLE_KEY,
    VITE_API_BASE_URL: gatewayOrigin, VITE_GATEWAY_V2_URL: gatewayOrigin,
    VITE_GATEWAY_V2_MCP_URL: `${mcpOrigin}/mcp`, VITE_AGENT_API_URL: agentApi,
    APIOSK_DEV_GATEWAY_TARGET: '', VITE_LOCAL_WORKSPACES: 'false',
  });
  Object.assign(mcp, {
    NODE_ENV: 'development', PORT: '3000', APIOSK_MCP_BIND_HOST: '127.0.0.1',
    APIOSK_MCP_PUBLIC_BASE_URL: mcpOrigin, APIOSK_MCP_ISSUER_URL: mcpOrigin,
    APIOSK_MCP_SERVER_URL: `${mcpOrigin}/mcp`, APIOSK_GATEWAY_V2_URL: gatewayOrigin,
    APIOSK_GATEWAY_URL: agentApi,
  });
  return { gateway, app, mcp, supabase, agentApi };
}

/** Recurring is optional locally; incomplete configuration must not block startup. */
export function enableLocalRecurring(gateway) {
  if (!gateway.RECURRING_ALLOWED_OWNER_IDS?.trim() || !gateway.RECURRING_WORKER_SECRET || gateway.RECURRING_WORKER_SECRET.trim().length < 32) {
    gateway.RECURRING_ORDERS_ENABLED = '0';
    gateway.RECURRING_MANUAL_TRIGGER_ENABLED = '0';
    return false;
  }
  gateway.RECURRING_ORDERS_ENABLED = '1';
  gateway.RECURRING_MANUAL_TRIGGER_ENABLED = '1';
  return true;
}

/** Reuse an explicitly provisioned local credential; never create one here. */
export function readExistingWorkerSecret(file) {
  try {
    const secret = readFileSync(file, 'utf8').trim();
    return secret.length >= 32 ? secret : undefined;
  } catch (error) {
    if (error.code === 'ENOENT') return undefined;
    throw error;
  }
}

async function checkPort(port, host) {
  await new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once('error', error => {
      if (host === '::1' && ['EAFNOSUPPORT', 'EADDRNOTAVAIL'].includes(error.code)) return resolve();
      reject(new Error(error.code === 'EADDRINUSE'
        ? `Port ${port} is already in use. Stop its existing service before running ./dev.`
        : `Cannot check local port ${port}: ${error.code}`));
    });
    server.listen({ port, host, exclusive: true }, () => server.close(resolve));
  });
}

export async function checkPorts(services) {
  for (const service of services) {
    await checkPort(service.port, '127.0.0.1');
    await checkPort(service.port, '::1');
  }
}

function signalGroup(child, signal) {
  if (!child.pid) return;
  try { process.kill(-child.pid, signal); } catch (error) {
    if (error.code !== 'ESRCH') throw error;
  }
}

function groupAlive(child) {
  if (!child.pid) return false;
  try { process.kill(-child.pid, 0); return true; }
  catch (error) {
    if (error.code === 'ESRCH') return false;
    throw error;
  }
}

/** Keep handlers installed throughout cleanup, including repeated Ctrl+C. */
export function installShutdownHandlers(controller, {
  input = process.stdin, signals = process, requireParent = false,
} = {}) {
  const stop = () => controller.abort();
  const keys = chunk => { if (chunk.includes('\u0004')) stop(); };
  for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) signals.on(signal, stop);
  if (requireParent) {
    signals.on('disconnect', stop);
    if (!signals.connected) stop();
  }
  // A terminal's Ctrl+D is EOF. Ignore stdin for detached/noninteractive runs,
  // where /dev/null or a closed pipe must not stop a healthy stack immediately.
  const terminal = Boolean(input.isTTY);
  if (terminal) {
    input.on('end', stop);
    input.on('close', stop);
    input.on('data', keys);
    input.resume();
  }
  return () => {
    for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) signals.removeListener(signal, stop);
    if (requireParent) signals.removeListener('disconnect', stop);
    if (terminal) {
      input.removeListener('end', stop);
      input.removeListener('close', stop);
      input.removeListener('data', keys);
      input.pause();
    }
  };
}

/** Wait for descendants too: Cargo can exit before its Gateway child does. */
export async function stopServices(children, { graceMs = 5000, killWaitMs = 5000, verifyPorts = checkPorts } = {}) {
  if (!children.length) return;
  const errors = [];
  const send = signal => {
    for (const { child } of children) {
      try { signalGroup(child, signal); }
      catch (error) { errors.push(error); }
    }
  };
  const alive = () => children.some(({ child }) => groupAlive(child));
  send('SIGTERM');
  const gracefulUntil = Date.now() + graceMs;
  while (alive() && Date.now() < gracefulUntil) await delay(50);
  // Always address the original groups, never whichever process owns a port.
  send('SIGKILL');
  const killedUntil = Date.now() + killWaitMs;
  let portError;
  do {
    try { await verifyPorts(children.map(({ service }) => service)); portError = null; }
    catch (error) { portError = error; }
    if (!alive() && children.every(entry => entry.stopped) && !portError) {
      if (errors.length) throw new AggregateError(errors, 'Some local services could not be signalled.');
      return;
    }
    await delay(50);
  } while (Date.now() < killedUntil);
  throw new Error(`Local shutdown incomplete: ${portError?.message || 'a service process group has not exited'}`);
}

/** Own every process group; a failed service or stop request stops its siblings. */
export async function runServices(services, {
  signal, timeoutMs = 600_000, stopWhenReady = false,
  onStatus = console.log, onReady = () => {}, pollMs = 500,
} = {}) {
  if (signal?.aborted) return;
  await checkPorts(services);
  const children = [];
  try {
    for (const service of services) {
      if (signal?.aborted) return;
      const log = openSync(service.log, 'w', 0o600);
      let child;
      try {
        child = spawn(service.command, service.args || [], {
          cwd: service.cwd, env: service.env, detached: true, stdio: ['ignore', log, log],
        });
      } finally { closeSync(log); }
      const entry = { service, child, stopped: false, ready: false, error: null };
      child.on('error', error => { entry.error = error; entry.stopped = true; });
      child.on('exit', (code, sig) => { entry.stopped = true; entry.exit = code ?? sig; });
      children.push(entry);
      onStatus(`Starting ${service.name}…`);
    }
    const deadline = Date.now() + timeoutMs;
    let allReady = false;
    let lastProgress = Date.now();
    while (!signal?.aborted) {
      for (const entry of children) {
        if (entry.stopped) throw new Error(`${entry.service.name} stopped (${entry.error?.code || entry.exit}). See ${entry.service.log}`);
      }
      if (!allReady) {
        await Promise.all(children.filter(entry => !entry.ready).map(async entry => {
          try {
            const response = await fetch(entry.service.health, { signal: AbortSignal.timeout(1200) });
            if (response.ok) {
              const body = await response.text();
              if (!entry.service.matches || entry.service.matches(body)) {
                entry.ready = true;
                onStatus(`${entry.service.name} ready`);
              }
            }
          } catch { /* The listener may still be starting or Cargo may be building. */ }
        }));
        if (children.some(entry => entry.stopped)) continue;
        allReady = children.every(entry => entry.ready);
        if (allReady) {
          await onReady();
          if (stopWhenReady) return;
        } else if (Date.now() >= deadline) {
          throw new Error(`Startup timed out: ${children.filter(e => !e.ready).map(e => e.service.name).join(', ')}. See the service logs.`);
        } else if (Date.now() - lastProgress > 15_000) {
          onStatus(`Waiting for ${children.filter(e => !e.ready).map(e => e.service.name).join(', ')}; the first Rust build can take several minutes.`);
          lastProgress = Date.now();
        }
      }
      await delay(pollMs);
    }
  } finally {
    if (children.length) onStatus('Stopping local services and releasing ports…');
    await stopServices(children);
  }
}

function localSecret(name = 'mcp-oauth-secret') {
  const file = path.join(runtime, name);
  try { writeFileSync(file, randomBytes(32).toString('hex'), { flag: 'wx', mode: 0o600 }); }
  catch (error) { if (error.code !== 'EEXIST') throw error; }
  return readFileSync(file, 'utf8').trim();
}

async function main() {
  const args = new Set(process.argv.slice(2));
  const allowed = ['--help', '--check', '--smoke', '--no-open', '--execute', '--recurring'];
  if ([...args].some(arg => !allowed.includes(arg))) throw new Error(`Unknown option. Use ./dev --help.`);
  if (args.has('--help')) {
    console.log(`Usage: ./dev [--no-open] [--execute] [--recurring]\n\nStarts App :5276, MCP :3000 and Gateway/API :8082. Opens the App when ready.\nUses the existing Supabase API, Auth and database; no migrations or env-file rewrites.\nPurchases and background workers are off by default. --execute permits approved\nrequests through your configured Supabase execution services.\n--recurring enables recurring setup and workers locally for the configured owner allowlist; requires --execute.\n\n--check     Check credentials, dependencies and local ports without starting services\n--smoke     Start all three, check HTTP readiness, then stop (no browser or purchases)\n--no-open   Leave the browser closed\nCtrl+C/D    Stop all three services and release their ports\nLogs: gateway/.local/dev-stack/`);
    return;
  }
  const [major, minor] = process.versions.node.split('.').map(Number);
  if (major < 22 || (major === 22 && minor < 12)) throw new Error('Node.js 22.12 or newer is required.');
  if (spawnSync('cargo', ['--version'], { stdio: 'ignore' }).status !== 0) throw new Error('Rust/Cargo is required.');
  for (const dir of ['app', 'mcp']) {
    if (!existsSync(path.join(root, dir, 'node_modules'))) throw new Error(`Install dependencies first: cd "${path.join(root, dir)}" && npm ci`);
  }
  const config = localConfiguration();
  if (config.gateway.EMBEDDED_EXECUTOR_ORIGIN) throw new Error('EMBEDDED_EXECUTOR_ORIGIN is set. This launcher uses hosted execution; unset that local override or start its executor separately.');
  if (args.has('--execute') && args.has('--smoke')) throw new Error('--smoke cannot be combined with --execute.');
  if (args.has('--recurring') && !args.has('--execute')) throw new Error('--recurring requires --execute.');
  if (args.has('--recurring') && !config.gateway.RECURRING_WORKER_SECRET?.trim()) {
    config.gateway.RECURRING_WORKER_SECRET = readExistingWorkerSecret(path.join(runtime, 'recurring-worker-secret'));
  }
  if (args.has('--recurring') && !enableLocalRecurring(config.gateway)) {
    console.log('Recurring workers skipped: configuration incomplete. Continuing local startup.');
  }
  if (args.has('--execute')) config.gateway.V2_EXECUTION_ENABLED = '1';
  const service = (name, dir, port, health, command, argv, env, matches) => ({
    name, cwd: path.join(root, dir), port, health, command, args: argv, env,
    log: path.join(runtime, `${name.toLowerCase()}.log`), matches,
  });
  const services = [
    service('Gateway', 'gateway', 8082, `${gatewayOrigin}/health`, 'cargo', ['run', '--locked', '--bin', 'apiosk-gateway-v2'], config.gateway, text => {
      try { return JSON.parse(text).service === 'apiosk-gateway-v2'; } catch { return false; }
    }),
    service('MCP', 'mcp', 3000, `${mcpOrigin}/health`, process.execPath, ['server.mjs'], config.mcp),
    service('App', 'app', 5276, appOrigin, process.execPath, ['node_modules/vite/bin/vite.js', '--host', 'localhost', '--port', '5276', '--strictPort'], config.app, text => text.includes('/src/main.tsx')),
  ];
  if (args.has('--check')) {
    await checkPorts(services);
    console.log('Configuration, dependencies and ports checked. No services started.');
    return;
  }
  mkdirSync(runtime, { recursive: true, mode: 0o700 });
  config.mcp.APIOSK_MCP_OAUTH_SECRET ||= localSecret();
  const controller = new AbortController();
  const removeShutdownHandlers = installShutdownHandlers(controller, {
    requireParent: process.env.APIOSK_LOCAL_SUPERVISED === '1',
  });
  console.log(`Supabase API: ${config.supabase}\nExecution: ${args.has('--execute') ? 'enabled for approved requests' : 'off (use --execute to enable approved requests)'}\nLogs: ${runtime}\n`);
  try { await runServices(services, {
    signal: controller.signal, stopWhenReady: args.has('--smoke'),
    onReady() {
      console.log(`\nApp          ${appOrigin}\nGateway/API  ${gatewayOrigin}\nMCP          ${mcpOrigin}/mcp\n\nAll three are ready. Ctrl+C or Ctrl+D stops them together.`);
      if (!args.has('--smoke') && !args.has('--no-open') && process.platform === 'darwin') {
        const browser = spawn('open', [appOrigin], { stdio: 'ignore' });
        browser.on('error', () => console.log(`Open ${appOrigin} in your browser.`));
      }
    },
  }); } finally { removeShutdownHandlers(); }
  console.log('Local services stopped. Ports 5276, 8082 and 3000 are released.');
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  if (process.env.APIOSK_LOCAL_SUPERVISED !== '1') {
    superviseLauncher(fileURLToPath(import.meta.url), process.argv.slice(2));
  } else {
    main().catch(error => { console.error(error.message); process.exitCode = 1; });
  }
}
