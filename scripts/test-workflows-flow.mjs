import { spawn } from 'node:child_process';
import { once } from 'node:events';
const cwd=new URL('../',import.meta.url);
const port=process.env.PGTEST_PORT || '54341';
// Each suite owns its schema and catalog; never reuse its database in another suite.
// The focused mode covers all normal named workflows and Pre-KYB identity
// variants without requiring the separate legacy invoice/edge scenarios.
const suites=process.env.PRE_KYB_FLOW_ONLY ? ['workflows_flow'] : ['supplier_check_flow','workflows_flow'];
for (const suite of suites) {
 const db=spawn(process.execPath,['tests/support-db.mjs'],{cwd,env:{...process.env,PGTEST_PORT:port},stdio:['ignore','pipe','inherit']});
 const closed=once(db,'exit');
 const ready=new Promise((resolve,reject)=>{
  const timer=setTimeout(()=>reject(new Error('test database startup timeout')),20_000);
  db.stdout.on('data',chunk=>{process.stdout.write(chunk);if(chunk.toString().includes('Test PostgreSQL')){clearTimeout(timer);resolve();}});
  db.once('exit',code=>{clearTimeout(timer);reject(new Error(`test database exited: ${code}`));});
 });
 try {
  await ready;
  const cargo=spawn('cargo',['test','--offline','--test',suite,'--','--ignored','--nocapture'],{cwd,env:{...process.env,RUST_MIN_STACK:process.env.RUST_MIN_STACK || '16777216',TEST_DATABASE_URL:`postgresql://postgres@127.0.0.1:${port}/postgres?sslmode=disable`},stdio:'inherit'});
  const [code]=await once(cargo,'exit');
  if(code !== 0) {process.exitCode=code ?? 1;break;}
 } finally {db.kill('SIGTERM');await closed;}
}
