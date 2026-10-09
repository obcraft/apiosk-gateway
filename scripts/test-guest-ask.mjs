import { spawn } from 'node:child_process';
import { once } from 'node:events';

const cwd = new URL('../', import.meta.url);
const port = process.env.PGTEST_PORT || '54348';
const env = { ...process.env, PGTEST_PORT: port, TEST_DATABASE_URL: `postgresql://postgres@127.0.0.1:${port}/postgres?sslmode=disable` };
const db = spawn(process.execPath, ['tests/support-db.mjs'], { cwd, env, stdio: ['ignore', 'pipe', 'inherit'] });
const closed = once(db, 'exit');
const ready = new Promise((resolve, reject) => {
  const timer = setTimeout(() => reject(new Error('Test database startup timeout')), 20_000);
  db.stdout.on('data', (chunk) => {
    process.stdout.write(chunk);
    if (chunk.toString().includes('Test PostgreSQL')) { clearTimeout(timer); resolve(); }
  });
  db.once('exit', (code) => { clearTimeout(timer); reject(new Error(`Test database exited: ${code}`)); });
});

try {
  await ready;
  for (const args of [
    ['test', '--offline', '--lib', 'guest_adoption', '--', '--ignored', '--nocapture'],
    ['test', '--offline', '--test', 'guest_ask_flow', '--', '--ignored', '--nocapture'],
  ]) {
    const cargo = spawn('cargo', args, { cwd, env, stdio: 'inherit' });
    const [code] = await once(cargo, 'exit');
    if (code !== 0) { process.exitCode = code ?? 1; break; }
  }
} finally {
  db.kill('SIGTERM');
  await closed;
}
