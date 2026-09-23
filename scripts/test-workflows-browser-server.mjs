// Real Gateway + temporary SQL state, recorded providers/model/auth. Loopback only.
import {spawn} from 'node:child_process';
import {once} from 'node:events';
const cwd=new URL('../',import.meta.url), port='54422';
const children=[];
const stop=()=>{for(const child of children)child.kill('SIGTERM');};
process.on('SIGINT',stop);process.on('SIGTERM',stop);
try {
 const db=spawn(process.execPath,['tests/support-db.mjs'],{cwd,env:{...process.env,PGTEST_PORT:port},stdio:['ignore','pipe','inherit']});children.push(db);
 await new Promise((resolve,reject)=>{const timer=setTimeout(()=>reject(Error('DB timeout')),20000);db.stdout.on('data',s=>{process.stdout.write(s);if(s.toString().includes('Test PostgreSQL')){clearTimeout(timer);resolve();}});db.once('exit',()=>{clearTimeout(timer);reject(Error('DB stopped'));});});
 const cargo=spawn('cargo',['test','--offline','--test','workflows_flow','--','--ignored','--nocapture'],{cwd,env:{...process.env,WORKFLOWS_BROWSER:'true',RUST_MIN_STACK:'16777216',TEST_DATABASE_URL:`postgresql://postgres@127.0.0.1:${port}/postgres?sslmode=disable`},stdio:'inherit'});children.push(cargo);
 const [code]=await once(cargo,'exit');process.exitCode=code??1;
}finally{stop();}
