#!/usr/bin/env python3
"""Generate reviewed Sirenic contracts from a pinned OpenAPI snapshot; never pays."""
import json, uuid
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
SNAP=ROOT/'output/sirenic-20260912'
OUT=ROOT/'tests/fixtures/sirenic'
OUT.mkdir(parents=True,exist_ok=True)
api=json.loads((SNAP/'openapi.json').read_text())
uid=lambda s:str(uuid.uuid5(uuid.NAMESPACE_URL,'https://api.sirenic.eu/'+s))
words=[]
def word(slug,schema,description,jurisdiction=None):
 w=dict(slug=slug,name=slug,description=description,subject_kind='company',jurisdiction=jurisdiction,value_schema=schema,status='active',freshness_seconds=3600)
 words.append(w); return slug
name=word('company.name',{'type':'string','minLength':1,'maxLength':500},'Company name')
siren=word('company.registration.fr.siren',{'type':'string','pattern':'^[0-9]{9}$'},'Nine-digit French SIREN; resolve from the French company search, never guess.','FR')
country=word('company.country_code',{'type':'string','pattern':'^[A-Z]{2}$'},'Country of the company register')
search_country=word('company.search.country_code',{'type':'string','pattern':'^[A-Z]{2}$'},'Country filter explicitly supplied for a search')
euid=word('company.registration.eu.national_id',{'type':'string','pattern':'^[A-Za-z0-9.-]{1,20}$'},'National company identifier, meaningful only together with company.country_code')
identity=word('company.registration.eu.identity',{'type':'object','required':['country','id'],'properties':{'country':{'type':'string','pattern':'^[A-Z]{2}$'},'id':{'type':'string','pattern':'^[A-Za-z0-9.-]{1,20}$'}}},'Composite identity: country plus national identifier. Equal IDs in different countries are different companies.')
services=[]; caps=[]; endpoints=[]
def service(path,cap,requires,produces,identifies=None,many=False,jurisdiction=None):
 op=api['paths'][path]['get']; sample=op['responses']['200']['content']['application/json']['example']
 price=round(float(op['x-price'].removeprefix('$'))*1_000_000)
 props={}; required=[]
 for p in op['parameters']:
  props[p['name']]={**p['schema'],'description':p.get('description',p['name']),'x-parameter-location':p['in']}
  if p.get('required'): required.append(p['name'])
 schema={'type':'object','properties':props,'required':required}
 endpoint_id={'/v1/recherche':'ae26f7f0-75fc-4d1b-b112-4ddb25c6e041','/v1/eu/recherche':'0e49f48b-ed5f-4f4c-95a3-aa672b875738'}.get(path,uid('endpoint:'+path))
 contract=dict(capability_slug=cap,version=1,requires=[{'fact_type':f,'subject_role':'subject','optional':optional} for f,n,l,optional in requires],input_map={f:{'name':n,'in':l} for f,n,l,optional in requires},output_map={'base':'$.resultats[*]' if many else '$','produces':produces},cardinality='many' if many else 'one',identifies=identifies,max_fan_out=1)
 services.append(dict(id=uid('contract:'+cap+path),endpoint_id=endpoint_id,api_slug='sirenic',name='Sirenic',source_url='https://api.sirenic.eu/openapi.json',method='GET',path=path,listing_type='federated',list_atomic=price,fixed_params={},preconditions={},pagination={},freshness_seconds=3600,jurisdiction=jurisdiction,sample_payload=sample,contract=contract))
 caps.append(dict(slug=cap,name=op['summary'],description=op.get('description',op['summary'])+' Preserve provenance states, partial coverage, financial quality flags and nulls; null never means zero.',input_contract=schema,output_contract={'type':'object'}))
 endpoints.append(dict(id=endpoint_id,path=path,price=price/1_000_000,description=op.get('description',op['summary']),query_params=schema,response_example=sample))
service('/v1/recherche','company.fr-registry-search',[(name,'q','query',False)],{name:{'path':'$.denomination'},siren:{'path':'$.siren'}},siren,True,'FR')
for path,cap,record,required in [('/v1/entreprise/{siren}','company.registry-profile','company.fr_registry_record',['siren','denomination']),('/v1/entreprise/{siren}/finances','company.fr-financials','company.fr_financial_record',['siren','exercices']),('/v1/kyb/{siren}','company.fr-kyb','company.fr_kyb_record',['siren','identite'])]:
 word(record,{'type':'object','required':required},'Complete Sirenic response, including provenance, quality, missing data and coverage. Nulls are unpublished, not zero.','FR')
 service(path,cap,[(siren,'siren','path',False)],{record:{'path':'$'},siren:{'path':'$.siren'}},siren,False,'FR')
composite={'fields':{'country':{'path':'$.pays'},'id':{'path':'$.id_national'}}}
eu_produces={name:{'path':'$.denomination'},country:{'path':'$.pays'},euid:{'path':'$.id_national'},identity:composite}
service('/v1/eu/recherche','company.eu-registry-search',[(name,'q','query',False),(search_country,'pays','query',True)],eu_produces,identity,True)
record=word('company.eu_registry_record',{'type':'object','required':['pays','id_national','denomination']},'Sirenic European register profile with provenance, register link, freshness and coverage.')
service('/v1/eu/entreprise/{pays}/{id}','company.registry-profile',[(country,'pays','path',False),(euid,'id','path',False)],{**eu_produces,record:{'path':'$'}},identity)
fixture=dict(description='Sirenic official OpenAPI examples captured 2026-09-12. These samples validate contracts, not live paid execution.',vocabulary=words,services=services,new_words=[w for w in words if w['slug'] not in [name,search_country]],capabilities=caps,endpoints=endpoints)
(OUT/'catalog.json').write_text(json.dumps(fixture,indent=2,ensure_ascii=False)+'\n')
(OUT/'search-quote.json').write_text((SNAP/'search-quote.json').read_text())
print(f'{len(services)} Sirenic contracts prepared')
