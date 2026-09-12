#!/usr/bin/env python3
"""Reviewed fixed-price JSON reads; preserve original migration fixtures."""
import copy,json,re,uuid
from pathlib import Path
R=Path(__file__).resolve().parents[1]
a=json.loads((R/'output/sirenic-20260913/openapi.json').read_text())
f=copy.deepcopy(json.loads((R/'tests/fixtures/sirenic/catalog.json').read_text()))
uid=lambda s:str(uuid.uuid5(uuid.NAMESPACE_URL,'https://api.sirenic.eu/'+s))
words={w['slug']:w for w in f['vocabulary']}
def word(slug,schema,description,jurisdiction=None):
 if slug not in words:
  words[slug]=dict(slug=slug,name=description[:100],description=description,subject_kind='company',jurisdiction=jurisdiction,value_schema=schema,status='active',freshness_seconds=3600)
 return slug
country_ids={c:word('company.registration.'+c.lower()+('.companies_house' if c=='GB' else '.national_id'),{'type':'string','pattern':'^[A-Z0-9]{8}$' if c=='GB' else '^[A-Za-z0-9.-]{1,20}$'},f'Official {c} company register identifier. Obtain from that country register search or explicit user input; never use another country identifier.',c) for c in ['BE','FI','NO','GB','LV','EE','SE','DK','CZ','PL','SK','ES']}
# Optional projections must never bind another country's ID or mark a complete
# search partial simply because it has no companies from every country.
for s in f['services']:
 if s['path'] in ['/v1/eu/recherche','/v1/eu/entreprise/{pays}/{id}']:
  s['id']=uid('contract:v2:'+s['path']);s['contract']['version']=2
  s['contract']['output_map']['produces'].update({kind:{'adapter':'record.country-identifier','key':c} for c,kind in country_ids.items()})
  s['optional_outputs']=list(country_ids.values())
