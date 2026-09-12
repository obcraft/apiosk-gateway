#!/usr/bin/env python3
"""Overlay only schema routing onto freshly downloaded production Edge Functions.

Input and output stay under ignored output/. Preserve each deployed JWT setting.
"""
from pathlib import Path
import json,shutil
root=Path(__file__).resolve().parents[1]
work=root/'output/schema-physical-20260912'
meta=json.loads((work/'edge-baseline.json').read_text())
release=work/'edge-release'
shutil.copytree(work/'deployed',release,dirs_exist_ok=True)
shared=release/'supabase/functions/_shared';shared.mkdir(exist_ok=True)
for name in ['domain-fetch.ts','domain-fetch-install.ts']:
 shutil.copy2(root/'deploy/domain-routing'/name,shared/name)
config=['project_id = "jgjoiyqdyypouskftzeq"']
for f in meta:
 name=f['name'];p=release/'supabase/functions'/name/'index.ts'
 if not p.exists():raise RuntimeError(f'Missing downloaded entrypoint: {name}')
 s=p.read_text()
 if 'domain-fetch-install.ts' not in s:p.write_text('import "../_shared/domain-fetch-install.ts"\n'+s)
 config.extend(['',f'[functions.{name}]',f'verify_jwt = {str(f["verify_jwt"]).lower()}',f'entrypoint = "./functions/{name}/index.ts"'])
(release/'supabase/config.toml').write_text('\n'.join(config)+'\n')
print(f'Prepared {len(meta)} functions, preserving deployed source and JWT settings')
