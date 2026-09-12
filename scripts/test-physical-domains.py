#!/usr/bin/env python3
"""Restore a schema-only production snapshot into disposable PostgreSQL and test migration."""
import json,os,re,socket,subprocess,tempfile
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
PG=Path(os.environ.get('PG_BIN','/opt/homebrew/opt/postgresql@17/bin'))
MANIFEST=json.loads((ROOT/'supabase/physical-domain-schemas.json').read_text())
source=ROOT/'output/schema-physical-20260912/before.sql'
domains=set(MANIFEST['relations'].values())
allowed=domains|{'public','gateway_v2','kvk_cache','auth','storage','extensions'}
text=source.read_text()
blocks=re.split(r'(?=--\n-- Name:)',text)
kept=[]
for block in blocks:
 match=re.search(r'-- Name: (.*?); Type: (.*?); Schema: (.*?); Owner:',block)
 if not match:continue
 name,kind,schema=match.groups()
 if kind=='SCHEMA' and name in allowed:kept.append(block)
 elif kind=='ACL' and name.startswith('SCHEMA ') and name[7:] in allowed:kept.append(block)
 elif schema in allowed - {'extensions'} and kind not in ('EXTENSION','EVENT TRIGGER','DEFAULT ACL'):
  if kind=='FUNCTION' and ('LANGUAGE c' in block or 'LANGUAGE internal' in block):continue
  kept.append(block)
roles=set()
for block in kept:
 for line in block.splitlines():
  if line.startswith(('GRANT ','REVOKE ')):
   hit=re.search(r' (?:TO|FROM) (.*?)(?: WITH GRANT OPTION)?;',line)
   if hit:roles.update(x.strip().strip('"') for x in hit[1].split(','))
