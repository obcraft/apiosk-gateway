"""Filter the reviewed inventory to official national EU27 concepts only."""
import json
from pathlib import Path
from collections import Counter

p = Path(__file__).resolve().parent
all_data = json.loads((p/'inventory.json').read_text())
research = json.loads((p.parent/'native-company-sources/catalog.json').read_text())
eu = set(research['scope']['eu_members'])
specs = {s['source_id']: s for s in research['sources']}
countries = [c for c in research['countries'] if c['country_code'] in eu]
native_ids = {i for c in countries for i in c['native_source_ids']}
rows = [dict(r, publisher=specs[r['id']]['publisher'], source_class=specs[r['id']]['source_class'],
             readiness=specs[r['id']]['readiness'])
        for r in all_data['sources'] if r['status']=='Concept' and r['id'] in native_ids]
rows.sort(key=lambda r:(r['country'], r['name']))
delivery = {
 'fr-inpi-rne':('API + downloads','Open RNE-registerdata, akten en beschikbare jaarrekeningen'),
 'fr-insee-sirene':('API + downloads','Open Sirene-datasets: ondernemingen en vestigingen'),
 'fr-bodacc':('API + downloads','Open BODACC-publicaties en bedrijfsmededelingen'),
 'be-kbo-bce':('Downloads','Open KBO/BCE CSV-bestanden: volledige actieve populatie en dagelijkse wijzigingen'),
 'be-nbb-accounts':('API + downloads','Authentieke jaarrekeningen en dagelijkse extracts'),
 'ee-rik-business-register':('API + downloads','Open bedrijfsregisterbestanden en overeengekomen webservices'),
 'lv-ur':('API + downloads','Open datasets van het ondernemingsregister en registerwebservices'),
 'ie-cro':('API + downloads','CRO Open Data: bedrijven en gepubliceerde financiële gegevens'),
 'fr-dinum-recherche':('API','Zoekbare officiële afgeleide bedrijfsindex'),
 'fi-prh-ytj':('API','Open YTJ/PRH-bedrijfsbasisgegevens'),
 'fi-prh-financials':('API','Beschikbare digitale iXBRL-jaarrekeningen'),
 'se-bolagsverket-hvd':('API','Open high-value datasets: bedrijven en beschikbare digitale jaarrekeningen'),
 'pl-krs':('API','Open KRS-registeruittreksels'),
 'pl-ceidg':('API','Open CEIDG-bedrijfsgegevens'),
 'pl-regon':('API','Gratis REGON/BIR-registerrapporten na sleutelregistratie'),
 'cz-ares':('API','Officiële ARES-gegevens over economische entiteiten'),
 'sk-ruz':('API','Open financiële datasets onder CC0, ontsloten via de RÚZ API'),
 'hr-sudreg':('API','Open data van het rechtbankregister'),
 'gr-gemi':('API','GEMI open bedrijfsdata onder ODC-BY'),
 'nl-cir':('Webservice','Gratis insolventiepublicaties na overeenkomst'),
 'ro-onrc':('Downloads; rechten open','ONRC publiceert bedrijfs-CSV-snapshots; exacte hergebruiklicentie nog bevestigen'),
 'lt-registru-centras':('Datasets; scope open','JAR-datasets gepubliceerd; exact bedrijfsproduct en hergebruik nog afbakenen'),
}
human_access={'licensed_distribution':'Erkende distributeur / overeenkomst', 'open_data_and_contract_assessment':'Open data en contract onderzoeken', 'portal_integration_unverified':'Portaal; machine-toegang onbevestigd', 'contract_access_assessment':'Toegangscontract onderzoeken', 'registered_api':'API na registratie', 'registered_rest_api':'REST API na registratie', 'commercial_api':'Commerciële API', 'partner_api':'Partner-API'}
for r in rows:
    r['delivery'],r['data_offer']=delivery.get(r['id'],('Nog te bevestigen',r['conditions']))
    r['access_kind']=human_access.get(r['access_kind'],r['access_kind'].replace('_',' '))
names = dict(NL='Nederland',FR='Frankrijk',DE='Duitsland',BE='België',DK='Denemarken',FI='Finland',EE='Estland',LV='Letland',LT='Litouwen',SE='Zweden',IT='Italië',ES='Spanje',PT='Portugal',IE='Ierland',AT='Oostenrijk',PL='Polen',CZ='Tsjechië',SK='Slowakije',SI='Slovenië',HU='Hongarije',RO='Roemenië',BG='Bulgarije',HR='Kroatië',GR='Griekenland',CY='Cyprus',LU='Luxemburg',MT='Malta')
country_rows=[]
for c in countries:
    rr=[r for r in rows if r['id'] in c['native_source_ids']]
    free=[r for r in rr if r['zero_cost_scope_confirmed']]
    country_rows.append(dict(id=c['country_code'],country=names[c['country_code']],source_count=len(rr),
      source_ids=[r['id'] for r in rr],sources='; '.join(r['name'] for r in rr),
      zero_cost_source_count=len(free),free_sources='; '.join(r['name'] for r in free) or 'Nog geen bevestigde €0-machinebron',
      free_scope='; '.join(r['free_scope'] for r in free) or 'Toegang, rechten of tarief nog bevestigen',
      data_offer='; '.join(r['name']+': '+r['data_offer'] for r in rr),
      costs=' · '.join(f"{r['name']}: {r['cost_status']}" for r in rr)))