# New direct reads share existing identifier facts wherever possible.
siren='company.registration.fr.siren'
common={'siren':siren,'q':'company.name','name':'company.name','pays':'company.country_code','id':'company.registration.eu.national_id'}
excluded={};existing={s['path'] for s in f['services']};new=[]
for path,methods in a['paths'].items():
 op=methods.get('get');
 if not op or path in existing:continue
 reason=None
 sample=op.get('responses',{}).get('200',{}).get('content',{}).get('application/json',{}).get('example')
 price=op.get('x-price','')
 if '/surveillance' in path:reason='State-changing monitoring subscription; separate approval/lifecycle required.'
 elif path.endswith('/dossier') and '/entreprise/{siren}' in path:reason='Price depends on selected blocks despite the advertised base price.'
 elif not re.fullmatch(r'\$[0-9]+\.[0-9]+',price):reason='Free utility or variable-priced batch; not a fixed-price data operation.'
 elif not isinstance(sample,dict):reason='Duplicate profile alias or binary response; no separate JSON operation.'
 elif '{reference}' in path or '{date_cloture}' in path:reason='Requires a selected filing reference and document-level binding, not a guessed date.'
 if reason:excluded[path]=reason;continue
 # BE/FI filings and BE/DE dealing endpoints need separate, country-pinned
 # contracts so the planner cannot buy the wrong country's operation.
 variants=['BE','FI'] if path=='/v1/eu/entreprise/{pays}/{id}/comptes' else ['BE','DE'] if path.endswith('/transactions-dirigeants') else [None]
 for variant in variants:
  c=variant or (re.search(r'/eu/entreprise/([A-Z]{2})/',path).group(1) if re.search(r'/eu/entreprise/([A-Z]{2})/',path) else None)
  key=re.sub(r'[^a-z0-9]+','-',path.replace('/v1/','').replace('{','').replace('}','').lower()).strip('-')+(('-'+variant.lower()) if variant else '')
  cap='company.sirenic-'+key
  endpoint_id=uid('endpoint:'+path)
  required=[];inputs={};props={};required_names=[];fixed={};check_inputs={}
  for p in op.get('parameters',[]):
   n=p['name'];schema=copy.deepcopy(p['schema']);schema.pop('format',None)
   if p.get('schema',{}).get('format')=='date':schema['pattern']='^[0-9]{4}-[0-9]{2}-[0-9]{2}$'
   if schema.get('type')=='string':schema.setdefault('maxLength',500);schema.setdefault('minLength',1)
   if n=='id' and '{regnr}' in path:n='regnr' # documented LV path/parameter typo
   props[n]={**schema,'description':p.get('description',n),'x-parameter-location':p['in']}
   req=p.get('required',False)
   if path=='/v1/regulateurs/fr/alertes' and n=='nom':req=True
   if req:required_names.append(n)
   if variant and n=='pays':fixed[n]=variant;check_inputs[n]=variant;continue
   if n=='siren':kind=siren
   elif c and n in ['id','company_number','regnr','registrikood','orgnr','cvr','ico','krs']:
    if c=='DE':kind=word('company.registration.lei',{'type':'string','pattern':'^[A-Z0-9]{18}[0-9]{2}$'},'Legal Entity Identifier from the official GLEIF register; never infer from a ticker.')
    else:kind=country_ids[c]
   elif n=='nom' and path=='/v1/regulateurs/fr/alertes':kind='company.name'
   elif n in common:kind=common[n]
   else:
    # Different endpoints can give the same parameter name different bounds.
    kind=word('company.sirenic.input.'+key.replace('-','_')+'.'+n,schema,p.get('description',n)+'. Supply explicitly; do not invent identifiers or dates.',c or 'FR')
   required.append({'fact_type':kind,'subject_role':'subject','optional':not req});inputs[kind]={'name':n,'in':p['in']}
   if 'example' in p:check_inputs[n]=p['example']
   elif req and n in sample and isinstance(sample[n],(str,int,bool)):check_inputs[n]=sample[n]
   elif req and n in ['id','company_number','regnr','registrikood','orgnr','cvr','ico']:check_inputs[n]=sample.get('id_national',sample.get('company_number'))
  if variant:
   check_inputs['id']={'BE':'0403170701','FI':'0103396-3','DE':'529900D6BF99LW9R2E68'}[variant]
   # Sirenic's dealing example is UMICORE; its enterprise number is documented
   # at https://www.umicore.com/en/terms-of-use/. BASF Antwerp is not that issuer.
   if variant=='BE' and path.endswith('/transactions-dirigeants'):check_inputs['id']='0401574852'
  check_inputs={k:v for k,v in check_inputs.items() if v is not None}
  schema={'type':'object','properties':props,'required':required_names}
  rec=word('company.sirenic.record.'+key.replace('-','_'),{'type':'object','required':list(sample.keys())[:2]},'Complete '+op['summary']+'. Preserve source, coverage, missing values, document scope and quality notes.',c or 'FR')
  outputs={rec:{'path':'$'}};identifies=None
  if 'siren' in sample:outputs[siren]={'path':'$.siren'};identifies=siren
  contract=dict(capability_slug=cap,version=1,requires=required,input_map=inputs,output_map={'base':'$','produces':outputs},cardinality='one',identifies=identifies,max_fan_out=1)
  s=dict(id=uid('contract:expanded:'+key),endpoint_id=endpoint_id,api_slug='sirenic',name='Sirenic',source_url='https://api.sirenic.eu/openapi.json',method='GET',path=path,listing_type='federated',list_atomic=round(float(price[1:])*1e6),fixed_params=fixed,preconditions={},pagination={},freshness_seconds=3600,jurisdiction=c or 'FR',sample_payload=sample,contract=contract,check_inputs=check_inputs)
  f['services'].append(s);new.append(s)
  desc=op.get('description') or op['responses']['200'].get('description') or op['summary']
  f['capabilities'].append(dict(slug=cap,name=op['summary'],description=(c+': ' if c else '')+desc+' Preserve coverage and provenance. Missing data is not zero; a name match is not proof of identity.',input_contract=schema,output_contract={'type':'object'}))
  if not any(e['id']==endpoint_id for e in f['endpoints']):f['endpoints'].append(dict(id=endpoint_id,path=path,price=s['list_atomic']/1e6,description=op['summary']+'. '+desc,query_params=schema,response_example=sample))
f['vocabulary']=list(words.values());f['description']='Expanded Sirenic fixed-price JSON operations, reviewed against official OpenAPI 2026-09-13. Paid evidence remains separate.'
f['new_words']=[w for w in f['vocabulary'] if w['slug'] not in [x['slug'] for x in json.loads((R/'tests/fixtures/sirenic/catalog.json').read_text())['vocabulary']]]
f['excluded']=excluded
out=R/'tests/fixtures/sirenic/expanded-catalog.json';out.write_text(json.dumps(f,indent=2,ensure_ascii=False)+'\n')
print(json.dumps({'unique_routes':len(set(s['path'] for s in f['services'])),'contracts':len(f['services']),'additional_routes':len(set(s['path'] for s in new)),'verification_cap_usd':sum(s['list_atomic'] for s in new)/1e6,'missing_example_inputs':[(s['path'],[r['fact_type'] for r in s['contract']['requires'] if not r['optional'] and s['contract']['input_map'][r['fact_type']]['name'] not in s['check_inputs']]) for s in new if any(not r['optional'] and s['contract']['input_map'][r['fact_type']]['name'] not in s['check_inputs'] for r in s['contract']['requires'])]},indent=2))
