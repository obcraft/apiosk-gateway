# Europese data: actuele stand en uitvoeringsplan

Peildatum: 23 september 2026, circa 17:32–17:40 Amsterdam. Dit is een product- en implementatieaudit, geen uitrol. Er zijn geen nieuwe betaalde broncalls gedaan.

## Besluit

Apiosk heeft al een bruikbare execution/access-laag: bronselectie, getypeerde contracten, afhankelijkheden, prijsplafond, goedkeuring, backenduitvoering, opgeslagen resultaten en bronverwijzingen. De Europese positionering vraagt geen nieuwe productarchitectuur.

Niet alle geregistreerde bronnen of operaties zijn bewezen bruikbaar. Bovendien is toegang tot brondata verder ontwikkeld dan een uniforme, brononafhankelijke Europese capability-API. De eerste partner-PoC moet daarom drie afgebakende capabilities aanbieden: bedrijfsidentiteit, screening en beschikbare filings. Tendersearch volgt; gunningen en werkelijk betaalde overheidsuitgaven vragen afzonderlijk werk.

Start met Nederland, het VK en Frankrijk voor de relevante bedrijfsoperaties. Dat is Europese dekking, geen EU-brede dekking. Andere landen worden per capability toegevoegd na verificatie. Sirenic levert veel verdere Europese bouwstenen, maar een profiel in een land bewijst niet automatisch beschikbaarheid van jaarrekeningen, bestuurders en UBO's in dat land.

## Wat live is vastgesteld

De [geschoonde momentopname](2026-09-23-european-layer-status.json) bevat aantallen, repo-commits en bronstatussen. Onderliggende metadata en readiness staan lokaal in `output/european-layer-audit-20260923/`. De catalogusaudit gebruikt uitsluitend leesverzoeken; klantgegevens en sleutels zijn niet in dit rapport opgenomen.

| Catalogusbreed, niet alleen Europa | Actueel |
| --- | ---: |
| Gegroepeerde bronnen | 37 |
| Diensten/API-listings | 112 |
| Actieve geregistreerde endpoints | 1.476 |
| Endpoints met actief gevalideerd leescontract | 435 |
| Endpoints met staged contract, exclusief actieve overlap | 783 |
| Endpoints zonder een van beide | 258 |
| Runtime toegelaten endpoints | 1.218 |
| Daarvan getypeerde, met voorbeeld gecontroleerde endpoints | 271 |
| Daarvan generieke documentopvragingen | 947 |

De 1.218 toegelaten endpoints zijn **geen 1.218 bewezen live integraties**. De nieuwste runtime laat ook beoordeelde `source-document-v1`-contracten toe die nog op hun eerste echte respons wachten. De database markeert ze als `validated`; de runtime als `unverified`. Een bruikbaar document kan het contract promoveren. Dat bewijst documentopvraging, niet automatisch correcte bedrijfsfeiten of financiële normalisatie. De buyer-API weigert automatische Ask-uitvoering bij zulke stappen en geeft een plan ter beoordeling terug.

De eerdere audit van vandaag met 271 actieve endpoints is achterhaald: inmiddels zijn er 435. Ook de eerdere aanname dat elk staged contract buiten de planner blijft, geldt niet meer.

De gateway meldt runtime `351f8cc`, met payment executor ready. Gateway HEAD `897bfa5` voegt uitsluitend de tweede SQL-promotiebatch toe; de productie-migratiehistorie bevat deze batch (`20260923190000`). Dit verschil is dus geen bewijs van ontbrekende runtimecode. De MCP-health geeft vijf tools. App, MCP, website, docs en gateway zijn afzonderlijke Git-roots; hun werkbomen waren bij aanvang schoon.

## Welke Europese bronnen kunnen we aanbieden?

