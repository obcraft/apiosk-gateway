// Explicit source-backed presets; no model-generated dataset identifiers.
import fs from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
const root=new URL('../',import.meta.url),file=new URL('tests/fixtures/statistics-presets.json',root);
const base=JSON.parse(await fs.readFile(new URL('tests/fixtures/public-statistics/catalog.json',root),'utf8'));
let fixture;
if(process.argv.includes('--fetch')) {
  fixture={vocabulary:base.vocabulary,services:[],capabilities:[],new_endpoints:[]};
  for(const [kind,table,word,select] of [
    ['measures','MeasureCodes','cbs.measure_codes','Identifier,Title,Unit'],
    ['sectors','BedrijfstakkenBranchesSBI2008Codes','cbs.branch_codes','Identifier,Title'],
    ['entity-types','TypeGefailleerdeCodes','cbs.bankruptcy_type_codes','Identifier,Title'],
  ]) {
    const model=base.services.find(s=>s.path.endsWith(kind==='sectors'?'BedrijfstakkenBranchesSBI2008Codes':'MeasureCodes'));
    const s=structuredClone(model);s.id=randomUUID();s.contract.capability_slug=`statistics.cbs-bankruptcy-${kind}`;
    s.contract.requires=[];s.contract.input_map={};s.fixed_params={tableId:'82244NED','$select':select};
    s.contract.output_map={base:'$',produces:{[word]:{path:'$'}}};
    if(kind==='entity-types') {
      s.endpoint_id=randomUUID();s.path='/odata/v1/CBS/{tableId}/TypeGefailleerdeCodes';
      fixture.new_endpoints.push({id:s.endpoint_id,template_endpoint:model.endpoint_id,path:s.path});
      fixture.vocabulary.push({slug:word,value_schema:{type:'object',required:['value'],properties:{value:{type:'array',items:{type:'object'}}}}});
    }
    const url=new URL(`https://datasets.cbs.nl/odata/v1/CBS/82244NED/${table}`);url.searchParams.set('$select',select);
    const r=await fetch(url,{redirect:'error',signal:AbortSignal.timeout(30000)});if(!r.ok)throw Error(`CBS ${table}: ${r.status}`);
    s.sample_payload=await r.json();s.evidence={url:url.href,retrieved_at:new Date().toISOString()};
    fixture.services.push(s);
    fixture.capabilities.push({slug:s.contract.capability_slug,name:`CBS bankruptcy ${kind}`,description:`Official ${kind} codebook for bankruptcy table 82244NED. Pair with statistics.cbs-bankruptcies to resolve labels and units.`});
  }
  for(const [country,geo] of [['netherlands','NL'],['germany','DE']]) {
    const s=structuredClone(base.services.find(s=>s.contract.capability_slug==='statistics.eurostat-data'));
    s.id=randomUUID();s.contract.capability_slug=`statistics.eurostat-gdp-${country}`;
    s.contract.requires=[{fact_type:'eurostat.input.time',subject_role:'subject',optional:false}];
    s.contract.input_map={'eurostat.input.time':{in:'query',name:'time'}};
    s.fixed_params={format:'JSON',lang:'EN',dataset:'nama_10_gdp',geo,unit:'CP_MEUR',na_item:'B1GQ'};
    s.sample_payload=JSON.parse(await fs.readFile(new URL(`output/direct-usecases-iuqQRD/gdp-${geo}.json`,root),'utf8')).data;
    fixture.services.push(s);
    fixture.capabilities.push({slug:s.contract.capability_slug,name:`GDP of ${country}`,description:`GDP at market prices in current-price million euro for ${country}. Requires the explicit year. Not real GDP, growth or GDP per capita.`});
  }
  await fs.writeFile(file,JSON.stringify(fixture,null,2)+'\n');
} else fixture=JSON.parse(await fs.readFile(file,'utf8'));
const q=x=>x==null?'NULL':`'${String(x).replaceAll("'","''")}'`,j=x=>`${q(JSON.stringify(x))}::jsonb`;
const sql=['-- Source-verified CBS codebooks and explicit nominal GDP presets. Every call is separately quoted.','BEGIN;'];
const word=fixture.vocabulary.find(w=>w.slug==='cbs.bankruptcy_type_codes');
sql.push(`INSERT INTO public.fact_types(slug,name,description,subject_kind,value_schema,status,freshness_seconds) VALUES(${q(word.slug)},'CBS bankruptcy entity types','Official table-specific codebook','dataset',${j(word.value_schema)},'active',86400) ON CONFLICT(slug) DO NOTHING;`);
for(const e of fixture.new_endpoints) sql.push(`INSERT INTO public.api_endpoints(id,api_id,path,method,price,active,description,query_params,response_body,response_example) SELECT ${q(e.id)},api_id,${q(e.path)},method,price,active,'Official CBS bankruptcy entity-type codebook',query_params,response_body,${j(fixture.services.find(s=>s.endpoint_id===e.id).sample_payload)} FROM public.api_endpoints WHERE id=${q(e.template_endpoint)};`);
for(const s of fixture.services) {
  const cap=fixture.capabilities.find(c=>c.slug===s.contract.capability_slug),c=s.contract;
  sql.push(`INSERT INTO public.capabilities(slug,name,description,category,input_contract,output_contract,status) VALUES(${q(cap.slug)},${q(cap.name)},${q(cap.description)},'data','{}','{}','active');`);
  sql.push(`INSERT INTO public.capability_execution_contracts(id,capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,output_map,cardinality,identifies,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,sample_payload,validated_at,validated_note) VALUES(${q(s.id)},${q(c.capability_slug)},${q(s.endpoint_id)},1,'active',${j(c.requires)},${j(Object.keys(c.output_map.produces).map(fact_type=>({fact_type,subject_role:'subject',cardinality:'one'})))},${j(c.input_map)},${j(s.fixed_params)},${j(c.output_map)},'one',NULL,1,'{}','{}','read','per_call',3600,${j(s.sample_payload)},now(),'Direct official source response and Rust extraction/input regression test');`);
}
sql.push('COMMIT;','');
const names=(await fs.readdir(new URL('supabase/migrations/',root))).filter(n=>n.endsWith('_add_validated_statistics_presets.sql'));
if(names.length!==1)throw Error('Create exactly one migration with supabase migration new add_validated_statistics_presets');
await fs.writeFile(new URL('supabase/migrations/'+names[0],root),sql.join('\n\n').trimEnd()+'\n');
console.log(`${fixture.services.length} source-backed presets rendered, no database writes`);
