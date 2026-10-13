// Catalog registration only. New integrations remain inactive until runtime acceptance.
import fs from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { render } from '../render.mjs';

const here = new URL('./', import.meta.url);
const date = '2026-10-13';
const probes = JSON.parse(await fs.readFile(new URL('evidence/probes.json',here),'utf8'));
const checked = probes.checked_at;
const owner = 'bf085fe8-41c4-4699-978f-e75e89518bf7';
const sruDocs = 'https://data.overheid.nl/sites/default/files/dataset/d0cca537-44ea-48cf-9880-fa21e1a7058f/resources/HandleidingSRU2.0.pdf';
const obj = (properties = {}, required = []) => ({type:'object',properties, ...(required.length ? {required} : {})});
const str = {type:'string'};
const int = {type:'integer'};
const sqlJson = value => `'${JSON.stringify(value).replaceAll("'", "''")}'::jsonb`;
const uid = slug => {
  const h = createHash('sha256').update(`nl-benefits-20261013:${slug}`).digest('hex');
  return `${h.slice(0,8)}-${h.slice(8,12)}-4${h.slice(13,16)}-8${h.slice(17,20)}-${h.slice(20,32)}`;
};
const bundles = [];
function source(slug, name, origin, description, category, docs, tags, blockers) {
  const b = {
    format:'apiosk.source-bundle.v1',bundle_id:uid(slug),
    provenance:{model:'codex',generated_at:checked,documentation_urls:[docs],reviewer:null,
      live_test:{status:'not_run',notes:'Documentation verification is not an Apiosk execution test.'}},
    source:{owner_id:owner,slug,name,origin_url:origin,description,long_description:description,
      category,docs_url:docs,website_url:docs,authentication_info:{type:'none'},static_headers:[],
      listing_metadata:{tags,sectors:['Government and public data'],jurisdiction:'NL',language:'nl',
        coverage_notice:description,readiness:{state:'staged',blockers},
        pricing:{state:'unpriced',upstream_cost_usd:null,listing_price_note:'Zero is a required database placeholder for inactive endpoints, not a free production offer.'},
        operator_notice:'Apiosk integration; not affiliated with the publisher.'}},
    credential_requirements:[],subject_kinds:[{slug:'dataset',description:'Dataset or source response'}],
    fact_types:[],capabilities:[],endpoints:[],contracts:[],offers:[],
  };
  bundles.push(b); return b;
}
function endpoint(b, {key,method='GET',path,name,capability,tags,category=b.source.category,
  query=obj(),body=null,response={type:['object','array']},example=null,docs=b.source.docs_url,
  notes='',fixed={},outputSchema=response,inputSchema=body??query,bindings=null,origin=null}) {
  const output = `${capability}.response`;
  const inputs = bindings ?? Object.entries(inputSchema.properties??{}).map(([name,schema]) => ({
    name,schema,location:body?'body':'query',required:(inputSchema.required??[]).includes(name),
  }));
  const facts = inputs.map(p=>({slug:`${capability}.input.${p.name.replaceAll(/[^a-zA-Z0-9_]/g,'_').toLowerCase()}`,
    name:p.name,description:`${name}: ${p.name}`,subject_kind:'dataset',jurisdiction:'NL',value_schema:p.schema,
    freshness_seconds:null,match_hints:{}}));
  b.fact_types.push(...facts,{slug:output,name,description:notes||name,subject_kind:'dataset',jurisdiction:'NL',
    value_schema:outputSchema,freshness_seconds:86400,match_hints:{}});
  b.capabilities.push({slug:capability,name,description:notes||name,category,input_contract:inputSchema,output_contract:outputSchema});
  const mapping=Object.fromEntries(inputs.map((p,i)=>[facts[i].slug,{in:p.location,name:p.name}]));
  b.endpoints.push({key,method,path,price:0,description:name,query_params:query,request_body:body,response_body:response,
    request_example:example,response_example:null,headers:{Accept:{type:'string',description:body?'application/json':response.type==='string'?'text/html or application/xml':'application/json'}},
    documentation_markdown:`${name}\n\n${notes}\n\nRegistered on ${date}. Inactive; no successful Apiosk execution is claimed. Price awaits review.`,
    external_docs_url:docs,authentication_notes:b.source.authentication_info.type==='none'?'Public read access.':'Nibud subscription key required; inject privately on the server. Never request the key from buyers.',
    origin_override:origin,cache_ttl_seconds:body?0:86400,tags,search_terms:[...tags,name],use_cases:[name],
    sectors:b.source.listing_metadata.sectors,input_entities:inputs.map(p=>p.name),output_entities:[output],
    endpoint_category:category,action_type:method==='POST'?'compute':'lookup',task_summary:name,
    metadata_source:'provider_documentation',metadata_model:'codex',metadata_confidence:0.9,metadata_at:checked,
    contract_documented:false});
  b.contracts.push({endpoint_key:key,capability_slug:capability,version:1,
    requires:inputs.map((p,i)=>({fact_type:facts[i].slug,subject_role:'subject',optional:!p.required})),
    produces:[{fact_type:output,subject_role:'subject',cardinality:'one'}],input_map:mapping,fixed_params:fixed,
    output_map:{base:'$',produces:{[output]:{path:'$'}}},cardinality:'one',max_fan_out:1,preconditions:{},
    pagination:{},effect:'read',price_model:'per_call',freshness_seconds:body?300:86400,adapter_version:'pending-nl-benefits-v1',
    notes:`DRAFT: ${notes} Requires a verified executor response shape and live extraction before activation.`});
  b.offers.push({endpoint_key:key,capability_slug:capability,source:'declared',confidence:0.9,
    input_map:Object.fromEntries(inputs.map(p=>[p.name,{in:p.location,name:p.name}])),fixed_params:fixed,
    notes:'Documentation-backed discovery link; endpoint and execution contract remain inactive/draft.'});
}

