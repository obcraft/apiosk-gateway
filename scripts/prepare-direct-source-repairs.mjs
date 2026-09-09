// Read-only metadata snapshot and deterministic migration renderer. Never reads provider credentials.
import fs from 'node:fs/promises';
const root = new URL('../', import.meta.url);
const fixture = new URL('tests/fixtures/direct-source-repairs.json', root);
const migration = new URL('supabase/migrations/20260909130757_repair_direct_source_contracts.sql', root);
const q = x => x == null ? 'NULL' : `'${String(x).replaceAll("'", "''")}'`;
const j = x => `${q(JSON.stringify(x))}::jsonb`;
let snapshot;
if (process.argv.includes('--snapshot')) {
  const read = async (table, select, filters = {}) => {
    const url = new URL('/rest/v1/' + table, process.env.SUPABASE_URL);
    for (const [k,v] of Object.entries({select,...filters})) url.searchParams.set(k,v);
    const key = process.env.SUPABASE_SERVICE_ROLE_KEY;
    const res = await fetch(url,{headers:{apikey:key,Authorization:`Bearer ${key}`},redirect:'error',signal:AbortSignal.timeout(30000)});
    if (!res.ok) throw Error(`Metadata HTTP ${res.status}`);
    return res.json();
  };
  const ids = ['b050c654-284a-49e8-a7ca-0bfca565816e','e1dc0681-d7e3-48d7-8f1a-eafca2eab00b','3f20905f-d8f3-4243-9728-e28f62a4890e','2f884bdc-79d7-45ac-9aeb-c52c88e6a0f2'];
  const contracts = await read('capability_execution_contracts','*',{endpoint_id:`in.(${ids.join(',')})`,status:'eq.active'});
  const endpoints = await read('api_endpoints','id,api_id,path,method,price',{id:`in.(${ids.join(',')})`});
  const apis = await read('apis','id,slug,name',{id:`in.(${endpoints.map(e=>e.api_id).join(',')})`});
  const words = new Set(contracts.flatMap(c=>[...c.requires.map(r=>r.fact_type),...Object.keys(c.output_map.produces)]));
  snapshot = {vocabulary:await read('fact_types','slug,value_schema',{slug:`in.(${[...words].join(',')})`}), services:[], originals:contracts};
  for (const c of contracts) {
    const e=endpoints.find(e=>e.id===c.endpoint_id),a=apis.find(a=>a.id===e.api_id);
    const file=e.path.includes('basisprofielen')?'coolblue-profile':e.path.includes('publicaties')?'tenders':e.path.includes('locatieserver')?'dam-address':'cbs-bankruptcies';
    const evidence=JSON.parse(await fs.readFile(new URL(`output/direct-usecases-iuqQRD/${file}.json`,root),'utf8'));
    snapshot.services.push({id:c.id,endpoint_id:e.id,api_slug:a.slug,name:a.name,method:e.method,path:e.path,listing_type:'api',list_atomic:Math.ceil(Number(e.price)*1e6),fixed_params:c.fixed_params,preconditions:c.preconditions,pagination:c.pagination,freshness_seconds:c.freshness_seconds,sample_payload:evidence.data,jurisdiction:null,contract:{capability_slug:c.capability_slug,version:c.version,cardinality:c.cardinality,requires:c.requires,input_map:c.input_map,output_map:c.output_map,identifies:c.identifies,max_fan_out:c.max_fan_out}});
  }
  await fs.writeFile(fixture,JSON.stringify(snapshot,null,2)+'\n');
}
// Reload to avoid sharing mutable maps with the immutable before snapshot.
snapshot=JSON.parse(await fs.readFile(fixture,'utf8'));
const newWords = [
  {slug:'tenderned.publication_page',subject_kind:'dataset',value_schema:{type:'object',required:['content','totalElements'],properties:{content:{type:'array',items:{type:'object'}},totalElements:{type:'number'}}}},
  {slug:'pdok.search_page',subject_kind:'geo',value_schema:{type:'object',required:['response'],properties:{response:{type:'object',required:['docs','numFound'],properties:{docs:{type:'array',items:{type:'object'}},numFound:{type:'number'}}}}}},
];
snapshot.vocabulary.push(...newWords);
const sql=['-- Versioned repairs proven against multi-row upstream responses. Preserve historic contracts and source data.','BEGIN;',"SELECT pg_advisory_xact_lock(hashtext('direct-source-contract-repairs'));"];
for (const w of newWords) sql.push(`INSERT INTO public.fact_types(slug,name,description,subject_kind,value_schema,status,freshness_seconds) VALUES(${q(w.slug)},${q(w.slug)},'Complete returned page, not all matching records. Preserve identity, dates, types and pagination.',${q(w.subject_kind)},${j(w.value_schema)},'active',3600) ON CONFLICT(slug) DO NOTHING;`);
for (const s of snapshot.services) {
  const c=s.contract,old=snapshot.originals.find(x=>x.id===s.id);
  if (s.path.includes('basisprofielen')) c.output_map.produces['company.name']={path:'$.statutaireNaam'};
  else if(s.path.includes('publicaties')) c.output_map={base:'$',produces:{'tenderned.publication_page':{path:'$'}}};
  else if(s.path.includes('locatieserver')) c.output_map={base:'$',produces:{'pdok.search_page':{path:'$'}}};
  else {
    s.fixed_params={...s.fixed_params,'$orderby':'Perioden desc'};
    c.requires=c.requires.filter(r=>r.fact_type!=='cbs.input.odata_orderby');
    delete c.input_map['cbs.input.odata_orderby'];
  }
  c.version++;
  const produces=Object.keys(c.output_map.produces).map(fact_type=>({fact_type,subject_role:'subject',cardinality:c.cardinality}));
  sql.push(`DO $guard$ BEGIN IF NOT EXISTS (SELECT 1 FROM public.capability_execution_contracts WHERE id=${q(old.id)} AND version=${old.version} AND status='active' AND output_map=${j(old.output_map)} AND fixed_params=${j(old.fixed_params)}) THEN RAISE EXCEPTION 'Source contract drift: %',${q(old.id)}; END IF; END $guard$;`);
  // The partial unique index permits one active version. Both changes are atomic.
  sql.push(`UPDATE public.capability_execution_contracts SET status='retired' WHERE id=${q(old.id)};`);
  sql.push(`INSERT INTO public.capability_execution_contracts(capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,output_map,cardinality,identifies,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,sample_payload,validated_at,validated_note,notes) SELECT capability_slug,endpoint_id,${c.version},'active',${j(c.requires)},${j(produces)},${j(c.input_map)},${j(s.fixed_params)},${j(c.output_map)},cardinality,identifies,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,${j(s.sample_payload)},now(),'Multi-row direct-source regression fixtures; no guessed identities or scalar arrays','direct-source-repairs-20260909' FROM public.capability_execution_contracts WHERE id=${q(old.id)};`);
}
sql.push('COMMIT;','');
await fs.writeFile(new URL('tests/fixtures/direct-source-repaired.json',root),JSON.stringify({vocabulary:snapshot.vocabulary,services:snapshot.services},null,2)+'\n');
await fs.writeFile(migration,sql.join('\n\n').trimEnd()+'\n');
console.log(`${snapshot.services.length} versioned contracts rendered; no database writes`);
