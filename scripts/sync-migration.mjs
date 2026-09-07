import fs from 'node:fs/promises';
const name='20260907083427_gateway_v2_state.sql';
const data=await fs.readFile(new URL(`../supabase/migrations/${name}`,import.meta.url),'utf8');
const target=new URL(`../../app/supabase/migrations/${name}`,import.meta.url);
if(process.argv.includes('--check')) {
 if(await fs.readFile(target,'utf8')!==data) throw new Error('App migration drift; run sync-migration.mjs');
} else {await fs.writeFile(target,data);}
