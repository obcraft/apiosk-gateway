"""Reproduce the cost inventory from preserved read-only snapshots. No network or DB writes."""
import json
from pathlib import Path
from collections import Counter

ROOT = Path(__file__).resolve().parent
DOCS = ROOT.parent
old = json.loads((DOCS / 'source-niches-20260930/complete-overview.json').read_text())
research = json.loads((DOCS / 'native-company-sources/catalog.json').read_text())
live = json.loads((ROOT / 'live-catalog.json').read_text())
specs = {s['source_id']: s for s in research['sources']}
groups = {'country_apis': 'Countries & Registries', 'open_government_datasets': 'Open Data', 'enrichment': 'Enrichment', 'search_tools': 'Search & Tools'}
A = {}

def assess(sid, code, scope, conditions, urls, access='API', excluded=''):
    A[sid] = dict(cost_code=code, free_scope=scope, conditions=conditions, evidence_urls=urls.split('|'), access_kind=access, excluded_scope=excluded)

# €0 refers to the listed upstream read/compute operation, not total operating cost.
assess('kvk-dutch-business-register','mixed','Zoeken; HVD basisbedrijfsgegevens; HVD jaarrekeningen',
 'Zoeken: API-key. Op verzoek telt €6,40 per maand niet mee. HVD: alleen beschikbare BV/NV-gegevens, beperkte identificatievelden; CC BY 4.0, 1 aanvraag/minuut/IP. HVD basisgegevens staat uit in de catalogus.',
 'https://developers.kvk.nl/nl/faq|https://developers.kvk.nl/nl/documentation/open-dataset-basis-bedrijfsgegevens-api|https://developers.kvk.nl/nl/documentation/open-dataset-jaarrekeningen-api', excluded='Basisprofiel, Vestigingsprofiel en Naamgevingen: betaald per aanvraag; HVD is geen volledig bedrijfsdossier.')
assess('companies-house','zero','Bedrijf zoeken; profiel; bestuurders; PSC; deponeringen; charges; insolventie',
 'Gratis public-data-API met eigen key en rate limits. Alleen openbare gegevens; betaalde certificaten en filing-acties vallen buiten deze scope.',
 'https://developer.company-information.service.gov.uk/get-started/|https://developer-specs.company-information.service.gov.uk/guides/rateLimiting')
assess('vies','zero','EU-btw-nummer controleren', 'Landcode en btw-nummer nodig. Geen algemene bedrijfszoekmachine. Storing of onbeschikbaarheid is niet hetzelfde als ongeldig.',
 'https://europa.eu/youreurope/business/finance-and-tax/vat/check-vat-number-vies/index_en.htm')
assess('gleif','zero','LEI zoeken; entiteit; consolidatie-relaties; referentiecodes', 'Open LEI-data onder CC0. Relaties betreffen accounting consolidation; geen compleet UBO-register.',
 'https://www.gleif.org/en/lei-data/gleif-api/|https://www.gleif.org/de/about/open-data/')
assess('rdw','zero','Open voertuiggegevens op kenteken', 'Alleen openbare RDW/Socrata-data; geen eigenaar of persoonsgegevens.',
 'https://opendata.rdw.nl/|https://www.rdw.nl/zakelijke-partners/verzekeraar/regels-over-gegevens-uit-onze-registers-opvragen')
assess('cbs-statline','zero','Datasets zoeken; metadata; dimensies; statistische cijfers', 'Open OData. Bewaar tabelcode, filters, periode, eenheid en bronvermelding; paginateer omvangrijke resultaten.',
 'https://www.cbs.nl/nl-nl/onze-diensten/open-data|https://www.cbs.nl/nl-nl/onze-diensten/open-data/statline-als-open-data/snelstartgids-odata-v4')
assess('eurostat','zero','Datasetmetadata; dimensies; Europese statistiek', 'Gratis REST/SDMX/JSON-stat. Comext heeft aparte routes. Gepubliceerde revisies kunnen cijfers wijzigen.',
 'https://ec.europa.eu/eurostat/web/user-guides/data-browser/api-data-access/api-introduction')
assess('pdok','zero','Adressen; gebouwen; kaarten; perceelgeometrie; geografische kenmerken', 'Gratis publieke services. Licentie verschilt per dataset; geometrie levert geen eigendomsbewijs.',
 'https://www.pdok.nl/veelgestelde-vragen-over-pdok-services')
assess('pdok-ruimtelijke-plannen','zero','Kaartlagen van ruimtelijke plannen raadplegen', 'Publieke PDOK-service; verifieer actualiteit van het plan. Een kaartlaag bewijst geen vergunning of bouwrecht.',
 'https://www.pdok.nl/veelgestelde-vragen-over-pdok-services')
assess('koop','zero','Officiële publicaties en regelgeving zoeken via SRU', 'Open hergebruik. BUS is uitgefaseerd; gebruik de ondersteunde SRU-route en bronmetadata.',
 'https://www.koopoverheid.nl/actueel/nieuws/2024/07/05/uitfaseren-bus|https://www.koopoverheid.nl/voor-overheden/gemeenten-provincies-en-waterschappen/drop/meer-weten-over-drop/kennisbank-kwaliteit')