Onderstaande dekking betreft de **geregistreerde** endpoints, niet de volledige API van de leverancier. Uitvoeringen komen uit het openbare readiness-rapport over de laatste 30 dagen, gegroepeerd per bron. Succes voor een bron bewijst niet dat ieder endpoint of ieder land is getest. Mislukkingen zijn historische tellingen, geen bewijs dat de bron nu permanent stuk is.

| Bron | Runtime endpoints | Succes / mislukt / onopgelost | Beoordeling |
| --- | ---: | ---: | --- |
| KVK | 5/5 | 53 / 4 / 2 | Sterke NL-basis; bestaande fouten en wachters onderzoeken. Jaarrekeningdekking en koude-cachecapaciteit expliciet begrenzen. |
| Companies House | 15/15 | 26 / 0 / 0 | Sterke VK-basis; vijf getypeerde endpoints, tien documentopvragingen, waarvan vier nog staged. Filing history is geen download van alle documenten. |
| Sirenic | 63/63 | 11 / 1 / 0 | Grootste bestaande Europese uitbreiding. 66 actuele contractvarianten; land- en dienstspecifieke dekking behouden. |
| GLEIF | 5/17 | 0 / 0 / 0 | LEI en relaties zijn getypeerd aanwezig; recente v2-uitvoering niet gemeten. Geen volledig handelsregister. |
| OpenSanctions | 7/7 | 9 / 1 / 0 | Vier getypeerde endpoints, drie documentopvragingen. Screeningresultaten zijn kandidaten, geen automatische compliancebeslissing. |
| VIES | 3/3 | 4 / 0 / 1 | EU-btwvalidatie aanwezig; beschikbaarheid en ontbrekende nationale identiteitsvelden expliciet houden. |
| TenderNed | 1/3 | 6 / 1 / 1 | Publicatiezoekopdracht met filters aanwezig; nog geen uniforme award-capability. |
| TED | 1/1 | 1 / 0 / 0 | Alleen KVK/RSIN-gerelateerde notice search; geen algemene Europese CPV/land/keyword search. |
| RVO | 1/1 | 0 / 0 / 1 | Eerste-pagina-subsidiezoekopdracht; geen recente geslaagde v2-uitvoering aangetoond. |
| EU Funding & Tenders | 1/1 | 1 / 0 / 1 | Calls zoeken; geen bewijs van toegekende subsidie of uitgekeerd geld. |
| CBS | 17/23 | 6 / 3 / 0 | Specifieke datasets en dimensies; geen universele statistiekadapter. |
| Eurostat | 2/10 | 0 / 0 / 0 | Datasetzoekopdracht en data-opvraging aanwezig; recent niet gemeten. |
| Data.overheid.nl | 3/3 | 0 / 0 / 0 | Catalogusmetadata en links; niet automatisch de data achter iedere dataset. |
| KOOP | 2/2 | 3 / 0 / 0 | Vijf contractvarianten voor publicaties/regelgeving; publicatie is niet hetzelfde als verleende vergunning. |
| EUR-Lex Cellar | 2/2 | 0 / 0 / 0 | Metadata en begrensde query; geen universele levering van volledige juridische documenten. |
| PDOK | 19/27 | 12 / 1 / 0 | Adres/BAG/geodata-bouwstenen; dekkingsgrenzen per dataset. |
| PDOK Ruimtelijke Plannen | 1/1 | 1 / 0 / 0 | Kaartinformatie; geen automatisch oordeel over actuele bouwrechten. |
| Bodemloket | 1/1 | 1 / 0 / 0 | Bodemkaartinformatie; geen volledig bodemonderzoek. |
| Overheid.io | 10/10 | 2 / 0 / 0 | Tussenleverancier van NL-data; herkomst van oorspronkelijke registers zichtbaar houden. |
| RDW | 1/1 | 1 / 0 / 0 | Getypeerd voertuigprofiel. |
| EODHD | 0/60 | 0 / 0 / 0 | Geblokkeerd: wederverkoopautorisatie niet geregistreerd. Niet aanbieden als koopbare bron. |

