// Deterministic contracts and staged migration; no network, credentials or DB writes.
import fs from 'node:fs/promises';
import crypto from 'node:crypto';
const root=new URL('../',import.meta.url);
const read=async p=>JSON.parse(await fs.readFile(new URL(p,root),'utf8'));
const company=await read('tests/fixtures/company-sources/catalog.json');
const sirenic=await read('tests/fixtures/sirenic/expanded-catalog.json');
const id=s=>{const h=crypto.createHash('sha256').update('uk-supplier-v1:'+s).digest('hex');return `${h.slice(0,8)}-${h.slice(8,12)}-4${h.slice(13,16)}-8${h.slice(17,20)}-${h.slice(20,32)}`};
const words=[];const services=[];const capabilities=[];
const word=(slug,schema,description)=>{words.push({slug,name:description,description,subject_kind:'company',jurisdiction:'GB',value_schema:schema,status:'active',freshness_seconds:3600});return slug;};
const object=(required=[])=>({type:'object',required});
const number=word('vat.uk.candidate_number',{type:'string',pattern:'^GB[0-9]{9}$'},'Published or supplied UK VAT candidate, not an identity assertion');
const vatEvidence=word('vat.uk.search_evidence',object(['candidates','distinct_candidates','notice']),'Published UK VAT candidate evidence with source links; absence is not non-registration');
const vatResult=word('vat.uk.validation_result',object(['data','source_url','notice','coverage']),'HMRC-derived UK VAT validity via Ask Merlin, registered identity, checkedAt and source/cache status');
const coverage=word('company.uk_director_screening.coverage',object(['complete','active_directors','screening_limit']),'Whether a complete active board fits the five-director screening ceiling');
const baseCH=company.services.filter(s=>s.api_slug==='companies-house'&&['company.uk-registry-search','company.registry-profile','company.filing-history','company.significant-control'].includes(s.contract.capability_slug));
const rosterBase=sirenic.services.find(s=>s.path==='/v1/eu/entreprise/GB/{company_number}/dirigeants');
const matchBase=company.services.find(s=>s.api_slug==='opensanctions'&&s.contract.capability_slug==='company.sanctions-screen');
if(baseCH.length!==4||!rosterBase||!matchBase)throw Error('Missing reviewed upstream fixture');
const req=(fact_type,optional=false)=>({fact_type,subject_role:'subject',optional});
function add(base,cap,description,requires,input_map,outputs,sample,fixed={},optional=[]){
 const s={...base,id:id(cap),contract:{capability_slug:cap,version:1,requires,input_map,output_map:{base:'$',produces:outputs},cardinality:'one',identifies:null,max_fan_out:1},fixed_params:fixed,preconditions:{},pagination:{},freshness_seconds:3600,jurisdiction:'GB',sample_payload:sample,optional_outputs:optional};
 services.push(s);capabilities.push({slug:cap,description});return s;
}
const rosterSample={pays:'GB',company_number:'00000006',nombre_actifs:2,tronque:false,dirigeants:[{nom:'Example Director One',fonction:'director',naissance:{year:1961,month:5}},{nom:'Example Director Two',fonction:'director',naissance:null}],fixture_notice:'Synthetic shape test; not a live company screening result'};
const rosterOutputs={ [coverage]:{adapter:'roster.active-director',key:'coverage'} };
const optional=[];
for(let i=0;i<5;i++){
 const query=word(`company.uk_director_${i+1}.screening_query`,object(['q1']),`Source-derived screening query for active director ${i+1}; Person or corporate director Company`);
 const result=word(`company.uk_director_${i+1}.screening_result`,{...object(['query','status','results']),properties:{status:{type:'integer',enum:[200]},results:{type:'array'},query:{type:'object'}}},`OpenSanctions watchlist response for active director ${i+1}; possible matches need review`);
 rosterOutputs[query]={adapter:'roster.active-director',key:`${i}.query`};optional.push(query);
 add(matchBase,`company.uk-director-screening-${i+1}`,`Screen active UK director ${i+1} with OpenSanctions, if present; one separately priced query`,[req(query)],{[query]:{in:'body',name:'queries'}},{[result]:{path:'$.responses.q1'}},
  {responses:{q1:{query:{schema:'Person',properties:{name:['Example Director']}},status:200,results:[],total:{value:0,relation:'eq'}}},fixture_notice:'Synthetic response, not live screening'},
  {dataset:'default',limit:10,threshold:0.7});
}
add(rosterBase,'company.uk-director-roster','Retrieve a complete active UK director roster for bounded OpenSanctions screening',[req('company.registration.gb.companies_house')],rosterBase.contract.input_map,rosterOutputs,rosterSample,{},optional);
add({endpoint_id:'22144f16-e658-49a7-94f8-2dc6edbf7e88',api_slug:'tavily',name:'Tavily',method:'POST',path:'/search',listing_type:'federated',list_atomic:10000,source_url:'https://docs.tavily.com/documentation/api-reference/endpoint/search'},'company.uk-vat-candidate-search','Find published GB VAT candidates for a company; search evidence is not VAT validity',[req('company.name')],{'company.name':{in:'body',name:'query',wrap:'uk_vat_search'}},{[number]:{adapter:'search.uk-vat-candidates',key:'number'},[vatEvidence]:{adapter:'search.uk-vat-candidates',key:'evidence'}},
 {results:[{url:'https://example.test/legal',title:'Example UK company',content:'VAT number GB220430231'}],fixture_notice:'Synthetic search response'},
 {search_depth:'advanced',max_results:5,include_answer:false,include_images:false,include_raw_content:false},[number]);