assess('data-overheid-nl','zero','Datasetcatalogus doorzoeken; metadata ophalen', 'Gratis catalogusmetadata. Beschikbaarheid en licentie van de gevonden dataset moeten afzonderlijk worden gecontroleerd.',
 'https://data.overheid.nl/opvragen-van-dataset-informatie-de-api-van-dataoverheidnl')
assess('rvo','zero','Open subsidieregelingen en financieringsmetadata', 'CC0-dataset. De huidige Apiosk-route is begrensd tot de eerste 50 records en bewijst geen volledigheid of subsidiegeschiktheid.',
 'https://data.overheid.nl/en/dataset/subsidies-en-financiering-rvo-nl|https://www.rvo.nl/open-data-van-rvonl')
assess('tenderned','zero','Open aankondigingen en aanbestedingsdatasets', 'Publieke data/XML-services; onderscheid van ingelogde diensten voor aanbestedende organisaties. Publicatie is geen bewezen omzet.',
 'https://www.tenderned.nl/cms/nl/aanbesteden-in-cijfers/datasets-aanbestedingen|https://www.tenderned.nl/info/swagger/')
assess('ted','zero','Europese aanbestedingsaankondigingen zoeken', 'Publieke Search API zonder authenticatie; commercieel hergebruik beschreven. Bestaande Apiosk-actie zoekt organisatienotices op identifiers.',
 'https://docs.ted.europa.eu/api/latest/search.html|https://docs.ted.europa.eu/ODS/latest/reuse/search-api.html')
assess('eu-funding-tenders','zero','EU-calls, topics, projecten en organisaties raadplegen', 'Publieke Funding & Tenders-services. Geen subsidieaanvraag indienen en geen eligibility-besluit.',
 'https://ec.europa.eu/info/funding-tenders/opportunities/portal/screen/support/apis')
assess('eurlex-cellar','zero','EU-wetgeving en publicatiemetadata zoeken/ophalen', 'Publieke Cellar REST/SPARQL-data; vermeld versie, datum en bron. Documentrechten waar van toepassing behouden.',
 'https://op.europa.eu/en/web/cellar/cellar-data|https://op.europa.eu/en/web/eu-vocabularies/cellar')
assess('bodemloket','access_only','Publieke bodemkaartlagen via WMS', 'Publieke WMS-toegang beschreven, maar commerciële hergebruiklicentie en eventuele specifieke voorwaarden niet afdoende bevestigd. Daarom buiten de bevestigde €0-selectie.',
 'https://www.bodemloket.nl/content/kan-ik-bodemloket-ook-mijn-gis-inladen', excluded='Geen volledige bodemrapporten; ontbreken van een feature betekent niet dat grond schoon is.')
assess('iban-check','zero','IBAN-formaat; checksum; land/SEPA; beperkte NL-bankcode/BIC-mapping', 'Bestaande adapter rekent lokaal; geen upstream API of providerfee. Geen bewijs van bestaand rekeningnummer, saldo of rekeninghouder.',
 str(DOCS.parent / 'legacy-executor/src/proxy/iban_check.rs'), access='Lokale berekening')
assess('domain-intelligence','zero','DNS/MX opvragen; openbare RDAP-registratiegegevens', 'Bestaande adapter gebruikt Google Public DNS en register-RDAP zonder betaald abonnement. RDAP: com/net/org/nl/uk/fr/app/dev; registryvoorwaarden en throttling blijven gelden. Geen universele WHOIS- of eigenaarcontrole.',
 str(DOCS.parent / 'legacy-executor/src/proxy/domain_intel.rs'), access='Publieke API')

assess('fr-inpi-rne','zero','RNE-profiel; wijzigingen; openbare akten en beschikbare jaarrekeningen', 'DATA INPI-account en technische toegang/licentie nodig; beschermde en vertrouwelijke gegevens ontbreken.',
 'https://www.inpi.fr/ressources/formalites-dentreprises/acces-lapi-formalite-rne|https://data.inpi.fr/content/editorial/Acces_API_Entreprises', 'API + bulk')
assess('fr-dinum-recherche','zero','Bedrijf zoeken; publieke identifiers en basisgegevens', 'Publieke afgeleide index van officiële bronnen. Rate limits en beperkte velden; niet het volledige RNE.',
 'https://recherche-entreprises.api.gouv.fr/docs/')
assess('fr-insee-sirene','zero','SIREN/SIRET-profielen; vestigingen; status; historische perioden', 'Open Sirene API/bulk; volg actuele API-registratie en quotaregels. Niet-diffuseerbare velden respecteren.',
 'https://www.insee.fr/fr/information/3591226', 'API + bulk')
assess('fr-bodacc','zero','Bedrijfsmededelingen; insolventie- en deponeringspublicaties zoeken', 'Vrij hergebruik met bron, datum en oorspronkelijke publicatiecontext; een publicatie is niet automatisch de actuele status.',
 'https://www.bodacc.fr/pages/donnees-ouvertes-et-api/', 'API + bulk')
