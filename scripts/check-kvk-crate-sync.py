#!/usr/bin/env python3
"""Check the standalone executor uses the exact reviewed KVK cache implementation."""
from pathlib import Path

root = Path(__file__).resolve().parents[1]
source = root / 'crates/kvk-accounts'
vendored = root / 'legacy-executor/crates/kvk-accounts'
files = [p.relative_to(source) for p in source.rglob('*') if p.is_file()]
for name in files:
    assert (source / name).read_bytes() == (vendored / name).read_bytes(), name
migration = 'supabase/migrations/20260912111106_kvk_annual_accounts_queue.sql'
assert (root / migration).read_bytes() == (root / 'legacy-executor' / migration).read_bytes()
print(f'PASS: {len(files)} KVK crate files and the queue migration match the standalone executor')