const sample={data:{vatNumber:'GB220430231',countryCode:'GB',valid:true,status:'valid',name:'TESCO PLC',address:'CIRRUS A, SHIRE PARK KESTREL WAY, WELWYN GARDEN CITY, AL7 1GA, GB',source:'live',checkedAt:'2026-09-21T22:44:06Z',stale:false,consultationNumber:null},source_url:'https://askmerlin.com/api/lookup?q=GB220430231&max_age=3600',coverage:{complete:true,scope:'One supplied GB VAT number, HMRC-derived response via Ask Merlin, at the recorded check time.'},notice:'Match the registered name/address; no consultation reference. An invalid candidate does not prove no other registration.'};
const merlin=add({endpoint_id:id('ask-merlin-endpoint'),api_slug:'ask-merlin',name:'Ask Merlin UK VAT',method:'GET',path:'/api/lookup',listing_type:'api',list_atomic:85066,source_url:'https://askmerlin.com/api'},'vat.uk.validate','Validate one GB VAT number via Ask Merlin (HMRC-derived, at most one-hour cache); not company-name search',[req(number)],{[number]:{in:'query',name:'q'}},{[vatResult]:{path:'$'}},sample,{max_age:3600});
const vocab=new Map([...company.vocabulary,...sirenic.vocabulary,...words].map(v=>[v.slug,v]));
const snapshot={description:'UK supplier contract tests. UK VAT sample from free live check 2026-09-21; roster, search and screening samples are explicitly synthetic. No paid E2E claim.',vocabulary:[...vocab.values()],services:[...baseCH,...services]};
await fs.mkdir(new URL('tests/fixtures/uk-supplier/',root),{recursive:true});
await fs.writeFile(new URL('tests/fixtures/uk-supplier/catalog.json',root),JSON.stringify(snapshot,null,2)+'\n');
const q=v=>v==null?'NULL':`'${String(v).replaceAll("'","''")}'`,j=v=>`${q(JSON.stringify(v))}::jsonb`;
const sql=['-- Staged UK supplier contracts. Publish only after runtime and contract tests.','BEGIN;',"SELECT pg_advisory_xact_lock(hashtext('uk-supplier-verification-v1'));",
 `DO $source$ DECLARE owner uuid; BEGIN SELECT owner_id INTO STRICT owner FROM providers.apis WHERE slug='vies' AND is_platform_owned AND settlement_mode='apiosk_revenue';
 INSERT INTO providers.apis(id,owner_id,slug,name,status,description,long_description,category,docs_url,website_url,origin_url,endpoint_url,authentication_info,is_verified,active,discoverable,listing_type,listing_metadata,integration_mode,settlement_mode,is_external,is_platform_owned,is_community,source,requires_payout_wallet)
 VALUES(gen_random_uuid(),owner,'ask-merlin','Ask Merlin UK VAT','inactive','UK VAT validation via Ask Merlin using HMRC-derived results. Search by name is a separate Tavily step.','Known GB number validation with checkedAt and source/cache evidence; no consultation reference. Not affiliated with HMRC.','finance','https://askmerlin.com/api','https://askmerlin.com','https://askmerlin.com','https://askmerlin.com','{"type":"none"}',false,false,false,'api','{"adapter":"uk-vat-v1","catalog_state":"staged","upstream_cost":0,"coverage_notice":"HMRC-derived intermediary check of one GB VAT number; no consultation reference. Maximum cache age one hour; stale/unknown results fail closed. Search candidates need entity matching."}','gateway','apiosk_revenue',false,true,false,'manual',false) ON CONFLICT(slug) DO NOTHING;
 IF NOT EXISTS(SELECT 1 FROM providers.apis WHERE slug='ask-merlin' AND origin_url='https://askmerlin.com' AND owner_id=owner) THEN RAISE EXCEPTION 'Ask Merlin source identity conflict'; END IF;
 END $source$;`,
 `INSERT INTO providers.api_endpoints(id,api_id,method,path,price,active,payment_required,cache_ttl_seconds,description,query_params,response_body,request_example,response_example,tags,search_terms,sectors,use_cases,input_entities,output_entities,endpoint_category,action_type,external_docs_url,documentation_markdown,metadata_source,metadata_confidence,task_summary,contract_documented)
 SELECT ${q(merlin.endpoint_id)}::uuid,id,'GET','/api/lookup',0.085066,false,true,60,'Validate a known GB VAT number via Ask Merlin',${j({type:'object',required:['q'],properties:{q:{type:'string',pattern:'^GB[0-9]{9}$',description:'Known GB VAT candidate; not company name'},max_age:{type:'integer',enum:[3600],default:3600}}})},${j(object(['data','coverage','notice','source_url']))},'{"q":"GB220430231","max_age":3600}',${j(sample)},ARRAY['VAT','UK','GB','HMRC','supplier verification'],ARRAY['VAT','UK','GB','HMRC'],ARRAY['Company data','Finance'],ARRAY['Validate UK VAT registration'],ARRAY['q'],ARRAY['data','source_url','coverage','notice'],'finance','lookup','https://askmerlin.com/api','Free upstream API, Apiosk public-source tariff (same as VIES). Only fresh, identity-matched source responses can settle.','provider_documentation_and_live_probe',1,'Validate UK VAT',true FROM providers.apis WHERE slug='ask-merlin' ON CONFLICT(id) DO NOTHING;`];
