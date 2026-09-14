// Reuters and Bloomberg news via the Apiosk RapidAPI subscriptions.
// Renders the catalog fixture and the onboarding migration from the captured
// 2026-09-14 provider responses; nothing here is fabricated or credentialed.
// Upstream keys are set separately in listing_metadata.auth_headers.
// Two migrations: 20260914103000 (initial, applied; kept byte-identical) and
// 20260914110000, which replaces the URL-input section contract with one
// fixed-section contract per Reuters section. The intent parser only accepts
// input values that appear literally in the question, so a buyer can say
// "Reuters Africa headlines" but never types the section URL.
// `--check` fails on drift instead of writing.
import fs from 'node:fs/promises';
import {createHash} from 'node:crypto';
const root=new URL('../',import.meta.url);
const evidence=new URL('../../evidence/2026-09-14/rapidapi-news/',import.meta.url);
const VERSION='20260914103000',SECTIONS_VERSION='20260914110000';
const capture=async(source,name)=>JSON.parse(await fs.readFile(new URL(`${source}/${name}.body`,evidence),'utf8'));
const idFor=s=>{const h=createHash('sha256').update('rapidapi-news-20260914:'+s).digest('hex');return `${h.slice(0,8)}-${h.slice(8,12)}-4${h.slice(13,16)}-a${h.slice(17,20)}-${h.slice(20,32)}`;};
const string={type:'string'},integer={type:'integer'};
const array=items=>({type:'array',items});
const object=(properties,required=Object.keys(properties))=>({type:'object',required,properties});
// Standard public-data tier: 47,259 list + 7,089 fee = 54,348 micro-USD buyer total.
const PRICE=0.047259;

const sections=await capture('reuters','sections');
if(sections.length!==19)throw Error('Expected the 19 captured Reuters sections');
const sectionUrls=sections.map(s=>s.url);
const sectionList=sections.map(s=>`${s.name} ${s.url}`).join('; ');

const vocabulary=[
 {slug:'news.reuters_search_query',name:'Reuters headline search keywords',freshness_seconds:900,
  value_schema:{type:'string',minLength:2,maxLength:120,description:'Plain keywords to search Reuters headlines for, e.g. bitcoin or Nvidia earnings. Not a boolean query.'}},
 {slug:'news.reuters_result_limit',name:'Reuters headline count',freshness_seconds:900,
  value_schema:{type:'integer',minimum:1,maximum:12,description:'Number of headlines to return, 1-12. The provider never returns more than 12.'}},
 {slug:'news.reuters_section_url',name:'Reuters section',freshness_seconds:86400,
  value_schema:{type:'string',enum:sectionUrls,description:`Exact Reuters section URL. Known sections: ${sectionList}.`}},
 {slug:'news.reuters_latest_headlines',name:'Latest Reuters headlines',freshness_seconds:300,
  value_schema:array(object({link:string,title:string,published_at:string})),
  description:'Latest Reuters headlines with article link, headline and provider-reported publication time. Headlines only, not article text.'},
 {slug:'news.reuters_search_headlines',name:'Reuters headline search results',freshness_seconds:900,
  value_schema:array(object({link:string,title:string})),
  description:'Reuters articles matching the keywords: link and headline only. No publication date field (the date is in the URL). Keyword relevance is loose; an empty list is not proof nothing was published.'},
 {slug:'news.reuters_section_headlines',name:'Reuters section headlines',freshness_seconds:300,
  value_schema:array(object({link:string,title:string,position:integer})),
  description:'Headlines on one Reuters section front page, in page order. Headlines only, not article text.'},
 {slug:'news.reuters_sections',name:'Reuters sections',freshness_seconds:86400,
  value_schema:array(object({name:string,url:string})),
  description:'Reuters sections with their section URL.'},
 {slug:'news.bloomberg_latest_stories',name:'Latest Bloomberg stories',freshness_seconds:300,
  // The whole envelope: status true and a data object, so HTTP 406 bodies ({data:null,status:false}) are rejected.
  value_schema:object({status:{type:'boolean',enum:[true]},data:object({title:string,modules:array(object({id:string,stories:array(object({id:string,title:string,published:integer,longURL:string}))}))},['modules'])},['status','data']),
  description:'Latest Bloomberg stories: headline, auto-generated summary, byline, Unix publication time, category, article URL, word count and premium flag. Summaries only, not article text.'},
];
for(const w of vocabulary)w.description??=w.value_schema.description;

