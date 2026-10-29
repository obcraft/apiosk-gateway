import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { EventEmitter, once } from 'node:events';
import { PassThrough } from 'node:stream';
import { spawn } from 'node:child_process';
import { setTimeout as delay } from 'node:timers/promises';
import { enableLocalRecurring, readExistingWorkerSecret, checkPorts, installShutdownHandlers, localConfiguration, runServices, stopServices } from './dev-local.mjs';

async function freePort() {
  const socket = net.createServer();
  await new Promise(resolve => socket.listen(0, '127.0.0.1', resolve));
  const port = socket.address().port;
  await new Promise(resolve => socket.close(resolve));
  return port;
}

const server = `
  import http from 'node:http';
  const server = http.createServer((req,res) => res.end('fixture-ready'));
  server.listen(Number(process.env.FIXTURE_PORT), '127.0.0.1');
  if (process.env.FIXTURE_FAIL) setTimeout(() => process.exit(7), 100);
`;

for (const event of ['SIGINT', 'SIGTERM', 'SIGHUP', 'end', 'close', 'data']) {
  test(`terminal shutdown handles ${event} and repeated stop requests`, () => {
    const input = new PassThrough();
    input.isTTY = true;
    const signals = new EventEmitter();
    const controller = new AbortController();
    let stops = 0;
    controller.signal.addEventListener('abort', () => stops++);
    const remove = installShutdownHandlers(controller, { input, signals });
    if (event.startsWith('SIG')) signals.emit(event);
    else input.emit(event, Buffer.from('\u0004'));
    signals.emit('SIGINT'); signals.emit('SIGINT');
    assert.equal(controller.signal.aborted, true);
    assert.equal(stops, 1);
    assert.equal(signals.listenerCount('SIGINT'), 1, 'handler remains during cleanup');
    remove();
    assert.equal(signals.listenerCount('SIGINT'), 0);
    assert.equal(input.listenerCount('data'), 0);
    assert.equal(input.isPaused(), true);
    input.destroy();
  });
}

test('noninteractive stdin EOF does not stop background services', () => {
  const input = new PassThrough();
  const signals = new EventEmitter();
  const controller = new AbortController();
  const remove = installShutdownHandlers(controller, { input, signals });
  input.emit('end'); input.emit('close');
  assert.equal(controller.signal.aborted, false);
  signals.emit('SIGTERM');
  assert.equal(controller.signal.aborted, true);
  remove(); input.destroy();
});

for (const connected of [true, false]) {
  test(`supervised launcher cleans up when parent ${connected ? 'disconnects' : 'already died'}`, () => {
    const input = new PassThrough();
    const signals = new EventEmitter();
    signals.connected = connected;
    const controller = new AbortController();
    const remove = installShutdownHandlers(controller, { input, signals, requireParent: true });
    if (connected) {
      assert.equal(controller.signal.aborted, false);
      signals.emit('disconnect');
    }
    assert.equal(controller.signal.aborted, true);
    remove();
    assert.equal(signals.listenerCount('disconnect'), 0);
    input.destroy();
  });
}

for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP', 'SIGKILL']) {
  test(`supervisor ${signal} reaches launcher cleanup through its IPC lifeline`, { timeout: 10000 }, async () => {
    const dir = mkdtempSync(path.join(os.tmpdir(), 'apiosk-supervisor-'));
    const marker = path.join(dir, 'cleaned-up');
    const launcher = path.join(dir, 'launcher.mjs');
    writeFileSync(launcher, `
      import { writeFileSync } from 'node:fs';
      import { installShutdownHandlers } from ${JSON.stringify(new URL('./dev-local.mjs', import.meta.url).href)};
      const controller = new AbortController();
      const remove = installShutdownHandlers(controller, { requireParent: process.env.APIOSK_LOCAL_SUPERVISED === '1' });
      const cleanup = () => {
        writeFileSync(${JSON.stringify(marker)}, 'done');
        remove();
        if (process.connected) process.disconnect();
      };
      controller.signal.addEventListener('abort', cleanup, { once: true });
      if (controller.signal.aborted) cleanup();
      console.log('ready');
    `);
    const supervisor = spawn(process.execPath, ['--input-type=module', '-e', `
      import { superviseLauncher } from ${JSON.stringify(new URL('./dev-local-supervisor.mjs', import.meta.url).href)};
      superviseLauncher(${JSON.stringify(launcher)});
    `], { stdio: ['ignore', 'pipe', 'pipe'] });
    const exited = once(supervisor, 'exit');
    try {
      await once(supervisor.stdout, 'data');
      assert.equal(supervisor.kill(signal), true);
      const deadline = Date.now() + 3000;
      while (!existsSync(marker) && Date.now() < deadline) await delay(20);
      assert.equal(existsSync(marker), true, 'launcher must clean up even if supervisor cannot handle the signal');
      await exited;
    } finally {
      if (supervisor.exitCode === null && supervisor.signalCode === null) supervisor.kill('SIGKILL');
      await exited;
      rmSync(dir, { recursive: true, force: true });
    }
  });
}