for(const w of words)sql.push(`INSERT INTO catalog.fact_types(slug,name,description,subject_kind,jurisdiction,value_schema,status,freshness_seconds) VALUES(${q(w.slug)},${q(w.name)},${q(w.description)},'company','GB',${j(w.value_schema)},'active',3600) ON CONFLICT(slug) DO NOTHING;`);
for(const s of services){const c=s.contract,description=capabilities.find(v=>v.slug===c.capability_slug).description;
 sql.push(`DO $guard$ BEGIN IF NOT EXISTS(SELECT 1 FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id WHERE e.id=${q(s.endpoint_id)}::uuid AND a.slug=${q(s.api_slug)} AND e.method=${q(s.method)} AND e.path=${q(s.path)} AND ceil(e.price*1000000)::bigint=${s.list_atomic}) THEN RAISE EXCEPTION 'Endpoint identity or price drift: %',${q(s.api_slug+' '+s.path)}; END IF; END $guard$;`);
 sql.push(`INSERT INTO catalog.capabilities(slug,name,description,category,input_contract,output_contract,status) VALUES(${q(c.capability_slug)},${q(description)},${q(description)},'company','{}','{}','active') ON CONFLICT(slug) DO NOTHING;`);
 const produces=Object.keys(c.output_map.produces).map(fact_type=>({fact_type,subject_role:'subject',cardinality:'one',...(s.optional_outputs.includes(fact_type)?{optional:true}:{})}));
 sql.push(`INSERT INTO catalog.capability_execution_contracts(id,capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,output_map,cardinality,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,sample_payload,validated_at,validated_note,notes,adapter_version) VALUES(${q(s.id)}::uuid,${q(c.capability_slug)},${q(s.endpoint_id)}::uuid,1,'validated',${j(c.requires)},${j(produces)},${j(c.input_map)},${j(s.fixed_params)},${j(c.output_map)},'one',1,'{}','{}','read','per_call',3600,${j(s.sample_payload)},now(),'Staged: contract and synthetic pipeline tests required; VAT source free live positive and invalid probes, no paid E2E claim','uk-supplier-v1','uk-supplier-v1') ON CONFLICT(id) DO NOTHING;`);
}
sql.push('COMMIT;','');
const output=process.argv[2];if(!output)throw Error('Pass a CLI-created migration path');
await fs.writeFile(new URL(output,root),sql.join('\n\n'));
console.log(`${services.length} staged contracts; ${snapshot.services.length} fixture services; ${words.length} new fact types`);