assess('be-kbo-bce','mixed','Open Data CSV: actieve entiteiten, vestigingen en dagelijkse wijzigingen', 'Gratis Open Data-account/SFTP; eigen lokale zoekindex bouwen. Persoonsgegevens niet voor direct marketing hergebruiken.',
 'https://economie.fgov.be/en/themes/enterprises/crossroads-bank-enterprises/services-everyone/public-data-available-reuse/cbe-open-data|https://kbopub.economie.fgov.be/kbo-open-data/login%3Blang%3Den', 'Bulk → eigen index',
 'Public Search webservice en volledige commerciële hergebruikbestanden zijn aparte producten; niet als gratis meegenomen.')
assess('be-nbb-accounts','mixed','Authentic Data Query; Daily Extract; Authentic Archive', 'Deze authentieke jaarrekeningproducten zijn gratis; registratie, client-ID en toegangsafspraken nodig. Gestructureerde JSON-dekking verschilt per jaar/formaat.',
 'https://www.nbb.be/en/central-balance-sheet-office/consultation/web-services', 'API + bulk',
 'Improved Data en Improved Archive blijven betaald.')
assess('no-brreg','zero','Enhetsregisteret: organisaties; subeenheden; zoeken; bulk', 'Publieke API en downloads onder NLOD. Financiële producten en afgeschermde persoonsgegevens niet automatisch inbegrepen.',
 'https://data.brreg.no/enhetsregisteret/api/dokumentasjon/en/index.html', 'API + bulk')
assess('fi-prh-ytj','zero','Finse bedrijfsbasisgegevens zoeken/ophalen', 'Gratis openbare PRH/YTJ-interface; rechtsvormen en beschikbare velden volgens productdekking.',
 'https://www.prh.fi/en/companiesandorganisations/tietopalvelut/prhopendata.html')
assess('fi-prh-financials','zero','Gestructureerde digitale jaarrekeningen ophalen', 'Gratis digitale retrievalinterface; alleen ingediende iXBRL-jaarrekeningen, geen volledige historische PDF-collectie.',
 'https://www.prh.fi/en/companiesandorganisations/financial_statements/digital-financial-reporting/what.html')
assess('ee-rik-business-register','zero','Open bedrijfsgegevens; bulk; toegelaten XML-webservices', 'API-only contract is gratis; overeenkomst en credentials nodig. Alleen de overeengekomen openbare diensten en velden.',
 'https://avaandmed.ariregister.rik.ee/en/open-data-api/introduction-api-services', 'API + bulk')
assess('lv-ur','zero','Open registerdatasets en toegelaten registerwebservices', 'Autoriteit vermeldt geautomatiseerde gegevensuitwisseling als gratis. Servicekeuze, registratie en veldrechten nog in adapter vastleggen.',
 'https://www.ur.gov.lv/en/services/request-automated-dissemination-data-between-different-information-systems-latvia-and-europe|https://www.ur.gov.lv/en/registry-api-web-services-service-list', 'API + bulk')
assess('se-bolagsverket-hvd','zero','HVD-bedrijfsgegevens; beschikbare digitale jaarrekeningen; classificaties', 'Gratis HVD-product; aanmelden met contactgegevens. HVD staat los van de betaalde reguliere company-information API en beperkte UBO-toegang.',
 'https://bolagsverket.se/apierochoppnadata/hamtaforetagsinformation/vardefulladatamangder/apiforvardefulladatamangder.5513.html')
assess('ie-cro','mixed','Nieuwe CRO Open Data: bedrijfsgegevens en gepubliceerde financiële gegevens', 'Open-data-portaal met API/bulk en CC BY 4.0. Exacte datasetdekking en actualiteit in adapter vastleggen.',
 'https://cro.ie/the-companies-registration-office-cro-announces-the-launch-of-new-open-data-portal/|https://opendata.cro.ie/', 'API + bulk',
 'Legacy documentproducten, certificaten en andere betaalde CRO-diensten niet inbegrepen.')
assess('pl-krs','zero','Huidig en volledig KRS-uittreksel ophalen', 'Open API; KRS-nummer nodig. Persoonsvelden zijn gefilterd; CEIDG-eenmanszaken zijn een ander register.',
 'https://www.gov.pl/web/sprawiedliwosc/uruchomienie-otwartego-api-krajowego-rejestru-sadowego')
assess('pl-ceidg','zero','Eenmanszaken zoeken; openbare bedrijfsprofielen', 'Gratis CEIDG-dataplatform, registratie/token en gebruiksvoorwaarden toepassen. Geen volledige KRS-dekking.',
 'https://www.gov.pl/web/ia/portale-interoperacyjnosci|https://www.gov.pl/web/rozwoj-technologia/e-uslugi-dla-biznesu')
assess('pl-regon','zero','Entiteit op REGON/NIP/KRS zoeken; openbare registerrapporten', 'BIR is ook voor commerciële partijen gratis. Productiesleutel aanvragen bij GUS; SOAP-adapter en tijdsgebonden limieten.',
 'https://api.stat.gov.pl/Home/RegonApi?lang=pl')
assess('cz-ares','zero','ARES zoeken; economische entiteit op IČO ophalen', 'Publieke REST-interface; afgeleide officiële aggregatie, geen complete rechtbankdocumentenservice.',
 'https://data.mf.gov.cz/api/ares.html|https://mf.gov.cz/cs/ministerstvo/informacni-systemy/ares')
