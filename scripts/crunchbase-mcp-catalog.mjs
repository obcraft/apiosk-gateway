import fs from 'node:fs';
const publicSchema=JSON.parse(fs.readFileSync(new URL('../legacy-executor/src/proxy/crunchbase/public-schema.json',import.meta.url),'utf8'));
// Documented read-only MCP tools, staged until authenticated discovery and
// entitlement/pricing validation. One source: never create a Jina listing.
export const source='apimarket-magicapi-crunchbase-bbcca0a6';
export const origin='https://mcp.crunchbase.com';
export const docs='https://data.crunchbase.com/docs/tool-reference';
// Buyer tariff approved 2026-10-11: EUR 0.05 per tools/call including the 15%
// fee, calibrated to billing.fx_rates EUR/USD=0.92 (2026-08-21). Settlement
// remains USDC; future display-rate changes also change the EUR equivalent.
export const pricing={listAtomic:47_258,buyerAtomic:54_347,buyerMarkupBps:1500,
 displayCurrency:'EUR',displayAmount:0.05,displayRate:0.92,rateAsOf:'2026-08-21',
 approvedOn:'2026-10-11',unit:'tools/call',internalEnrichmentIncluded:true};
const str=(description,maxLength=200)=>({type:'string',minLength:1,maxLength,description});
const array=(description)=>({type:'array',items:str('Field or card identifier'),maxItems:100,description});
const count=(max,def)=>({type:'integer',minimum:1,maximum:max,default:def});
const cursor={type:'string',format:'uuid',description:'UUID of the last returned entity. Use only for an explicitly requested next page.'};
const predicate={type:'object',required:['field_id','operator_id','values'],properties:{field_id:str('Exact field from cb_reference'),operator_id:str('Operator supported by the field from cb_reference'),values:{type:'array',maxItems:100,items:{type:['string','number','boolean']}}},additionalProperties:false};
const searchDescription='Use cb_reference for valid fields, operators and collection IDs. Resolve location/category names to returned identifiers before filtering. Money is integer USD and dates are ISO dates. Preserve all user filters; never silently drop a filter. Simple public-field predicates only; private CRM/saved-list and recursive relationship filters are unsupported. One page of at most 15 results.';
export const tools=[
 {tool:'cb_entity_autocomplete',name:'Official company and people name search',action:'search',category:'company',tags:['crunchbase','official-data','company','people','founders','executives','identifier','location','industry','name-search'],description:'Search official Crunchbase names and resolve company, people, location or category identifiers. Returns profile URLs and descriptions; resolve ambiguous matches before fetching a profile.',schema:{type:'object',required:['query'],properties:{query:str('Literal name, not the entire task'),collection_ids:{type:'string',maxLength:300,pattern:'^(organizations|people|funding_rounds|locations|categories|category_groups)(,(organizations|people|funding_rounds|locations|categories|category_groups))*$',description:'Comma-separated public collections only: organizations, people, funding_rounds, locations, categories, category_groups.'},limit:count(25,10)},additionalProperties:false},example:{query:'Starbucks',collection_ids:'organizations',limit:5}},
 {tool:'cb_search_query',name:'Official company, people and funding filters',action:'search',category:'business-intelligence',tags:['crunchbase','official-data','company','people','funding','funding-rounds','investors','venture-capital','industry','location','filter','search','financials'],description:'Filter official Crunchbase companies, people and funding rounds by industry, geography, date, funding amount or stage. '+searchDescription,schema:{type:'object',required:['collection_id'],properties:{collection_id:{type:'string',enum:['organizations','organization.companies','people','funding_rounds'],description:'Reviewed public entity collection only.'},predicates:{type:'array',maxItems:25,items:predicate},order:{type:'array',maxItems:5,items:{type:'object',required:['field_id','sort'],properties:{field_id:str('Sortable field'),sort:{enum:['asc','desc']},nulls:{enum:['first','last']}},additionalProperties:false}},limit:count(15,15),after_id:cursor},additionalProperties:false},example:{collection_id:'funding_rounds',predicates:[{field_id:'announced_on',operator_id:'gte',values:['2026-06-01']}],field_ids:['identifier','announced_on','investment_type','money_raised','funded_organization_identifier'],limit:15}},
 {tool:'cb_entity_get',name:'Official company, people and funding profiles',action:'lookup',category:'company',tags:['crunchbase','official-data','company','people','funding','funding-rounds','investors','founders','leadership','board-members','employees','financials','profile','fetch','enrichment'],description:'Fetch an identified official Crunchbase company, person or funding-round profile. Retain all returned public fields and supported relationship cards: description, headquarters, website, headcount, founders, employees, board members, funding rounds, amounts and investors when the account provides them. Missing fields are unavailable, never inferred. Public Crunchbase page content may supplement the same result internally. No extra source or page charge. No automatic pagination.',schema:{type:'object',required:['entity_id'],properties:{entity_id:{...str('Real UUID or permalink returned by Crunchbase or explicitly supplied in a profile URL. Never fabricate.'),pattern:'^[A-Za-z0-9_-]+$'},entity_def_id:{type:'string',enum:['organization','person','funding_round'],default:'organization'},field_ids:array('Omit to fetch all readable fields. [] skips top-level fields.'),card_ids:array('Omit for public founders, employees, board/advisors, funding and investor cards. Explicit cards must come from the reviewed public reference.'),related_entity_limit:count(100,100),after_id:cursor},additionalProperties:false},example:{entity_id:'starbucks',entity_def_id:'organization',related_entity_limit:100}},
 {tool:'cb_reference',name:'Official Crunchbase field and filter reference',action:'lookup',category:'reference',tags:['crunchbase','official-data','schema','fields','filters','operators','company','people','funding'],description:'Read the official Crunchbase schema before building filters: collections, company/person/funding fields, valid operators, enum values and relationship cards. Use returned paths verbatim. This returns schema documentation, not company records.',schema:{type:'object',properties:{path:{type:'string',maxLength:300,default:'index',description:'index, entities, or an exact reference path returned by a previous response.'}},additionalProperties:false},example:{path:'entities'}},
];
for(const tool of tools){
 if(['cb_entity_get','cb_search_query'].includes(tool.tool)){
  tool.schema.properties.field_ids??=array('Requested public output fields; defaults to all reviewed public fields.');
  tool.schema.properties.field_ids.items.enum=[...new Set(Object.values(publicSchema).flatMap(s=>s.fields))];
  if(tool.schema.properties.card_ids)tool.schema.properties.card_ids.items.enum=[...new Set(Object.values(publicSchema).flatMap(s=>s.cards))];
 }
}
export const capabilities=[
 ['company.crunchbase-official-search','Find official company identities','cb_entity_autocomplete',{collection_ids:'organizations'},['query'],'company'],
 ['person.crunchbase-search','Find official people, founders and executives','cb_entity_autocomplete',{collection_ids:'people'},['query'],'person'],
 ['research.crunchbase-identifiers','Resolve Crunchbase location or industry identifiers','cb_entity_autocomplete',{},['query','collection_ids'],'research'],
 ['company.crunchbase-filter','Filter companies by industry, geography and funding','cb_search_query',{collection_id:'organizations'},['predicates'],'company'],
 ['person.crunchbase-filter','Filter people and professional profiles','cb_search_query',{collection_id:'people'},['predicates'],'person'],
 ['funding.crunchbase-search','Find funding rounds, amounts, stages and investors','cb_search_query',{collection_id:'funding_rounds'},['predicates'],'funding'],
 ['company.crunchbase-profile','Fetch company profile, founders, employees, board members, funding rounds and investors','cb_entity_get',{entity_def_id:'organization'},['entity_id'],'company'],
 ['person.crunchbase-profile','Fetch official professional profile and company relationships','cb_entity_get',{entity_def_id:'person'},['entity_id'],'person'],
 ['funding.crunchbase-profile','Fetch official funding round, amount and investor relationships','cb_entity_get',{entity_def_id:'funding_round'},['entity_id'],'funding'],
 ['research.crunchbase-reference','Read Crunchbase fields, collections and filter operators','cb_reference',{},[],'research'],
].map(([slug,name,tool,fixed,required,category])=>({slug,name,tool,fixed,required,category}));
export function capabilityDescription(capability) {
 const detail={
  'company.crunchbase-profile':'Retrieve one identified organization, its description, website, headquarters, headcount, founders, employee profiles, board members and advisors, funding rounds, raised amounts and investors. Returns the available public relationship cards in one company-profile call; no separate people or funding search is needed for those cards.',
  'person.crunchbase-profile':'Retrieve one identified professional person, biography, current jobs, board roles, founded organizations and investments. Requires a person profile identifier, not a company identifier.',
  'funding.crunchbase-profile':'Retrieve one identified funding round, announcement date, investment stage, money raised, funded company and lead investors. Requires a funding-round identifier, not a company identifier.',
 }[capability.slug] ?? tools.find(t=>t.tool===capability.tool).description;
 return capability.name+'. '+detail;
}
export const outputSchema={type:'object',required:['source','transport','tool','data','retrieved_at'],properties:{source:{type:'string',enum:['Crunchbase']},transport:{type:'string',enum:['official_mcp']},tool:{enum:tools.map(t=>t.tool)},data:{type:['object','array']},retrieved_at:{type:'string'}}};

