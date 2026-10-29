// Actual CommerceRuntime initialization and native route gates, using only an
// in-memory PGlite database. No sibling checkout, .env, API key or live provider.
import { spawn } from 'node:child_process';
import { once } from 'node:events';

const cwd = new URL('../', import.meta.url);
const port = Number(process.env.PGTEST_PORT || '54369');
if (!Number.isInteger(port) || port < 1024 || port > 65535) {
  throw new Error('PGTEST_PORT must be an unprivileged local TCP port');
}

// Pass through toolchain settings, never the caller's application credentials.
const env = {};
for (const name of [
  'PATH', 'HOME', 'USER', 'TMPDIR', 'CARGO_HOME', 'RUSTUP_HOME', 'RUSTUP_TOOLCHAIN',
  'RUSTFLAGS', 'CARGO_BUILD_JOBS', 'CARGO_TARGET_DIR',
  'CARGO_PROFILE_DEV_DEBUG', 'CARGO_PROFILE_TEST_DEBUG',
]) {
  if (process.env[name] !== undefined) env[name] = process.env[name];
}
Object.assign(env, {
  PGTEST_PORT: String(port),
  CARGO_NET_OFFLINE: 'true',
  RUST_MIN_STACK: '16777216',
  NATIVE_RUNTIME_LOCAL_FIXTURE: '1',
  TEST_DATABASE_URL: `postgresql://postgres@127.0.0.1:${port}/postgres?sslmode=disable`,
  EXECUTOR_DATABASE_URL: `postgresql://postgres@127.0.0.1:${port}/postgres?sslmode=disable`,
  DATABASE_URL: 'postgresql://unused_v2_role@127.0.0.1:9/unused_v2_database',
  GATEWAY_DB_POOL_SIZE: '1',
  GATEWAY_LOG_POOL_SIZE: '1',
  FACILITATOR_URL: 'http://127.0.0.1:9/never-called',
  ALLOW_TESTNET_FACILITATOR: 'true',
  SOURCE_INDEX_ENABLED: 'false',
  TELEMETRY_V2_ENABLED: 'false',
  EXTERNAL_FETCH_ENABLED: 'false',
  RESEARCH_WORKER_ENABLED: '0',
  REPLAY_TAPE: 'strict',
});

const db = spawn(process.execPath, ['tests/support-db.mjs'], {
  cwd, env, stdio: ['ignore', 'pipe', 'inherit'],
});
const closed = once(db, 'exit');
const ready = new Promise((resolve, reject) => {
  let output = '';
  const timer = setTimeout(() => reject(new Error('test database startup timeout')), 20_000);
  db.stdout.on('data', chunk => {
    process.stdout.write(chunk);
    output += chunk.toString();
    if (output.includes('Test PostgreSQL')) {
      clearTimeout(timer);
      resolve();
    }
  });
  db.once('error', error => { clearTimeout(timer); reject(error); });
  db.once('exit', code => { clearTimeout(timer); reject(new Error(`test database exited: ${code}`)); });
});

try {
  await ready;
  const cargo = spawn('cargo', [
    'test', '--locked', '--offline', '--test', 'native_runtime',
    '--', '--ignored', '--nocapture', '--test-threads=1',
  ], { cwd, env, stdio: 'inherit' });
  const [code] = await once(cargo, 'exit');
  process.exitCode = code ?? 1;
} finally {
  db.kill('SIGTERM');
  await closed;
}
