import { spawn } from 'node:child_process';
import { once } from 'node:events';

const cwd = new URL('../', import.meta.url);
const port = process.env.PGTEST_PORT || '54342';
const db = spawn(process.execPath, ['tests/support-db.mjs'], {
  cwd, env: { ...process.env, PGTEST_PORT: port }, stdio: ['ignore', 'pipe', 'inherit'],
});
try {
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('Local test database startup timed out')), 20_000);
    db.stdout.on('data', bytes => {
      if (bytes.toString().includes('Test PostgreSQL')) { clearTimeout(timer); resolve(); }
    });
    db.once('exit', code => { clearTimeout(timer); reject(new Error(`Test database exited: ${code}`)); });
    db.once('error', error => { clearTimeout(timer); reject(error); });
  });
  const test = spawn('cargo', ['test', '--offline', '--test', 'typesafe_flow', '--', '--ignored', '--nocapture'], {
    cwd, env: { ...process.env, TEST_DATABASE_URL: `postgresql://postgres@127.0.0.1:${port}/postgres?sslmode=disable` }, stdio: 'inherit',
  });
  const [code] = await once(test, 'exit');
  process.exitCode = code ?? 1;
} finally {
  db.kill('SIGTERM');
}
