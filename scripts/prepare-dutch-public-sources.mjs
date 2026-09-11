// Deterministic catalog fixtures and staged, data-only onboarding migration.
// First export the REAL adapter samples: PUBLIC_SOURCE_EXPORT=1 cargo test --test dutch_public_sources.
import fs from 'node:fs/promises';
import crypto from 'node:crypto';
const root = new URL('../', import.meta.url);
const samples = JSON.parse(await fs.readFile(new URL('tests/fixtures/dutch-public-sources/normalized.json', root), 'utf8'));
const id = text => { const h=crypto.createHash('sha256').update(text).digest('hex'); return `${h.slice(0,8)}-${h.slice(8,12)}-4${h.slice(13,16)}-8${h.slice(17,20)}-${h.slice(20,32)}`; };
const string = {type:'string',minLength:1,maxLength:200};
const date = {type:'string',pattern:'^[0-9]{4}-[0-9]{2}-[0-9]{2}$'};
const words = [];
function word(slug, schema, description, kind='dataset') {
  words.push({slug,name:slug.replaceAll('.',' '),description,subject_kind:kind,jurisdiction:'NL',value_schema:schema,status:'active',freshness_seconds:3600});return slug;
}
word('government.publication.keyword',string,'Literal Dutch words to match in an official publication title. Not a CQL expression.');
word('government.publication.authority',string,'Exact publishing authority, e.g. Amsterdam.');
word('government.publication.date_from',date,'Inclusive publication date from, YYYY-MM-DD.');
word('government.publication.date_until',date,'Inclusive publication date until, YYYY-MM-DD.');
word('government.publication.start',{type:'integer',minimum:1,maximum:100000},'One-based start record from next_start in a saved KOOP page.');
word('government.publication.identifier',{type:'string',pattern:'^[A-Za-z0-9-]{1,100}$'},'Exact official publication identifier from a search result, e.g. gmb-2026-427235.');
word('subsidy.discovery.keyword',string,'Literal keyword, searched only within the first 50 public RVO descriptions. No complete-catalog, application-status or eligibility claim.');
word('subsidy.discovery.sector',string,'Exact sector label from RVO source descriptions.');
word('subsidy.discovery.audience',string,'Exact audience label from RVO source descriptions.');
const pageSchema = {type:'object',required:['items','coverage','notice','source_url'],properties:{items:{type:'array',items:{type:'object'}},coverage:{type:'object',required:['complete','scope'],properties:{complete:{type:'boolean'},scope:{type:'string'}}},notice:{type:'string'},source_url:{type:'string'}}};
word('government.publication.page',pageSchema,'Returned official notices, their authority, type, dates, links, total and continuation. Notices are not current consolidated law or permit decisions.');
word('vehicle.rdw.technical_page',pageSchema,'RDW exact-plate technical record or empty result. Missing fields remain unknown. Excludes owner and fuel-dataset information.','vehicle');
word('subsidy.description.partial_page',pageSchema,'Filtered first RVO description page only; total and application status unknown; eligibility not assessed.');
const plate={slug:'vehicle.registration.nl.plate',value_schema:{type:'string',pattern:'^[A-Za-z0-9-]{6,10}$'}};
const sources=[
 {slug:'koop',name:'KOOP Official Publications',origin:'https://repository.overheid.nl',path:'/sru',description:'Search official Dutch notices by title, publishing authority and publication date; retrieve an identified notice. Includes historical applications and decisions. An application is not an issued permit. One bounded page per purchase.',docs:'https://www.koopoverheid.nl/binaries/koop/documenten/instructies/2021/02/09/handleiding-voor-het-uitvragen-van-de-collectie-officiele-publicaties/HandleidingSRU2.0.pdf',website:'https://zoek.officielebekendmakingen.nl/',category:'government',tags:['KOOP','overheid.nl','bekendmakingen','vergunningen','official notices','permits','municipal decisions'],sectors:['Government and public data','Real estate'],license:'Official government publications; preserve attribution and source links',example:{q:'omgevingsvergunning',authority:'Amsterdam',from:'2026-09-01',until:'2026-09-11'},ttl:3600},
 {slug:'rdw',name:'RDW Vehicle Technical Data',origin:'https://opendata.rdw.nl',path:'/resource/m9d7-ebf2.json',description:'Direct official RDW technical vehicle lookup by exact Dutch registration plate. Includes make, model, APK expiry and available register fields. No owner identity, fleet ownership or separate fuel/emissions dataset.',docs:'https://opendata.rdw.nl/en/en/Vehicles/Open-Data-RDW-Gekentekende_voertuigen/m9d7-ebf2',website:'https://opendata.rdw.nl/',category:'mobility',tags:['RDW','kenteken','voertuig','auto','vehicle registration','license plate','APK','technical vehicle data'],sectors:['Mobility','Government and public data'],license:'CC0',example:{kenteken:'RZ874R'},ttl:3600},
 {slug:'rvo',name:'RVO Subsidy Descriptions — First Page',origin:'https://www.rvo.nl',path:'/api/v1/opendata/subsidies',description:'Keyword discovery within the first 50 public RVO subsidy descriptions only. Includes historical schemes. Later-page access is unverified; no-match does not mean no subsidy exists. Current application status, deadlines and eligibility are unknown.',docs:'https://www.rvo.nl/onderwerpen/open-data',website:'https://www.rvo.nl/subsidies-financiering',category:'government',tags:['RVO','subsidies','financiering','subsidy descriptions','grants','funding','first page','partial coverage'],sectors:['Government and public data','Company data'],license:'CC0 for this dataset; no RVO imagery reused',example:{q:'energie'},ttl:3600},
];
const spec=[
 ['koop','government.publication.search','Official notice title search, authority and dates; one page', [['government.publication.keyword','q',false],['government.publication.authority','authority',true],['government.publication.date_from','from',true],['government.publication.date_until','until',true],['government.publication.start','start',true]],'government.publication.page',{limit:10}],
 ['koop','government.publication.detail','Official notice metadata by exact publication identifier',[['government.publication.identifier','publication_id',false]],'government.publication.page',{}],
 ['rdw','vehicle.technical-profile','Direct RDW technical profile by plate, excluding fuel/emissions',[['vehicle.registration.nl.plate','kenteken',false]],'vehicle.rdw.technical_page',{}],
 ['rvo','subsidy.scheme.first-page-search','Search first 50 RVO descriptions only; status and eligibility unknown',[['subsidy.discovery.keyword','q',false],['subsidy.discovery.sector','sector',true],['subsidy.discovery.audience','audience',true]],'subsidy.description.partial_page',{}],
];
const services=spec.map(([slug,cap,name,bindings,output,fixed])=>{
 const source=sources.find(s=>s.slug===slug);
 return {id:id(cap),endpoint_id:id(slug+source.path),api_slug:slug,name:source.name,source_url:source.docs,method:'GET',path:source.path,listing_type:'api',list_atomic:47259,fixed_params:fixed,preconditions:{},pagination:cap==='government.publication.search'?{type:'start_record',input:'start',next:'$.next_start',automatic:false}:{},freshness_seconds:source.ttl,sample_payload:samples[cap==='government.publication.detail'?'koop-detail':slug],jurisdiction:'NL',
 contract:{capability_slug:cap,version:1,requires:bindings.map(([fact_type,,optional])=>({fact_type,subject_role:'subject',optional})),input_map:Object.fromEntries(bindings.map(([fact,name])=>[fact,{in:'query',name}])),output_map:{base:'$',produces:{[output]:{path:'$'}}},cardinality:'one',identifies:null,max_fan_out:1},capability_name:name};
});
const fixture={description:'Real upstream responses normalized by the production adapter; staged until deployed and verified.',sources,new_words:words,vocabulary:[...words,plate],services};
const q=v=>v==null?'NULL':`'${String(v).replaceAll("'","''")}'`;
const j=v=>`${q(JSON.stringify(v))}::jsonb`;
const sql=['-- Generated by scripts/prepare-dutch-public-sources.mjs. Staged; activate only after adapter deployment and verification.',
 '-- Matches the existing data.overheid.nl tier: 47,259 list + 7,089 fee = 54,348 micro-USD buyer total.',
 'BEGIN;',"SELECT pg_advisory_xact_lock(hashtext('apiosk-dutch-public-sources'));",'DO $onboard$ DECLARE owner uuid; source_id uuid; endpoint uuid; BEGIN',
 "SELECT owner_id INTO STRICT owner FROM public.apis WHERE slug='data-overheid-nl' AND is_platform_owned AND settlement_mode='apiosk_revenue';"];
