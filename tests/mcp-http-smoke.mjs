import assert from 'node:assert/strict';
import { createApioskMcpRuntime } from '../../mcp/src/runtime.mjs';
const base=process.env.FIXTURE_GATEWAY_URL;
assert.match(base,/^http:\/\/127\.0\.0\.1:\d+$/);
const runtime=createApioskMcpRuntime({env:{APIOSK_GATEWAY_V2_URL:base,APIOSK_CONNECT_TOKEN:'apk_access_local_fixture'}});
assert.deepEqual((await runtime.listTools()).map(t=>t.name).sort(),
  ['apiosk_approve','apiosk_discover','apiosk_execute','apiosk_sources','apiosk_status']);
async function tool(name,args){const r=await runtime.callTool(name,args);assert.ok(!r.isError,JSON.stringify(r));return r.structuredContent;}
const sources=await tool('apiosk_sources',{category:'finance',tag:'registry',limit:1});
assert.equal(sources.sources.length,1);
assert.ok(sources.sources[0].capabilities.length > 0);
assert.equal(Object.hasOwn(sources.sources[0], 'available_in_v2'), false);
assert.deepEqual(sources.sources[0].tags,['registry']);
const capability = sources.sources[0].capabilities[0];
assert.ok((await tool('apiosk_sources',{capability})).sources.every(s=>s.capabilities.includes(capability)));
assert.ok((await tool('apiosk_sources',{search:capability})).total>0);
assert.equal((await fetch(`${base}/v2/sources`)).status,401);
assert.equal((await tool('apiosk_sources',{search:'missing source'})).total,0);
let r=await tool('apiosk_discover',{question:'Get accounts for Example'});
assert.equal(r.status,'ready');assert.equal(r.proposal.max_total_atomic,'138');
const task=r.state.state_ref;
const forged=await fetch(`${base}/v2/execute`,{method:'POST',headers:{authorization:'Bearer apk_access_local_fixture','content-type':'application/json'},body:JSON.stringify({state:r.state,approved:true})});
assert.equal(forged.status,422);
function action(v){return {action_id:v.next_actions.find(a=>a.kind==='execute_quoted_step').action_id,state:v.state,quote_ref:v.proposal.quote_ref};}
r=await tool('apiosk_execute',action(r));assert.equal(r.status,'requires_approval');
const authBody={quote_ref:r.proposal.quote_ref,max_total_atomic:r.proposal.max_total_atomic};
let response=await fetch(`${base}/v2/tasks/${task}/authorize`,{method:'POST',headers:{authorization:'Bearer apk_access_local_fixture','content-type':'application/json'},body:JSON.stringify(authBody)});
assert.equal(response.status,403);
assert.equal(r.context_view.approval_mode,'chatbot');
assert.equal((await fetch(`${base}/v2/approve`,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({...authBody,request_id:crypto.randomUUID(),state:r.state})})).status,401);
const wrong=await runtime.callTool('apiosk_approve',{state:r.state,...authBody,max_total_atomic:'139'});assert.equal(wrong.isError,true);
const tampered={...r.state,revision:r.state.revision+1};
assert.equal((await runtime.callTool('apiosk_approve',{state:tampered,...authBody})).isError,true);
r=await tool('apiosk_approve',{state:r.state,...authBody});
assert.equal(r.billing.authorization_active,true);
const repeated=await tool('apiosk_approve',{state:r.state,...authBody});
assert.equal(repeated.billing.authorization_active,true);
// Approval starts the whole request. The client only reads SSE updates.
const stream = await fetch(new URL(r.context_view.events_path, base), {headers:{origin:'https://embedded-host.example'}});
assert.equal(stream.status, 200);
assert.match(stream.headers.get('content-type'), /text\/event-stream/);
assert.equal(stream.headers.get('access-control-allow-origin'), '*');
const approvalCors = await fetch(`${base}/v2/approve`, {method:'OPTIONS',headers:{origin:'https://embedded-host.example','access-control-request-method':'POST','access-control-request-headers':'authorization,content-type'}});
assert.notEqual(approvalCors.headers.get('access-control-allow-origin'), '*');
assert.notEqual(approvalCors.headers.get('access-control-allow-origin'), 'https://embedded-host.example');
assert.equal((await fetch(new URL(r.context_view.events_path.replace(/signature=[^&]+/, 'signature=00'), base))).status, 403);
const reader = stream.body.getReader();
let buffer = ''; const decoder = new TextDecoder();
while (r.status !== 'succeeded') {
  const { value, done } = await reader.read(); assert.equal(done, false);
  buffer += decoder.decode(value, { stream: true });
  let end;
  while ((end = buffer.indexOf('\n\n')) >= 0) {
    const block = buffer.slice(0, end); buffer = buffer.slice(end + 2);
    // This card only ever listened for `task`/`task_delta` (R5); the new
    // `stream_open`, `heartbeat` and `stream_end` events also carry a
    // `data:` line but must stay invisible to it.
    const event = block.split('\n').find(line => line.startsWith('event:'))?.slice(6).trim();
    if (event !== 'task' && event !== 'task_delta') continue;
    const line = block.split('\n').find(line => line.startsWith('data:'));
    if (line) r = JSON.parse(line.slice(5).trim());
  }
}
await reader.cancel();
assert.equal(r.result.charged_atomic, '115');
assert.equal(r.context_view.execution_mode, 'server');
const read=r.next_actions.find(a=>a.kind==='read_result');
r=await tool('apiosk_execute',{action_id:read.action_id,state:r.state});
assert.equal(r.result.data.accounts,'2025');assert.equal(r.result.charged_atomic,'115');
const recovered=await tool('apiosk_execute',{recover_task_ref:task});
assert.equal(recovered.state.state_ref,task);assert.equal(recovered.status,'succeeded');
assert.equal((await fetch(`${base}/v2/tasks/${task}`)).status,401);
console.log('MCP -> Rust HTTP/Auth -> SQLx -> provider fixture: consent, replay, result read and recovery passed.');