const common={
 tags:['news','headlines','breaking news','world news','business news','markets news','RapidAPI'],
 sectors:['Media and news','Finance'],
 billing:{mode:'apiosk_balance'},
};
const reuters={slug:'reuters',name:'Reuters News',origin:'https://reuters-api.p.rapidapi.com',
 docs:'https://rapidapi.com/mahmudulhasandev/api/reuters-api',website:'https://www.reuters.com',
 description:'Reuters news headlines: the latest wire headlines, keyword headline search, headlines per section (World, Business, Markets, Technology and more) and the section list. Headlines and links only, no article text. Up to 12 results per call.',
 metadata:{...common,tags:[...common.tags,'Reuters','wire'],logo:'https://apiosk.com/brands/reuters.png',
  license:{name:'Third-party Reuters headline API on RapidAPI (publisher mahmudulhasandev); Apiosk subscription. Preserve Reuters attribution and article links.',cost_upstream:'RapidAPI subscription'},
  operator_notice:'Delivered through an independent RapidAPI provider; not affiliated with or endorsed by Thomson Reuters.',
  upstream_quota:{requests_per_month:10000,provider:'RapidAPI'},
  coverage_notice:'Four live-checked read operations: latest headlines, keyword headline search, section headlines and section list. At most 12 headlines per call; search results carry no date field and keyword matching is loose. The provider /details route is not offered because it returns text synthesized from the URL instead of the article.',
  catalog_state:'published',
  production_probe:{probed_on:'2026-09-14',evidence:'evidence/2026-09-14/rapidapi-news/measurements.json',operations_connected:4,operations_measured:5}}};
const bloomberg={slug:'bloomberg',name:'Bloomberg News',origin:'https://bloomberg-finance.p.rapidapi.com',
 docs:'https://rapidapi.com/search/bloomberg',website:'https://www.bloomberg.com',
 description:'Latest Bloomberg news stories: headline, summary, byline, publication time, category and article link. Summaries only, no full article text.',
 metadata:{...common,tags:[...common.tags,'Bloomberg'],logo:'https://apiosk.com/brands/bloomberg.png',
  license:{name:'Third-party Bloomberg news API on RapidAPI; Apiosk free-plan subscription. Preserve Bloomberg attribution and article links.',cost_upstream:'RapidAPI free plan'},
  operator_notice:'Delivered through an independent RapidAPI provider; not affiliated with or endorsed by Bloomberg L.P.',
  upstream_quota:{requests_per_month:500,provider:'RapidAPI'},
  coverage_notice:'One live-checked read operation: the latest Bloomberg stories feed (about 25 stories). Regional lists, ticker stories, story detail, financials and key statistics returned HTTP 406 upstream on 2026-09-14 and are not offered. Small monthly upstream quota.',
  catalog_state:'published',
  production_probe:{probed_on:'2026-09-14',evidence:'evidence/2026-09-14/rapidapi-news/measurements.json',operations_connected:1,operations_measured:6}}};
const sources=[reuters,bloomberg];