for (const source of sources) {
 const params={type:'object',properties:{}};
 for(const s of services.filter(s=>s.api_slug===source.slug)) for(const binding of s.contract.requires) {
  const name=s.contract.input_map[binding.fact_type].name;const w=fixture.vocabulary.find(w=>w.slug===binding.fact_type);
  params.properties[name]={...w.value_schema,required:source.slug==='koop'?false:!binding.optional,description:w.description??'Dutch plate, with optional hyphens; normalized to uppercase.', 'x-parameter-location':'query'};
 }
 if(source.slug==='koop') params.properties.limit={type:'integer',minimum:1,maximum:25,default:10,'x-parameter-location':'query'};
 if(source.slug!=='koop')params.required=services.find(s=>s.api_slug===source.slug).contract.requires.filter(r=>!r.optional).map(r=>services.find(s=>s.api_slug===source.slug).contract.input_map[r.fact_type].name);
 const metadata={tags:source.tags,sectors:source.sectors,billing:{mode:'apiosk_balance'},license:{name:source.license,cost_upstream:'EUR 0.00'},operator_notice:'Apiosk integration of public official data; not affiliated with the publisher.',adapter:'dutch-public-sources-v1',catalog_state:'staged',coverage_notice:source.description,auth_headers:[{name:'Accept',value:'application/json'}]};
 sql.push(`INSERT INTO public.apis(id,owner_id,slug,name,status,description,long_description,category,docs_url,website_url,origin_url,endpoint_url,authentication_info,is_verified,active,discoverable,listing_type,listing_metadata,integration_mode,settlement_mode,is_external,is_platform_owned,is_community,source,requires_payout_wallet)
 VALUES(gen_random_uuid(),owner,${q(source.slug)},${q(source.name)},'inactive',${q(source.description)},${q(source.description)},${q(source.category)},${q(source.docs)},${q(source.website)},${q(source.origin)},${q(source.origin)},${j({type:'none',caller:'Apiosk balance approval required; upstream public read access.'})},false,false,false,'api',${j(metadata)},'gateway','apiosk_revenue',false,true,false,'manual',false) ON CONFLICT(slug) DO NOTHING;
 SELECT id INTO STRICT source_id FROM public.apis WHERE slug=${q(source.slug)} AND owner_id=owner AND origin_url=${q(source.origin)} AND listing_metadata->>'adapter'='dutch-public-sources-v1';
 INSERT INTO public.api_endpoints(id,api_id,method,path,price,active,cache_ttl_seconds,description,query_params,response_body,request_example,response_example,tags,sectors,use_cases,endpoint_category,action_type,payment_required,external_docs_url,documentation_markdown,metadata_source,metadata_confidence)
 SELECT gen_random_uuid(),source_id,'GET',${q(source.path)},0.047259,true,${source.ttl},${q(source.description)},${j(params)},${j(pageSchema)},${j(source.example)},${j(samples[source.slug])},ARRAY[${source.tags.map(q).join(',')}],ARRAY[${source.sectors.map(q).join(',')}],ARRAY[${q(source.description)}],'data','lookup',true,${q(source.docs)},${q(source.description)},'provider_documentation_and_live_probe',1
 WHERE NOT EXISTS(SELECT 1 FROM public.api_endpoints WHERE api_id=source_id AND path=${q(source.path)} AND method='GET');
 SELECT id INTO STRICT endpoint FROM public.api_endpoints WHERE api_id=source_id AND path=${q(source.path)} AND method='GET' AND price=0.047259 AND query_params=${j(params)};`);
}
sql.push('END $onboard$;');
for (const w of words) {
 sql.push(`INSERT INTO public.fact_subject_kinds(slug,description) VALUES(${q(w.subject_kind)},'Public source query or record') ON CONFLICT DO NOTHING;
 INSERT INTO public.fact_types(slug,name,description,subject_kind,jurisdiction,value_schema,status,freshness_seconds) VALUES(${q(w.slug)},${q(w.name)},${q(w.description)},${q(w.subject_kind)},'NL',${j(w.value_schema)},'active',3600) ON CONFLICT(slug) DO NOTHING;
 DO $guard$ BEGIN IF NOT EXISTS(SELECT 1 FROM public.fact_types WHERE slug=${q(w.slug)} AND value_schema=${j(w.value_schema)} AND subject_kind=${q(w.subject_kind)} AND status='active') THEN RAISE EXCEPTION 'Conflicting fact: %',${q(w.slug)}; END IF; END $guard$;`);
}
for (const s of services) {
 const c=s.contract;const produces=Object.keys(c.output_map.produces).map(fact_type=>({fact_type,subject_role:'subject',cardinality:'one'}));
 sql.push(`INSERT INTO public.capabilities(slug,name,description,category,input_contract,output_contract,status) VALUES(${q(c.capability_slug)},${q(s.capability_name)},${q(s.capability_name)},${q(s.api_slug==='rdw'?'mobility':'government')},'{"type":"object"}','{"type":"object"}','active') ON CONFLICT(slug) DO NOTHING;
 INSERT INTO public.capability_execution_contracts(id,capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,output_map,cardinality,identifies,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,sample_payload,validated_at,validated_note,notes)
 SELECT gen_random_uuid(),${q(c.capability_slug)},e.id,1,'validated',${j(c.requires)},${j(produces)},${j(c.input_map)},${j(s.fixed_params)},${j(c.output_map)},'one',NULL,1,'{}',${j(s.pagination)},'read','per_call',${s.freshness_seconds},${j(s.sample_payload)},now(),'Real upstream fixtures; Rust adapter and catalog tests','dutch-public-sources-v1; activate after deployed verification'
 FROM public.api_endpoints e JOIN public.apis a ON a.id=e.api_id WHERE a.slug=${q(s.api_slug)} AND e.path=${q(s.path)} AND e.method='GET'
 AND NOT EXISTS(SELECT 1 FROM public.capability_execution_contracts c WHERE c.endpoint_id=e.id AND c.capability_slug=${q(c.capability_slug)} AND c.version=1);
 DO $guard$ BEGIN IF NOT EXISTS(SELECT 1 FROM public.capability_execution_contracts c JOIN public.api_endpoints e ON e.id=c.endpoint_id JOIN public.apis a ON a.id=e.api_id WHERE a.slug=${q(s.api_slug)} AND e.path=${q(s.path)} AND c.capability_slug=${q(c.capability_slug)} AND c.version=1 AND c.requires=${j(c.requires)} AND c.input_map=${j(c.input_map)} AND c.output_map=${j(c.output_map)} AND c.fixed_params=${j(s.fixed_params)} AND c.status IN ('validated','active')) THEN RAISE EXCEPTION 'Conflicting immutable contract: %',${q(c.capability_slug)}; END IF; END $guard$;`);
}
sql.push('COMMIT;','');
const outputs=[['tests/fixtures/dutch-public-sources/catalog.json',JSON.stringify(fixture,null,2)+'\n'],['supabase/migrations/20260911100204_onboard_koop_rdw_rvo.sql',sql.join('\n\n')]];
for(const [path,text] of outputs) {
 const url=new URL(path,root);
 if(process.argv.includes('--check')) {if(await fs.readFile(url,'utf8')!==text)throw Error('Generated file drift: '+path);}
 else await fs.writeFile(url,text);
}
console.log('3 staged sources, 3 endpoints, 4 typed contracts; RVO is explicitly first-page-only.');