assess('sk-ruz','zero','Rapporterende entiteiten; jaarrekeningen; staten; wijzigingen; codelijsten', 'Open API onder CC0. Alleen openbaar gepubliceerde documenten; RÚZ-ID en IČO apart bewaren.',
 'https://www.registeruz.sk/cruz-public/home/api')
assess('hr-sudreg','zero','Open rechtbankregister: entiteiten; identifiers; status; adres; rechtsvorm', 'Gratis REST API met registratie en client-credentials. Werkdagupdates; openbare subset volgens OpenAPI.',
 'https://sudreg-data.gov.hr/ords/r/srn_rep/vanjski-srn-rep/home|https://sudreg-data.gov.hr/api/javni/dokumentacija/open_api')
assess('gr-gemi','zero','GEMI open bedrijfsgegevens zoeken/ophalen', 'Open API onder ODC-BY 1.0. Registratie/key, bronvermelding en quota; alleen openbaar product.',
 'https://opendata.businessportal.gr/|https://opendata.businessportal.gr/license/')
assess('nl-cir','zero','Open insolventiepublicaties en wijzigingen via CIR-webservice', 'Abonnement én gebruik gratis. Organisatie aanmelden en overeenkomst sluiten; gebruiksvoorwaarden, bewaartermijnen en seriële verzoeken toepassen.',
 'https://www.rechtspraak.nl/registers/webservice-centraal-insolventieregister|https://www.rechtspraak.nl/registers/voorwaarden-voor-gebruik-cir')
assess('ch-zefix','mixed','Zefix Linked Data/LINDAS: actuele kerngegevens van actieve entiteiten', 'Publieke SPARQL-dataset met open-use voorwaarden; naam, zetel, adres en rechtsvorm. REST-accountvoorwaarden apart bevestigen.',
 'https://opendata.swiss/de/dataset/zefix-zentraler-firmenindex|https://ckan.opendata.swiss/api/3/action/package_show?id=zefix-zentraler-firmenindex', 'Linked data / SPARQL',
 'Geen gratisgarantie voor alle Zefix REST-, Regix-, historische of gecertificeerde producten.')
assess('gleif-registration-authorities','reference','Registration Authorities-codebestand', 'Gratis openbare referentielijst voor registeridentificatie; geen afzonderlijke bedrijfszoek-API.',
 'https://www.gleif.org/en/about-lei/code-lists/gleif-registration-authorities-list', 'Referentiedataset')
assess('ebra-directory','reference','Directory van nationale registers', 'Vrij raadpleegbare directory; geen bewijs van gratis API-toegang of hergebruik van de onderliggende registers.',
 'https://ebra.be/worldwide-registers/', 'Directory')
assess('openownership-datasets','mixed','Geselecteerde BODS-downloads, waaronder UK-publicatie onder CC0', 'Licentie en peildatum per dataset. Het oude Open Ownership Register sloot in november 2024; downloads zijn geen live wereldwijde ownership-API.',
 'https://www.openownership.org/en/topics/open-ownership-register/|https://bods-data.openownership.org/source/uk_version_0_4/', 'Bulk → eigen index',
 'Geen blanketlicentie voor alle datasets en geen universeel actueel UBO-register.')
assess('icij-offshore-leaks','zero','Offshore Leaks CSV/graaf; entiteiten en relaties; publieke API/reconciliation als kandidaat', 'Gratis dataset: ODbL; inhoud CC BY-SA. ICIJ-attributie en share-alikeverplichtingen behouden. Historische onderzoeksdata, geen bewijs van actuele eigendom of misstanden.',
 'https://offshoreleaks.icij.org/pages/database|https://offshoreleaks.icij.org/docs/reconciliation', 'Bulk / onderzoeksdata')
assess('us-sec-edgar','zero','Submissions; filings; XBRL companyfacts/concepts/frames', 'Gratis data-API zonder key; identificerende User-Agent en SEC fair-accesslimiet. Alleen SEC-filers; VS-aanvulling buiten de 42 Europese landen.',
 'https://www.sec.gov/search-filings/edgar-application-programming-interfaces')
assess('opensanctions','commercial','', 'Commercieel gebruik binnen Apiosk vereist API-/datalicentie. Gratis download of proefkey maakt het commerciële gebruik niet gratis.',
 'https://www.opensanctions.org/docs/commercial/exemption/|https://www.opensanctions.org/docs/api/', excluded='Trial en gratis niet-commerciële toegang niet meetellen.')

