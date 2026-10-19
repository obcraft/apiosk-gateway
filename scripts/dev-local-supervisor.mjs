#!/usr/bin/env node
import { spawn } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

/** Keep an IPC lifeline open so even SIGKILL of this command requests cleanup. */
export function superviseLauncher(script, args = []) {
  const child = spawn(process.execPath, [script, ...args], {
    env: { ...process.env, APIOSK_LOCAL_SUPERVISED: '1' },
    stdio: ['inherit', 'inherit', 'inherit', 'ipc'],
  });
  const stop = () => {
    // Closing the lifeline also works if the launcher is still initializing.
    if (child.connected) child.disconnect();
  };
  const signals = ['SIGINT', 'SIGTERM', 'SIGHUP'];
  for (const signal of signals) process.on(signal, stop);
  const remove = () => {
    for (const signal of signals) process.removeListener(signal, stop);
  };
  child.on('error', error => {
    remove();
    console.error(`Cannot start local Apiosk: ${error.message}`);
    process.exitCode = 1;
  });
  child.on('exit', (code) => {
    remove();
    process.exitCode = code ?? 1;
  });
  return child;
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  superviseLauncher(fileURLToPath(new URL('./dev-local.mjs', import.meta.url)), process.argv.slice(2));
}