Sirenic is aanzienlijk verder dan een catalogusvermelding. De bestaande, tijdens deze audit gelezen bestanden van 13 september bevatten 65 contractbewijzen en een uitbreidingscampagne met 59 betaalde controles en bijbehorende verificatieregistratie van receipts, voor 2,586 USDC. Dat is historisch treasurybewijs; geen nieuwe upstreamcontrole of nieuwe on-chain verificatie vandaag. De twee toen mislukte routes bleven uitgesloten. Zie [Sirenic-integratie](../sirenic.md).

## Stand van de zes gewenste capabilities

| Gewenst contract | Bestaande bouwstenen | Ontbrekend voor een betrouwbare partner-API |
| --- | --- | --- |
| `resolve_company` | KVK search/profile, Companies House search/profile, Sirenic EU/FR search/profile, GLEIF. Deterministische kandidaatselectie en keuze bij ambiguïteit. | Eén versieerbaar bedrijfsobject, identifier-scheme per land, expliciete landendekking, herleidbare matchregels en een duurzame API-route voor kandidaatkeuze. |
| `find_tenders` | TenderNed-publicaties; workflow `tender-finder` combineert TenderNed, Linkup en optioneel KVK. | Algemene TED-search; uniforme filters, status en paging; deduplicatie van dezelfde TenderNed/TED-publicatie. Webzoekresultaten mogen niet de enige grond voor officiële tenderclaims zijn. |
| `get_tender_awards` | TED buyer/winner-vermeldingen en Sirenic FR/EU-procurementroutes. | Award- en lotmodel; expliciete winner/buyer-rollen; identifier-verificatie; bedragen/valuta/datum; correcties en annuleringen. |
| `get_company_filings` | KVK financial statements, Companies House filing history, Sirenic FR-financials en landgebonden accounts/documentroutes. | Eén filingschema dat metadata, gedownload document en geëxtraheerde cijfers onderscheidt; periodekeuze, paginering, documentselectie en volledigheidsstatus. |
| `screen_entity` | OpenSanctions, VIES, onboardingworkflows, UK-directorscreening. | Uniforme entity-input, dataset/tijdstip, score en matchgronden, onderscheid sanctions/PEP/andere topics, expliciete reviewstatus. |
| `get_public_spending` | Procurementvermeldingen, subsidieprogramma's en statistieken als aangrenzende data. | Nog geen aangetroffen end-to-end capability voor daadwerkelijke overheidstransacties. Eerst één concrete transactiedataset kiezen en betaler, begunstigde, bedrag, datum en herkomst normaliseren. |

Deze zes namen zijn de voorgestelde publieke productcontracten; ze bestaan nog niet als die zes uniforme API/MCP-functies. Er bestaan wel al echte semantische capabilities, zoals `company.registry-profile`, `company.filing-history` en `company.sanctions-screen`. Veel uitbreiding van vandaag bestaat daarnaast uit brongebonden `operation.*`-contracten. Die mogen niet gelijkgesteld worden aan verdere semantische normalisatie.

Een concrete TED-beperking staat in `legacy-executor/src/proxy/ted_links.rs`: alleen acht- of negencijferige KVK/RSIN-input, zoekvraag op buyer/winner-identifier, twintig resultaten per pagina. De adapter bevestigt uitdrukkelijk niet het identifierschema of de juridische entiteit. Er worden geen awardbedragen opgevraagd. Hiermee kan `get_tender_awards` dus nog niet eerlijk als afgeronde capability worden verkocht.

## Bestaande architectuur behouden

