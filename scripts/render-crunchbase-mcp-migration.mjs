import fs from 'node:fs/promises';
import {source,origin,docs,tools,capabilities,outputSchema,pricing} from './crunchbase-mcp-catalog.mjs';
const q=v=>`'${String(v).replaceAll("'","''")}'`;
const j=v=>`${q(JSON.stringify(v))}::jsonb`;
const arr=v=>`ARRAY[${v.map(q).join(',')}]::text[]`;
const sql=[`-- Official Crunchbase MCP read tools. Documentation is not live execution proof.
-- Existing Crunchbase listing and API.market operations remain intact.
-- MCP endpoints stay inactive/draft until OAuth, tool schemas, entitlement,
-- pricing and real output validation have completed. No Jina source is created.
BEGIN;
SELECT pg_advisory_xact_lock(hashtext('apiosk-crunchbase'));
DO $guard$ BEGIN
 IF NOT EXISTS(SELECT 1 FROM providers.apis WHERE slug=${q(source)} AND name='Crunchbase') THEN RAISE EXCEPTION 'Existing Crunchbase source missing'; END IF;
END $guard$;
INSERT INTO catalog.fact_subject_kinds(slug,description) VALUES ('company','Company or organization'),('person','Professional person'),('funding','Funding round'),('research','Source query or reference') ON CONFLICT DO NOTHING;`];
for(const t of tools){
 sql.push(`INSERT INTO providers.api_endpoints(id,api_id,method,path,price,payment_required,active,description,request_body,request_example,response_body,tags,sectors,endpoint_category,action_type,task_summary,external_docs_url,metadata_source,metadata_at,origin_override)
 SELECT gen_random_uuid(),id,'POST',${q('/mcp/'+t.tool)},0,false,false,${q(t.description)},${j(t.schema)},${j(t.example)},${j(outputSchema)},${arr(t.tags)},ARRAY['Business','Finance'],${q(t.category)},${q(t.action)},${q(t.name)},${q(docs)},'official-provider-documentation-awaiting-oauth',now(),${q(origin)} FROM providers.apis WHERE slug=${q(source)}
 ON CONFLICT(api_id,method,path) DO NOTHING;`);
}
// The legacy fact validator supports a smaller schema vocabulary. Endpoint
// JSON Schema and adapter bounds remain authoritative, including maxItems/UUID.
function factSchema(value){
 if(Array.isArray(value))return value.map(factSchema);
 if(!value||typeof value!=='object')return value;
 return Object.fromEntries(Object.entries(value).filter(([k])=>!['maxItems','format'].includes(k)).map(([k,v])=>[k,factSchema(v)]));
}
const facts=new Map();
export function factName(cap,key){
 return key==='entity_id'?`${cap.category}.crunchbase_entity_id`:`crunchbase.${cap.tool.replace('cb_','')}_${key}`;
}
for(const c of capabilities){
 const t=tools.find(t=>t.tool===c.tool);
 const required=c.required;
 const properties=t.schema.properties;
 const requires=[],inputMap={};
 const fixed={...c.fixed};
 for(const [key,schema]of Object.entries(properties)){
  if(key in fixed)continue;
  const slug=factName(c,key);
  facts.set(slug,{name:`Crunchbase ${key.replaceAll('_',' ')}`,description:schema.description||`Crunchbase ${key}`,schema:factSchema(schema),kind:key==='entity_id'?c.category:'research'});
  requires.push({fact_type:slug,subject_role:'subject',optional:!required.includes(key)});
  inputMap[slug]={name:key,in:'body',...(schema.default!==undefined?{default:schema.default}:{})};
 }
 const result=`${c.slug.replaceAll('-','_')}_result`;
 facts.set(result,{name:c.name+' result',description:t.description,schema:outputSchema,kind:c.category});
 c.contract={requires,produces:[{fact_type:result,subject_role:'subject',cardinality:'one'}],input_map:inputMap,fixed_params:fixed,output_map:{base:'$',produces:{[result]:{path:'$'}}},cardinality:'one',max_fan_out:1};
 sql.push(`INSERT INTO catalog.capabilities(slug,name,description,category,input_contract,output_contract,status)
 VALUES(${q(c.slug)},${q(c.name)},${q(c.name+'. '+t.description)},${q(c.category)},${j(t.schema)},${j(outputSchema)},'draft') ON CONFLICT(slug) DO NOTHING;`);
}
for(const [slug,f]of facts){
 sql.push(`INSERT INTO catalog.fact_types(slug,name,description,subject_kind,value_schema,status,freshness_seconds)
 VALUES(${q(slug)},${q(f.name)},${q(f.description)},${q(f.kind)},${j(f.schema)},'draft',3600) ON CONFLICT(slug) DO NOTHING;`);
}
for(const c of capabilities){
 const k=c.contract;
 sql.push(`INSERT INTO catalog.capability_execution_contracts(id,capability_slug,endpoint_id,version,status,adapter_version,requires,produces,input_map,fixed_params,output_map,cardinality,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,notes)
 SELECT gen_random_uuid(),${q(c.slug)},e.id,1,'draft','crunchbase-mcp-v1',${j(k.requires)},${j(k.produces)},${j(k.input_map)},${j(k.fixed_params)},${j(k.output_map)},'one',1,'{}','{}','read','per_call',3600,'Official MCP; bounded single page; retain all readable company, people and funding fields/cards. Public-page enrichment is internal and best effort. Awaiting OAuth and live validation.'
 FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id WHERE a.slug=${q(source)} AND e.path=${q('/mcp/'+c.tool)} AND e.method='POST'
 ON CONFLICT(capability_slug,endpoint_id,version) DO NOTHING;`);
}
sql.push(`UPDATE providers.apis SET listing_metadata=coalesce(listing_metadata,'{}'::jsonb) || ${j({official_mcp:{url:origin,tools:tools.map(t=>t.tool),status:'awaiting_oauth_validation',documentation:docs},page_enrichment:{provider:'Jina AI Reader',internal:true,best_effort:true,extra_charge:false,limit:1},coverage_notice:'API.market company name lookup and unfiltered browsing are available. Official Crunchbase company, people and funding search/filter/profile tools are registered but await OAuth, account entitlement and live validation. Public-page enrichment is best effort; login/verification pages are excluded.'})} WHERE slug=${q(source)} AND NOT (coalesce(listing_metadata,'{}'::jsonb) ? 'official_mcp');
COMMIT;`);
const file=new URL('../supabase/migrations/20261011133137_register_crunchbase_official_mcp.sql',import.meta.url);
if(!process.argv.includes('--fixture-only'))await fs.writeFile(file,sql.join('\n\n')+'\n');
// Local fixture uses synthetic responses explicitly; it is never activation evidence.
const base=JSON.parse(await fs.readFile(new URL('../tests/fixtures/crunchbase/catalog.json',import.meta.url)));
const fixture={known_sources:[{...base.known_sources[0],metadata:{description:'Official Crunchbase company, people and funding search, filter and profile data',category:'company',tags:[...new Set(tools.flatMap(t=>t.tags))],identity_documented:true}}],services:[],vocabulary:[...facts].map(([slug,f])=>({slug,name:f.name,description:f.description,value_schema:f.schema})),capabilities:capabilities.map(c=>({slug:c.slug,name:c.name,description:c.name+'. '+tools.find(t=>t.tool===c.tool).description}))};
let n=0;
for(const c of capabilities){
 const t=tools.find(t=>t.tool===c.tool),i=tools.indexOf(t)+1;
 const payload={source:'Crunchbase',transport:'official_mcp',tool:c.tool,data:{fixture:true,properties:{identifier:{entity_def_id:c.fixed.entity_def_id||'organization',permalink:'example'}},cards:{employees:[{title:'CEO'}],raised_funding_rounds:[{money_raised:1000000}]}},retrieved_at:'2026-10-11T00:00:00Z'};
 fixture.services.push({id:`1cb00000-0000-4000-8000-${String(++n).padStart(12,'0')}`,endpoint_id:`2cb00000-0000-4000-8000-${String(i).padStart(12,'0')}`,api_slug:source,name:'Crunchbase',description:c.name+'. '+t.description,source_url:docs,method:'POST',path:'/mcp/'+c.tool,listing_type:'api',list_atomic:pricing.listAtomic,fixed_params:c.contract.fixed_params,preconditions:{},pagination:{},freshness_seconds:3600,sample_payload:payload,contract:{...c.contract,capability_slug:c.slug,version:1,effect:'read',price_model:'per_call'}});
}
await fs.writeFile(new URL('../tests/fixtures/crunchbase/official-mcp.json',import.meta.url),JSON.stringify(fixture,null,2)+'\n');
console.log(`${tools.length} documented endpoints, ${capabilities.length} draft capabilities; no activation or prices inferred.`);
