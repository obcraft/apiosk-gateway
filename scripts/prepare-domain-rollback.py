#!/usr/bin/env python3
"""Prepare a transaction-only rollback from the schema snapshot (no data copies)."""
import json,re
from pathlib import Path
root=Path(__file__).resolve().parents[1]
m=json.loads((root/'supabase/physical-domain-schemas.json').read_text())
sql=["-- Restore physical locations and function definitions without deleting rows.","BEGIN;", "SET LOCAL lock_timeout='3s';", "SET LOCAL statement_timeout='120s';", "SET LOCAL check_function_bodies=false;"]
# ALTER TABLE accepts regular tables and views and preserves their identities.
for src,dest in m['relations'].items():
 schema,name=src.split('.')
 sql.append(f'ALTER TABLE {dest}.{name} SET SCHEMA {schema};')
text=(root/'output/schema-physical-20260912/before.sql').read_text()
for b in re.split(r'(?=--\n-- Name:)',text):
 match=re.search(r'-- Name: .*?; Type: FUNCTION; Schema: (.*?); Owner:',b)
 if match and match[1] in {'public','gateway_v2','kvk_cache'}:
  start=b.index('CREATE FUNCTION')
  sql.append(b[start:].replace('CREATE FUNCTION','CREATE OR REPLACE FUNCTION',1).rstrip())
sql.extend(["NOTIFY pgrst, 'reload schema';", "COMMIT;"])
p=root/'output/schema-physical-20260912/rollback.sql';p.write_text('\n'.join(sql)+'\n');p.chmod(0o600)
print('Prepared rollback.sql from the original function definitions')
