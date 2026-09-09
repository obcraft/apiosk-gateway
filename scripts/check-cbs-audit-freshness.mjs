// A diagnostic follow-up only; does not change the planner's default query.
import fs from 'node:fs/promises';
import path from 'node:path';
const evidenceDir=process.argv[2];
if(!evidenceDir)throw Error('Provide the existing evidence directory');
const url=new URL('https://datasets.cbs.nl/odata/v1/CBS/82244NED/Observations');
url.searchParams.set('$top','5');url.searchParams.set('$orderby','Perioden desc');
const r=await fetch(url,{headers:{Accept:'application/json'},signal:AbortSignal.timeout(30000),redirect:'error'});
const data=await r.json();const record={id:'cbs-freshness-diagnostic',status:r.status,url:url.toString(),at:new Date().toISOString(),note:'Additional diagnostic request, not part of the original single-call plan.',data};
await fs.writeFile(path.join(evidenceDir,'cbs-freshness-orderby.json'),JSON.stringify(record,null,2));
console.log(JSON.stringify(record));