# Commercial products: no permanent zero-cost production entitlement established.
commercial = {
 'agentbodega':'Betaalbare x402-utilities; discovery is geen gratis uitvoering.',
 'ask-merlin':'Commerciële UK-VAT-route; HMRC-herkomst maakt deze providerroute niet gratis.',
 'bloomberg':'Catalogus gebruikt een RapidAPI-route; geen directe gratis Bloomberg-licentie.',
 'browserbase':'Cloudbrowser-sessies verbruiken betaalde resources; gratis status/close is geen gratis sessie.',
 'cityfalcon-financial-api':'Commerciële financiële nieuws-API; plan en distributierechten vereist.',
 'crowdpull':'Commerciële/x402-route voor verzameling en verrijking.',
 'eodhd':'Commerciële marktdata; beperkte free tier is geen algemene commerciële distributielicentie.',
 'exa':'Betaalde zoek-/extractiediensten; proefcredits niet meetellen.',
 'linkup':'Betaalde zoek-/research-diensten; proefcredits niet meetellen.',
 'overheid-io':'Commerciële tussenpartij voor o.a. KVK/BAG/RDW; open oorspronkelijke data maakt deze route niet gratis.',
 'pixelforge-x402-data-services':'Bestaande x402-utilities hebben providerprijzen. Een lokale vervanging zou een nieuwe implementatie zijn.',
 'pulsenetwork':'76 service-listings met verschillende x402-prijzen; beoordeel iedere gekozen route.',
 'reuters':'Catalogus gebruikt een RapidAPI-route; geen directe gratis Reuters-feed.',
 'sirenic':'Betaalde verrijkingsacties; prijs en dekking per actie.',
 'stableenrich':'Betaalde verrijkingsacties; prijs en dekking per actie.',
 'tavily':'Betaalde productiecapaciteit; gratis credits zijn begrensd en geen blijvende nulgarantie.',
 'webbersites':'Betaalde uitvoering; eventueel gratis board/discovery is geen gratis uitvoeringsdienst.',
 'it-infocamere':'Commerciële overeenkomst en producttarieven.',
 'es-registro-mercantil':'Uittreksels/informatieproducten hebben tarieven; machinecontract apart nodig.',
 'pt-irn':'Certificaten en databaseprotocol hebben product-/contractkosten.',
 'at-firmenbuch':'Registerquerykosten en mogelijke toeslag van erkende distributeur.',
 'se-bolagsverket-information':'Reguliere company-information API heeft een betaald contract; gebruik apart HVD-product voor €0.',
 'mk-central-registry':'Betaalde/prepaid informatieproducten; machinetarief nog vastleggen.',
 'opencorporates':'Commercieel API-/hergebruikplan vereist; open-projecttoegang niet automatisch beschikbaar voor Apiosk.',
 'orbis':'Commercieel abonnement.', 'dnb-directplus':'Commerciële datablocks en contract.',
 'creditsafe-connect':'Commerciële rapport-/API-eenheden.', 'company-info':'Commercieel abonnement.',
 'graydoncreditsafe-legacy':'Bestaande klant-/legacy-overeenkomst nodig.',
 'sayari':'Commercieel API-plan.', 'north-data':'Commerciële API-/exportproducten.'}
unknown_notes = {
 'de-handelsregister':'Kosteloze portaalinzage is geen bevestigde vrij herbruikbare machine-API. Automatiseer niet op basis van een gevonden intern endpoint.',
 'de-unternehmensregister':'Open inzage en documentkosten verschillen; publieke machine-toegang/contract niet bevestigd.',
 'de-bundesanzeiger':'Publicatieportal; machinegebruik en productkosten niet bevestigd.',
 'de-insolvency-notices':'Zoekportal voor insolventie; hergebruik, bewaartermijnen en toegestane machine-interface vastleggen.',
 'dk-cvr':'Systeem-toegang via CVR/Elasticsearch bestaat; exact gratis commercieel toegangscontract niet bevestigd. UBO heeft aparte toegangsregels.',
 'lt-registru-centras':'Open JAR-datasets bestaan, maar exact geselecteerde bedrijfsdataset, API-licentie en commerciële productkosten nog afbakenen.',
 'cz-commercial-register':'Rechtbankportal staat los van gratis ARES; directe machinecontracten/documenttoegang nog niet bevestigd.',
 'sk-commercial-register':'ORSR-portal staat los van RÚZ en RPO; machinecontract niet bevestigd.',
 'sk-rpo':'Publieke RPO-gegevens bestaan; na overdracht in juni 2026 actuele productiehost en toegangsvoorwaarden bevestigen. Testhost niet als productie activeren.',
 'ro-onrc':'ONRC publiceert open CSV-snapshots; actuele snapshot van 2 september 2026 gevonden. Datasetlicentie nog exact vastleggen; geen gratis live-API geclaimd.',
 'si-ajpes':'Gratis ePRS-portaal is geen gratis machinecontract. Gebruik actuele REST-productvoorwaarden; oude SOAP is vervallen.',
 'li-handelsregister':'Gedeeltelijk portaaluittreksel gratis; volledig gecertificeerd uittreksel betaald. Gratis API niet bevestigd.',
 'mt-mbr':'API-pakketten en HVD-eligibility/tarieven niet voldoende bevestigd.',
 'is-skatturinn':'Actueel API-product en tarief verifiëren; oude v2 niet als actuele v2.1 activeren.',
 'bris':'Vrije basisinzage via EU-portaal; nationale documenten kunnen betaald zijn. Geen algemeen publiek machinecontract bevestigd.',
 'boris':'Toegang tot UBO-gegevens is afhankelijk van land en bevoegdheid; niet als openbare gratis API behandelen.',
 'occrp-aleph':'Software is open source; toegang en hergebruik van collecties verschillen. Geen gratis commerciële bulklicentie bevestigd.'}