test('shutdown waits for owned processes and escalates an ignored SIGTERM', async t => {
  const child = spawn(process.execPath, ['-e', `
    process.on('SIGTERM', () => {});
    process.on('message', () => process.exit(0));
    process.on('disconnect', () => process.exit(0));
    setInterval(() => {}, 1000);
    process.send('ready');
  `], { detached: true, stdio: ['ignore', 'ignore', 'ignore', 'ipc'] });
  const entry = { child, stopped: false, service: { name: 'stubborn', port: 12345 } };
  const exited = once(child, 'exit');
  child.on('exit', () => { entry.stopped = true; });
  let probes = 0;
  try {
    await once(child, 'message');
    try { process.kill(-child.pid, 0); }
    catch (error) {
      if (error.code !== 'EPERM') throw error;
      t.skip('Sandbox does not permit process-group signals.');
      return;
    }
    await stopServices([entry], { graceMs: 100, killWaitMs: 2000, verifyPorts: async services => {
      assert.deepEqual(services, [entry.service]);
      probes++;
      if (probes === 1) throw new Error('port not released yet');
    } });
    assert.equal(entry.stopped, true);
    assert.equal(child.signalCode, 'SIGKILL');
    assert.ok(probes >= 2, 'waits for the port-release check to pass');
    assert.throws(() => process.kill(-child.pid, 0), { code: 'ESRCH' });
  } catch (error) {
    if (error.code !== 'EPERM') throw error;
    t.skip('Sandbox blocks verification of exited process groups.');
  } finally {
    if (!entry.stopped && child.connected) child.send('stop');
    await exited;
  }
});

test('shutdown reports an occupied port instead of claiming success or killing its owner', async () => {
  await assert.rejects(stopServices([{ child: {}, stopped: true, service: { port: 12345 } }], {
    graceMs: 0, killWaitMs: 10, verifyPorts: async () => { throw new Error('Port 12345 is already in use.'); },
  }), /shutdown incomplete: Port 12345 is already in use/);
});

test('shutdown keeps tracking descendants after the direct child exits', async t => {
  let descendantAlive = true;
  const sent = [];
  t.mock.method(process, 'kill', (pid, signal) => {
    assert.equal(pid, -123456, 'signals only the group started by this launcher');
    if (signal === 0) {
      if (descendantAlive) return true;
      throw Object.assign(new Error('gone'), { code: 'ESRCH' });
    }
    sent.push(signal);
    if (signal === 'SIGKILL') descendantAlive = false;
    return true;
  });
  let checks = 0;
  await stopServices([{ child: { pid: 123456 }, stopped: true, service: { port: 12345 } }], {
    graceMs: 10, killWaitMs: 500, verifyPorts: async () => {
      checks++;
      if (checks < 2) throw new Error('still closing');
    },
  });
  assert.deepEqual(sent, ['SIGTERM', 'SIGKILL']);
  assert.equal(descendantAlive, false);
  assert.ok(checks >= 2);
});

async function fixture(directory, name, extra = {}) {
  const port = await freePort();
  return { name, port, health: 'http://127.0.0.1:' + port,
    command: process.execPath, args: ['--input-type=module', '-e', server],
    env: { ...process.env, FIXTURE_PORT: String(port), ...extra },
    cwd: directory, log: path.join(directory, name + '.log'), matches: text => text === 'fixture-ready' };
}