const sc = source('samenwerkende-catalogi','Samenwerkende Catalogi — overheidsproducten en diensten',
  'https://repository.overheid.nl','Landelijke index van overheidsproducten en diensten, met gemeentelijke aanvraaginformatie en bronlinks. Geen volledige gemeentelijke website-index en geen berekening van persoonlijk recht.',
  'government',sruDocs,['gemeenten','Samenwerkende Catalogi','bijzondere bijstand','inkomenstoeslag','aanvragen','lokale regelingen'],
  ['SRU XML normalization and bounded collection binding','Apiosk execution acceptance','Listing price review']);
sc.source.static_headers=[{name:'Accept',value:'application/xml'}];
sc.source.listing_metadata.upstream={collection:'samenwerkendecatalogi',protocol:'SRU 2.0',fair_use:true,maximum_records:1000};
sc.source.listing_metadata.municipal_websites={state:'requires_connector',discovery:'Official links from Samenwerkende Catalogi',
  blockers:['Verify official domain per municipality','HTML/PDF extraction','Per-domain robots and reuse policy','Change detection'],
  subjects:['bijzondere bijstand','individuele inkomenstoeslag','energiefondsen','kwijtschelding','kindregelingen','sport en cultuur','aanvraagvoorwaarden']};
sc.provenance.live_test={status:'not_run',tested_at:null,notes:'Apiosk execution not run. Upstream-only probe: HTTP 200, valid SRU XML, 736 results and 2 returned records for bijstand. Evidence: evidence/samenwerkende-catalogi.xml and probes.json'};
sc.source.listing_metadata.upstream_probe=probes.probes.find(p=>p.name==='samenwerkende-catalogi');
endpoint(sc,{key:'search',path:'/sru',name:'Zoek gemeentelijke en landelijke producten en diensten',capability:'nl.municipal.services.search',tags:sc.source.listing_metadata.tags,
  query:obj({query:{type:'string',minLength:1,description:'CQL scoped to c.product-area==samenwerkendecatalogi; collection enforcement must be implemented before activation.'},
    startRecord:{type:'integer',minimum:1},maximumRecords:{type:'integer',minimum:1,maximum:25}},['query']),
  fixed:{version:'2.0',operation:'searchRetrieve'},example:{query:'c.product-area==samenwerkendecatalogi AND dt.title=bijstand',maximumRecords:2},
  response:{type:'string',description:'Raw SRU XML. Executor parsing and normalized response contract are pending.'},
  notes:'Search product descriptions and links. One bounded SRU page. No entitlement or complete municipal coverage claim; enforce collection before activation.'});

const toeslagen = source('dienst-toeslagen','Dienst Toeslagen — voorwaarden en tabellen','https://www.belastingdienst.nl',
  'Officiële publieke informatie over zorgtoeslag, huurtoeslag, kinderopvangtoeslag en kindgebonden budget. HTML-bronnen; geen bevestigde openbare berekenings-API en geen persoonlijke aanspraakberekening.',
  'government','https://www.belastingdienst.nl/wps/wcm/connect/nl/toeslagen/toeslagen',
  ['toeslagen','zorgtoeslag','huurtoeslag','kinderopvangtoeslag','kindgebonden budget','voorwaarden'],
  ['HTML extraction and source provenance','Year-specific content/version checks','Apiosk execution acceptance','Listing price review']);
