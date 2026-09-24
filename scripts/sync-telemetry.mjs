// The App reads the telemetry vocabulary from its own byte copy of
// perf/phases.json. Run after editing it; `--check` fails on drift.
import fs from 'node:fs/promises';
const source=new URL('../perf/phases.json',import.meta.url);
const target=new URL('../../app/src/lib/analytics/trace-phases.json',import.meta.url);
const data=await fs.readFile(source,'utf8');
if(process.argv.includes('--check')) {
 const copy=await fs.readFile(target,'utf8').catch(()=>null);
 if(copy!==data) throw new Error('App telemetry vocabulary drift; run node scripts/sync-telemetry.mjs');
} else {await fs.writeFile(target,data);}
