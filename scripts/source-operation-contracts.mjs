// Provider-neutral document retrieval contracts. Dry run; never calls providers
// or promotes contracts. Publication requires the Gateway runtime audit.
import crypto from 'node:crypto';
const digest = x => crypto.createHash('sha256').update(JSON.stringify(x)).digest('hex');
const uuid = x => { const h=digest(x); return `${h.slice(0,8)}-${h.slice(8,12)}-4${h.slice(13,16)}-8${h.slice(17,20)}-${h.slice(20,32)}`; };
const slug = s => s.toLowerCase().replace(/[^a-z0-9]+/g,'-').replace(/^-|-$/g,'');
const supported = new Set(['type','enum','pattern','minimum','maximum','minLength','maxLength','description','title','default','examples']);
const annotations = new Set(['required','example','format','x-parameter-location','nullable']);
export function scalarSchema(spec) {
  if(!['string','integer','number'].includes(spec?.type) || spec.nullable) return null;
  if(Object.keys(spec).some(k=>!supported.has(k)&&!annotations.has(k))) return null;
  // A format is a real constraint; only formats the transport or an authored
  // pattern already enforces are admissible without an additional validator.
  if(spec.format && !['int32','int64','float','double'].includes(spec.format)) return null;
  return Object.fromEntries(Object.entries(spec).filter(([k])=>supported.has(k)&&k!=='default'&&k!=='examples'));
}
export function buildOperations(snapshot) {
  const words=new Map(); const services=[]; const capabilities=[]; const report=[];
  for(const e of [...snapshot.endpoints].sort((a,b)=>a.id.localeCompare(b.id))) {
    const a=snapshot.apis.find(a=>a.id===e.api_id); if(!a)continue;
    const row={source:a.slug,endpoint_id:e.id,method:e.method,path:e.path,blockers:[],omitted_optional_inputs:[]};report.push(row);
    const stop=reason=>row.blockers.push(reason);
    if(a.planner_block_reason)stop('source_blocked');
    if(!e.contract_documented || !e.description?.trim() || !(e.external_docs_url||e.openapi_url||a.docs_url)?.startsWith('https://')) stop('missing_operation_documentation');
    if(!['GET','POST'].includes(e.method)||!['lookup','search','compute','generate'].includes(e.action_type))stop('effect_review_required');
    if(/\b(create|delete|send|publish|transfer|order|subscribe|unsubscribe|render|upload|execute|deploy)\b/i.test(e.path))stop('effect_review_required');
    if(/exploit|penetration|pentest|intrusion|payload|offensive|vulnerability.?scan|attack.surface|open ports|credential.dump|initial.access.broker/i.test(`${e.path} ${e.description}`))stop('specialized_review_required');
    if(/pending job|asynchronous|async job/i.test(e.description))stop('asynchronous_result_review_required');
    if(!Number.isFinite(Number(e.price))||Number(e.price)<0)stop('missing_fixed_price');
    // A reviewed input decision (snapshot.reviewed_inputs) stands in for a
    // curated offer and names which optional inputs a question must supply.
    const review=snapshot.reviewed_inputs?.[e.id]?.decision==='contract'?snapshot.reviewed_inputs[e.id]:null;
    if(!review&&!snapshot.offers.some(o=>o.endpoint_id===e.id&&['curated','declared'].includes(o.source)))stop('missing_reviewed_offer');
    // Existing authored contracts keep priority and identity. Never rewrite
    // their mappings, limits, dependencies or optional input behavior.
    if(snapshot.contracts.some(c=>c.endpoint_id===e.id&&c.status==='active'&&c.validated_at&&c.effect==='read'&&c.price_model==='per_call'))stop('already_mapped');
    if(row.blockers.length)continue;
    const requires=[];const input_map={};const localWords=[];const names=new Set();
    let documented=false;
    for(const [location,shape] of [['query',e.query_params],['body',e.request_body]]) {
      if(shape==null)continue;
      if(shape.type!=='object'||!shape.properties||typeof shape.properties!=='object'||Array.isArray(shape.properties)
          ||Object.keys(shape).some(k=>!['type','properties','required','description','additionalProperties'].includes(k))) {
        stop('unsupported_input_schema');continue;
      }
      documented=true;
      if(shape.required!=null&&!Array.isArray(shape.required)){stop('unsupported_required_schema');continue;}
      for(const name of shape.required??[])if(!Object.hasOwn(shape.properties,name))stop('undeclared_required_input');
      for(const [name,spec]of Object.entries(shape.properties)) {
        const required=(shape.required??[]).includes(name)||spec.required===true||!!review?.required?.includes(name);
        const schema=scalarSchema(spec);
        const variableCost=/\b(credits?|extra cost|additional cost|surcharge)\b|\+\s*\d/.test(spec.description??'');
        if(!schema||variableCost) {if(required)stop('unsupported_required_input');else row.omitted_optional_inputs.push(name);continue;}
        const wire=spec['x-parameter-location']??location;
        if(!['query','body','path'].includes(wire)||names.has(name)||!/^\w+$/.test(name)){stop('unsupported_input_binding');continue;}
        names.add(name);
        // Endpoint-scoped schemas prevent a q/name/country from another API
        // silently imposing different enums, meanings or bounds.
        const fact=`source_input.e${e.id.replaceAll('-','')}.${slug(name)}`;
        localWords.push({slug:fact,value_schema:schema,subject_kind:'source_input',description:spec.description||`${a.name}: ${name}`});
        requires.push({fact_type:fact,subject_role:'subject',optional:!required});
        input_map[fact]={in:wire,name};
      }
    }
    if(!documented)stop('missing_input_schema');
    const pathNames=[...e.path.matchAll(/\{([^}]+)\}/g)].map(m=>m[1]);
    if(pathNames.some(name=>!Object.values(input_map).some(b=>b.in==='path'&&b.name===name)))stop('unbound_path_input');
    // Optional-only input sets often mean undocumented alternatives (company
    // OR ticker). Keep them for review rather than charging an empty request.
    // A review may also state that the operation needs no input at all.
    if(!requires.some(r=>!r.optional)&&!(review&&review.required?.length===0&&review.no_input_reason))stop('required_input_alternatives_need_review');
    if(review?.required?.some(name=>!names.has(name)))stop('reviewed_input_not_bound');
    if(row.blockers.length)continue;
    const cap=`operation.${slug(a.slug)}.${e.method.toLowerCase()}-${slug(e.path)}-${e.id.slice(0,8)}`;
    const output=`source_document.e${e.id.replaceAll('-','')}.response`;
    localWords.push({slug:output,subject_kind:'source_document',description:'Opaque source document. No normalized identifiers or financial claims; analyze saved data with source citations.',value_schema:{type:'object',required:['document']}});
    localWords.forEach(w=>words.set(w.slug,w));
    const description=e.description;
    const id=uuid(['source-document-v1',e.id]);
    const service={id,endpoint_id:e.id,api_slug:a.slug,name:a.name,description,source_url:e.external_docs_url||a.docs_url,
      method:e.method,path:e.path,listing_type:a.listing_type||'api',list_atomic:Math.ceil(Number(e.price)*1e6),
      fixed_params:{},preconditions:{},pagination:{},freshness_seconds:3600,jurisdiction:null,
      sample_payload:{data:'Synthetic document-adapter fixture; not a provider response or live execution.'},
      contract:{capability_slug:cap,version:1,requires,input_map,output_map:{base:'$',produces:{[output]:{adapter:'document.response',key:'document'}}},cardinality:'one',identifies:null,max_fan_out:1}};
    services.push(service);capabilities.push({slug:cap,description});row.capability=cap;row.contract_id=id;
  }
  // These reviewed capabilities have the same input/behavior. Keep each native
  // contract and expose an additional neutral family for price comparison.
  const families={
    'web.browserbase-fetch':['web.source-fetch','Retrieve the content of one public web page.'],
    'web.linkup-fetch':['web.source-fetch','Retrieve the content of one public web page.'],
    'web.browserbase-search':['web.source-search','Search the public web for relevant pages and supporting evidence.'],
    'web.stableenrich-search':['web.source-search','Search the public web for relevant pages and supporting evidence.'],
  };
  for(const original of snapshot.contracts.filter(c=>c.status==='active'&&c.validated_at&&c.effect==='read'&&c.price_model==='per_call')) {
    const family=families[original.capability_slug];if(!family)continue;
    const e=snapshot.endpoints.find(e=>e.id===original.endpoint_id),a=snapshot.apis.find(a=>a.id===e?.api_id);
    if(!e||!a||a.planner_block_reason)continue;
    const [cap,description]=family,id=uuid(['source-family-v1',original.id,cap]);
    const requiredWords=[...original.requires.map(r=>r.fact_type),...Object.keys(original.output_map.produces)];
    for(const fact of requiredWords){const w=snapshot.vocabulary?.find(w=>w.slug===fact);if(!w)throw Error(`Missing authored vocabulary ${fact}`);words.set(fact,w);}
    services.push({id,endpoint_id:e.id,api_slug:a.slug,name:a.name,description,source_url:a.docs_url,
      method:e.method,path:e.path,listing_type:a.listing_type||'api',list_atomic:Math.ceil(Number(e.price)*1e6),
      fixed_params:original.fixed_params,preconditions:original.preconditions,pagination:original.pagination,
      freshness_seconds:original.freshness_seconds,jurisdiction:null,sample_payload:original.sample_payload,
      optional_outputs:original.produces.filter(p=>p.optional).map(p=>p.fact_type),
      contract:{capability_slug:cap,version:1,requires:original.requires,input_map:original.input_map,
        output_map:original.output_map,cardinality:original.cardinality,identifies:original.identifies,max_fan_out:original.max_fan_out}});
    capabilities.push({slug:cap,description});
    report.push({source:a.slug,endpoint_id:e.id,capability:cap,contract_id:id,blockers:[],omitted_optional_inputs:[],kind:'authored_family_alias'});
  }
  return {vocabulary:[...words.values()],services,capabilities,report};
}
