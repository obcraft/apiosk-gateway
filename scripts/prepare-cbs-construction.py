"""Capture reviewed, bounded official CBS construction data and render its catalog migration."""
from pathlib import Path
import json, urllib.request, urllib.parse, uuid, datetime
base=json.loads(Path('tests/fixtures/public-statistics/catalog.json').read_text())
file=Path('tests/fixtures/cbs-construction.json')
fixture=json.loads(file.read_text()) if file.exists() else {'vocabulary':[], 'services':[], 'capabilities':[]}
if not fixture['services']:
 parts=[('annual','Observations',{'$filter':"Bedrijfsgrootte eq 'WP19077' and BedrijfstakkenBranchesSBI2008 eq '350000' and contains(Perioden,'JJ')",'$count':True,'$orderby':'Perioden desc'},'Annual Dutch construction turnover, all published years. Total F Bouwnijverheid, all business sizes. Index 2021=100 and year-on-year percentage; pair with construction codebooks. Not the risers/fallers survey.'),('measures','MeasureCodes',{'$select':'Identifier,Title,Unit'},'Measures and units for construction turnover table 85809NED.'),('periods','PeriodenCodes',{'$filter':"contains(Identifier,'JJ')",'$select':'Identifier,Title,Status'},'Annual period labels and provisional/definitive status for construction turnover table 85809NED.'),('sectors','BedrijfstakkenBranchesSBI2008Codes',{'$filter':"Identifier eq '350000'",'$select':'Identifier,Title'},'Total construction sector definition in table 85809NED.'),('sizes','BedrijfsgrootteCodes',{'$filter':"Identifier eq 'WP19077'",'$select':'Identifier,Title'},'Total business size definition in construction turnover table 85809NED.'),('metadata','Properties',{},'Publication date, source description and revision policy for construction turnover table 85809NED.')]
 for kind,part,query,desc in parts:
  u='https://datasets.cbs.nl/odata/v1/CBS/85809NED/'+part+('?' +urllib.parse.urlencode({k:str(v).lower() if isinstance(v,bool) else v for k,v in query.items()}) if query else '')
  d=json.load(urllib.request.urlopen(u,timeout=20)); assert '@odata.nextLink' not in d
  if kind=='annual': assert d['@odata.count']==len(d['value'])
  slug='statistics.cbs-construction-'+kind; word='cbs.construction_'+kind
  schema={'type':'object','required':['Identifier','Title'] if kind=='metadata' else ['value'],'properties':{} if kind=='metadata' else {'value':{'type':'array','items':{'type':'object'}}}}
  fixture['vocabulary'].append({'slug':word,'value_schema':schema}); fixture['capabilities'].append({'slug':slug,'name':'CBS construction '+kind,'description':desc})
  template=next(s for s in base['services'] if s['path'].endswith('/86414NED/Observations')) if kind=='annual' else next(s for s in base['services'] if s['path'].endswith('/MeasureCodes'))
  s=json.loads(json.dumps(template));s.update(id=str(uuid.uuid4()),endpoint_id=str(uuid.uuid4()),path='/odata/v1/CBS/85809NED/'+part,fixed_params=query,sample_payload=d,source_url='https://www.cbs.nl/nl-nl/cijfers/detail/85809NED',evidence={'url':u,'retrieved_at':datetime.datetime.now(datetime.timezone.utc).isoformat()},endpoint_description=desc)
  s['contract'].update(capability_slug=slug,requires=[],input_map={},output_map={'base':'$','produces':{word:{'path':'$'}}});fixture['services'].append(s)
 file.write_text(json.dumps(fixture,ensure_ascii=False,indent=2)+'\n')
def q(s):return "'"+str(s).replace("'","''")+"'"
def j(v):return q(json.dumps(v,ensure_ascii=False))+'::jsonb'
sql=['-- Source-backed CBS construction annual series and codebooks. Existing prices and contracts are preserved.','BEGIN;',"SELECT pg_advisory_xact_lock(hashtext('apiosk-cbs-construction'));", "DO $$ BEGIN IF NOT EXISTS(SELECT 1 FROM providers.apis WHERE slug='cbs-statline' AND status='active' AND is_verified AND discoverable) THEN RAISE EXCEPTION 'CBS source is not active'; END IF; END $$;"]
for w in fixture['vocabulary']:
 sql.append(f"INSERT INTO catalog.fact_types(slug,name,description,subject_kind,value_schema,status,freshness_seconds) VALUES({q(w['slug'])},{q(w['slug'])},'Official CBS construction source response','dataset',{j(w['value_schema'])},'active',3600) ON CONFLICT(slug) DO NOTHING;")
for s,cap in zip(fixture['services'],fixture['capabilities']):
 c=s['contract'];template='/odata/v1/CBS/86414NED/Observations' if cap['slug'].endswith('-annual') else '/odata/v1/CBS/{tableId}/MeasureCodes'
 sql.append(f"INSERT INTO providers.api_endpoints(id,api_id,path,method,price,active,description,query_params,response_body,response_example) SELECT {q(s['endpoint_id'])},e.api_id,{q(s['path'])},e.method,e.price,true,{q(cap['description'])},e.query_params - 'required' #- '{{properties,tableId}}',{j(next(w['value_schema'] for w in fixture['vocabulary'] if w['slug'] in c['output_map']['produces']))},{j(s['sample_payload'])} FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id WHERE a.slug='cbs-statline' AND e.path={q(template)} AND e.active ON CONFLICT(id) DO NOTHING;")
 sql.append(f"INSERT INTO catalog.capabilities(slug,name,description,category,input_contract,output_contract,status) VALUES({q(cap['slug'])},{q(cap['name'])},{q(cap['description'])},'data','{{}}','{{}}','active') ON CONFLICT(slug) DO NOTHING;")
 sql.append(f"INSERT INTO catalog.capability_execution_contracts(id,capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,output_map,cardinality,identifies,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,sample_payload,validated_at,validated_note) VALUES({q(s['id'])},{q(cap['slug'])},{q(s['endpoint_id'])},1,'active','[]',{j([{'fact_type':word,'subject_role':'subject','cardinality':'one'} for word in c['output_map']['produces']])},'{{}}',{j(s['fixed_params'])},{j(c['output_map'])},'one',NULL,1,'{{}}','{{}}','read','per_call',3600,{j(s['sample_payload'])},now(),'Direct official source retrieval, complete annual-series count and codebook extraction regression') ON CONFLICT(id) DO NOTHING;")
sql+=['COMMIT;','']
names=list(Path('supabase/migrations').glob('*_cbs_construction_annual_source.sql')); assert len(names)==1
names[0].write_text('\n\n'.join(sql))
print('Prepared six source-backed contracts:',names[0])
