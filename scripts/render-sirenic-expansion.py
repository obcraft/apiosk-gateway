#!/usr/bin/env python3
"""Stage reviewed contracts without enabling untested planner purchases."""
import json
from pathlib import Path
R=Path(__file__).resolve().parents[1];f=json.loads((R/'tests/fixtures/sirenic/expanded-catalog.json').read_text())
q=lambda v:"'"+str(v).replace("'","''")+"'"
j=lambda v:q(json.dumps(v,ensure_ascii=False))+'::jsonb'
api='804c77ce-bac1-4880-9e83-04f8b701b839'
sql=['-- Sirenic expansion: candidate contracts remain draft until paid verification.','BEGIN;']
for w in f['new_words']:
 sql.append(f"INSERT INTO catalog.fact_types(slug,name,description,subject_kind,jurisdiction,value_schema,status,freshness_seconds) VALUES({q(w['slug'])},{q(w['name'])},{q(w['description'])},'company',{q(w['jurisdiction']) if w['jurisdiction'] else 'NULL'},{j(w['value_schema'])},'active',3600) ON CONFLICT(slug) DO NOTHING;")
 sql.append(f"DO $check$ BEGIN IF NOT EXISTS (SELECT 1 FROM catalog.fact_types WHERE slug={q(w['slug'])} AND value_schema={j(w['value_schema'])} AND status='active') THEN RAISE EXCEPTION 'Sirenic fact schema conflict'; END IF; END $check$;")
for cap in f['capabilities']:
 sql.append(f"INSERT INTO catalog.capabilities(slug,name,description,category,input_contract,output_contract,status) VALUES({q(cap['slug'])},{q(cap['name'])},{q(cap['description'])},'company',{j(cap['input_contract'])},{j(cap['output_contract'])},'active') ON CONFLICT(slug) DO NOTHING;")
for e in f['endpoints']:
 sql.append(f"INSERT INTO providers.api_endpoints(id,api_id,path,method,price,payment_required,active,description,query_params,response_body,response_example,tags,endpoint_category,metadata_source,metadata_at,network_token_id) VALUES({q(e['id'])},{q(api)},{q(e['path'])},'GET',{e['price']},true,true,{q(e['description'])},{j(e['query_params'])},'{{\"type\":\"object\"}}',{j(e['response_example'])},ARRAY['company','registry','sirenic'],'company','openapi',now(),'26adabc5-92ec-44f3-9540-3bd636eeb1f4') ON CONFLICT(id) DO UPDATE SET description=EXCLUDED.description,query_params=EXCLUDED.query_params,price=EXCLUDED.price,active=true WHERE api_endpoints.api_id=EXCLUDED.api_id AND api_endpoints.method=EXCLUDED.method AND api_endpoints.path=EXCLUDED.path;")
for s in f['services']:
 c=s['contract'];produces=[dict(fact_type=k,subject_role='subject',cardinality=c['cardinality'],optional=k in s.get('optional_outputs',[])) for k in c['output_map']['produces']]
 sql.append(f"INSERT INTO catalog.capability_execution_contracts(id,capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,output_map,cardinality,identifies,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,sample_payload,validated_note,notes) VALUES({q(s['id'])},{q(c['capability_slug'])},{q(s['endpoint_id'])},{c['version']},'draft',{j(c['requires'])},{j(produces)},{j(c['input_map'])},{j(s['fixed_params'])},{j(c['output_map'])},{q(c['cardinality'])},{q(c['identifies']) if c['identifies'] else 'NULL'},1,'{{}}','{{}}','read','per_call',3600,{j(s['sample_payload'])},'Official OpenAPI and local extraction checks; pending live payment verification.','sirenic-expansion-20260913') ON CONFLICT(id) DO NOTHING;")
sql.append(f"UPDATE providers.apis SET listing_metadata=listing_metadata || {j({'logo':'https://app.apiosk.com/brands/sirenic.png','favicon':'https://app.apiosk.com/brands/sirenic.png'})} WHERE id={q(api)};")
sql+=['COMMIT;',''];content='\n\n'.join(sql)
p=next((R/'supabase/migrations').glob('*_sirenic_expanded_company_routes.sql'));p.write_text(content)
print(p.name)