extra_evidence = {
 'ro-onrc':['https://data.gov.ro/organization/onrc'],
 'dk-cvr':['https://erhvervsstyrelsen.dk/det-centrale-virksomhedsregister-cvr','https://erhvervsstyrelsen.dk/kom-godt-igang-med-elasticSearch']}

services_by_group = {}
for s in live['services']:
    group = s['provider_handle'] if s['provider_handle'] in ('pulsenetwork','coinbase-bazaar') else s['slug']
    services_by_group.setdefault(group, []).append(s)

rows = []
for record in old['records']:
    sid = record['id']; spec = specs.get(sid, {}); services = services_by_group.get(sid, [])
    urls = [r['url'] for r in spec.get('references',[])]
    urls += [s['docs_url'] or s['origin_url'] for s in services if s.get('docs_url') or s.get('origin_url')]
    if sid not in A:
        if sid in commercial:
            assess(sid,'commercial','',commercial[sid], '|'.join(dict.fromkeys(urls)),spec.get('access',{}).get('mode','API / x402'))
        else:
            note = unknown_notes.get(sid,'Autoritatieve registerbron gevonden; gratis commerciële machine-toegang, voorwaarden en eventueel tarief nog niet bevestigd. Portaalinzage telt niet als werkende Apiosk-API.')
            assess(sid,'unknown','',note,'|'.join(dict.fromkeys(extra_evidence.get(sid,[])+urls)), spec.get('access',{}).get('mode','Te bevestigen'))
    a = A[sid]
    endpoints = [e for s in services for e in s['endpoints']]
    code = a['cost_code']
    label = {'zero':'€0 — genoemde scope','mixed':'€0-deel + overige producten','commercial':'Commercieel / geen €0-garantie','unknown':'Onbekend voor machinegebruik','reference':'€0 — alleen referentie','access_only':'Publieke toegang; rechten open'}[code]
    countries = spec.get('country_codes',record.get('country_codes',[]))
    if not countries and sid in ('rdw','bodemloket','cbs-statline','data-overheid-nl','koop','pdok','pdok-ruimtelijke-plannen','rvo','tenderned','overheid-io'): countries=['NL']
    if not countries and sid=='ask-merlin': countries=['GB']
    region = ', '.join(countries) or ('EU' if sid in ('vies','eurostat','ted','eu-funding-tenders','eurlex-cellar','bris','boris') else 'Internationaal')
    row = dict(id=sid,name=record['name'],group=groups[record['primary_niche']],country=region,country_codes=countries,
       status='Bestaande catalogus' if services else 'Concept', cost_status=label,
       zero_cost_scope_confirmed=code in ('zero','mixed'), evidence_level='Documentatie / lokale adapter' if code in ('zero','mixed','access_only','reference') else 'Catalogus / productspecificatie; tarief of recht niet volledig bevestigd',
       **a, service_count=len(services),endpoint_count=len(endpoints),enabled_endpoints=sum(bool(e['active']) for e in endpoints),
       contracted_endpoints=sum(any(c['status']=='active' for c in e['contracts']) for e in endpoints),
       description='; '.join(s.get('description','') for s in record.get('details',[]) if s.get('description')) or spec.get('coverage',{}),
       authentication=spec.get('access',{}).get('authentication','Volgens de bestaande provider-/adapterconfiguratie'),
       proposed_actions=spec.get('proposed_capabilities',[]), researched_endpoints=spec.get('endpoints',[]),
       research_references=spec.get('references',[]), required_work=spec.get('required_work',[]),
       checked_at='2026-09-30', fresh_execution_tested=False)
    if sid=='kvk-dutch-business-register':
        row['ignored_monthly_fee']={'actual_eur':6.40,'comparison_eur':0,'reason':'Explicit user instruction; applies only to this comparison, not provider billing.'}
    rows.append(row)

by_id={r['id']:r for r in rows}
endpoint_rows=[]
for group, services in services_by_group.items():
    source=by_id[group]
    for svc in services:
        for ep in svc['endpoints']:
            cost=source['cost_code']; note=source['conditions']
            if group=='kvk-dutch-business-register':
                cost='zero' if '/zoeken' in ep['path'] or '/hvds/' in ep['path'] else 'commercial'
                note=('€0 per aanvraag; maandfee genegeerd volgens instructie.' if '/zoeken' in ep['path'] else
                      'Gratis HVD-subset; 1/minuut/IP, CC BY 4.0 en beperkte velden.' if '/hvds/' in ep['path'] else
                      'Betaalde KVK-aanvraag; uitgesloten van €0-selectie.')
            control_only = ep.get('payment_required') is False and ep.get('catalog_price')==0 and cost=='commercial'
            if control_only:
                cost='catalog_zero_unverified'; note='Catalogus vermeldt geen betaling: status, metadata, demo of ondersteunende actie. Upstream €0 niet opnieuw bij provider bevestigd; eventuele betaalde sessie/voorgaande actie blijft betaald.'
            endpoint_rows.append(dict(id=ep['id'],source_id=group,source=source['name'],service=svc['name'],service_slug=svc['slug'],
              country=source['country'],group=source['group'],method=ep['method'],path=ep['path'],description=ep.get('description'),
              enabled=bool(ep['active']),enabled_label='Aan' if ep['active'] else 'Uit',
              active_contracts=[c['capability'] for c in ep['contracts'] if c['status']=='active'],
              active_contract_count=sum(c['status']=='active' for c in ep['contracts']),contracts=ep['contracts'],action_type=ep.get('action_type'),
              upstream_cost_code=cost,upstream_cost_label={'zero':'€0','commercial':'Niet als €0 bevestigd','access_only':'Rechten nog bevestigen','catalog_zero_unverified':'€0 in catalogus; upstream onbevestigd'}.get(cost,cost),
              cost_note=note,zero_cost_scope_confirmed=cost=='zero',catalog_price=ep.get('catalog_price'),
              catalog_price_meaning='Apiosk/customer catalog price, NOT upstream fee',payment_required=ep.get('payment_required'),
              source_docs=svc.get('docs_url'),provider_url=svc.get('origin_url'),evidence_urls=source['evidence_urls'],fresh_execution_tested=False))

