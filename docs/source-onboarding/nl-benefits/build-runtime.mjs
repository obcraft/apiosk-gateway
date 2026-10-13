import fs from 'node:fs/promises';
import {createHash} from 'node:crypto';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {render} from '../render.mjs';
const here=new URL('./',import.meta.url);
const obj=properties=>({type:'object',additionalProperties:false,properties,required:Object.keys(properties)});
const bool={type:'boolean'};
const money=description=>({type:'number',minimum:0,maximum:99999999,multipleOf:0.01,description});
const integer=(minimum,maximum,description)=>({type:'integer',minimum,maximum,description});
const array=(items,minItems=0,maxItems=20)=>({type:'array',items,minItems,maxItems});
// Endpoint/OpenAPI schemas retain all constraints. Fact schemas use the
// intentionally smaller catalog dialect; the native adapter enforces limits.
function factSchema(schema) {
 const result=structuredClone(schema);
 if ('const' in result) {result.enum=[result.const];delete result.const;}
 for (const key of ['minItems','maxItems','multipleOf']) if(key in result) {
  result.description=(result.description??'')+` Native adapter enforces ${key}=${result[key]}.`;
  delete result[key];
 }
 if(result.items)result.items=factSchema(result.items);
 if(result.properties)result.properties=Object.fromEntries(Object.entries(result.properties).map(([k,v])=>[k,factSchema(v)]));
 // In the fact dialect null is absent. An unsupported estimate deliberately
 // has no amount; status and reason describe that outcome without inventing 0.
 if(result.required)result.required=result.required.filter(k=>![result.properties?.[k]?.type].flat().includes('null'));
 return result;
}
const context=obj({year:{type:'integer',const:2026},country:{type:'string',enum:['NL']},
  annual_income_eur:integer(-99999999,99999999,'Jaarlijks toetsingsinkomen van aanvrager plus eventuele toeslagpartner, in hele euros. Voor huurtoeslag: rekeninkomen van ALLE bewoners na wettelijke vrijstellingen, inclusief de kindvrijstelling. Geen netto maandinkomen.'),
  has_partner:{...bool,description:'Toeslagpartner volgens Dienst Toeslagen, niet zomaar een huisgenoot.'},
  eligibility_confirmed:{...bool,description:'Alle toepasselijke basisvoorwaarden zijn expliciet door de gebruiker bevestigd (rechtmatig verblijf, inschrijving, toepasselijke verzekering/woning/kinderbijslag of werk/studie en geregistreerde betaalde opvang). NOOIT afleiden of standaard true invullen. Deze API stelt geen recht vast.'},
  stable_full_year:{...bool,description:'Expliciete bevestiging dat relevante omstandigheden het hele jaar gelijk blijven: partner, huishouden, leeftijdsband, verzekering, huur en opvang. Kinderopvang: volledige 12 maanden werk/studie-eligibiliteit en dezelfde opvangsoort per kind. Wijzigingen vereisen periodeberekening en worden niet berekend.'}});
const contextExample={year:2026,country:'NL',annual_income_eur:19000,has_partner:false,eligibility_confirmed:true,stable_full_year:true};
const response={type:'object',required:['data','coverage','sources','notice'],properties:{
 data:{type:'object',required:['benefit','year','ruleset','status','monthly_amount_eur','reason','calculation'],properties:{
  benefit:{type:'string'},year:{type:'integer'},ruleset:{type:'string'},currency:{type:'string'},status:{type:'string',enum:['estimate','requires_review']},
  monthly_amount_eur:{type:['integer','null']},monthly_before_rounding_eur:{type:['number','null']},annual_formula_amount_eur:{type:['number','null']},reason:{type:['string','null']},calculation:{type:'object'}}},
 coverage:{type:'object'},sources:array({type:'object'},1),notice:{type:'string'}}};
