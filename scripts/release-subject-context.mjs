#!/usr/bin/env node
// Run from any directory. Every failed check stops the release.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
const cwd = new URL('..', import.meta.url);
function run(command, args, capture = false) {
  const result = spawnSync(command, args, { cwd, encoding: 'utf8', stdio: capture ? ['ignore', 'pipe', 'inherit'] : 'inherit' });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} failed (${result.status}); release stopped.`);
  return result.stdout?.trim();
}
assert.equal(run('git', ['branch', '--show-current'], true), 'main', 'Release requires main');
assert.equal(run('git', ['status', '--porcelain'], true), '', 'Commit the reviewed changes before deployment');
const sha = run('git', ['rev-parse', 'HEAD'], true);
assert.equal(run('git', ['ls-remote', 'origin', 'refs/heads/main'], true)?.split(/\s+/)[0], sha, 'Local main must match published main');
run('cargo', ['test', '--locked', '--lib', 'live_subject_release', '--', '--ignored', '--nocapture']);
run('flyctl', ['deploy', '--app', 'apiosk-gateway-v2', '--remote-only', '--build-arg', `GIT_SHA=${sha}`, '--yes']);
const response = await fetch('https://apiosk-gateway-v2.fly.dev/health', { signal: AbortSignal.timeout(20_000) });
assert.equal(response.status, 200);
const health = await response.json();
assert.equal(health.version, sha, 'Live Gateway must report the released commit');
assert.equal(health.payment_executor, 'ready');
run('flyctl', ['checks', 'list', '--app', 'apiosk-gateway-v2']);
run('node', ['scripts/verify-request-release.mjs']);
console.log(`Verified Gateway release ${sha}`);