| Repo | Al aanwezig | Gericht vervolg |
| --- | --- | --- |
| Gateway | Contractcatalogus, getypeerde feiten, dependency planner, entity selection, één cap/approval, opgeslagen uitvoering/resultaten, publieke buyer-API. | Publieke capabilitycontracten boven dezelfde engine; landenmatrix, operationele bewijsstatus, paging en semantische normalisatie. |
| App | Gedeelde source-directory, gepagineerde endpointkeuze, quote/approval, resultaten, elf workflows inclusief de aparte supplier-payment-workflow. | Capabilities als ingang; landen/dekking/bronbewijs zichtbaar. Onderscheid genormaliseerd resultaat, brondocument en ongeteste operatie. Bestaande routes/layout behouden. |
| MCP | Vijf tools voor sources, discover, execute, status en card approval; gedeelde gateway. | Zes capabilitynamen als duidelijke ingang/metadata naar dezelfde engine. Alleen extra tools toevoegen als de partner dat nodig heeft; geen aparte planner of betaalroute. |
| Docs | API-sleutels, source-execution, Ask, budgetten, idempotency, opgeslagen runs. | Land/capability-matrix, getypeerde schema's, partnerquickstart, limieten en voorbeelden met echte resultaten. |
| Website | Bestaande bronpresentatie. | Pas na acceptatie precieze claim publiceren: welke capabilities, landen en garanties werkelijk geleverd worden. |

Een partner kan de bestaande API gebruiken: bronnen lezen, een bronoperatie uitvoeren of `/v2/ask` plannen en de opgeslagen Ask uitvoeren. Directe uitvoering en Ask delen de engine en het ledger. Voor backendgebruik kan een expliciet budgetmandaat dienen als autorisatie; er hoeft niet per upstream-call een nieuwe approval te komen.

Wel is er een relevante API-kloof: de buyer-HTTP-API documenteert nog geen voortzettingsactie voor `needs_selection`/`needs_input`, terwijl App/MCP die hebben. Voor een eerste integratie met bekende registry-ID's is dat te beperken. Voor volwaardige `resolve_company` moet de partner dezelfde opgeslagen taak kunnen voortzetten, vooral nadat een zoekstap al betaald is. Een nieuwe Ask mag geen automatische betaalde replay worden.

## Uitvoering in volgorde

De termijnen hieronder zijn planningsramingen voor één ervaren engineer met reviewcapaciteit, geen toezegging van al gebouwde functionaliteit. Productiepromotie is afhankelijk van de acceptatiecriteria, niet van de kalender.

### P0 — Bewijs en partnergrens, dag 1–2

1. Maak een expliciete release-allowlist per capability × land × operatie. Begin met KVK, Companies House, Sirenic FR, OpenSanctions en VIES. GLEIF is aanvullende LEI-data. Scheid direct register, tussenleverancier en afgeleide analyse.
2. Controleer per launch-operatie rechten op doorlevering aan een partner, attributie, caching, documentdistributie en retentie. Een werkend API-account of x402-betaling bewijst die rechten niet. Bestaande contract-/licentiebewijzen hergebruiken; ontbrekend bewijs blokkeert alleen die scope. EODHD blijft uitgesloten.
3. Herstel de drie falende `source_operations`-tests met representatieve bronfixtures en een expliciet apart staged/unverified-pad. Behoud de productiecontrole die synthetische voorbeelden weigert. Voeg acceptatie voor actieve en ongeteste contracten toe waar die werkelijk gedrag onderscheidt.
4. Diagnoseer de open uitvoeringen voor KVK, TenderNed, VIES, RVO en EU Funding. Bekijk bestaande tasks/logs en het ledger; geen blinde retry of nieuwe aankoop. Scheid oude meetresten van actuele operationele fouten.
5. Leg een kleine matrix van betaalde acceptatiecases en één totaalbudget ter beoordeling klaar. Fixtures en bestaande receipts zijn voorbereiding; een nieuwe aankoop vraagt een concrete cap. Deze audit koopt niets.

Acceptatie: elke launch-operatie heeft eigenaar, schema, landenbereik, rechtenstatus, recente uitvoerreferentie en expliciete beperkingen. Geen ongeteste operatie telt als partner-ready. De betrokken regressietests zijn groen.

### P1 — Drie uniforme capabilities, dag 3–5