toeslagen.source.static_headers=[{name:'Accept',value:'text/html'}];
toeslagen.source.listing_metadata.eligibility={state:'not_implemented',public_calculation_api_confirmed:false,
  requirements:['Year-versioned parameters','Validated calculation rules','Comparison with official trial calculations']};
for(const [key,label,tail] of [['zorgtoeslag','Zorgtoeslag','zorgtoeslag/zorgtoeslag'],['huurtoeslag','Huurtoeslag','huurtoeslag/huurtoeslag'],
  ['kinderopvangtoeslag','Kinderopvangtoeslag','kinderopvangtoeslag/kinderopvangtoeslag'],['kindgebonden-budget','Kindgebonden budget','kindgebonden-budget/kindgebonden-budget'],
  ['zorgtoeslag-tabellen','Zorgtoeslagtabellen','zorgtoeslag/content/hoeveel-zorgtoeslag']]) {
  const path=`/wps/wcm/connect/nl/${tail}`;
  endpoint(toeslagen,{key,path,name:`Lees officiële ${label.toLowerCase()} informatie`,capability:`nl.benefits.${key}.information`,
    tags:['Dienst Toeslagen',label,'voorwaarden','bedragen','inkomensgrenzen','Nederland'],docs:`https://www.belastingdienst.nl${path}`,
    response:{type:'string',description:'Official HTML page; not a JSON benefits calculator.'},
    notes:'Public HTML information only. Preserve source URL, retrieval date and applicable year. No entitlement calculation, login, application submission or personal data.'});
}

const nibud = source('nibud','Nibud — huishoudelijke uitgaven en afloscapaciteit','https://api.nibud.nl',
  'Gelicentieerde referentie-uitgaven voor huishoudens, pensioenuitgaven en afloscapaciteit. Vereist een Nibud-abonnement en API-key; prijs en doorleveringsrechten zijn nog niet bevestigd. Geen toeslagrechten-database.',
  'finance','https://developer.nibud.nl/products2',['Nibud','budget','huishouden','referentiebedragen','pensioen','afloscapaciteit'],
  ['Subscription/API key','Commercial price and redistribution licence','Private credential binding','Resolve documented schema/example inconsistencies','Live response schemas and Apiosk execution acceptance']);
nibud.source.authentication_info={type:'api_key',in:'header',name:'Ocp-Apim-Subscription-Key',alternative:{in:'query',name:'key'},upstream:{required:true}};
nibud.source.static_headers=[{name:'Accept',value:'application/json'},{name:'Content-Type',value:'application/json'}];
nibud.source.listing_metadata.sectors=['Personal finance','Financial services'];
nibud.source.listing_metadata.license={name:'Nibud commercial subscription',status:'required',redistribution_rights:'unconfirmed',price_per_call_usd:null,url:'https://developer.nibud.nl/getting-started'};
nibud.credential_requirements=[{vault_name:'APIOSK_NIBUD_SUBSCRIPTION_KEY',endpoint_key:null,in:'header',name:'Ocp-Apim-Subscription-Key',prefix:'',allowed_origin:'https://api.nibud.nl',runtime_binding:'missing',notes:'Product scope must be checked. No secret supplied or stored in catalog metadata.'}];
const openapi=JSON.parse(await fs.readFile(new URL('evidence/nibud-uitgaven-v6.openapi.json',here),'utf8'));
nibud.provenance.documentation_urls.push('https://developer.nibud.nl/technical-uitgaven','https://developer.nibud.nl/technical-pensioen-uitgaven','https://developer.nibud.nl/technical-afloscapaciteit');
nibud.provenance.openapi_sha256=createHash('sha256').update(JSON.stringify(openapi)).digest('hex');
const unverifiedResponse={type:['object','array'],description:'Exact JSON response envelope is not published in OpenAPI and has not been verified with an authenticated call.'};
for(const [path,methods] of Object.entries(openapi.paths)) {
  const [method,op]=Object.entries(methods)[0];
  const body=method==='post'?openapi.components.schemas.gezin2:null;
  const key=path.slice(1).toLowerCase();
  endpoint(nibud,{key:`uitgaven-${key}`,method:method.toUpperCase(),path:`/Uitgaven/v6${path}`,name:`Nibud Uitgaven: ${op.summary}`,
    capability:method==='post'?'nl.nibud.expenditures':`nl.nibud.expenditures.${key}`,tags:['Nibud','Uitgaven',op.summary,'huishoudbudget'],
    body,response:unverifiedResponse,docs:`https://developer.nibud.nl/reference#api=Uitgaven-v6&operation=${op.operationId}`,
    notes:'Official v6 OpenAPI input schema preserved. It disagrees with examples on casing and some numeric types, and requires woning.bouwenergielabel without defining it. Resolve with Nibud before activation. Output references Basis/Voorbeeld amounts; exact envelope unverified.'});
}
const pension=obj({Hoofdpersonen:{type:'array',items:obj({Geslacht:int})},Kinderen:{type:'array',items:obj({Leeftijd:int,Geslacht:int},['Leeftijd'])},
  Autos:{type:'array',items:obj({KilometersPerJaar:int,TypeAuto:int,StatusAuto:int},['KilometersPerJaar','TypeAuto','StatusAuto'])},
  Woning:obj({TypeWoning:int,SoortWoning:int},['TypeWoning','SoortWoning']),NettoBesteedbaarInkomenPerMaand:int},
  ['Hoofdpersonen','Kinderen','Autos','Woning','NettoBesteedbaarInkomenPerMaand']);
