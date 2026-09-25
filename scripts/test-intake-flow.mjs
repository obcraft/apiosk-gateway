// Intake through the real router on a fresh local PGlite: question length,
// structured endpoint selection, compiled lookups, request receipts and
// requotes, with a fake model that counts every interpretation. One scenario
// at a time: the PGlite socket serves one connection.
import { spawn } from 'node:child_process';
import { once } from 'node:events';
const cwd=new URL('../',import.meta.url);
const port=process.env.PGTEST_PORT || '54360';
const db=spawn(process.execPath,['tests/support-db.mjs'],{cwd,env:{...process.env,PGTEST_PORT:port},stdio:['ignore','pipe','inherit']});
const closed=once(db,'exit');
const ready=new Promise((resolve,reject)=>{
 const timer=setTimeout(()=>reject(new Error('test database startup timeout')),20_000);
 db.stdout.on('data',chunk=>{process.stdout.write(chunk);if(chunk.toString().includes('Test PostgreSQL')){clearTimeout(timer);resolve();}});
 db.once('exit',code=>{clearTimeout(timer);reject(new Error(`test database exited: ${code}`));});
});
try {
 await ready;
 const cargo=spawn('cargo',['test','--offline','--test','intake_flow','--','--ignored','--nocapture','--test-threads=1',...process.argv.slice(2)],{cwd,env:{...process.env,RUST_MIN_STACK:process.env.RUST_MIN_STACK || '16777216',TEST_DATABASE_URL:`postgresql://postgres@127.0.0.1:${port}/postgres?sslmode=disable`},stdio:'inherit'});
 const [code]=await once(cargo,'exit');process.exitCode=code ?? 1;
} finally {db.kill('SIGTERM');await closed;}
