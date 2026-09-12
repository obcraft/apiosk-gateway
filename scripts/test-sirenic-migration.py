#!/usr/bin/env python3
"""Exercise Sirenic data migration + triggers against a disposable full schema."""
import json,os,socket,subprocess,tempfile
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
PG=Path(os.environ.get('PG_BIN','/opt/homebrew/opt/postgresql@17/bin'))
f=json.loads((ROOT/'tests/fixtures/sirenic/catalog.json').read_text())
q=lambda s:"'"+str(s).replace("'","''")+"'"
with tempfile.TemporaryDirectory(prefix='sirenic-db-') as tmp:
 data=Path(tmp)/'data'
 with socket.socket() as sock: sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]
 subprocess.run([str(PG/'initdb'),'-D',str(data),'-U','postgres','-A','trust','--no-locale'],check=True,stdout=subprocess.DEVNULL)
 subprocess.run([str(PG/'pg_ctl'),'-D',str(data),'-l',str(Path(tmp)/'log'),'-o',f'-h 127.0.0.1 -p {port} -k {tmp} -c wal_level=logical','-w','start'],check=True,stdout=subprocess.DEVNULL)
 def sql(s):
  r=subprocess.run([str(PG/'psql'),'-X','-qAt','-v','ON_ERROR_STOP=1','-h','127.0.0.1','-p',str(port),'-U','postgres','-d','postgres'],input=s,text=True,capture_output=True)
  if r.returncode: raise RuntimeError(r.stderr[-5000:])
  return r.stdout.strip()
 try:
  sql((ROOT/'output/schema-physical-20260912/local-restore.sql').read_text())
  sql('BEGIN;'+(ROOT/'supabase/migrations/20260912130830_physically_organize_domain_tables.sql').read_text()+'COMMIT;')
  sql((ROOT/'supabase/migrations/20260912131123_preserve_domain_column_access.sql').read_text())
  seed="""SET session_replication_role=replica;
  INSERT INTO providers.apis(id,owner_id,slug,name,origin_url,wallet_address,status,is_verified,discoverable,listing_type) VALUES('804c77ce-bac1-4880-9e83-04f8b701b839','00000000-0000-4000-8000-000000000001','sirenic','Sirenic','https://api.sirenic.eu/.well-known/x402','0x76A672EEe56D29D475b0715cc03B8C99D70EC8A2','active',true,false,'federated');
  INSERT INTO wallets.networks(id,name,display_name,chain_id,rpc_url,is_active,is_testnet) VALUES('6a2b3af8-fa63-4c08-be00-ee5824796e1e','base','Base',8453,'https://mainnet.base.org',true,false);
  INSERT INTO wallets.tokens(id,name,symbol,decimals,is_active) VALUES('577c4a1e-e515-4138-83b4-4e96906bf6e8','USD Coin','USDC',6,true);
  INSERT INTO wallets.network_tokens(id,token_id,network_id,contract_address) VALUES('26adabc5-92ec-44f3-9540-3bd636eeb1f4','577c4a1e-e515-4138-83b4-4e96906bf6e8','6a2b3af8-fa63-4c08-be00-ee5824796e1e','0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913');
  INSERT INTO catalog.fact_subject_kinds(slug,description) VALUES('company','Company') ON CONFLICT DO NOTHING;
  INSERT INTO providers.api_endpoints(id,api_id,path,method,price,active) VALUES('ae26f7f0-75fc-4d1b-b112-4ddb25c6e041','804c77ce-bac1-4880-9e83-04f8b701b839','/v1/recherche','POST',0.001,true),('a8dfad7f-dfc5-4ee2-b700-718bd9f0144c','804c77ce-bac1-4880-9e83-04f8b701b839','/v1/entreprise/552032534','GET',0.005,true);
  """
  for w in f['vocabulary']:
   if w['slug'] in ['company.name','company.search.country_code']:
    seed+=f"INSERT INTO catalog.fact_types(slug,name,description,subject_kind,value_schema,status) VALUES({q(w['slug'])},{q(w['name'])},'fixture','company',{q(json.dumps(w['value_schema']))},'active');\n"
  sql(seed+'SET session_replication_role=origin;')
  migration=(ROOT/'supabase/migrations/20260912202445_sirenic_planner_source.sql').read_text()
  repair=(ROOT/'supabase/migrations/20260912202706_sirenic_search_request_methods.sql').read_text()
  sql(migration);sql(repair);sql(migration);sql(repair)
  assert sql("SELECT count(*) FROM catalog.capability_execution_contracts WHERE status='active'")=='6'
  assert sql("SELECT active FROM providers.api_endpoints WHERE path='/v1/entreprise/552032534'")=='f'
  manifest=json.loads((ROOT/'supabase/physical-domain-schemas.json').read_text())
  snapshot=sql('SET search_path TO '+manifest['search_path']+';'+(ROOT/'src/catalog.sql').read_text())
  parsed=json.loads(snapshot);assert len(parsed['services'])==6
  assert all(s['method']=='GET' for s in parsed['services'])
  assert sorted(s['list_atomic'] for s in parsed['services'])==[2000,3000,5000,10000,10000,150000]
  (ROOT/'output/sirenic-20260912/local-catalog.json').write_text(json.dumps(parsed,indent=2)+'\n')
  env=dict(os.environ, SIRENIC_ROUTE_TEST_DATABASE_URL=f'postgres://postgres@127.0.0.1:{port}/postgres?options=-csearch_path%3D'+manifest['search_path'].replace(' ',''))
  subprocess.run(['cargo','test','--offline','--manifest-path',str(ROOT/'legacy-executor/Cargo.toml'),'--bin','apiosk-gateway','database_resolves_active_templates_and_ignores_retired_literal_routes','--','--ignored'],env=env,check=True)
  print('PASS: actual schema/triggers, repeat-safe migration, stale routes withdrawn, six runtime-visible contracts')
  if '--expansion' in __import__('sys').argv:
   expanded=json.loads((ROOT/'tests/fixtures/sirenic/expanded-catalog.json').read_text())
   for w in expanded['new_words']:
    if w['slug'] in ['company.registration.gb.companies_house','company.registration.lei']:
     sql(f"INSERT INTO catalog.fact_types(slug,name,description,subject_kind,jurisdiction,value_schema,status) VALUES({q(w['slug'])},{q(w['name'])},'fixture','company',{q(w['jurisdiction']) if w['jurisdiction'] else 'NULL'},{q(json.dumps(w['value_schema']))},'active');")
   stage=next((ROOT/'supabase/migrations').glob('*_sirenic_expanded_company_routes.sql')).read_text()
   sql(stage);sql(stage)
   assert sql("SELECT count(*) FROM catalog.capability_execution_contracts WHERE status='active'")=='6'
   assert sql("SELECT count(*) FROM catalog.capability_execution_contracts WHERE status='draft'")=='63'
   ids=','.join(q(x['id']) for x in expanded['services'])
   sql("UPDATE catalog.capability_execution_contracts SET status='retired' WHERE status='active' AND id NOT IN ("+ids+"); UPDATE catalog.capability_execution_contracts SET status='active',validated_at=now() WHERE id IN ("+ids+");")
   snapshot=sql('SET search_path TO '+manifest['search_path']+';'+(ROOT/'src/catalog.sql').read_text())
   parsed=json.loads(snapshot);assert len(parsed['services'])==67,len(parsed['services'])
   (ROOT/'output/sirenic-20260913/local-catalog.json').write_text(json.dumps(parsed,indent=2)+'\n')
   print('PASS: repeat-safe expansion, originals remain active, 63 drafts, 67 runtime-visible contracts after local-only promotion')
   if '--promote' in __import__('sys').argv:
    originals=','.join(q(x['id']) for x in f['services'])
    sql("UPDATE catalog.capability_execution_contracts SET status='draft',validated_at=NULL; UPDATE catalog.capability_execution_contracts SET status='active',validated_at=now() WHERE id IN ("+originals+");")
    promotion=next((ROOT/'supabase/migrations').glob('*_sirenic_verified_company_routes.sql')).read_text()
    assert len(promotion)>100
    sql(promotion);sql(promotion)
    parsed=json.loads(sql('SET search_path TO '+manifest['search_path']+';'+(ROOT/'src/catalog.sql').read_text()))
    expected=json.loads((ROOT/'output/sirenic-20260913/promotion-summary.json').read_text())['active_contracts']
    assert len(parsed['services'])==expected
    (ROOT/'output/sirenic-20260913/promoted-local-catalog.json').write_text(json.dumps(parsed,indent=2)+'\n')
    print(f'PASS: final promotion applied twice; {expected} verified contracts, failed candidates excluded')


 finally:
  subprocess.run([str(PG/'pg_ctl'),'-D',str(data),'-m','immediate','stop'],stdout=subprocess.DEVNULL)
