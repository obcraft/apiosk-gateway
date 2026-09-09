// Reproducible, source-backed report; reads saved responses only.
import fs from 'node:fs/promises';
import path from 'node:path';
const root=path.resolve('output');
const oldDir=path.join(root,'direct-usecases-jn7cJ6'),newDir=path.join(root,'direct-usecases-auWgAm');
const load=async(dir,name)=>JSON.parse(await fs.readFile(path.join(dir,name+'.json'),'utf8'));
const money=n=>new Intl.NumberFormat('nl-NL',{maximumFractionDigits:2}).format(Number(n));
const escape=x=>String(x??'—').replaceAll('|','\\|').replaceAll('\n',' ');
const table=(columns,rows)=>'| '+columns.join(' | ')+' |\n| '+columns.map(()=>'---').join(' | ')+' |\n'+rows.map(r=>'| '+r.map(escape).join(' | ')+' |').join('\n')+'\n';
const proof=(dir,name)=>`[Bronantwoord](${path.join(dir,name+'.json')})`;
const questions=[
 ['11','Vergelijk beschikbare balansen','Haal de beschikbare jaarrekeningen op van Mollie B.V. (KVK 30204462) en Volve Capital B.V. Vergelijk de balanscijfers en vermeld de boekjaren.','KVK'],
 ['12','Controleer een winkelbedrijf','Zoek het bedrijfsprofiel van HEMA B.V. op en controleer de bedrijfsnaam op mogelijke sanctiematches.','KVK + OpenSanctions'],
 ['13','Verken een internationaal concern','Toon de eerste gevonden directe dochterbedrijven van Unilever PLC in GLEIF en vermeld hoeveel registraties de bron telt.','GLEIF'],
 ['14','Bekijk een Britse bank','Wie zijn de geregistreerde bestuurders en partijen met significante zeggenschap van Starling Bank Limited?','Companies House'],
 ['15','Vind onderwijsopdrachten','Zoek TenderNed-publicaties met de zoekterm school en vat de gevonden opdrachten samen.','TenderNed'],
 ['16','Bekijk een historische statistiek','Toon maximaal 100 CBS-faillissementswaarnemingen uit 2025, nieuwste periode eerst, met sectoren en eenheden.','CBS'],
 ['17','Vergelijk economische omvang','Vergelijk het bbp van Nederland en Duitsland in 2024, in dezelfde eenheid.','Eurostat'],
 ['18','Vind bodemdata','Welke datasets over bodemkwaliteit staan op data.overheid.nl? Toon uitgevers en bronlinks.','data.overheid.nl'],
 ['19','Controleer een Rotterdams adres','Controleer Coolsingel 40 Rotterdam en toon het officiële adres, postcode, gemeente en buurt.','PDOK'],
 ['20','Bekijk adressen rond een postcode','Welke BAG-adressen vind je binnen postcode 3011AD?','Overheid.io'],
];
const first=[
 ['1','HEMA en AFAS vergelijken','Niet uitvoerbaar met de aangesloten jaarrekeningbron. HEMA 34215639 en AFAS 31046821 zijn gevonden, maar beide jaarrekeningcalls geven HTTP 404 / IPD7002. Er is geen financiële vergelijking gemaakt. De wachttijd en nummers waren aanwezig.','hema-accounts'],
 ['2','Coolblue controleren','KVK 24330087, statutaire naam Coolblue B.V., bezoekadres Weena 664, 3012CN Rotterdam; negen handelsnamen. OpenSanctions retourneert nul kandidaten op de bedrijfsnaam. Dit is geen garantie dat het bedrijf veilig is.','coolblue-profile'],
 ['3','ASML-bedrijfsfamilie','LEI 724500Y6DUVHQD6OXN27; elf directe-dochterrecords teruggegeven, waaronder ASML Netherlands B.V., Cymer B.V. en ASML US, LLC. De lijst bevat ook LAPSED- en DUPLICATE-registraties; dus niet elf bewezen unieke actieve dochters.','asml-children'],
 ['4','Monzo doorlichten','MONZO BANK LIMITED, nummer 09446231, actief. Acht huidige directors en één secretary, plus 21 afgetreden officer-records. Actieve zeggenschap: Monzo Bank Holding Group Limited, met 75–100% aandelen en stemrechten en het recht bestuurders te benoemen/verwijderen.','monzo-officers'],
 ['5','Laadpaalpublicaties','Tien publicaties van 221 matches. Onder meer marktconsultatie Nijmegen, laadpalen voor Hoogheemraadschap van Schieland en de Krimpenerwaard, en publieke laadinfrastructuur Utrecht. Er zitten historische en gegunde opdrachten tussen; geen lijst van uitsluitend nog open tenders.','tenders'],
 ['6','CBS-faillissementen','Honderd observaties teruggegeven, allemaal met periodecode 2026MM07 (juli 2026). De totaalrij voor bedrijven, instellingen en eenmanszaken is 254 uitgesproken faillissementen; landbouw/bosbouw/visserij is 3. Officiële maat, eenheid en sectorcodes zijn opgehaald. De pagina bevat niet elke sector.','cbs-bankruptcies'],
 ['7','Bbp 2023','Nederland: 1.050.133 miljoen euro. Duitsland: 4.254.930 miljoen euro. Beide nominaal bbp tegen marktprijzen, lopende prijzen, kalenderjaar 2023. Duitsland is ongeveer 4,05 maal Nederland; het Duitse cijfer heeft bronvlag p (voorlopig).','gdp-NL'],
 ['8','Waterkwaliteitsdata','Twintig datasets teruggegeven van 87 matches. Onder andere oppervlaktewater- en grondwatermeetpunten van provincie Zeeland, Waterkwaliteit - Projecten van Rijkswaterstaat, en LMM van RIVM. Dit zijn datasetbeschrijvingen en links, niet de meetwaarden.','waterquality'],
 ['9','Dam 1','Exacte match: Dam 1, 1012JS Amsterdam; gemeente Amsterdam; buurt Oude Kerk e.o.; wijk Burgwallen-Oude Zijde. Andere zoekhits zijn geen alternatieve feiten over hetzelfde adres.','dam-address'],
 ['10','Postcode 1012JS','33 adresrecords teruggegeven; de bron meldt één pagina en 33 totaal. Alle gevonden adressen liggen aan de Dam in Amsterdam, inclusief huisletters. Volledige lijst hieronder.','postcode-addresses'],
];
const second=[
 ['11','Mollie en Volve','Beide broncalls slagen, maar de jaren verschillen: Mollie (30204462) boekjaar 2020, Volve (86717162) boekjaar 2025. Mollie: activa 194.495.000, eigen vermogen 7.663.000, resultaat na belasting -5.166.000. Volve: activa 682.987, eigen vermogen -373.267; winst/verlies ontbreekt. De API vermeldt geen expliciete valuta of schaal: hieronder staan ongeschaalde bronwaarden, niet automatisch eurobedragen. Geen gelijkwaardige actuele prestatievergelijking.','mollie-accounts'],
 ['12','HEMA naamcheck','HEMA B.V., KVK 34215639, NDSM-straat 10, 1033SB Amsterdam. De naamcheck geeft één kandidaat: Hema Trading Co Pvt Ltd, jurisdictie India, op een debarment-lijst. Dit is geen bevestigde match met HEMA Nederland en mag niet als sanctie tegen HEMA worden gepresenteerd.','hema-sanctions'],
 ['13','Unilever-dochters','UNILEVER PLC, LEI 549300MKFYEKVRWML317. Vijftien records teruggegeven uit 70 totaal, verdeeld over vijf bronpagina’s. Voorbeelden: UNILEVER CANADA INC., UNILEVER DE PUERTO RICO, INC en ROLON B.V. De overige pagina’s zijn niet opgehaald.','unilever-children'],
 ['14','Starling Bank','STARLING BANK LIMITED, nummer 09092149, actief. Zeven huidige directors en één secretary, plus 21 afgetreden officer-records. Actieve zeggenschap: Starling Intermediate Holdings Limited met 75–100% aandelen en stemrechten en benoemings-/ontslagrecht. Drie andere PSC-records zijn beëindigd.','starling-officers'],
 ['15','Schoolpublicaties','Tien publicaties teruggegeven van 9.272 matches voor school. Onder meer catering voor Europese School Bergen, tijdelijke onderwijsaccommodatie Waalwijk en jeugdhulp op scholen in Zeeland. De resultaten zijn breed en bevatten historische en gegunde opdrachten. De aanvankelijke smallere zoekterm schoolrenovatie gaf nul resultaten; dat bewijs is behouden.','tenders-school'],
 ['16','CBS in 2025','De filter op 2025 meldt 18.972 observaties. De eerste 100, nieuwste eerst, hebben allemaal periodecode 2025MM12 (december 2025). De totaalrij is 296 faillissementen; landbouw/bosbouw/visserij 3. Dit is geen jaartotaal en geen volledige jaarreeks.','cbs-bankruptcies'],
 ['17','Bbp 2024','Nederland: 1.115.252 miljoen euro. Duitsland: 4.386.940 miljoen euro. Beide nominaal bbp tegen marktprijzen in lopende prijzen, 2024. Duitsland is ongeveer 3,93 maal Nederland; de Duitse waarde is voorlopig (p).','gdp-NL'],
 ['18','Bodemkwaliteitsdata','Twintig datasets teruggegeven van 58 matches. Onder andere Bodemkwaliteit_WFL1 (Hilversum), Functieklassekaart en Toepassingsmogelijkheden (Enschede), Bodemkwaliteitskaart Haarlem en Diffuse bodemkwaliteit (Zuid-Holland). Links en uitgevers zijn gekoppeld vanuit hetzelfde bronrecord.','soilquality'],
 ['19','Coolsingel 40','Exacte match: Coolsingel 40, 3011AD Rotterdam; gemeente Rotterdam; buurt Stadsdriehoek; wijk Rotterdam Centrum. BAG-nummeraanduiding 0599200000239721.','coolsingel-address'],
 ['20','Postcode 3011AD','26 adresrecords teruggegeven, 26 totaal, één bronpagina. Alle gevonden adressen liggen aan de Coolsingel in Rotterdam. Volledige huisnummer-/huisletterlijst hieronder.','postcode-addresses'],
];
let report='# Apiosk — 20 voorbeeldvragen en gecontroleerde bronresultaten\n\n';
report+='Peildatum: 9 september 2026. Eerste reeks: opgeslagen directe brontests rond 15:09–15:10 Nederlandse tijd. Nieuwe reeks: directe brontests vanaf 16:37, met een aanvullende expliciete Mollie-aanvraag om 16:38. Geen Apiosk-connector, geen Apiosk-aankoop of saldo-autorisatie. Provider-API-quota kunnen wel zijn gebruikt. Dit bewijst bronresultaten, niet twintig nieuwe end-to-end betaalde chatbotruns.\n\n';
report+='## Tien nieuwe vragen\n\n'+table(['#','Naam in de app','Voorbeeldvraag','Bronnen'],questions)+'\n';
for(const [title,rows,dir] of [['Antwoorden op de oorspronkelijke tien vragen',first,oldDir],['Antwoorden op de tien nieuwe vragen',second,newDir]]){
 report+='## '+title+'\n\n';
 report+=table(['#','Vraag','Antwoord / beperking','Bewijs'],rows.map(([n,name,answer,file])=>[n,name,answer,proof(dir,file)]))+'\n';
}
report+='## Volledige opgehaalde lijsten en exacte bronvelden\n\n';
for(const [title,dir,prefix] of [['ASML',oldDir,'asml'],['Unilever',newDir,'unilever']]){
 const r=await load(dir,prefix+'-children');
 report+='### '+title+' — directe-dochterrecords\n\n';
 report+=table(['Naam','LEI','Land','Entiteitsstatus','Registratiestatus'],r.data.data.map(x=>[x.attributes.entity.legalName.name,x.id,x.attributes.entity.legalAddress.country,x.attributes.entity.status,x.attributes.registration.status]));
 report+='\n'+proof(dir,prefix+'-children')+'; totaal volgens bron: '+r.data.meta.pagination.total+'.\n\n';
}
for(const [name,dir,prefix] of [['Monzo',oldDir,'monzo'],['Starling',newDir,'starling']]){
 const r=await load(dir,prefix+'-officers');report+='### '+name+' — huidige functionarissen\n\n';
 report+=table(['Naam zoals geregistreerd','Functie'],r.data.items.filter(x=>!x.resigned_on).map(x=>[x.name,x.officer_role]));
 report+='\n'+proof(dir,prefix+'-officers')+'\n\n';
}
for(const [name,dir,file] of [['Waterkwaliteit',oldDir,'waterquality'],['Bodemkwaliteit',newDir,'soilquality']]){
 const r=await load(dir,file);report+='### '+name+' — alle opgehaalde datasets\n\n';
 report+=table(['Titel','Uitgever volgens bron','Bronlinks'],r.data.result.results.map(x=>[x.title,x.publisher,(x.resources??[]).map(z=>z.url).filter(Boolean).map((u,i)=>`[Link ${i+1}](${encodeURI(u)})`).join('; ')]));
 report+='\n'+proof(dir,file)+'; '+r.data.result.results.length+' teruggegeven van '+r.data.result.count+'. Links kunnen ook previews of beschrijvingen zijn; niet elke URL levert direct meetdata.\n\n';
}
for(const [postcode,dir] of [['1012JS',oldDir],['3011AD',newDir]]){
 const r=await load(dir,'postcode-addresses');report+='### Postcode '+postcode+' — alle opgehaalde adressen\n\n';
 report+=table(['Straat','Huisnummer','Huisletter','Toevoeging','Woonplaats'],r.data._embedded.adres.map(x=>[x.openbareruimte,x.huisnummer,x.huisletter??'',x.huisnummertoevoeging??'',x.woonplaats]));
 report+='\n'+proof(dir,'postcode-addresses')+'\n\n';
}
function flatten(v,out=[]){if(!v||typeof v!=='object')return out;if(v.key&&Object.hasOwn(v,'value'))out.push([v.key,v.value]);for(const child of Object.values(v))if(child&&typeof child==='object')flatten(child,out);return out;}
report+='### Mollie en Volve — beschikbare jaarrekeningvelden\n\nGeen actuele vergelijkbare jaren. Valuta en schaal ontbreken in de API-response en worden niet verzonnen.\n\n';
const reports=await Promise.all(['mollie','volve'].map(x=>load(newDir,x+'-accounts')));
const maps=reports.map(r=>new Map(flatten(r.data)));
const keys=['FinancialYear','DocumentAdoptionDate','Assets','AssetsNoncurrent','AssetsCurrent','Equity','Liabilities','LiabilitiesCurrent','LiabilitiesMaturityLessThanOneYear','LiabilitiesMaturityExceedingOneYear','ResultAfterTax','OperatingResult'];
report+=table(['Bronveld','Mollie 30204462','Volve 86717162'],keys.map(k=>[k,...maps.map(m=>m.has(k)?(/Date|Year/.test(k)?m.get(k):money(m.get(k))):'Niet geleverd')]));
report+='\nMollie naamzoekopdracht gaf ook KVK 92327737 terug. Die registratie is niet automatisch gekozen of onderzocht; de nieuwe vraag noemt 30204462 expliciet. '+proof(newDir,'mollie-search')+'\n\n';
report+='## Conclusie voor voorbeeldvragen in de app\n\nGebruik HEMA/AFAS niet als werkende jaarrekeningdemo. De vervangende Mollie/Volve-vraag levert wel gegevens, maar is door de verschillende boekjaren geschikt om beperkingen te demonstreren, niet als actuele benchmark. De HEMA-naamcheck is een goed voorbeeld van waarom een gelijkende naam niet automatisch een sanctiematch is. Alle opsommingen behouden bronpaginering en aantallen. De GDP-vragen gebruiken expliciete jaren en dezelfde eenheid. Er zijn geen productie-appwijzigingen, nieuwe broncontracten of prijzen gepubliceerd tijdens deze rapportage.\n';
const filename=path.join(root,'apiosk-20-vragen-en-resultaten-2026-09-09.md');await fs.writeFile(filename,report);
const records=[];for(const name of await fs.readdir(newDir)){if(!name.endsWith('.json')||name==='manifest.json')continue;const r=JSON.parse(await fs.readFile(path.join(newDir,name),'utf8'));if(r.endpoint_id&&r.status){const{data,request_body,...meta}=r;records.push(meta);}}
await fs.writeFile(path.join(newDir,'complete-manifest.json'),JSON.stringify({scope:'Direct-source second set including explicit Mollie follow-up and broader school search; no Apiosk purchase',count:records.length,calls:records},null,2));
console.log(filename);console.log('Source responses in new set including supplemental checks: '+records.length);