Implementeer versieerbare contracten voor `resolve_company`, `screen_entity` en `get_company_filings`, op bestaande Catalog/Resolver/Engine en buyer-auth. Geen tweede executor, saldo of planner.

- Bedrijf: stabiele interne entity-reference; `{scheme, country, value}`-identifiers; legal name; land; adres; status; kandidaten en matchgronden. Geen verzonnen waarschijnlijkheidsscore.
- Filings: filing-ID, entiteit, type, periode, deponeringsdatum, oorspronkelijke URL; `metadata_only`, `document_available` of `facts_extracted`; paginering en ontbrekende perioden.
- Screening: gecontroleerde entiteit, datasets, retrieval-tijd, matchkandidaten en redenen; `review_required` en `coverage`. Nul matches is geen algemene veiligheidsgarantie.
- Gemeenschappelijke envelope: `schema_version`, `status`, `coverage`, `missing_fields`, bewijsverwijzingen met veldpointer, oorspronkelijke bron én tussenleverancier, `retrieved_at`, bronpublicatiedatum indien beschikbaar, charge en saved-run-reference.
- Voeg opgeslagen kandidaatselectie/clarification toe aan de buyer-API met bestaande autorisatie en idempotency. Bewaar de oorspronkelijke cap; herprijs alleen expliciet wanneer werkelijk nieuw werk wordt toegevoegd.

Acceptatie: NL, GB en FR cases voor alleen de beloofde capabilities; exacte en ambigue namen, geen resultaat, foutieve ID, ontbrekend verslagjaar en lege screening. Eén approval/cap; resultaat via App én partner-API; restart/recovery en herhaalde idempotency-key veroorzaken geen extra betaalde call. Live acceptatie betreft de complete keten en werkelijk resultaat, niet alleen een quote.

### P2 — Betrouwbare tendersearch, dag 6–8

Breid TED uit met algemene search op onderwerp/CPV, landen, publicatieperiode en notice-type, na controle van actuele officiële API-documentatie. Normaliseer samen met TenderNed naar één tenderobject. Geef buyer-ID, deadline met tijdzone, publicatiestatus, notice-ID/versie, lots en oorspronkelijke publicatie terug. Gebruik de bestaande prijs-/approval-laag voor begrensde paging.

Acceptatie: niet-lege en lege cases, meerdere pagina's, gewijzigde/geannuleerde publicatie, verlopen deadline en dubbel gepubliceerde TenderNed/TED-notice. Iedere bronclaim heeft een originele notice. Totale dekking en resterende pagina's worden expliciet gerapporteerd.

### P3 — Gunningen, dag 9–12

Implementeer `get_tender_awards` uit expliciete awardnotices. Verbind supplier-ID en identifierschema met `resolve_company`; behoud consortium/lotrelaties. Scheid aanbestedende dienst, winnaar en slechts genoemde partij. Bewaar oorspronkelijke valuta, waardetype en correcties.

Acceptatie: echte award met leverancier en bedrag; notice zonder bedrag; meerdere winnaars; ingetrokken of gecorrigeerde gunning; gelijk nummer in verschillend identifierschema. Geen noticehit wordt automatisch een gunning en geen contractwaarde wordt uitgegeven geld genoemd.

### P4 — Landenuitbreiding en public spending, daarna

Breid Sirenic-landen per capability uit op basis van gedocumenteerde contracten en echte bewijsgevallen. Nationaliteitsdekking van company search is onvoldoende voor filing- of ownershipclaims. Geef per land een lijst met ondersteunde en ontbrekende functies.

Voor `get_public_spending`: kies eerst één concrete publieke transactiedataset in één jurisdictie, controleer rechten en data-eigenschappen, en bouw een herhaalbare import/query. Behandel subsidies toegekend, contractwaarden en werkelijk betaalde transacties als afzonderlijke feiten. Pas na dekking, correcties en deduplicatie naar meerdere datasets uitbreiden. Hiervoor is nog geen onderbouwde leverdatum te geven.