test('local overrides reconnect App and MCP without giving them Gateway secrets', () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), 'apiosk-dev-config-'));
  try {
    for (const folder of ['gateway', 'app', 'mcp']) mkdirSync(path.join(dir, folder));
    writeFileSync(path.join(dir, 'gateway/.env'), 'DATABASE_URL=postgresql://test:test@localhost/test\nEXECUTOR_DATABASE_URL=postgresql://commerce:test@localhost/test\nSUPABASE_URL=https://project.example\nSUPABASE_PUBLISHABLE_KEY=public\nSTATE_SIGNING_KEY=' + 'x'.repeat(32) + '\nINTENT_API_KEY=server-only-model-key\nTREASURY_PRIVATE_KEY=server-only-wallet\nV2_EXECUTION_ENABLED=1\n');
    writeFileSync(path.join(dir, 'app/.env.local'), 'VITE_GATEWAY_V2_URL=https://old.example\nAPIOSK_DEV_GATEWAY_TARGET=https://old.example\n');
    const c = localConfiguration(dir, {});
    assert.equal(c.app.VITE_GATEWAY_V2_URL, 'http://127.0.0.1:8082');
    assert.equal(c.app.APIOSK_DEV_GATEWAY_TARGET, '');
    assert.equal(c.mcp.APIOSK_GATEWAY_V2_URL, c.app.VITE_GATEWAY_V2_URL);
    assert.equal(c.mcp.APIOSK_GATEWAY_URL, 'https://project.example/functions/v1/agent-gateway');
    assert.equal(c.gateway.V2_EXECUTION_ENABLED, '0');
    assert.equal(c.gateway.RECURRING_MANUAL_TRIGGER_ENABLED, '0');
    assert.equal(c.gateway.RECURRING_LOCAL_MANAGEMENT_ENABLED, '1');
    assert.equal(c.gateway.WORKSPACE_CONTEXT_ENABLED, 'true');
    assert.equal(c.gateway.OPENROUTER_API_KEY, 'server-only-model-key');
    for (const env of [c.app, c.mcp]) {
      assert.equal(env.TREASURY_PRIVATE_KEY, undefined);
      assert.equal(env.INTENT_API_KEY, undefined);
      assert.equal(env.STATE_SIGNING_KEY, undefined);
    }
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('readiness waits for every service and smoke mode releases every port', async () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), 'apiosk-dev-smoke-'));
  try {
    const services = await Promise.all(['one', 'two', 'three'].map(name => fixture(dir, name)));
    let ready = 0;
    await runServices(services, { timeoutMs: 5000, stopWhenReady: true, pollMs: 20,
      onStatus() {}, onReady() { ready++; } });
    assert.equal(ready, 1);
    await checkPorts(services);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('one failed service stops its already-running sibling', async () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), 'apiosk-dev-failure-'));
  try {
    const services = [await fixture(dir, 'healthy'), await fixture(dir, 'fails', { FIXTURE_FAIL: '1' })];
    await assert.rejects(runServices(services, { timeoutMs: 5000, pollMs: 20, onStatus() {} }), /fails stopped \(7\)/);
    await checkPorts(services);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('stop signal closes all services without killing an existing port owner', async () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), 'apiosk-dev-stop-'));
  const owner = net.createServer();
  try {
    await new Promise(resolve => owner.listen(0, '127.0.0.1', resolve));
    await assert.rejects(checkPorts([{ port: owner.address().port }]), /already in use/);
    assert.equal(owner.listening, true);
    const services = [await fixture(dir, 'stop-one'), await fixture(dir, 'stop-two')];
    const controller = new AbortController();
    await runServices(services, { signal: controller.signal, timeoutMs: 5000, pollMs: 20,
      onStatus() {}, onReady() { controller.abort(); } });
    await checkPorts(services);
  } finally {
    await new Promise(resolve => owner.close(resolve));
    rmSync(dir, { recursive: true, force: true });
  }
});

test('recurring configuration never blocks local startup and stays disabled when incomplete', () => {
  for (const secret of [undefined, '', 'too-short', ' '.repeat(32)]) {
    const gateway = { RECURRING_ALLOWED_OWNER_IDS: 'owner', RECURRING_WORKER_SECRET: secret,
      RECURRING_ORDERS_ENABLED: '1', RECURRING_MANUAL_TRIGGER_ENABLED: '1' };
    assert.equal(enableLocalRecurring(gateway), false);
    assert.equal(gateway.RECURRING_ORDERS_ENABLED, '0');
    assert.equal(gateway.RECURRING_MANUAL_TRIGGER_ENABLED, '0');
    assert.equal(gateway.RECURRING_WORKER_SECRET, secret, 'never invent a replacement credential');
  }
  for (const owners of [undefined, '', '   ']) {
    const gateway = { RECURRING_ALLOWED_OWNER_IDS: owners, RECURRING_WORKER_SECRET: 'shared-with-hosted-executor-1234567890' };
    assert.equal(enableLocalRecurring(gateway), false);
    assert.equal(gateway.RECURRING_ORDERS_ENABLED, '0');
    assert.equal(gateway.RECURRING_MANUAL_TRIGGER_ENABLED, '0');
  }
  const gateway = { RECURRING_ALLOWED_OWNER_IDS: 'owner', RECURRING_WORKER_SECRET: 'shared-with-hosted-executor-1234567890' };
  assert.equal(enableLocalRecurring(gateway), true);
  assert.equal(gateway.RECURRING_WORKER_SECRET, 'shared-with-hosted-executor-1234567890');
  assert.equal(gateway.RECURRING_ORDERS_ENABLED, '1');
  assert.equal(gateway.RECURRING_MANUAL_TRIGGER_ENABLED, '1');
});

test('local recurring reuses only an existing sufficiently long worker credential', () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), 'apiosk-recurring-secret-'));
  try {
    const file = path.join(dir, 'recurring-worker-secret');
    assert.equal(readExistingWorkerSecret(file), undefined);
    writeFileSync(file, 'too-short', { mode: 0o600 });
    assert.equal(readExistingWorkerSecret(file), undefined);
    const secret = 's'.repeat(64);
    writeFileSync(file, secret, { mode: 0o600 });
    assert.equal(readExistingWorkerSecret(file), secret);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
