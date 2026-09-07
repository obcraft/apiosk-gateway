import fs from 'node:fs/promises';
for (const name of (await fs.readdir(new URL('../supabase/migrations/',import.meta.url))).filter(n=>n.endsWith('.sql'))) {
const data=await fs.readFile(new URL(`../supabase/migrations/${name}`,import.meta.url),'utf8');
const target=new URL(`../../app/supabase/migrations/${name}`,import.meta.url);
if(process.argv.includes('--check')) {
 if(await fs.readFile(target,'utf8')!==data) throw new Error('App migration drift; run sync-migration.mjs');
} else {await fs.writeFile(target,data);}
}
