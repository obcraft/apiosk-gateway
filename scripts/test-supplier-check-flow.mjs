import {spawn} from 'node:child_process';
import {once} from 'node:events';
const cwd=new URL('../',import.meta.url),port=process.env.PGTEST_PORT||'54336';
const db=spawn(process.execPath,['tests/support-db.mjs'],{cwd,env:{...process.env,PGTEST_PORT:port},stdio:['ignore','pipe','inherit']});
try {
 await new Promise((resolve,reject)=>{const timer=setTimeout(()=>reject(Error('Local DB timeout')),20000);db.stdout.on('data',s=>{if(s.toString().includes('Test PostgreSQL')){clearTimeout(timer);resolve();}});db.once('exit',code=>{clearTimeout(timer);reject(Error('DB exited '+code));});});
 const test=spawn('cargo',['test','--offline','--test','supplier_check_flow','--','--ignored','--nocapture'],{cwd,env:{...process.env,TEST_DATABASE_URL:`postgresql://postgres@127.0.0.1:${port}/postgres?sslmode=disable`},stdio:'inherit'});
 const [code]=await once(test,'exit');process.exitCode=code??1;
}finally{db.kill('SIGTERM');}
