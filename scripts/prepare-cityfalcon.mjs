// Render reviewed contracts from captured upstream responses, never fabricated rows.
import fs from 'node:fs/promises';
import {createHash} from 'node:crypto';
const evidence=new URL('../../evidence/2026-09-13/catalog-repair/',import.meta.url);
const metadata=JSON.parse(await fs.readFile(new URL('catalog.json',evidence)));
const api=metadata.apis.find(a=>a.slug==='cityfalcon-financial-api');
const docs='https://www.cityfalcon.ai/blog/the-startup-journey/building-financial-intelligence-a-developers-guide-to-our-news-sentiment-and-sector-data-apis/';
const idFor=s=>{const h=createHash('sha256').update('cityfalcon-v2-20260913:'+s).digest('hex');return `${h.slice(0,8)}-${h.slice(8,12)}-4${h.slice(13,16)}-a${h.slice(17,20)}-${h.slice(20,32)}`;};
const object=(properties,required=Object.keys(properties))=>({type:'object',required,properties});
const string={type:'string'},number={type:'number'};
const array=items=>({type:'array',items});
const ticker={type:'string',pattern:'^[A-Za-z0-9.:-]+_[A-Za-z]{2,6}(,[A-Za-z0-9.:-]+_[A-Za-z]{2,6})*$'};
const configs=[
 ['/stories','news','Financial news',object({stories:array(object({uuid:string,title:string,url:string,publishTime:string}))}),['languages','time_filter','categories','country_codes','with_sentiment','with_sentiments_by_entities','min_cityfalcon_score','registration_required'],{identifier_type:'full_tickers'}],
 ['/services/sentiment','sentiment','Market sentiment',object({entities:array(object({name:string,chartData:object({labels:array(string),values:array(number),confidence:array(number)})}))}),['period','average_for_period','statistics_for_period'],{identifier_type:'full_tickers'}],
 ['/insider_transactions','insider-transactions','US insider transactions',array(object({ticker:string,transactionDate:string,transactionType:string,sourceURL:string})),['start_date','end_date','transaction_type','info_type'],{}],
 ['/investor_relations','investor-relations','Investor relations documents',array(object({companyTicker:string,title:string,link:string})),['start_date','end_date','type'],{}],
];
const vocabulary=[{slug:'finance_markets.cityfalcon_identifiers',name:'CityFALCON full tickers',description:'Explicit full ticker identifiers, ticker_COUNTRY (AAPL_US), comma-separated if multiple. Do not guess a ticker or exchange country from a company name.',subject_kind:'finance_markets',value_schema:ticker}];
const services=[],capabilities=[],endpoints=[];
for(const [route,key,title,output,options,fixed_params] of configs){
 const e=metadata.endpoints.find(e=>e.api_id===api.id&&e.path===route);
 const capture=JSON.parse(await fs.readFile(new URL('cityfalcon/'+route.replaceAll('/','_')+'.json',evidence)));
 if(capture.status!==200||capture.endpoint_id!==e.id)throw Error('Missing live response '+route);
 // Small literal excerpts keep migration artifacts reviewable. Full captured
 // responses remain in the audit directory and are separately extraction-tested.
 const pick=(row,keys)=>Object.fromEntries(keys.map(k=>[k,row[k]]));
 if(route==='/stories')capture.data={stories:capture.data.stories.slice(0,2).map(r=>pick(r,['uuid','title','url','publishTime','source']))};
 else if(route==='/services/sentiment')capture.data={entities:capture.data.entities.slice(0,1)};
 else capture.data=capture.data.slice(0,2);
 const required='finance_markets.cityfalcon_identifiers', result='finance_markets.cityfalcon_'+key.replaceAll('-','_');
 const requires=[{fact_type:required,subject_role:'subject',optional:false}];
 const input_map={[required]:{location:'query',name:'identifiers'}};
 for(const p of options){
  const spec=e.query_params.properties[p], slug=`finance_markets.cityfalcon_${key.replaceAll('-','_')}_${p}`;
  const value_schema=Object.fromEntries(Object.entries(spec).filter(([k])=>['type','enum','minimum','maximum','minLength','maxLength','description'].includes(k)));
  if(!value_schema.type)throw Error('Missing declared input type');
  vocabulary.push({slug,name:spec.description||p,description:spec.description||p,subject_kind:'finance_markets',value_schema});
  requires.push({fact_type:slug,subject_role:'subject',optional:true});input_map[slug]={location:'query',name:p};
 }
 vocabulary.push({slug:result,name:title,description:'Complete returned response page, preserving source URLs, dates, attribution and empty results. Not exhaustive historical coverage.',subject_kind:'finance_markets',value_schema:output});
 const capability_slug='finance_markets.cityfalcon-'+key;
 capabilities.push({slug:capability_slug,name:title,description:`CityFALCON ${title.toLowerCase()} for explicit full tickers (AAPL_US). Recent 30-day provider window only; this is the returned page, not an exhaustive historical record. ${key==='insider-transactions'?'US companies only.':''} Use optional period/date fields when requested.`,input_contract:e.query_params,output_contract:output});
 services.push({id:idFor(route),endpoint_id:e.id,api_slug:api.slug,name:api.name,source_url:docs,method:e.method,path:route,listing_type:'api',list_atomic:Math.ceil(e.price*1e6),fixed_params,preconditions:{},pagination:{},freshness_seconds:300,sample_payload:capture.data,jurisdiction:key==='insider-transactions'?'US':null,contract:{capability_slug,version:1,cardinality:'one',requires,input_map,output_map:{base:'$',produces:{[result]:{path:'$'}}},identifies:null,max_fan_out:1},test_inputs:{[required]:'AAPL_US'}});
 endpoints.push({...e,response_example:capture.data,response_body:output});
}
const fixture={known_sources:[{slug:api.slug,name:api.name,block_reason:null,metadata:{description:'Financial news, sentiment, investor relations and US insider transactions.',identity_documented:true}}],services,vocabulary,capabilities,endpoints};
await fs.mkdir(new URL('../tests/fixtures/cityfalcon/',import.meta.url),{recursive:true});
await fs.writeFile(new URL('../tests/fixtures/cityfalcon/catalog.json',import.meta.url),JSON.stringify(fixture,null,2)+'\n');
const q=s=>s==null?'NULL':`'${String(s).replaceAll("'","''")}'`,j=s=>`${q(JSON.stringify(s))}::jsonb`;
const sql=['-- Four CityFALCON read operations, checked against actual 2026-09-13 provider responses.','BEGIN;',"SELECT pg_advisory_xact_lock(hashtext('cityfalcon-v2-contracts'));",
`DO $guard$ BEGIN IF NOT EXISTS(SELECT 1 FROM providers.apis WHERE id=${q(api.id)} AND slug=${q(api.slug)} AND origin_url='https://api.cityfalcon.com/v0.2' AND status='active' AND suspended_at IS NULL) THEN RAISE EXCEPTION 'CityFALCON identity changed'; END IF; END $guard$;`,
`UPDATE providers.apis SET docs_url=${q(docs)},website_url='https://www.cityfalcon.ai',listing_metadata=coalesce(listing_metadata,'{}') || ${j({coverage_notice:'Four live-checked read operations for explicit full tickers: news, sentiment, investor relations and US insider transactions. Recent 30-day API window. Returned pages may be partial; no exhaustive historical claim.'})} WHERE id=${q(api.id)};`];
for(const w of vocabulary)sql.push(`INSERT INTO catalog.fact_types(slug,name,description,subject_kind,value_schema,status,freshness_seconds) VALUES(${q(w.slug)},${q(w.name)},${q(w.description)},${q(w.subject_kind)},${j(w.value_schema)},'active',300) ON CONFLICT(slug) DO NOTHING;
DO $guard$ BEGIN IF NOT EXISTS(SELECT 1 FROM catalog.fact_types WHERE slug=${q(w.slug)} AND status='active' AND value_schema=${j(w.value_schema)}) THEN RAISE EXCEPTION 'CityFALCON fact conflict'; END IF; END $guard$;`);
for(const c of capabilities)sql.push(`INSERT INTO catalog.capabilities(slug,name,description,category,input_contract,output_contract,status) VALUES(${q(c.slug)},${q(c.name)},${q(c.description)},'finance',${j(c.input_contract)},${j(c.output_contract)},'active') ON CONFLICT(slug) DO NOTHING;`);
for(const s of services){const c=s.contract,e=endpoints.find(e=>e.id===s.endpoint_id);
sql.push(`DO $guard$ BEGIN IF NOT EXISTS(SELECT 1 FROM providers.api_endpoints WHERE id=${q(e.id)} AND api_id=${q(api.id)} AND path=${q(e.path)} AND method='GET' AND price=${e.price} AND active) THEN RAISE EXCEPTION 'CityFALCON endpoint changed'; END IF; IF EXISTS(SELECT 1 FROM catalog.capability_execution_contracts WHERE endpoint_id=${q(e.id)} AND status='active' AND id<>${q(s.id)}) THEN RAISE EXCEPTION 'Concurrent CityFALCON contract'; END IF; END $guard$;
UPDATE providers.api_endpoints SET response_example=${j(e.response_example)},response_body=${j(e.response_body)},external_docs_url=${q(docs)},metadata_source='provider-docs-and-live-response',metadata_at=now() WHERE id=${q(e.id)};
INSERT INTO catalog.capability_execution_contracts(id,capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,output_map,cardinality,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,sample_payload,validated_at,validated_note,notes)
VALUES(${q(s.id)},${q(c.capability_slug)},${q(e.id)},1,'active',${j(c.requires)},${j(Object.keys(c.output_map.produces).map(fact_type=>({fact_type,subject_role:'subject',cardinality:'one'})))},${j(c.input_map)},${j(s.fixed_params)},${j(c.output_map)},'one',1,'{}','{}','read','per_call',300,${j(s.sample_payload)},now(),'Actual upstream HTTP 200; Rust Catalog::build, input binding, resolver and response extraction checked. No buyer payment made.','20260913 CityFALCON integration repair') ON CONFLICT(id) DO NOTHING;`);
}
sql.push('COMMIT;');
await fs.writeFile(new URL('../supabase/migrations/20260913091134_cityfalcon_execution_contracts.sql',import.meta.url),sql.join('\n\n')+'\n');
console.log(JSON.stringify({contracts:services.length,words:vocabulary.length}));
