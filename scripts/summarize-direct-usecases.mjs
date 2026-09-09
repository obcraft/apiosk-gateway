// Read saved source responses; no network, AI, billing or database writes.
import fs from 'node:fs';
import path from 'node:path';
const dir=process.argv[2];
if(!dir)throw Error('Pass a saved audit directory');
function fields(v,out=[]){if(!v||typeof v!=='object')return out;if(v.key&&Object.hasOwn(v,'value'))out.push({key:v.key,value:v.value});for(const child of Object.values(v))if(child&&typeof child==='object')fields(child,out);return out;}
for(const file of fs.readdirSync(dir).filter(x=>x.endsWith('.json'))){
 const r=JSON.parse(fs.readFileSync(path.join(dir,file),'utf8'));if(!r.source||!r.data)continue;
 const d=r.data;let summary;
 if(r.status!==200)summary=d;
 else if(file.includes('accounts'))summary=fields(d);
 else if(file.includes('search')&&r.source==='kvk-dutch-business-register')summary={total:d.totaal,companies:d.resultaten.map(x=>({name:x.naam,kvk:x.kvkNummer,type:x.type}))};
 else if(file.includes('profile')&&r.source==='kvk-dutch-business-register')summary={kvk:d.kvkNummer,name:d.statutaireNaam,tradeNames:d.handelsnamen,activities:d.sbiActiviteiten,employees:d.totaalWerkzamePersonen,embedded:d._embedded};
 else if(file.includes('children'))summary={meta:d.meta,records:d.data.map(x=>({name:x.attributes.entity.legalName.name,lei:x.id,country:x.attributes.entity.legalAddress?.country,status:x.attributes.entity.status,registration:x.attributes.registration?.status}))};
 else if(file.includes('sanctions'))summary=d;
 else if(r.source==='companies-house'&&file.includes('profile'))summary={name:d.company_name,number:d.company_number,status:d.company_status,created:d.date_of_creation,type:d.type,accounts:d.accounts};
 else if(file.includes('officers'))summary={total:d.total_results,active:d.items.filter(x=>!x.resigned_on).map(x=>({name:x.name,role:x.officer_role})),resigned:d.items.filter(x=>x.resigned_on).length,returned:d.items.length};
 else if(file.includes('significant-control'))summary={total:d.total_results,records:d.items.map(x=>({name:x.name,ceased:x.ceased_on,kind:x.kind,control:x.natures_of_control}))};
 else if(r.source==='tenderned')summary={total:d.totalElements,returned:d.content?.length,rows:d.content?.slice(0,6).map(x=>({name:x.aanbestedingNaam,authority:x.opdrachtgeverNaam,type:x.typePublicatie,at:x.publicatieDatum??x.publicatiedatum,closing:x.sluitingsDatum??x.sluitingsdatum}))};
 else if(file==='cbs-bankruptcies.json')summary={total:d['@odata.count'],returned:d.value.length,periods:[...new Set(d.value.map(x=>x.Perioden))],rows:d.value.slice(0,4)};
 else if(file.startsWith('cbs-'))summary={count:d.value.length,rows:d.value.slice(0,2)};
 else if(r.source==='eurostat')summary={values:d.value,status:d.status,dimensions:d.dimension};
 else if(r.source==='data-overheid-nl')summary={total:d.result.count,returned:d.result.results.length,rows:d.result.results.slice(0,5).map(x=>({title:x.title,publisher:x.publisher,urls:x.resources?.map(z=>z.url)}))};
 else if(r.source==='pdok')summary={total:d.response.numFound,returned:d.response.docs.length,addresses:d.response.docs.filter(x=>x.type==='adres').slice(0,5)};
 else if(r.source==='overheid-io')summary=d;
 else continue;
 console.log(JSON.stringify({file,at:r.at,status:r.status,summary}));
}
