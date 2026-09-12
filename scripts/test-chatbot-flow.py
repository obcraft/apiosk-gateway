#!/usr/bin/env python3
"""Run the MCP/HTTP/approval/recovery fixture with a fresh isolated local database."""
import argparse
import os
from pathlib import Path
import selectors
import socket
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--suite', choices=['durable_flow', 'research_report_flow'], default='durable_flow')
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]
with socket.socket() as sock:
    sock.bind(('127.0.0.1', 0))
    port = sock.getsockname()[1]
env = {**os.environ, 'PGTEST_PORT': str(port), 'TEST_DATABASE_URL': f'postgresql://postgres@127.0.0.1:{port}/postgres?sslmode=disable'}
db = subprocess.Popen(['node', 'tests/support-db.mjs'], cwd=root, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
try:
    with selectors.DefaultSelector() as selector:
        selector.register(db.stdout, selectors.EVENT_READ)
        if not selector.select(timeout=30):
            raise RuntimeError('Local test database startup timed out')
        line = db.stdout.readline()
        if not line.startswith('Test PostgreSQL on 127.0.0.1:'):
            raise RuntimeError('Local test database failed to start: ' + line)
    result = subprocess.run(['cargo', 'test', '--locked', '--test', args.suite, '--', '--ignored', '--nocapture'], cwd=root, env=env)
finally:
    db.terminate()
    try:
        db.wait(timeout=10)
    except subprocess.TimeoutExpired:
        db.kill()
        db.wait()
raise SystemExit(result.returncode)
