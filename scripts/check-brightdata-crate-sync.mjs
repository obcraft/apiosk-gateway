import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { realpathSync } from 'node:fs';
import { dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('..', import.meta.url));
const canonical = realpathSync(new URL('../crates/brightdata-linkedin/', import.meta.url));
const metadata = JSON.parse(execFileSync('cargo', [
  'metadata', '--no-deps', '--format-version', '1', '--locked', '--offline',
], { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] }));
const shared = metadata.packages.find(pkg => pkg.name === 'brightdata-linkedin');
assert(shared && metadata.workspace_members.includes(shared.id), 'Bright Data must be a workspace member');
assert.equal(realpathSync(dirname(shared.manifest_path)), canonical, 'Unexpected Bright Data crate location');
for (const name of ['apiosk-gateway-v2', 'commerce-runtime']) {
  const consumer = metadata.packages.find(pkg => pkg.name === name);
  const dependency = consumer?.dependencies.find(dep => dep.name === 'brightdata-linkedin');
  assert(dependency?.path, `${name} must use the shared Bright Data path dependency`);
  assert.equal(realpathSync(dependency.path), canonical, `${name} must use the canonical Bright Data crate`);
}
console.log('PASS: gateway and commerce-runtime use the same Bright Data workspace crate');