const word=slug=>vocabulary.find(w=>w.slug===slug);
const limitParam={type:'integer',minimum:1,maximum:12,required:false,description:word('news.reuters_result_limit').value_schema.description,'x-parameter-location':'query'};
const ops=[
 {source:reuters,path:'/latest',capability:'news.reuters-latest',name:'Latest Reuters headlines',capture:'latest',
  description:'Latest Reuters news headlines across all sections, newest wire stories with link and publication time. Optional headline count 1-12. Headlines only, not article text.',
  query_params:{type:'object',properties:{limit:limitParam}},request_example:{limit:12},
  requires:[['news.reuters_result_limit','limit',true]],fixed:{},produces:'news.reuters_latest_headlines',freshness:300,
  tags:['news','reuters','headlines','latest'],use_cases:['see the latest Reuters headlines','monitor breaking world and business news'],
  search_terms:['latest reuters news','reuters headlines today','breaking news reuters'],task:'Return the latest Reuters headlines with links and publication times.'},
 {source:reuters,path:'/search',capability:'news.reuters-search',name:'Search Reuters headlines',capture:'search',
  description:'Search Reuters news headlines by keywords (a company, person, topic or event). Returns up to 12 article links and headlines, no dates; keyword matching is loose.',
  query_params:{type:'object',required:['q'],properties:{q:{...word('news.reuters_search_query').value_schema,required:true,'x-parameter-location':'query'},limit:limitParam}},request_example:{q:'bitcoin',limit:12},
  requires:[['news.reuters_search_query','q',false],['news.reuters_result_limit','limit',true]],fixed:{},produces:'news.reuters_search_headlines',freshness:900,
  tags:['news','reuters','search','headlines'],use_cases:['find Reuters coverage of a company or topic','check recent Reuters reporting on an event'],
  search_terms:['search reuters news','reuters articles about','reuters coverage'],task:'Search Reuters headlines for keywords.'},
 {source:reuters,path:'/category',capability:'news.reuters-section',name:'Reuters section headlines',capture:'category',
  description:'Current headlines from one Reuters section front page: World, Africa, Americas, Asia Pacific, China, Europe, India, Middle East, UK, US, Business, Markets, Technology, Legal, Breakingviews, Sustainability, Science, Sports or Lifestyle. Up to 12 headlines in page order, no article text.',
  query_params:{type:'object',required:['url'],properties:{url:{type:'string',enum:sectionUrls,required:true,description:word('news.reuters_section_url').value_schema.description,'x-parameter-location':'query'}}},request_example:{url:'https://www.reuters.com/world/africa/'},
  requires:[['news.reuters_section_url','url',false]],fixed:{},produces:'news.reuters_section_headlines',freshness:300,
  tags:['news','reuters','section','headlines','region'],use_cases:['headlines for a region such as Africa or Europe','technology or markets headlines from Reuters'],
  search_terms:['reuters africa news','reuters technology news','reuters markets headlines'],task:'Return headlines from one Reuters section.'},
 {source:reuters,path:'/sections',capability:'news.reuters-sections',name:'Reuters sections',capture:'sections',
  description:'List the Reuters news sections (World, regions, Business, Markets, Technology, Legal and more) with their section URLs.',
  query_params:{type:'object',properties:{}},request_example:{},
  requires:[],fixed:{},produces:'news.reuters_sections',freshness:86400,
  tags:['news','reuters','sections'],use_cases:['see which Reuters sections exist'],
  search_terms:['reuters sections','reuters categories'],task:'List Reuters news sections.'},
 {source:bloomberg,path:'/news/list',capability:'news.bloomberg-latest',name:'Latest Bloomberg stories',capture:'news-list',
  description:'Latest Bloomberg news stories (about 25): headline, summary, byline, publication time, category such as Markets, Technology or Politics, and article link. Summaries only, not article text.',
  query_params:{type:'object',required:['id'],properties:{id:{type:'string',enum:['phx-latest'],required:true,description:'Latest-news feed identifier; fixed by Apiosk.','x-parameter-location':'query'}}},request_example:{id:'phx-latest'},
  requires:[],fixed:{id:'phx-latest'},produces:'news.bloomberg_latest_stories',freshness:300,
  tags:['news','bloomberg','headlines','latest','markets'],use_cases:['see the latest Bloomberg stories','monitor financial and business news'],
  search_terms:['latest bloomberg news','bloomberg headlines today','bloomberg markets news'],task:'Return the latest Bloomberg stories with summaries and links.'},
];
// Measured but not offered: kept inactive so the catalog records why.
const inactive=[
 {source:reuters,path:'/details',description:'Not offered: returns text synthesized from the article URL (slug title, boilerplate body, URL date), not the Reuters article. Measured 2026-09-14.',param:'url'},
 ...[['/news/list-by-region','id'],['/stories/list','id'],['/stories/detail','internalID'],['/stock/get-financials','id'],['/stock/get-keystats','id']].map(([path,param])=>({source:bloomberg,path,param,
  description:'Not offered: upstream returned HTTP 406 Not Acceptable on two attempts on 2026-09-14.'})),
];