countries=[]
for c in research['countries']:
    rr=[by_id[i] for i in c['native_source_ids']]
    free=[r for r in rr if r['zero_cost_scope_confirmed']]
    countries.append(dict(id=c['country_code'],country=c['name'],region=c['region'],source_count=len(rr),
      sources='; '.join(r['name'] for r in rr),source_ids=c['native_source_ids'],
      free_sources='; '.join(r['name'] for r in free) or 'Geen bevestigde gratis machinebron in deze review',
      free_scope='; '.join(r['free_scope'] for r in free) or 'Tarief/toegang/rechten nog bevestigen',
      zero_cost_source_count=len(free),status='Gratis deel beschikbaar' if free else 'Niet als €0 bevestigd'))

free_rows=[dict(id=r['id'],name=r['name'],country=r['country'],group=r['group'],status=r['status'],access_kind=r['access_kind'],
               free_scope=r['free_scope'],conditions=r['conditions'],excluded_scope=r['excluded_scope'],evidence_urls=r['evidence_urls'])
           for r in rows if r['zero_cost_scope_confirmed']]
counts=dict(source_records=len(rows),existing_sources=sum(r['status']=='Bestaande catalogus' for r in rows),
  candidate_source_records=sum(r['status']=='Concept' for r in rows),researched_countries=len(countries),
  national_source_records=sum(c['source_count'] for c in countries),service_listings=len(live['services']),
  endpoints=len(endpoint_rows),enabled_endpoints=sum(e['enabled'] for e in endpoint_rows),
  endpoints_with_active_contract=sum(bool(e['active_contracts']) for e in endpoint_rows),active_contract_records=sum(e['active_contract_count'] for e in endpoint_rows),
  sources_with_zero_cost_scope=len(free_rows),existing_sources_with_zero_cost_scope=sum(r['status']=='Bestaande catalogus' for r in free_rows),
  candidate_sources_with_zero_cost_scope=sum(r['status']=='Concept' for r in free_rows),
  countries_with_zero_cost_national_source=sum(bool(c['zero_cost_source_count']) for c in countries),
  registered_zero_cost_endpoints=sum(e['zero_cost_scope_confirmed'] for e in endpoint_rows),
  enabled_zero_cost_endpoints=sum(e['enabled'] and e['zero_cost_scope_confirmed'] for e in endpoint_rows),
  enabled_contracted_zero_cost_endpoints=sum(e['enabled'] and e['zero_cost_scope_confirmed'] and bool(e['active_contracts']) for e in endpoint_rows),
  zero_price_supporting_catalog_endpoints=sum(e['upstream_cost_code']=='catalog_zero_unverified' for e in endpoint_rows),
  cost_classes=dict(Counter(r['cost_code'] for r in rows)))
policy={'currency':'EUR','meaning':'Zero upstream marginal source fee for the explicitly listed scope, including commercial-use conditions; excludes hosting, storage, indexing, compute/AI, maintenance, VAT and Apiosk customer prices.',
 'kvk_monthly_fee_ignored_eur':6.40,'free_tiers':'Trials, temporary credits, demos and non-commercial exemptions do not count as permanent production €0.',
 'status_boundary':'Catalog registration / enabled flag / active execution contract are different. None establishes a fresh successful execution in this audit.',
 'unknown':'Unknown means no confirmed zero-cost commercial machine-use path in this review, not proof that none exists.',
 'counting':'42 countries contain 57 national product records. 76 research records overlap 5 existing sources, giving 37 + 71 = 108 distinct source/product records. References and datasets are not all executable APIs.',
 'discovery_channels':'Coinbase Bazaar is a marketplace/discovery channel, not one free company API. StableEnrich and Pulse are separate source groups; downstream prices apply.'}
inventory={'as_of':'2026-09-30','policy':policy,'counts':counts,'sources':rows,'countries':countries,'endpoints':endpoint_rows,
 'separate_discovery_channel':old['separate_discovery_channel'],'additional_backlog_not_counted':old['further_research_families_not_counted']}
