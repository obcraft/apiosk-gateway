import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { checkPorts, localConfiguration, runServices } from './dev-local.mjs';

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
    writeFileSync(path.join(dir, 'gateway/.env'), 'DATABASE_URL=postgresql://test:test@localhost/test\nSUPABASE_URL=https://project.example\nSUPABASE_PUBLISHABLE_KEY=public\nSTATE_SIGNING_KEY=' + 'x'.repeat(32) + '\nINTENT_API_KEY=server-only-model-key\nTREASURY_PRIVATE_KEY=server-only-wallet\nV2_EXECUTION_ENABLED=1\n');
    writeFileSync(path.join(dir, 'app/.env.local'), 'VITE_GATEWAY_V2_URL=https://old.example\nAPIOSK_DEV_GATEWAY_TARGET=https://old.example\n');
    const c = localConfiguration(dir, {});
    assert.equal(c.app.VITE_GATEWAY_V2_URL, 'http://127.0.0.1:8082');
    assert.equal(c.app.APIOSK_DEV_GATEWAY_TARGET, '');
    assert.equal(c.mcp.APIOSK_GATEWAY_V2_URL, c.app.VITE_GATEWAY_V2_URL);
    assert.equal(c.mcp.APIOSK_GATEWAY_URL, 'https://project.example/functions/v1/agent-gateway');
    assert.equal(c.gateway.V2_EXECUTION_ENABLED, '0');
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