// Samples keep migrations reviewable; complete captures are extraction-tested separately.
const sampleOf=(op,data)=>{
 if(op.capture==='news-list'){
  const pick=s=>Object.fromEntries(['id','type','internalID','title','autoGeneratedSummary','published','updatedAt','byline','longURL','primaryCategory','wordCount','premium'].map(k=>[k,s[k]]));
  return {data:{title:data.data.title,modules:data.data.modules.filter(m=>m.stories?.length).map(m=>({id:m.id,type:m.type,title:m.title,stories:m.stories.slice(0,3).map(pick)}))},status:true,message:'Successful'};
 }
 return op.capture==='sections'?data:data.slice(0,3);
};
async function build(ops){
const services=[],capabilities=[],endpoints=[];
for(const op of ops){
 const data=await capture(op.source.slug,op.capture);
 const endpoint_id=idFor(op.source.slug+op.path);
 const sample=sampleOf(op,data);
 const output=word(op.produces);
 const contract={capability_slug:op.capability,version:1,cardinality:'one',
  requires:op.requires.map(([fact_type,,optional])=>({fact_type,subject_role:'subject',optional})),
  input_map:Object.fromEntries(op.requires.map(([fact,name])=>[fact,{in:'query',name}])),
  output_map:{base:'$',produces:{[op.produces]:{path:'$'}}},identifies:null,max_fan_out:1};
 capabilities.push({slug:op.capability,name:op.name,description:op.description,input_contract:op.query_params,output_contract:output.value_schema});
 services.push({id:idFor('contract:'+op.capability),endpoint_id,api_slug:op.source.slug,name:op.source.name,source_url:op.source.docs,method:'GET',path:op.path,listing_type:'api',
  list_atomic:Math.ceil(PRICE*1e6),fixed_params:op.fixed,preconditions:{},pagination:{},freshness_seconds:op.freshness,sample_payload:sample,jurisdiction:null,contract,capture:op.capture});
 if(!endpoints.some(e=>e.id===endpoint_id))endpoints.push({id:endpoint_id,api_slug:op.source.slug,path:op.path,active:true,description:op.description,query_params:op.query_params,request_example:op.request_example,
  response_body:output.value_schema,response_example:sample,tags:op.tags,use_cases:op.use_cases,search_terms:op.search_terms,task_summary:op.task,cache_ttl_seconds:op.freshness});
}
for(const e of inactive)endpoints.push({id:idFor(e.source.slug+e.path),api_slug:e.source.slug,path:e.path,active:false,description:e.description,
 query_params:{type:'object',required:[e.param],properties:{[e.param]:{type:'string',required:true,'x-parameter-location':'query'}}},
 request_example:null,response_body:null,response_example:null,tags:['news',e.source.slug,'not offered'],use_cases:[],search_terms:[],task_summary:e.description,cache_ttl_seconds:null});
return {services,capabilities,endpoints};
}
// Breakingviews answered HTTP 502 on two attempts on 2026-09-14 and is not offered.
const offeredSections=sections.filter(s=>!s.url.endsWith('/breakingviews/'));
const sectionSlug=s=>s.url.replace('https://www.reuters.com/','').replace(/\/$/,'').split('/').pop();
const sectionOps=offeredSections.map(section=>{
 const slug=sectionSlug(section),label=section.name==='World'?'World (all regions)':section.name;
 return {source:reuters,path:'/category',capability:`news.reuters-section-${slug}`,name:`Reuters ${section.name} headlines`,capture:`sections/${slug}`,
  description:`Current Reuters ${label} news headlines: up to 12 article links and headlines from the Reuters ${section.name} section front page (${section.url}), in page order. Headlines only, not article text.`,
  requires:[],fixed:{url:section.url},produces:'news.reuters_section_headlines',freshness:300};
});
const categoryOp={source:reuters,path:'/category',capture:'category',
 description:`Current headlines from one Reuters section front page: ${offeredSections.map(s=>s.name).join(', ')}. Up to 12 headlines in page order, no article text. Each section is a separate capability with the section URL fixed by Apiosk.`,
 query_params:{type:'object',required:['url'],properties:{url:{type:'string',enum:offeredSections.map(s=>s.url),required:true,description:'Reuters section URL; fixed per section capability.','x-parameter-location':'query'}}},request_example:{url:'https://www.reuters.com/world/africa/'},
 tags:['news','reuters','section','headlines','region'],use_cases:['headlines for a region such as Africa or Europe','technology or markets headlines from Reuters'],
 search_terms:['reuters africa news','reuters technology news','reuters markets headlines'],task:'Return headlines from one Reuters section.'};
