#!/usr/bin/env python3
"""Exercise Bright Data admission on disposable local PostgreSQL; no provider calls."""
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
pgbin = Path(os.environ.get('PG_BIN', '/opt/homebrew/opt/postgresql@16/bin'))
if not (pgbin / 'initdb').exists():
    found = shutil.which('initdb')
    if not found:
        raise SystemExit('Set PG_BIN to a local PostgreSQL bin directory')
    pgbin = Path(found).parent
with tempfile.TemporaryDirectory(prefix='apiosk-brightdata-admission-') as tmp:
    data = Path(tmp) / 'data'
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    subprocess.run([str(pgbin/'initdb'), '-D', str(data), '-U', 'postgres', '-A', 'trust', '--no-locale'], check=True, stdout=subprocess.DEVNULL)
    subprocess.run([str(pgbin/'pg_ctl'), '-D', str(data), '-l', str(Path(tmp)/'postgres.log'), '-o', f'-h 127.0.0.1 -p {port} -k {tmp}', '-w', 'start'], check=True)
    try:
        env = {**os.environ, 'BRIGHTDATA_ADMISSION_TEST_DATABASE_URL': f'postgresql://postgres@127.0.0.1:{port}/postgres?sslmode=disable'}
        result = subprocess.run(['cargo', 'test', '--offline', '--test', 'brightdata_admission', '--', '--ignored', '--nocapture'], cwd=root, env=env)
    finally:
        subprocess.run([str(pgbin/'pg_ctl'), '-D', str(data), '-m', 'immediate', '-w', 'stop'], check=True)
    raise SystemExit(result.returncode)
