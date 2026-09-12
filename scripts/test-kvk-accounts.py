#!/usr/bin/env python3
"""Run concurrency tests against a disposable real PostgreSQL, never production."""
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
bindir = Path(os.environ.get('PG_BIN', '/opt/homebrew/opt/postgresql@17/bin'))
if not (bindir / 'initdb').exists():
    found = shutil.which('initdb')
    if not found:
        raise SystemExit('Set PG_BIN to a PostgreSQL bin directory with initdb and pg_ctl')
    bindir = Path(found).parent
with tempfile.TemporaryDirectory(prefix='apiosk-kvk-test-') as tmp:
    data = Path(tmp) / 'data'
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    subprocess.run([str(bindir/'initdb'), '-D', str(data), '-U', 'postgres', '-A', 'trust', '--no-locale'], check=True, stdout=subprocess.DEVNULL)
    subprocess.run([str(bindir/'pg_ctl'), '-D', str(data), '-l', str(Path(tmp)/'postgres.log'), '-o', f'-h 127.0.0.1 -p {port} -k {tmp}', '-w', 'start'], check=True)
    try:
        env = {**os.environ, 'KVK_TEST_DATABASE_URL': f'postgresql://postgres@127.0.0.1:{port}/postgres?sslmode=disable'}
        result = subprocess.run(['cargo', 'test', '--offline', '-p', 'kvk-accounts', '--', '--include-ignored', '--nocapture'], cwd=root, env=env)
        if result.returncode == 0:
            result = subprocess.run(['cargo', 'test', '--offline', '--test', 'kvk_queue_flow', '--', '--ignored', '--nocapture'], cwd=root, env=env)
    finally:
        subprocess.run([str(bindir/'pg_ctl'), '-D', str(data), '-m', 'immediate', '-w', 'stop'], check=True)
    raise SystemExit(result.returncode)