const initial=await build(ops);
const final=await build([...ops.filter(o=>o.capability!=='news.reuters-section').flatMap(o=>o.path==='/sections'?[...sectionOps.map(x=>({...categoryOp,...x})),o]:[o])]);
const {services,capabilities,endpoints}=initial;

const apiId=s=>idFor('api:'+s.slug);
const fixture={
 known_sources:sources.map(s=>({slug:s.slug,name:s.name,block_reason:null,metadata:{description:s.description,category:'news',tags:s.metadata.tags,identity_documented:true}})),
 services:final.services,vocabulary:vocabulary.filter(w=>w.slug!=='news.reuters_section_url'),capabilities:final.capabilities,endpoints:final.endpoints,
 sources:sources.map(s=>({id:apiId(s),slug:s.slug,name:s.name,origin_url:s.origin,logo:s.metadata.logo})),
};

const q=v=>v==null?'NULL':`'${String(v).replaceAll("'","''")}'`,j=v=>`${q(JSON.stringify(v))}::jsonb`,arr=v=>`ARRAY[${v.map(q).join(',')}]::text[]`;
const count=slug=>services.filter(s=>s.api_slug===slug).length;
const sql=[`-- Reuters and Bloomberg news via Apiosk RapidAPI subscriptions.
-- Generated by gateway/scripts/prepare-rapidapi-news.mjs from live 2026-09-14 captures.
-- Upstream keys are NOT in this file; set listing_metadata.auth_headers separately.
-- Price: 47,259 list + 7,089 fee = 54,348 micro-USD buyer total per call.
BEGIN;`,
"SELECT pg_advisory_xact_lock(hashtext('apiosk-rapidapi-news'));",
"INSERT INTO catalog.fact_subject_kinds(slug,description) VALUES('news','News headlines, stories and sections') ON CONFLICT DO NOTHING;"];
for(const w of vocabulary)sql.push(`INSERT INTO catalog.fact_types(slug,name,description,subject_kind,value_schema,status,freshness_seconds) VALUES(${q(w.slug)},${q(w.name)},${q(w.description)},'news',${j(w.value_schema)},'active',${w.freshness_seconds}) ON CONFLICT(slug) DO NOTHING;
DO $guard$ BEGIN IF NOT EXISTS(SELECT 1 FROM catalog.fact_types WHERE slug=${q(w.slug)} AND status='active' AND value_schema=${j(w.value_schema)}) THEN RAISE EXCEPTION 'News fact conflict: %',${q(w.slug)}; END IF; END $guard$;`);
for(const c of capabilities)sql.push(`INSERT INTO catalog.capabilities(slug,name,description,category,input_contract,output_contract,status) VALUES(${q(c.slug)},${q(c.name)},${q(c.description)},'news',${j(c.input_contract)},${j(c.output_contract)},'active') ON CONFLICT(slug) DO NOTHING;
DO $guard$ BEGIN IF NOT EXISTS(SELECT 1 FROM catalog.capabilities WHERE slug=${q(c.slug)} AND status='active' AND category='news') THEN RAISE EXCEPTION 'News capability conflict: %',${q(c.slug)}; END IF; END $guard$;`);
const onboard=[`DO $onboard$ DECLARE owner uuid; BEGIN
SELECT owner_id INTO STRICT owner FROM providers.apis WHERE slug='data-overheid-nl' AND is_platform_owned AND settlement_mode='apiosk_revenue';
IF EXISTS(SELECT 1 FROM providers.apis WHERE slug IN ('reuters','bloomberg') AND id NOT IN (${sources.map(s=>q(apiId(s))).join(',')})) THEN RAISE EXCEPTION 'Slug reuters or bloomberg already used by another listing'; END IF;`];
for(const s of sources){
 onboard.push(`INSERT INTO providers.apis(id,owner_id,slug,name,status,description,long_description,category,docs_url,website_url,origin_url,endpoint_url,authentication_info,is_verified,active,discoverable,listing_type,listing_metadata,integration_mode,settlement_mode,is_external,is_platform_owned,is_community,source,requires_payout_wallet,commission_rate)
 VALUES(${q(apiId(s))},owner,${q(s.slug)},${q(s.name)},'inactive',${q(s.description)},${q(s.metadata.coverage_notice)},'news',${q(s.docs)},${q(s.website)},${q(s.origin)},${q(s.origin)},${j({type:'none',caller:'Apiosk balance approval required; Apiosk holds the RapidAPI subscription.'})},false,false,false,'api',${j(s.metadata)},'gateway','apiosk_revenue',false,true,false,'manual',false,0.2174)
 ON CONFLICT(id) DO NOTHING;`);
 for(const e of endpoints.filter(e=>e.api_slug===s.slug))onboard.push(`INSERT INTO providers.api_endpoints(id,api_id,method,path,price,payment_required,active,cache_ttl_seconds,description,query_params,response_body,request_example,response_example,tags,sectors,use_cases,search_terms,endpoint_category,action_type,task_summary,external_docs_url,documentation_markdown,metadata_source,metadata_confidence,metadata_at)
 VALUES(${q(e.id)},${q(apiId(s))},'GET',${q(e.path)},${PRICE},true,${e.active},${e.cache_ttl_seconds??'NULL'},${q(e.description)},${j(e.query_params)},${e.response_body?j(e.response_body):'NULL'},${e.request_example?j(e.request_example):'NULL'},${e.response_example?j(e.response_example):'NULL'},${arr(e.tags)},${arr(['media','finance'])},${arr(e.use_cases)},${arr(e.search_terms)},'news','lookup',${q(e.task_summary)},${q(s.docs)},${q(s.metadata.coverage_notice)},'provider-docs-and-live-response',0.9,now())
 ON CONFLICT(id) DO NOTHING;`);
}
onboard.push('END $onboard$;');
sql.push(onboard.join('\n'));
for(const s of services){const c=s.contract;
 sql.push(`INSERT INTO catalog.capability_execution_contracts(id,capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,output_map,cardinality,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,sample_payload,validated_at,validated_note,notes)
VALUES(${q(s.id)},${q(c.capability_slug)},${q(s.endpoint_id)},1,'validated',${j(c.requires)},${j(Object.keys(c.output_map.produces).map(fact_type=>({fact_type,subject_role:'subject',cardinality:'one'})))},${j(c.input_map)},${j(s.fixed_params)},${j(c.output_map)},'one',1,'{}','{}','read','per_call',${s.freshness_seconds},${j(s.sample_payload)},now(),'Actual upstream HTTP 200 on 2026-09-14; Rust Catalog::build, resolver, input binding and full-response extraction checked.','20260914 RapidAPI news onboarding') ON CONFLICT(id) DO NOTHING;`);
}
// Publish in the same transaction, only once every invariant holds.
sql.push(`DO $publish$ DECLARE source_id uuid; source_slug text; expected integer; BEGIN
FOR source_slug, expected IN SELECT * FROM (VALUES ('reuters',${count('reuters')}),('bloomberg',${count('bloomberg')})) v(slug,n) LOOP
 SELECT id INTO STRICT source_id FROM providers.apis WHERE slug=source_slug AND is_platform_owned AND settlement_mode='apiosk_revenue' AND NOT requires_payout_wallet AND listing_type='api' FOR UPDATE;
 IF (SELECT count(*) FROM providers.api_endpoints WHERE api_id=source_id AND active AND price=${PRICE})<>expected
  OR (SELECT count(*) FROM catalog.capability_execution_contracts c JOIN providers.api_endpoints e ON e.id=c.endpoint_id WHERE e.api_id=source_id AND c.status IN ('validated','active') AND c.validated_at IS NOT NULL AND c.sample_payload IS NOT NULL)<>expected THEN
  RAISE EXCEPTION '% publication invariant failed',source_slug; END IF;
 PERFORM set_config('apiosk.api_review_approved_id',source_id::text,true);
 UPDATE providers.apis SET status='active',active=true,is_verified=true,discoverable=true,verified_at=coalesce(verified_at,now()),approved_at=coalesce(approved_at,now()),suspended_at=NULL WHERE id=source_id;
 PERFORM set_config('apiosk.api_review_approved_id','',true);
 UPDATE catalog.capability_execution_contracts c SET status='active' FROM providers.api_endpoints e WHERE c.endpoint_id=e.id AND e.api_id=source_id AND c.status='validated';
 IF NOT EXISTS(SELECT 1 FROM providers.apis WHERE id=source_id AND status='active' AND active AND is_verified AND discoverable)
  OR (SELECT count(*) FROM catalog.capability_execution_contracts c JOIN providers.api_endpoints e ON e.id=c.endpoint_id WHERE e.api_id=source_id AND c.status='active')<>expected THEN
  RAISE EXCEPTION '% post-publication invariant failed',source_slug; END IF;
END LOOP; END $publish$;`);
sql.push('COMMIT;\n');

