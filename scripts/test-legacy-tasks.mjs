// Saved tasks of every earlier shape, read back through the production view
// on a fresh local PGlite (tests/legacy_task_views.rs).
import { spawn } from 'node:child_process';
import { once } from 'node:events';
const cwd=new URL('../',import.meta.url);
const port=process.env.PGTEST_PORT || '54353';
const db=spawn(process.execPath,['tests/support-db.mjs'],{cwd,env:{...process.env,PGTEST_PORT:port},stdio:['ignore','pipe','inherit']});
const ready=new Promise((resolve,reject)=>{
 const timer=setTimeout(()=>reject(new Error('test database startup timeout')),20_000);
 db.stdout.on('data',chunk=>{process.stdout.write(chunk);if(chunk.toString().includes('Test PostgreSQL')){clearTimeout(timer);resolve();}});
 db.once('exit',code=>{clearTimeout(timer);reject(new Error(`test database exited: ${code}`));});
});
try {
 await ready;
 const cargo=spawn('cargo',['test','--offline','--test','legacy_task_views','--','--ignored','--nocapture'],{cwd,env:{...process.env,RUST_MIN_STACK:process.env.RUST_MIN_STACK || '16777216',TEST_DATABASE_URL:`postgresql://postgres@127.0.0.1:${port}/postgres?sslmode=disable`},stdio:'inherit'});
 const [code]=await once(cargo,'exit');process.exitCode=code ?? 1;
} finally {db.kill('SIGTERM');}
