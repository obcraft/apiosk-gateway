import fs from 'node:fs/promises';
import {buildOperations} from './source-operation-contracts.mjs';
const [input,output,migration,chunksDir]=process.argv.slice(2);
if(!input||!output||!migration)throw Error('Usage: node prepare-source-operations.mjs input.json output-dir cli-created-migration.sql');
const snapshot=JSON.parse(await fs.readFile(input,'utf8'));
const result=buildOperations(snapshot);
await fs.mkdir(output,{recursive:true});
const known_sources=snapshot.apis.map(a=>({slug:a.slug,name:a.name,block_reason:a.planner_block_reason||null,metadata:{
 description:'',provider_slug:snapshot.provider_slugs?.[a.slug]??null,
 capability_descriptions:Object.fromEntries(result.services.filter(s=>s.api_slug===a.slug).map(s=>[s.contract.capability_slug,s.description])),
 endpoint_count:snapshot.endpoints.filter(e=>e.api_id===a.id).length,
}}));
await fs.writeFile(`${output}/catalog.json`,JSON.stringify({services:result.services,vocabulary:result.vocabulary,known_sources},null,2)+'\n');
await fs.writeFile(`${output}/coverage.json`,JSON.stringify(result.report,null,2)+'\n');
const q=x=>x==null?'NULL':`'${String(x).replaceAll("'","''")}'`,j=x=>`${q(JSON.stringify(x))}::jsonb`;
const sql=['-- Provider-neutral opaque document retrieval. Staged, not published.\n-- Synthetic adapter fixtures prove mechanics, not provider data or live success.','BEGIN;',
 "SELECT pg_advisory_xact_lock(hashtext('source-document-v1'));",
 "INSERT INTO catalog.fact_subject_kinds(slug,description) VALUES ('source_input','Endpoint-scoped declared inputs'),('source_document','Opaque provider documents without normalized fact claims') ON CONFLICT DO NOTHING;"];
for(const w of result.vocabulary)if(w.slug.startsWith('source_input.')||w.slug.startsWith('source_document.'))sql.push(`INSERT INTO catalog.fact_types(slug,name,description,subject_kind,value_schema,status) VALUES(${q(w.slug)},${q(w.description)},${q(w.description)},${q(w.subject_kind)},${j(w.value_schema)},'active') ON CONFLICT(slug) DO NOTHING;`);
for(const s of result.services){
 const e=snapshot.endpoints.find(e=>e.id===s.endpoint_id),c=s.contract;
 sql.push(`DO $guard$ BEGIN IF NOT EXISTS(SELECT 1 FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id WHERE e.id=${q(e.id)}::uuid AND a.slug=${q(s.api_slug)} AND a.status='active' AND a.is_verified AND coalesce(a.discoverable,true) AND a.suspended_at IS NULL AND a.listing_metadata->>'planner_block_reason' IS NULL AND e.active AND e.method=${q(e.method)} AND e.path=${q(e.path)} AND ceil(e.price*1000000)::bigint=${s.list_atomic} AND e.query_params IS NOT DISTINCT FROM ${e.query_params==null?'NULL':j(e.query_params)} AND e.request_body IS NOT DISTINCT FROM ${e.request_body==null?'NULL':j(e.request_body)} AND e.contract_documented AND e.action_type=${q(e.action_type)}) THEN RAISE EXCEPTION 'Source operation drift: %',${q(s.api_slug+' '+e.path)}; END IF; END $guard$;`);
 sql.push(`INSERT INTO catalog.capabilities(slug,name,description,category,input_contract,output_contract,status) VALUES(${q(c.capability_slug)},${q(s.description.slice(0,180))},${q(s.description)},'source_document','{}','{}','active') ON CONFLICT(slug) DO NOTHING;`);
 const produces=Object.keys(c.output_map.produces).map(fact_type=>({fact_type,subject_role:'subject',cardinality:c.cardinality,...(s.optional_outputs?.includes(fact_type)?{optional:true}:{})}));
 sql.push(`INSERT INTO catalog.capability_execution_contracts(id,capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,output_map,cardinality,identifies,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,sample_payload,validated_at,validated_note,notes,adapter_version) VALUES(${q(s.id)}::uuid,${q(c.capability_slug)},${q(e.id)}::uuid,1,'validated',${j(c.requires)},${j(produces)},${j(c.input_map)},${j(s.fixed_params)},${j(c.output_map)},${q(c.cardinality)},${q(c.identifies)},1,${j(s.preconditions)},${j(s.pagination)},'read','per_call',3600,${j(s.sample_payload)},now(),'Document adapter mechanics fixture or preserved authored family alias; NOT proof of live execution','source-document-v1; omitted optional inputs: ${result.report.find(r=>r.contract_id===s.id).omitted_optional_inputs.join(', ').replaceAll("'","''")}','source-document-v1') ON CONFLICT(id) DO NOTHING;`);
}
sql.push('COMMIT;','');
await fs.writeFile(migration,sql.join('\n\n'));
// The Management API rejects large requests. Staged rows may be written in
// bounded idempotent transactions; promotion remains one separate atomic step.
if(chunksDir){
 await fs.mkdir(chunksDir,{recursive:true});let chunk=[],bytes=0,index=0;
 const flush=async()=>{if(chunk.length)await fs.writeFile(`${chunksDir}/${String(index++).padStart(3,'0')}.sql`,['BEGIN;',...chunk,'COMMIT;'].join('\n\n'));chunk=[];bytes=0;};
 for(const statement of sql.filter(s=>s!=='BEGIN;'&&s!=='COMMIT;'&&s!=='')){
  if(bytes+Buffer.byteLength(statement)>300_000)await flush();chunk.push(statement);bytes+=Buffer.byteLength(statement);
 }
 await flush();
}
console.log(JSON.stringify({staged_contracts:result.services.length,sources:new Set(result.services.map(s=>s.api_slug)).size,vocabulary:result.vocabulary.length,output,migration}));
