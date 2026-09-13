// Contracts promoted only from successful, settled gateway procurement captures.
import fs from 'node:fs/promises';
import {createHash} from 'node:crypto';
const root=new URL('../../evidence/2026-09-13/catalog-repair/',import.meta.url);
const meta=JSON.parse(await fs.readFile(new URL('catalog.json',root)));
const object=(properties,required=Object.keys(properties))=>({type:'object',properties,required}),string={type:'string'};
const browser=process.argv.includes('--browserbase');
const rows=browser?[
 {source:'browserbase',path:'/search',cap:'web.browserbase-search',name:'Web search via Browserbase',input:'query',word:'web.source_search_query',fixed:{numResults:5},schema:object({results:{type:'array',items:object({url:string})}}),docs:'https://x402.browserbase.com/'},
 {source:'browserbase',path:'/fetch',cap:'web.browserbase-fetch',name:'Read a web page via Browserbase',input:'url',word:'web.source_page_url',fixed:{},schema:object({content:string,contentType:string,statusCode:{type:'number'}}),docs:'https://x402.browserbase.com/'},
]:[
 {source:'stableenrich',path:'/api/exa/search',cap:'web.stableenrich-search',name:'Web search via StableEnrich',input:'query',word:'web.source_search_query',fixed:{numResults:5},schema:object({results:{type:'array',items:object({url:string})}}),docs:'https://stableenrich.dev/openapi.json'},
 {source:'linkup',path:'/v1/search',cap:'web.linkup-answer',name:'Sourced web answer via Linkup',input:'q',word:'web.source_search_query',fixed:{depth:'standard',outputType:'sourcedAnswer'},schema:object({answer:string,sources:{type:'array',items:object({url:string})}}),docs:'https://docs.linkup.so/pages/documentation/endpoints/search/reference'},
 {source:'linkup',path:'/v1/fetch',cap:'web.linkup-fetch',name:'Read a web page via Linkup',input:'url',word:'web.source_page_url',fixed:{mode:'standard'},schema:object({markdown:string}),docs:'https://docs.linkup.so/pages/documentation/endpoints/fetch/reference'},
];
const vocabulary=[{slug:'web.source_search_query',name:'Web search question',description:'The user\'s explicit web search query or question.',subject_kind:'web',value_schema:{type:'string',minLength:1}},{slug:'web.source_page_url',name:'Page URL',description:'The exact HTTP(S) page URL supplied or resolved for this request.',subject_kind:'web',value_schema:{type:'string',pattern:'^https?://[^\\s]+$'}}];
const services=[],capabilities=[],endpoints=[];
const known_sources=[];
for(const row of rows){
 const a=meta.apis.find(a=>a.slug===row.source),e=structuredClone(meta.endpoints.find(e=>e.api_id===a.id&&e.path===row.path));
 if(browser)e.request_body=object({[row.input]:{type:'string'},...(row.input==='query'?{numResults:{type:'number',minimum:1,maximum:25}}:{})},[row.input]);
 const capture=JSON.parse(await fs.readFile(new URL(`procurement/${row.source}-${row.path.replaceAll('/','_')}-paid.json`,root)));
 if(capture.body.did!==true||capture.body.execution.paid!==true||!/^0x[0-9a-f]{64}$/i.test(capture.body.execution.tx_hash))throw Error('Missing confirmed settlement');
 const sample=structuredClone(capture.body.execution.body);
 if(sample.sources)sample.sources=sample.sources.slice(0,2).map(({name,url})=>({name,url}));
 const out=row.cap.replaceAll('-','_')+'_page';
 vocabulary.push({slug:out,name:row.name,description:'Returned response page with provider attribution and original URLs. Search is bounded, not an exhaustive crawl.',subject_kind:'web',value_schema:row.schema});
 const h=createHash('sha256').update('source-repair-20260913:'+row.cap).digest('hex'),id=`${h.slice(0,8)}-${h.slice(8,12)}-4${h.slice(13,16)}-a${h.slice(17,20)}-${h.slice(20,32)}`;
 services.push({id,endpoint_id:e.id,api_slug:a.slug,name:a.name,source_url:row.docs,method:e.method,path:e.path,listing_type:'api',list_atomic:Math.ceil(e.price*1e6),fixed_params:row.fixed,preconditions:{},pagination:{},freshness_seconds:300,sample_payload:sample,jurisdiction:null,contract:{capability_slug:row.cap,version:1,cardinality:'one',requires:[{fact_type:row.word,subject_role:'subject',optional:false}],input_map:{[row.word]:{location:'body',name:row.input}},output_map:{base:'$',produces:{[out]:{path:'$'}}},identifies:null,max_fan_out:1},test_inputs:{[row.word]:capture.inputs[row.input]}});
 capabilities.push({slug:row.cap,name:row.name,description:row.name+'. Returns the requested page or bounded search response with original source URLs. No exhaustive coverage claim.',input_contract:e.request_body,output_contract:row.schema});
 endpoints.push({...e,response_example:sample,response_body:row.schema});
 if(!known_sources.some(s=>s.slug===a.slug))known_sources.push({slug:a.slug,name:a.name,block_reason:null,metadata:{identity_documented:true}});
}
const fixture={services,vocabulary,capabilities,endpoints,known_sources};
const folder=browser?'browserbase':'search-source-repairs';
await fs.mkdir(new URL(`../tests/fixtures/${folder}/`,import.meta.url),{recursive:true});
await fs.writeFile(new URL(`../tests/fixtures/${folder}/catalog.json`,import.meta.url),JSON.stringify(fixture,null,2)+'\n');
const q=s=>s==null?'NULL':`'${String(s).replaceAll("'","''")}'`,j=s=>`${q(JSON.stringify(s))}::jsonb`;
const sql=['BEGIN;',"SELECT pg_advisory_xact_lock(hashtext('search-source-repairs'));",'-- All three operations returned data through the deployed treasury path with confirmed settlement.'];
for(const w of vocabulary)sql.push(`INSERT INTO catalog.fact_types(slug,name,description,subject_kind,value_schema,status,freshness_seconds) VALUES(${q(w.slug)},${q(w.name)},${q(w.description)},'web',${j(w.value_schema)},'active',300) ON CONFLICT(slug) DO NOTHING;
DO $g$ BEGIN IF NOT EXISTS(SELECT 1 FROM catalog.fact_types WHERE slug=${q(w.slug)} AND value_schema=${j(w.value_schema)} AND status='active') THEN RAISE EXCEPTION 'Search fact conflict'; END IF; END $g$;`);
for(const c of capabilities)sql.push(`INSERT INTO catalog.capabilities(slug,name,description,category,input_contract,output_contract,status) VALUES(${q(c.slug)},${q(c.name)},${q(c.description)},'web',${j(c.input_contract)},${j(c.output_contract)},'active') ON CONFLICT(slug) DO NOTHING;`);
for(const s of services){const c=s.contract,e=endpoints.find(e=>e.id===s.endpoint_id);sql.push(`
DO $g$ BEGIN IF NOT EXISTS(SELECT 1 FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id WHERE e.id=${q(e.id)} AND a.slug=${q(s.api_slug)} AND e.path=${q(e.path)} AND e.method=${q(e.method)} AND e.price=${e.price} AND e.active AND a.status='active' AND a.suspended_at IS NULL) THEN RAISE EXCEPTION 'Reviewed search endpoint changed'; END IF; IF EXISTS(SELECT 1 FROM catalog.capability_execution_contracts WHERE endpoint_id=${q(e.id)} AND status='active' AND id<>${q(s.id)}) THEN RAISE EXCEPTION 'Concurrent search contract'; END IF; END $g$;
UPDATE providers.apis SET docs_url=coalesce(docs_url,${q(s.source_url)}) WHERE slug=${q(s.api_slug)};
UPDATE providers.api_endpoints SET response_example=${j(s.sample_payload)},response_body=${j(e.response_body)},external_docs_url=${q(s.source_url)} WHERE id=${q(e.id)};
INSERT INTO catalog.capability_execution_contracts(id,capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,output_map,cardinality,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,sample_payload,validated_at,validated_note,notes)
VALUES(${q(s.id)},${q(c.capability_slug)},${q(e.id)},1,'active',${j(c.requires)},${j(Object.keys(c.output_map.produces).map(fact_type=>({fact_type,subject_role:'subject',cardinality:'one'})))},${j(c.input_map)},${j(s.fixed_params)},${j(c.output_map)},'one',1,'{}','{}','read','per_call',300,${j(s.sample_payload)},now(),'Gateway data response with confirmed x402 settlement, 2026-09-13; Rust input and output checks passed. No customer debit.','20260913 source integration repair') ON CONFLICT(id) DO NOTHING;`);}
sql.push('COMMIT;');await fs.writeFile(new URL(browser?'../supabase/migrations/20260913092615_browserbase_execution_contracts.sql':'../supabase/migrations/20260913091751_search_source_execution_contracts.sql',import.meta.url),sql.join('\n\n')+'\n');