const sectionEndpoint=final.endpoints.find(e=>e.path==='/category');
const newContracts=final.services.filter(s=>s.contract.capability_slug.startsWith('news.reuters-section-'));
const sectionMetadata={coverage_notice:`Read operations checked live on 2026-09-14: latest headlines, keyword headline search, the section list and headlines for ${offeredSections.length} sections (${offeredSections.map(s=>s.name).join(', ')}). At most 12 headlines per call; search results carry no date field and keyword matching is loose. Breakingviews returned HTTP 502 and is not offered. The provider /details route is not offered because it returns text synthesized from the URL instead of the article. The upstream plan also limits requests per second.`,
 production_probe:{probed_on:'2026-09-14',evidence:'evidence/2026-09-14/rapidapi-news/measurements.json',operations_connected:3+offeredSections.length,operations_measured:5+sections.length-1,gateway_paid_checks:5}};
const sectionsSql=[`-- Replace the URL-input Reuters section contract with one fixed-section contract per section.
-- The intent parser requires literal values from the question, so section URLs could never be bound.
-- Generated by gateway/scripts/prepare-rapidapi-news.mjs from live 2026-09-14 section captures.
BEGIN;`,
"SELECT pg_advisory_xact_lock(hashtext('apiosk-rapidapi-news'));",
`DO $guard$ BEGIN IF NOT EXISTS(SELECT 1 FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id WHERE e.id=${q(sectionEndpoint.id)} AND a.slug='reuters' AND e.path='/category' AND e.method='GET' AND e.active AND e.price=${PRICE}) THEN RAISE EXCEPTION 'Reuters /category endpoint changed'; END IF; END $guard$;`,
`UPDATE catalog.capability_execution_contracts SET status='retired',notes=coalesce(notes,'')||'; retired 20260914: section URL cannot be a literal question value' WHERE capability_slug='news.reuters-section' AND endpoint_id=${q(sectionEndpoint.id)};`,
"UPDATE catalog.capabilities SET status='retired' WHERE slug='news.reuters-section';",
"UPDATE catalog.fact_types SET status='retired' WHERE slug='news.reuters_section_url' AND NOT EXISTS(SELECT 1 FROM catalog.capability_execution_contracts WHERE status='active' AND requires @> '[{\"fact_type\":\"news.reuters_section_url\"}]');",
`UPDATE providers.api_endpoints SET description=${q(sectionEndpoint.description)},query_params=${j(sectionEndpoint.query_params)},request_example=${j(sectionEndpoint.request_example)},response_example=${j(sectionEndpoint.response_example)},metadata_at=now() WHERE id=${q(sectionEndpoint.id)};`];
for(const c of final.capabilities.filter(c=>c.slug.startsWith('news.reuters-section-')))sectionsSql.push(`INSERT INTO catalog.capabilities(slug,name,description,category,input_contract,output_contract,status) VALUES(${q(c.slug)},${q(c.name)},${q(c.description)},'news','{"type":"object","properties":{}}'::jsonb,${j(c.output_contract)},'active') ON CONFLICT(slug) DO NOTHING;
DO $guard$ BEGIN IF NOT EXISTS(SELECT 1 FROM catalog.capabilities WHERE slug=${q(c.slug)} AND status='active' AND category='news') THEN RAISE EXCEPTION 'News capability conflict: %',${q(c.slug)}; END IF; END $guard$;`);
for(const s of newContracts){const c=s.contract;
 sectionsSql.push(`INSERT INTO catalog.capability_execution_contracts(id,capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,output_map,cardinality,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,sample_payload,validated_at,validated_note,notes)
VALUES(${q(s.id)},${q(c.capability_slug)},${q(s.endpoint_id)},1,'active','[]','[{"fact_type":"news.reuters_section_headlines","subject_role":"subject","cardinality":"one"}]','{}',${j(s.fixed_params)},${j(c.output_map)},'one',1,'{}','{}','read','per_call',${s.freshness_seconds},${j(s.sample_payload)},now(),'Actual upstream HTTP 200 for this section on 2026-09-14; Rust Catalog::build, resolver, fixed-section binding and full-response extraction checked.','20260914 Reuters fixed-section contracts') ON CONFLICT(id) DO NOTHING;`);
}
sectionsSql.push(`UPDATE providers.apis SET listing_metadata=listing_metadata || ${j(sectionMetadata)},long_description=${q(sectionMetadata.coverage_notice)} WHERE slug='reuters' AND is_platform_owned;`);
sectionsSql.push(`DO $verify$ BEGIN
 IF (SELECT count(*) FROM catalog.capability_execution_contracts WHERE endpoint_id=${q(sectionEndpoint.id)} AND status='active' AND capability_slug LIKE 'news.reuters-section-%')<>${newContracts.length}
  OR EXISTS(SELECT 1 FROM catalog.capability_execution_contracts WHERE capability_slug='news.reuters-section' AND status='active') THEN
  RAISE EXCEPTION 'Reuters section contract invariant failed'; END IF;
END $verify$;`);
sectionsSql.push('COMMIT;\n');

const outputs=[
 [new URL('tests/fixtures/rapidapi-news/catalog.json',root),JSON.stringify(fixture,null,2)+'\n'],
 [new URL(`supabase/migrations/${VERSION}_onboard_reuters_bloomberg_news.sql`,root),sql.join('\n\n')],
 [new URL(`supabase/migrations/${SECTIONS_VERSION}_reuters_section_capabilities.sql`,root),sectionsSql.join('\n\n')],
];
for(const [file,content] of outputs){
 if(process.argv.includes('--check')){if(await fs.readFile(file,'utf8')!==content)throw Error('Drift: '+file.pathname);}
 else {await fs.mkdir(new URL('.',file),{recursive:true});await fs.writeFile(file,content);}
}
console.log(JSON.stringify({initial_services:services.length,final_services:final.services.length,section_contracts:newContracts.length,inactive_endpoints:inactive.length}));