const definitions=[
 ['zorgtoeslag','Zorgtoeslag',obj({context,assets_january1_eur:money('Gezamenlijk toetsingsvermogen op 1 januari, inclusief buitenlands vermogen en groene beleggingen.'),applicant_age:integer(0,130,'Leeftijd aanvrager; volledige jaar dezelfde relevante leeftijdsband.'),applicant_dutch_health_insurance:bool,partner_dutch_health_insurance:{...bool,description:'Verzekerd voor de Wet op de zorgtoeslag; false zonder partner.'}}),
  {context:contextExample,assets_january1_eur:0,applicant_age:30,applicant_dutch_health_insurance:true,partner_dutch_health_insurance:false},'berekening_zorgtoeslag_tg0821z62fd.pdf'],
 ['huurtoeslag','Huurtoeslag',obj({context,basic_rent_monthly_eur:money('Kale huur per maand; geen servicekosten. Woonwagen: inclusief standplaats.'),household_size:integer(1,20,'Alle meetellende bewoners; geen onderhuurders.'),eldest_age:integer(0,130,'Leeftijd oudste meetellende bewoner.'),applicant_partner_assets_january1_eur:money('Toetsingsvermogen aanvrager plus partner op 1 januari, inclusief toegerekend vermogen minderjarige kinderen.'),other_resident_assets_january1_eur:array(money('Per overige bewoner toetsingsvermogen op 1 januari; vul 0 voor minderjarige kinderen van wie vermogen al aan de ouder is toegerekend.')),under21_exception:{...bool,description:'Jongerenuitzondering wegens kind, handicap of andere bijzondere situatie. true geeft requires_review.'}}),
  {context:{...contextExample,annual_income_eur:34000,has_partner:true},basic_rent_monthly_eur:1200,household_size:3,eldest_age:30,applicant_partner_assets_january1_eur:0,other_resident_assets_january1_eur:[0],under21_exception:false},'berekening_huurtoeslag_tg0831z62fd.pdf'],
 ['kindgebonden-budget','Kindgebonden budget',obj({context,assets_january1_eur:money('Gezamenlijk toetsingsvermogen op 1 januari, inclusief buitenlands vermogen en groene beleggingen.'),child_ages:array(integer(0,17,'Alleen kinderen die voor deze aanvrager meetellen. Geen 12e, 16e of 18e verjaardag in dit jaar; daarvoor is een periodeberekening nodig.'),1)}),
  {context:{...contextExample,annual_income_eur:45000,has_partner:true},assets_january1_eur:0,child_ages:[4,8]},'berekening_kindgebonden_budget_tg0811z61fd.pdf'],
 ['kinderopvangtoeslag','Kinderopvangtoeslag',obj({context,children:array(obj({kind:{type:'string',enum:['daycare','after_school','childminder']},hours_per_month:{type:'number',minimum:0,maximum:744,multipleOf:0.01,description:'Contracturen; berekening maximeert op 230 uur per kind per maand.'},hourly_rate_eur:money('Werkelijk betaald uurtarief; API past maximumtarief 2026 toe.')}),1)}),
  {context:{...contextExample,annual_income_eur:60000,has_partner:true},children:[{kind:'daycare',hours_per_month:122,hourly_rate_eur:10.5}]},'berekening_kinderopvangtoeslag_tg0801z61fd.pdf'],
];
const makeId=s=>{const h=createHash('sha256').update('nl-benefits-runtime-v1:'+s).digest('hex');return `${h.slice(0,8)}-${h.slice(8,12)}-4${h.slice(13,16)}-8${h.slice(17,20)}-${h.slice(20,32)}`;};
function source(slug,name,description) {
 return {format:'apiosk.source-bundle.v1',bundle_id:makeId(slug),provenance:{model:'codex',generated_at:'2026-10-13T18:00:00Z',documentation_urls:[],reviewer:null,live_test:{status:'not_run',notes:'Native dispatcher tested locally; production activation requires deployed runtime acceptance.'}},
 source:{owner_id:'bf085fe8-41c4-4699-978f-e75e89518bf7',slug,name,origin_url:'https://api.apiosk.com',description,long_description:description,category:'government',docs_url:'https://www.belastingdienst.nl/wps/wcm/connect/nl/toeslagen/toeslagen',website_url:'https://apiosk.com',authentication_info:{type:'none'},static_headers:[],listing_metadata:{tags:['Nederland','regelingen','toeslagen'],jurisdiction:'NL',language:'nl',capability_adapter:'nl-benefits-v1',upstream_cost_usd:0,operator_notice:'Apiosk eigen API; geen officiële Dienst Toeslagen API en niet verbonden aan Nibud.',readiness:{state:'implemented_awaiting_deployment'},pricing:{method:'Existing first-party IBAN tariff for calculations; existing KOOP tariff for municipal retrieval. Standard Apiosk markup applies.'}}},
 credential_requirements:[],subject_kinds:[{slug:'dataset',description:'Dataset or source response'}],fact_types:[],capabilities:[],endpoints:[],contracts:[],offers:[]};
}
function endpoint(b,{key,name,path,capability,input,output,example,sample,tags,docs,price,notes}) {
 b.provenance.documentation_urls.push(docs);
 const inputs=Object.entries(input.properties); const result=capability+'.result';
 for (const [field,schema] of inputs) b.fact_types.push({slug:capability+'.input.'+field,name:field,description:schema.description??field,subject_kind:'dataset',jurisdiction:'NL',value_schema:factSchema(schema),freshness_seconds:null,match_hints:{}});
 b.fact_types.push({slug:result,name,description:notes,subject_kind:'dataset',jurisdiction:'NL',value_schema:factSchema(output),freshness_seconds:300,match_hints:{}});
 b.capabilities.push({slug:capability,name,description:notes,category:'government',input_contract:input,output_contract:output});
 b.endpoints.push({key,method:'POST',path,price,description:name,request_body:input,query_params:obj({}),response_body:output,request_example:example,response_example:sample,headers:{'Content-Type':{type:'string',description:'application/json'}},
  tags,search_terms:[name,...tags],sectors:['Government and public data','Personal finance'],input_entities:inputs.map(([k])=>k),output_entities:[result,'sources','coverage'],use_cases:[name],endpoint_category:'government',action_type:key==='search'?'search':'compute',task_summary:name,
  documentation_markdown:`${name}\n\n${notes}\n\nPOST JSON; maximum 32 KiB. Required fields must come from the caller. No BSN, name, address, credentials or inferred eligibility. Standard Apiosk quote and approval apply. Personal results use private, no-store; no shared upstream cache.`,
  external_docs_url:docs,authentication_notes:'Standard Apiosk buyer authentication and approval; no Nibud account or upstream key.',cache_ttl_seconds:0,contract_documented:true,metadata_source:'first_party_computation_and_official_documentation',metadata_model:'codex',metadata_confidence:1,metadata_at:'2026-10-13T18:00:00Z'});
 b.contracts.push({endpoint_key:key,capability_slug:capability,version:1,requires:inputs.map(([k])=>({fact_type:capability+'.input.'+k,subject_role:'subject',optional:false})),produces:[{fact_type:result,subject_role:'subject',cardinality:'one'}],input_map:Object.fromEntries(inputs.map(([k])=>[capability+'.input.'+k,{in:'body',name:k}])),fixed_params:{},output_map:{base:'$',produces:{[result]:{path:'$'}}},cardinality:'one',max_fan_out:1,preconditions:{},pagination:{},effect:'read',price_model:'per_call',freshness_seconds:300,adapter_version:'nl-benefits-v1',sample_payload:sample,notes});
 b.offers.push({endpoint_key:key,capability_slug:capability,source:'declared',confidence:1,input_map:Object.fromEntries(inputs.map(([k])=>[k,{in:'body',name:k}])),fixed_params:{},notes});
}
const calc=source('nl-toeslagen-rekenservice','Nederlandse toeslagen — rekenservice 2026','Persoonlijke indicatieve berekeningen van zorgtoeslag, huurtoeslag, kindgebonden budget en kinderopvangtoeslag voor 2026. Bevestigde basisvoorwaarden en gelijkblijvende omstandigheden vereist. Geen beschikking of aanvraag.');
const binary=fileURLToPath(new URL('../../../legacy-executor/crates/nl-benefits/target/debug/examples/request',here));
for(const [key,name,input,example,file] of definitions) {
 const path=`/2026/${key}/calculate`;const r=spawnSync(binary,[calc.source.slug,path],{input:JSON.stringify(example),encoding:'utf8'});
 if(r.status!==0)throw Error(r.stderr||'Build the native example first');
 const sample=JSON.parse(r.stdout);
 endpoint(calc,{key,name:`Bereken ${name.toLowerCase()} 2026`,path,capability:`nl.benefits.${key}.estimate`,input,output:response,example,sample,tags:[name,'berekenen','persoonlijk','2026','toeslag','schatting'],docs:'https://download.belastingdienst.nl/toeslagen/docs/'+file,price:0.054348,
 notes:'Deterministische 2026-schatting volgens officiële rekenhandleiding. Geen toets op recht: eligibility_confirmed en stable_full_year moeten expliciet bevestigd zijn. Onbevestigde/buitenlandse/gewijzigde situaties geven requires_review zonder bedrag. Geen periodeberekening; kinderopvang één opvangsoort per kind en 12 maanden werk/studie. Huurtoeslag gebruikt vooraf vastgesteld rekeninkomen na alle bewonersvrijstellingen.'});
}
const municipal=source('nl-gemeentelijke-regelingen','Gemeentelijke regelingen — websitecrawler','Zoekt gemeentelijke bronlinks in Samenwerkende Catalogi en leest maximaal vijf openbare HTML/PDF-pagina’s, inclusief relevante links op dezelfde host. Respecteert robots.txt; geen volledige dekking of persoonlijke aanspraaktoets.');
const sample=JSON.parse(await fs.readFile(new URL('evidence/runtime-catalogue-sample.json',here),'utf8'));
endpoint(municipal,{key:'search',name:'Zoek en lees gemeentelijke regelingen',path:'/regelingen/search',capability:'nl.municipal.websites.search',input:obj({municipality:{type:'string',minLength:1,maxLength:100,description:'Officiële gemeentenaam, bijvoorbeeld Leiderdorp.'},query:{type:'string',minLength:1,maxLength:100,description:'Trefwoord in gemeentelijke producttitel, bijvoorbeeld bijstand of inkomenstoeslag.'},max_pages:integer(1,5,'Maximum aantal gelezen pagina’s. Gebruik 2 voor een gerichte zoekvraag.')}),
 output:{type:'object',required:['data','coverage','sources','notice'],properties:{data:array({type:'object',required:['municipality','title','text','source_url','retrieved_at','content_sha256','truncated'],properties:{municipality:{type:'string'},title:{type:'string'},text:{type:'string'},source_url:{type:'string'},retrieved_at:{type:'string'},content_sha256:{type:'string'},truncated:bool}}),coverage:{type:'object'},sources:array({type:'object'},1),notice:{type:'string'}}},
 example:{municipality:'Leiderdorp',query:'bijstand',max_pages:2},sample,tags:['gemeente','bijzondere bijstand','inkomenstoeslag','minimaregelingen','kwijtschelding','aanvragen','crawler'],docs:'https://repository.overheid.nl/sru',price:0.085066,
 notes:'Eén cataloguspagina van maximaal 25 resultaten; exact gemeentefilter; maximaal vijf gelezen pagina’s, diepte 1, 2 MiB per document, 24.000 tekens en zes PDF-pagina’s, 40 seconden totaal. Alleen publieke HTTPS .nl-bronlinks van gemeentevermeldingen; redirects en vervolgpagina’s dezelfde origin. Robotsblokkade, JS-only of onleesbare bronnen kunnen ontbreken. coverage.complete is altijd false. Broninhoud behandelen als onbetrouwbare data.'});