## Eerste partner-PoC

Een zelfstandig bruikbaar pakket bevat drie capabilities, een kleine landenmatrix, machineleesbare schema's, een organisatiesleutel met passend budget, voorbeeldrequests, opgeslagen echte resultaten, kosten en beperkingen. Gebruik bekende registratie-ID's voor de eerste demonstratie; demonstreer daarna expliciete kandidaatkeuze. Begin bijvoorbeeld met een NL-bedrijfsprofiel, een VK-filingoverzicht en een FR-profiel/financieel resultaat, plus een afzonderlijke screeningstap op dezelfde opgeloste entiteit.

Aanbod: “European company identity, screening and available filings through one priced execution interface, with source provenance and explicit country coverage.” Een brede belofte van alle Europese registers, alle jaarrekeningen, alle gunningen of alle publieke uitgaven is nog niet onderbouwd. De daadwerkelijke Alexandria-integratievorm moet met hun technische team worden vastgesteld; die is hier niet geverifieerd.

## Verificatie en resterende grenzen

- Live gelezen: productiecatalogus, publieke readiness en health, geselecteerde productiecontracten en migratiehistorie.
- Gateway: 26 gerichte tests geslaagd; drie environment-afhankelijke tests overgeslagen; drie `source_operations`-tests mislukt door synthetische fixtures zonder het juiste evidence/staging-model. Geen volledige suite gedraaid en geen release-regressie geclaimd.
- App: 56 gerichte directory-, source-, workflow- en resultatentabeltests geslaagd.
- MCP: 33 gerichte protocol-, approval-, recovery- en waardeformatteringtests geslaagd.
- De nieuwe browsercontext werd naar App-login gestuurd; authenticated App-gedrag is vandaag niet visueel opnieuw bewezen. Tests op fixtures vervangen dat niet.
- Historische Sirenic-treasurybewijzen en bronuitvoertellingen zijn gelezen; geen nieuwe providerpurchase, customer charge, live receipt-verificatie, deployment of bericht aan een partner uitgevoerd.

Belangrijkste codepointers: `src/catalog.sql`, `src/catalog.rs`, `src/readiness.rs`, `src/entity_resolution.rs`, `src/engine/results.rs`, `src/public_api.rs`, `src/workflow_catalog.json`, `legacy-executor/src/proxy/ted_links.rs`; in App `src/lib/domain/source-directory.ts` en `src/components/ask/gateway-v2-endpoints.tsx`; in MCP `src/gateway-v2.mjs`; in Docs `api/ask.mdx`, `api/execute.mdx` en `api/sources.mdx`.

## Voortgang na de audit

De buyer-API-voortzetting uit P1 is inmiddels lokaal geïmplementeerd en via HTTP/SQL/worker-integratietests gecontroleerd: kandidaat kiezen, dezelfde taak/quote/approval behouden en geen zoekopdracht opnieuw kopen. De nieuwe route is `/v2/asks/{id}/continue`; gestructureerde ontbrekende invoer wordt ook ondersteund. Nog niet uitgerold of met een live betaalde bron getest. Zie [implementatie en verificatie](../buyer-api-continuation-20260923.md). De productiestand hierboven blijft de oorspronkelijke auditmomentopname.


## Implemented follow-up: company and tender dossiers

Local implementation now spans Gateway, App, MCP and buyer documentation. `european-company-dossier` has 11 explicit country variants; `tender-company-dossier` covers NL/FR counterparties. Both use registry-returned names for screening, existing filings sources, one quote and one approval. Runtime country availability, payment-edge workflow pricing, shared structured buyer/MCP entry points and saved history are included. See [implementation, evidence and release order](../european-dossiers-20260923.md). No deployment or new paid production evidence is claimed. General TED discovery, typed awards and bounded multi-counterparty selection remain the next tender milestone.
