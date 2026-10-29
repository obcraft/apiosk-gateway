#!/usr/bin/env python3
"""Check gateway and commerce use the same reviewed KVK workspace crate."""
import json
import subprocess
from pathlib import Path

root = Path(__file__).resolve().parents[1]
canonical = (root / 'crates/kvk-accounts').resolve()
metadata = json.loads(subprocess.check_output([
    'cargo', 'metadata', '--no-deps', '--format-version', '1', '--locked', '--offline',
], cwd=root, text=True))
packages = {package['name']: package for package in metadata['packages']}
shared = packages['kvk-accounts']
assert shared['id'] in metadata['workspace_members'], 'KVK must be a workspace member'
assert Path(shared['manifest_path']).resolve().parent == canonical
for name in ('apiosk-gateway-v2', 'commerce-runtime'):
    dependency = next(dep for dep in packages[name]['dependencies'] if dep['name'] == 'kvk-accounts')
    assert dependency.get('path'), f'{name} must use the shared KVK path dependency'
    assert Path(dependency['path']).resolve() == canonical, name
assert (root / 'supabase/migrations/20260912111106_kvk_annual_accounts_queue.sql').is_file()
print('PASS: gateway and commerce-runtime use one KVK workspace crate and the canonical queue migration')
