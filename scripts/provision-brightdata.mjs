#!/usr/bin/env node
// Read the key locally and send it to Fly over stdin: never argv, SQL or catalog metadata.
import { readFile } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { parseEnv } from 'node:util';

const args = process.argv.slice(2);
if (args.some(arg => arg !== '--check')) {
  console.error('Usage: node scripts/provision-brightdata.mjs [--check]');
  process.exit(1);
}

try {
  const file = await readFile(new URL('../.env', import.meta.url), 'utf8')
    .catch(error => { if (error.code === 'ENOENT') return ''; throw error; });
  const config = { ...parseEnv(file), ...process.env };
  const key = config.BRIGHTDATA_API_KEY?.trim();
  if (!key || !/^[A-Za-z0-9._~+\/-]+={0,2}$/.test(key)) {
    throw new Error('Set BRIGHTDATA_API_KEY in gateway/.env or the server environment to a valid Bearer API key.');
  }
  if (args.includes('--check')) {
    console.log('BRIGHTDATA_API_KEY is configured locally (value hidden; provider authentication not yet verified).');
  } else {
    const result = spawnSync('fly', ['secrets', 'import', '--stage', '--app', 'apiosk-gateway-v2'], {
      input: `BRIGHTDATA_API_KEY=${key}\n`,
      encoding: 'utf8',
      stdio: ['pipe', 'pipe', 'pipe'],
      timeout: 60_000,
    });
    // Do not print CLI output: errors may echo stdin or environment values.
    if (result.error || result.status !== 0) {
      throw new Error('Could not stage the Fly secret. Check Fly CLI authentication and app access; no secret value was printed.');
    }
    console.log('Bright Data key staged as a Fly server secret. Deploy the gateway adapter before enabling the Supabase listing.');
  }
} catch (error) {
  console.error(error instanceof Error ? error.message : 'Bright Data provisioning failed.');
  process.exitCode = 1;
}
