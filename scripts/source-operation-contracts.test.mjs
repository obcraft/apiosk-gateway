import test from 'node:test';
import assert from 'node:assert/strict';
import {buildOperations,scalarSchema} from './source-operation-contracts.mjs';
function fixture(source='example') {
 return {apis:[{id:'api',slug:source,name:source,docs_url:'https://example.com/docs'}],
 endpoints:[{id:'11111111-1111-4111-8111-111111111111',api_id:'api',method:'GET',path:'/lookup',price:0.02,
 contract_documented:true,description:'Retrieve one public record.',action_type:'lookup',query_params:{type:'object',required:['name'],properties:{name:{type:'string',minLength:1},country:{type:'string',enum:['NL','GB']}}}}],
 offers:[{endpoint_id:'11111111-1111-4111-8111-111111111111',source:'declared'}],contracts:[]};
}
test('admission does not privilege a source or provider name',()=>{
 for(const name of ['kvk','filingspulse','stableenrich','browserbase','future-provider']) {
  const b=buildOperations(fixture(name));assert.equal(b.services.length,1,name);
  const s=b.services[0];assert.equal(s.contract.output_map.base,'$');assert.equal(s.contract.identifies,null);
  assert.equal(s.unverified,true,'a staged document contract waits for its first live response');
  assert.deepEqual(b.vocabulary.find(w=>w.slug.endsWith('.country')).value_schema,{type:'string',enum:['NL','GB']});
 }
});
test('requires documented inputs and refuses unknown constraints or action effects',()=>{
 for(const mutate of [
  s=>s.apis[0].planner_block_reason='No permission',
  s=>s.endpoints[0].query_params.properties.name={type:'object'},
  s=>s.endpoints[0].query_params.anyOf=[{required:['name']}],
  s=>s.endpoints[0].query_params.required=['absent'],
  s=>s.endpoints[0].method='DELETE',
  s=>s.endpoints[0].action_type='action',
  s=>s.endpoints[0].path='/send',
  s=>s.endpoints[0].path='/companies/{id}',
  s=>s.offers[0].source='inferred',
  s=>s.endpoints[0].query_params.required=[],
 ]) {const s=fixture();mutate(s);const b=buildOperations(s);assert.equal(b.services.length,0);assert.ok(b.report[0].blockers.length);}
});
test('existing authored contracts and published source blocks survive the onboarding pass',()=>{
 const s=fixture();s.contracts.push({endpoint_id:s.endpoints[0].id,status:'active',validated_at:'now',effect:'read',price_model:'per_call'});
 assert.equal(buildOperations(s).services.length,0);assert.equal(s.contracts.length,1);
});
test('schema constraints are preserved or explicitly refused',()=>{
 assert.equal(scalarSchema({type:'string',format:'email'}),null);
 assert.equal(scalarSchema({type:'string',maxItems:2}),null);
 assert.equal(scalarSchema({type:'string',nullable:true}),null);
 assert.deepEqual(scalarSchema({type:'integer',minimum:1,maximum:8,example:4}),{type:'integer',minimum:1,maximum:8});
});