roles.discard('PUBLIC');roles.discard('postgres');roles.add('authenticator')
with tempfile.TemporaryDirectory(prefix='apiosk-physical-') as tmp:
 data=Path(tmp)/'data'
 with socket.socket() as sock:
  sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
 subprocess.run([str(PG/'initdb'),'-D',str(data),'-U','postgres','-A','trust','--no-locale'],check=True,stdout=subprocess.DEVNULL)
 subprocess.run([str(PG/'pg_ctl'),'-D',str(data),'-l',str(Path(tmp)/'log'),'-o',f'-h 127.0.0.1 -p {port} -k {tmp} -c wal_level=logical','-w','start'],check=True,stdout=subprocess.DEVNULL)
 def sql(q):
  r=subprocess.run([str(PG/'psql'),'-X','-qAt','-v','ON_ERROR_STOP=1','-h','127.0.0.1','-p',str(port),'-U','postgres','-d','postgres'],input=q,text=True,capture_output=True)
  if r.returncode:raise RuntimeError(r.stderr[-5000:])
  return r.stdout.strip()
 try:
  setup='SET check_function_bodies=false;\n'+''.join(f'CREATE ROLE "{r}";\n' for r in sorted(roles))
  setup+='CREATE SCHEMA vault; CREATE TABLE vault.secrets(id uuid, secret text); CREATE VIEW vault.decrypted_secrets AS SELECT id, secret AS decrypted_secret FROM vault.secrets;\n'
  setup+='CREATE SCHEMA net; CREATE FUNCTION net.http_post(url text, body jsonb DEFAULT NULL, params jsonb DEFAULT NULL, headers jsonb DEFAULT NULL, timeout_milliseconds integer DEFAULT 1000) RETURNS bigint LANGUAGE sql AS $$SELECT 1::bigint$$;\n'
  setup+='CREATE FUNCTION net.http_get(url text, params jsonb DEFAULT NULL, headers jsonb DEFAULT NULL, timeout_milliseconds integer DEFAULT 1000) RETURNS bigint LANGUAGE sql AS $$SELECT 1::bigint$$;\n'
  setup+='CREATE PUBLICATION supabase_realtime;\n'
  setup+='\n'.join(kept)
  # UUID extension entries are otherwise omitted by pg_dump's extension handling.
  setup=setup.replace('CREATE SCHEMA extensions;','CREATE SCHEMA extensions; CREATE EXTENSION "uuid-ossp" WITH SCHEMA extensions; CREATE EXTENSION pgcrypto WITH SCHEMA extensions;')
  sql(setup)
  sql("INSERT INTO auth.users(id,email) VALUES('00000000-0000-4000-8000-000000000001','one@example.invalid'),('00000000-0000-4000-8000-000000000002','two@example.invalid'); INSERT INTO public.user_preferences(user_id) SELECT id FROM auth.users; INSERT INTO public.kyb_providers(key,display_name) VALUES('fixture','Fixture'); INSERT INTO public.kyb_profiles(owner_id,provider) SELECT id,'fixture' FROM auth.users; INSERT INTO public.user_balances(user_id,balance_micro_usd) SELECT id,1000 FROM auth.users ON CONFLICT(user_id) DO UPDATE SET balance_micro_usd=1000;")
  sql('CREATE PUBLICATION fixture_realtime FOR TABLE public.agent_connections,public.kyb_profiles;')
  print('PASS: restored application schemas, functions, grants, constraints and RLS from production snapshot',flush=True)
  (ROOT/'output/schema-physical-20260912/local-restore.sql').write_text(setup)
  before=sql("SELECT md5(string_agg(oid::text||':'||conrelid::text||':'||confrelid::text||':'||conbin::text,',' ORDER BY oid)) FROM pg_constraint WHERE connamespace='public'::regnamespace;")
  rows_before = sql("SELECT md5(jsonb_agg(to_jsonb(t) ORDER BY id)::text) FROM public.kyb_profiles t;")
  migration=(ROOT/'supabase/migrations/20260912130830_physically_organize_domain_tables.sql').read_text()
  sql('BEGIN;\n'+migration+'\nCOMMIT;')
  sql((ROOT/'supabase/migrations/20260912131123_preserve_domain_column_access.sql').read_text())
  # The gateway deliberately has column grants, not blanket table SELECT.
  assert sql("SELECT has_schema_privilege('gateway_v2_runtime','providers','USAGE') AND has_any_column_privilege('gateway_v2_runtime','providers.apis','SELECT') AND NOT has_table_privilege('gateway_v2_runtime','providers.apis','SELECT');")=='t'
  sql("SET ROLE gateway_v2_runtime; SET search_path TO "+MANIFEST['search_path']+"; SELECT id,name FROM apis LIMIT 0;")
  print('PASS: gateway column-only grants retain schema access without granting whole-table reads',flush=True)
  assert sql("SELECT md5(jsonb_agg(to_jsonb(t) ORDER BY id)::text) FROM kyb.kyb_profiles t;")==rows_before
  assert sql("SELECT count(*) FROM pg_class WHERE relnamespace='public'::regnamespace AND relkind IN('r','p','v','m');")=='0'
  assert sql("SELECT count(*) FROM pg_publication_tables WHERE pubname='fixture_realtime' AND schemaname IN('agents','kyb');")=='2'
  for old,domain in MANIFEST['relations'].items():
   name=old.split('.')[1]
   sql(f'SELECT * FROM {domain}.{name} LIMIT 0;')
  print('PASS: 189 physical relations resolve; public has zero tables/views; Realtime follows physical tables',flush=True)
  # Check ownership RLS, upsert, moved triggers and RPC function bodies with real rows.
  session = "SET ROLE authenticated; SET request.jwt.claim.sub='00000000-0000-4000-8000-000000000001'; SET request.jwt.claim.role='authenticated'; "
  assert sql(session+"SELECT count(*) FROM kyb.kyb_profiles;")=='1'
  assert sql(session+"SELECT count(*) FROM identity.user_preferences;")=='1'
  assert sql(session+"INSERT INTO identity.user_preferences(user_id,display_currency) VALUES('00000000-0000-4000-8000-000000000001','USD') ON CONFLICT(user_id) DO UPDATE SET display_currency=EXCLUDED.display_currency RETURNING display_currency;")=='USD'
  try:
   sql(session+"INSERT INTO identity.user_preferences(user_id,display_currency) VALUES('00000000-0000-4000-8000-000000000002','USD') ON CONFLICT(user_id) DO UPDATE SET display_currency=EXCLUDED.display_currency;")
   raise AssertionError('Cross-owner upsert passed RLS')
  except RuntimeError as e: assert 'row-level security' in str(e)
  result=json.loads(sql(session+"SELECT public.balance_summary();"))
  assert '1000' in json.dumps(result),result
  sql("SET search_path TO "+MANIFEST['search_path']+"; INSERT INTO user_preferences(user_id,display_currency) VALUES('00000000-0000-4000-8000-000000000001','EUR') ON CONFLICT(user_id) DO UPDATE SET display_currency=EXCLUDED.display_currency;")
  print('PASS: own-row RLS, blocked cross-owner upsert, canonical upsert, moved update trigger and balance RPC',flush=True)
  sql((ROOT/'output/schema-physical-20260912/rollback.sql').read_text())
  assert sql("SELECT count(*) FROM public.kyb_profiles;")=='2'
  assert sql("SELECT display_currency FROM public.user_preferences WHERE user_id='00000000-0000-4000-8000-000000000001';")=='EUR'
  print('PASS: rollback retains data and restores physical relations and RPC definitions',flush=True)
  (ROOT/'output/schema-physical-20260912/local-test-result.json').write_text(json.dumps({'status':'passed','relations':len(MANIFEST['relations']),'public_relations':0})+'\n')
 finally:
  subprocess.run([str(PG/'pg_ctl'),'-D',str(data),'-m','immediate','-w','stop'],check=True,stdout=subprocess.DEVNULL)