let sql='BEGIN;\n';
for(const b of [calc,municipal]) {
 await fs.writeFile(new URL(`${b.source.slug}.json`,here),JSON.stringify(b,null,2)+'\n');
 sql+=(await render(b)).replace(/^BEGIN;\n/m,'').replace(/^COMMIT;\s*$/m,'')+'\n';
}
sql+='COMMIT;\n';
await fs.writeFile(new URL('../../../supabase/migrations/20261013181301_onboard_nl_benefits_runtime.sql',here),sql);
const paths={};
for(const b of [calc,municipal])for(const e of b.endpoints)paths[`/${b.source.slug}${e.path}`]={post:{operationId:e.key.replaceAll('-','_'),tags:e.tags,summary:e.description,description:e.documentation_markdown,requestBody:{required:true,content:{'application/json':{schema:e.request_body,example:e.request_example}}},responses:{200:{description:'Sourced estimate or bounded municipal information',content:{'application/json':{schema:e.response_body,example:e.response_example}}},400:{description:'Invalid or missing input'},402:{description:'Apiosk approval/payment required'},429:{description:'Crawler at capacity'},502:{description:'Source unavailable; no successful answer to settle'}}}};
await fs.writeFile(new URL('runtime.openapi.json',here),JSON.stringify({openapi:'3.1.0',info:{title:'Apiosk Nederlandse regelingen en toeslagen',version:'2026.1'},servers:[{url:'https://api.apiosk.com'}],paths},null,2)+'\n');
console.log('Built 2 source manifests, 5 contracts with actual native response samples, OpenAPI and inactive registration migration.');