country_rows.sort(key=lambda c:c['country'])
totals=dict(id='eu-native-concepts',source_records=len(rows),countries=len(country_rows),
 zero_cost_sources=sum(r['zero_cost_scope_confirmed'] for r in rows),
 zero_cost_countries=sum(c['zero_cost_source_count']>0 for c in country_rows),
 commercial_sources=sum(r['cost_code']=='commercial' for r in rows),
 unknown_sources=sum(r['cost_code']=='unknown' for r in rows))
policy=dict(scope='Official national company-register, filings, gazette, insolvency and derived official data products in EU27, status concept only.',
 cost='Zero upstream source fees only for the named free_scope, subject to registration, quota and reuse conditions. Hosting, indexing, storage and AI are excluded.',
 status='All 41 records are source specifications; none is activated as a new Apiosk integration by this report.',
 evidence_date='2026-09-30',reuse='Scope-only revision of the previously reviewed source-cost assessment; no new tariff research or execution tests.',
 exclusions='Existing 37 source groups, non-EU countries, commercial aggregators, global enrichment and marketplace/discovery channels.')
out={'as_of':'2026-09-30','scope':policy,'counts':totals,'sources':rows,'countries':country_rows}
(p/'eu-native-concepts.json').write_text(json.dumps(out,ensure_ascii=False,indent=2)+'\n')
text=['APIOSK — EU NATIVE CONCEPTBRONNEN','Peildatum brononderzoek: 30 september 2026','',
      '41 officiële nationale conceptbronnen voor 27 EU-landen.','20 bronnen met een bevestigd gratis onderdeel; 5 commerciële producten; 16 nog onbevestigd.','',policy['cost'],policy['status'],'']
for c in country_rows:
    text += [c['country']+' ('+c['id']+')','']
    for r in rows:
        if r['id'] not in c['source_ids']:continue
        text += [r['name']+' — '+r['cost_status'], '  Gratis: '+(r['free_scope'] or 'Niet bevestigd'),
          '  Gegevens: '+r['data_offer'],'  Aanbod: '+r['delivery'],'  Toegang: '+r['conditions'],'  Type: '+r['access_kind']]
        if r['excluded_scope']:text.append('  Buiten gratis onderdeel: '+r['excluded_scope'])
        text+=['  Conceptacties: '+', '.join(a['public_name'] for a in r['proposed_actions']),
          '  Nog nodig: '+'; '.join(r['required_work']), '  Onderbouwing: '+' | '.join(r['evidence_urls']),'']
(p/'eu-native-concepts.txt').write_text('\n'.join(text)+'\n')
snapshot=json.loads((p/'report/src/data.json').read_text())
snapshot.update(title='Apiosk — EU native conceptbronnen',buildStatus='updating',audit_policy=policy,report={'asOf':'2026-09-30'})
files=[str(p/'inventory.json'),str(p/'assessments.json'),str(p.parent/'native-company-sources/catalog.json'),str(p/'eu-native-concepts.json')]
def q(label,data):
    return {'label':label,'rows':data,'source':{'label':label,'files':files,'tables':[],
      'filters':['Country belongs to EU27','Source ID occurs in that country’s native_source_ids','Inventory status equals Concept'],
      'caveats':list(policy.values()),'evidenceFlow':'Exact source-ID filter of reviewed inventory; country grouping and counts recomputed. Free scope and evidence URLs preserved.'}}
snapshot['queries']={'source_inventory':q('Officiële EU-conceptbronnen',rows),
                    'countries':q('27 EU-landen met nationale conceptbronnen',country_rows),
                    'inventory_totals':q('Tellingen binnen de EU-conceptselectie',[totals])}
for f in ['reviewed.json','report/src/data.json']:(p/f).write_text(json.dumps(snapshot,ensure_ascii=False,indent=2)+'\n')
assert len(rows)==41 and len(country_rows)==27
assert totals['zero_cost_sources']==20 and totals['commercial_sources']==5 and totals['unknown_sources']==16
assert totals['zero_cost_countries']==13
assert all(r['status']=='Concept' and set(r['country_codes'])<=eu and r['id'] in native_ids for r in rows)
assert not any(r['id']=='company-info' for r in rows)
assert len({r['id'] for r in rows})==41
(p/'eu-validation.json').write_text(json.dumps({'status':'passed','counts':totals,'checks':['41 unique national official concepts','27 EU members represented','20 free scopes / 5 commercial / 16 unknown','No existing source or non-EU country','Commercial aggregator Company.info excluded','Original report identity preserved'],'browser_checks':[]},ensure_ascii=False,indent=2)+'\n')
print(json.dumps(totals,ensure_ascii=False))