// Actual MCP records contain labelled field/card arrays, not a flat REST
// profile. These reviewed aliases describe only the public default selection.
// Fields/cards remain optional: entitlement, empty data and pagination vary.
const aliases=names=>({$comment:JSON.stringify({apiosk_output_aliases_v1:names})});
const fieldRows={type:'array',items:{type:'object',properties:{field_id:{type:'string'},label:{type:['string','null']},value:{}}}};
const profileScope={
 organization:{fields:['name','description','website','headquarters','headcount','employee_count','funding_total'],cards:['founders','employees','board_members','advisors','funding_rounds','investors']},
 person:{fields:['name','description','location'],cards:['current_jobs','board_roles','founded_organizations','investments']},
 funding_round:{fields:['name','description','announced_on','investment_type','money_raised','funded_company'],cards:['funded_organization','investors','lead_investors']},
};
export function capabilityOutputSchema(capability){
 if(capability.tool!=='cb_entity_get')return outputSchema;
 const kind=capability.fixed.entity_def_id,scope=profileScope[kind];
 return {...outputSchema,properties:{...outputSchema.properties,data:{type:'object',required:['properties','cards'],
  ...aliases(['profile',`${capability.category}_profile`]),properties:{
   request_id:{type:'string'},entity_def_id:{type:['string','null'],enum:[kind,null]},
   properties:{...fieldRows,...aliases(scope.fields)},
   cards:{type:'array',...aliases(scope.cards),items:{type:'object',properties:{
    card_id:{type:'string'},label:{type:['string','null']},entity_def_id:{type:['string','null']},
    entities:{type:['array','null'],items:{type:'object',properties:{properties:fieldRows}}},
    has_more_entities:{type:'boolean'},
   }}},
  }}}};
}