endpoint(nibud,{key:'pensioen-resultaat',method:'POST',path:'/pensioenuitgaven/Resultaat',name:'Nibud pensioenuitgaven berekenen',capability:'nl.nibud.pension-expenditures',
  tags:['Nibud','pensioen','uitgaven','referentiebedragen'],body:pension,response:unverifiedResponse,
  docs:'https://developer.nibud.nl/reference#api=pensioenuitgaven&operation=berekenresultaat',
  notes:'Input fields transcribed from the official operation reference. Enum string examples differ from integer schemas; verify before activation. Technical docs describe Bedrag/Uitgavepost outputs; exact envelope unverified.'});
const repayment=obj({LeeftijdHoofdpersoon:int,HeeftPartner:{type:'boolean'},AantalThuiswonendeKinderen:int,AantalAutos:int,NettoBesteedbaarInkomenPerMaand:int,
  BijzondereUitgaven:obj(Object.fromEntries(['KostenZorg','KostenPartnerAlimentatie','KostenKinderAlimentatie','KostenKinderOpvang','KostenOverigeAflossingen'].map(k=>[k,int]))),
  Woning:obj(Object.fromEntries(['WoningSoort','BrutoHypotheeklasten','HuurPerMaand','HuurServiceKosten','BijdrageVve'].map(k=>[k,int])),['WoningSoort'])},
  ['LeeftijdHoofdpersoon','HeeftPartner','AantalThuiswonendeKinderen','AantalAutos','NettoBesteedbaarInkomenPerMaand','BijzondereUitgaven','Woning']);
endpoint(nibud,{key:'afloscapaciteit-resultaat',method:'POST',path:'/Afloscapaciteit/Resultaat',name:'Nibud afloscapaciteit berekenen',capability:'nl.nibud.repayment-capacity',
  tags:['Nibud','afloscapaciteit','schulden','betalingsregeling','huishoudbudget'],body:repayment,response:unverifiedResponse,
  docs:'https://developer.nibud.nl/reference#api=afloscapaciteit&operation=post-berekenresultaat',
  notes:'Official raw request schema transcribed. Example uses string woning type/null costs where schema says integer: resolve before activation. Describes repayment and maximum repayment capacity, not benefit eligibility.'});