def save(name,data): (ROOT/name).write_text(json.dumps(data,ensure_ascii=False,indent=2)+'\n')
save('inventory.json',inventory)
save('free-operations.json',{'as_of':'2026-09-30','policy':policy,'sources':free_rows,'registered_endpoints':[e for e in endpoint_rows if e['zero_cost_scope_confirmed']]})
save('assessments.json',A)

snapshot=json.loads((ROOT/'report/src/data.json').read_text())
snapshot.update(surface='report',title='Apiosk — alle bronnen en kosten',status='reviewed',buildStatus='creating',report={'asOf':'2026-09-30'},filters=[])
base_files=[str(DOCS/'source-niches-20260930/complete-overview.json'),str(DOCS/'native-company-sources/catalog.json'),str(ROOT/'live-catalog.json'),str(ROOT/'assessments.json')]
def query(label,rr,detail):
 return {'label':label,'rows':rr,'source':{'label':label,'files':base_files,'sql':live['sql'],
   'tables':['providers.apis','providers.api_endpoints','providers.provider_profiles','catalog.capability_execution_contracts'],
   'filters':['Active, verified, discoverable source listings; all their enabled and disabled endpoints retained.'],
   'caveats':[policy['meaning'],policy['status_boundary'],policy['unknown'],detail],
   'evidenceFlow':'Read-only production snapshot grouped by provider; merged by source ID with country specifications and operation-scoped pricing documentation. URLs and per-row evidence retained.'}}
snapshot['queries']={
 'source_inventory':query('108 bron- en productrecords',rows,policy['counting']),
 'free_sources':query('Bronnen met bevestigde €0-scope',free_rows,'Gratis geldt alleen voor free_scope. Reference-only records en onbevestigde hergebruiksrechten zijn uitgesloten.'),
 'endpoint_inventory':query('Alle geregistreerde endpoints',endpoint_rows,'Upstream cost is inferred only for the bounded official/public or local adapter; catalog prices are kept separately. No purchase performed.'),
 'countries':query('Alle 42 onderzochte landen',countries,'Alleen native_source_ids; internationale bronnen zijn niet per land opnieuw meegeteld.'),
 'inventory_totals':query('Gecontroleerde tellingen',[dict(id='all',**{k:v for k,v in counts.items() if k!='cost_classes'})],'Derived exact counts; source records and endpoint records use different denominators.')}
snapshot['audit_policy']=policy
save('reviewed.json',snapshot)
(ROOT/'report/src/data.json').write_text(json.dumps(snapshot,ensure_ascii=False,indent=2)+'\n')

lines=['APIOSK BRONNEN- EN KOSTENOVERZICHT — 30 september 2026','',policy['meaning'],policy['counting'],'KVK: €6,40 maandfee telt op uitdrukkelijk verzoek als €0 in deze vergelijking.','',json.dumps(counts,ensure_ascii=False,indent=2),'','ALLE 108 BRON-/PRODUCTRECORDS','']
for r in rows:
 lines += [f"{r['name']} [{r['id']}] — {r['country']} — {r['group']}",f"  {r['status']} | {r['cost_status']} | {r['access_kind']}",f"  Gratis: {r['free_scope'] or 'Niet bevestigd'}",f"  Voorwaarden: {r['conditions']}"]
 if r['excluded_scope']:lines.append('  Buiten gratis scope: '+r['excluded_scope'])
 lines += [f"  Catalogus: {r['endpoint_count']} endpoints, {r['enabled_endpoints']} aan, {r['contracted_endpoints']} met actief contract.", '  Conceptacties: '+', '.join(a['public_name'] for a in r['proposed_actions']), '  Bronnen: '+' | '.join(r['evidence_urls']),'']
lines+=['ALLE 42 LANDEN','']
for c in countries:lines += [f"{c['id']} — {c['country']}",c['sources'],'Gratis: '+c['free_sources'],'']
lines+=['ALLE ENDPOINTS','Volledige methode, pad, status, prijsveld, contracten en kostenscope: inventory.json → endpoints.','',policy['status_boundary'],policy['unknown']]
(ROOT/'volledig-overzicht.txt').write_text('\n'.join(lines)+'\n')

assert len(rows)==108 and len({r['id'] for r in rows})==108
assert len(countries)==42 and counts['existing_sources']==37 and counts['candidate_source_records']==71
assert len(endpoint_rows)==1524 and len({e['id'] for e in endpoint_rows})==1524
assert all(r['evidence_urls'] and r['free_scope'] and r['conditions'] for r in rows if r['zero_cost_scope_confirmed'])
assert not by_id['opensanctions']['zero_cost_scope_confirmed']
assert by_id['kvk-dutch-business-register']['ignored_monthly_fee']['comparison_eur']==0
assert len([e for e in endpoint_rows if e['source_id']=='kvk-dutch-business-register' and e['zero_cost_scope_confirmed']])==3
assert all(not r['fresh_execution_tested'] for r in rows)
save('validation.json',{'status':'passed','checked_at':'2026-09-30','checks':['108 unique sources','37 existing + 71 candidate','42 countries','1524 unique endpoints preserved','Every €0 scope has evidence and conditions','KVK monthly exception only','OpenSanctions not free for commercial use','No execution claimed'],'counts':counts})
print(json.dumps(counts,ensure_ascii=False,indent=2))