// Enrich the two existing KOOP endpoints without touching their price, state or executable contracts.
const enrichment = [
  {path:'/sru',tags:['officiële bekendmakingen','gemeenteblad','Staatscourant','wijzigingen','besluiten','bijstand'],
    inputs:['trefwoord','publicatie-ID','publicerende overheid','publicatiedatum'],outputs:['publicatiemetadata','bronlinks','publicatiedatum','vervolgpagina'],
    uses:['Zoek officiële publicaties en besluiten','Zoek aankondigingen over bijstand en lokale regelingen'],summary:'Zoek officiële bekendmakingen op titel, overheid en publicatiedatum; één pagina.'},
  {path:'/sru/Search',tags:['BWB','CVDR','wetten.nl','lokale regelgeving','bijzondere bijstand','individuele inkomenstoeslag','Participatiewet','toeslagen'],
    inputs:['trefwoord','BWB of CVDR collectie','startrecord'],outputs:['wetgevingsmetadata','versiegegevens','bronlinks','vervolgpagina'],
    uses:['Zoek landelijke wetgeving in BWB','Zoek gemeentelijke regelingen en beleidsregels in CVDR'],summary:'Zoek BWB- en CVDR-regelingstitels; controleer versie en geldigheid in het brondocument.'},
];
const prelude=`BEGIN;\nSELECT pg_advisory_xact_lock(hashtext('apiosk-nl-benefits-20261013'));\nDO $guard$ BEGIN\nIF NOT EXISTS (SELECT 1 FROM providers.apis WHERE slug='koop' AND is_platform_owned AND owner_id='${owner}'::uuid) THEN RAISE EXCEPTION 'Expected platform-owned KOOP source missing'; END IF;\nEND $guard$;\n`;
let sql=prelude;
for(const b of bundles) {
  await fs.writeFile(new URL(`${b.source.slug}.json`,here),JSON.stringify(b,null,2)+'\n');
  const rendered=await render(b);
  sql+=rendered.replace(/^BEGIN;\n/m,'').replace(/^COMMIT;\s*$/m,'')+'\n';
}
for(const e of enrichment) {
  sql+=`UPDATE providers.api_endpoints e SET tags=(SELECT array_agg(DISTINCT v ORDER BY v) FROM unnest(coalesce(e.tags,'{}')||ARRAY(SELECT jsonb_array_elements_text(${sqlJson(e.tags)}))) v), search_terms=(SELECT array_agg(DISTINCT v ORDER BY v) FROM unnest(coalesce(e.search_terms,'{}')||ARRAY(SELECT jsonb_array_elements_text(${sqlJson(e.tags)}))) v), sectors=(SELECT array_agg(DISTINCT v ORDER BY v) FROM unnest(coalesce(e.sectors,'{}')||ARRAY['Government and public data','Legal']) v), input_entities=(SELECT array_agg(DISTINCT v ORDER BY v) FROM unnest(coalesce(e.input_entities,'{}')||ARRAY(SELECT jsonb_array_elements_text(${sqlJson(e.inputs)}))) v), output_entities=(SELECT array_agg(DISTINCT v ORDER BY v) FROM unnest(coalesce(e.output_entities,'{}')||ARRAY(SELECT jsonb_array_elements_text(${sqlJson(e.outputs)}))) v), use_cases=(SELECT array_agg(DISTINCT v ORDER BY v) FROM unnest(coalesce(e.use_cases,'{}')||ARRAY(SELECT jsonb_array_elements_text(${sqlJson(e.uses)}))) v), endpoint_category='government', action_type='search', task_summary=${sqlJson(e.summary)}#>>'{}', metadata_source='provider_documentation_and_live_probe', metadata_at=now() FROM providers.apis a WHERE a.id=e.api_id AND a.slug='koop' AND e.method='GET' AND e.path='${e.path}';\n`;
}
sql+=`UPDATE providers.apis SET listing_metadata=listing_metadata || jsonb_build_object('nl_benefits_research',${sqlJson({checked_at:date,documentation_url:sruDocs,collections:['officielepublicaties','BWB','cvdr'],capability_aliases:{'nl.legal.search':'government.bwb.search','nl.municipal.regulations':'government.cvdr.search'},changes_feed:{url:'https://repository.officiele-overheidspublicaties.nl/officielepublicaties/_events/',state:'documented_not_integrated'},scope:'Existing title-search contracts retained. No municipal filter or full-text retrieval added.'})}),docs_url='${sruDocs}' WHERE slug='koop';\n`;
sql+='COMMIT;\n';
const target=new URL('../../../supabase/migrations/20261013172106_onboard_nl_benefits_sources.sql',here);
await fs.writeFile(target,sql);
const inventory=bundles.map(b=>({slug:b.source.slug,endpoints:b.endpoints.length,capabilities:b.capabilities.length,contracts:b.contracts.length,status:'inactive',blockers:b.source.listing_metadata.readiness.blockers}));
await fs.writeFile(new URL('inventory.json',here),JSON.stringify({checked_at:date,new_sources:inventory,existing_koop_endpoints_enriched:2,existing_koop_capabilities:5,
  unimplemented:['Municipal website crawler beyond official links','KOOP event-feed synchronization','Personal benefits eligibility engine']},null,2)+'\n');
console.log(JSON.stringify(inventory));
