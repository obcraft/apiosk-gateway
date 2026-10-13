Nederlandse regelingen en toeslagen — bronregistratie, 13 oktober 2026

Actuele catalogusstatus: migratie 20261013182516_brand_nl_benefits_sources is
toegepast. Vier bronnen (Nibud uitgezonderd) zijn zichtbaar met logo en elf
endpoints. Browse-only; uitvoering staat uit. Zie README.runtime.txt en de
catalogue-production-readback.json voor de actuele verificatie. De onderstaande
inactive-statussen beschrijven het oorspronkelijke registratiemoment.

Vervolg: de gemeentelijke crawler en vier toeslagberekeningen zijn inmiddels
gebouwd als aparte Apiosk-API's. Zie README.runtime.txt en runtime.openapi.json
voor implementatie, tests, grenzen en huidige activatiestatus. Nibud is op
verzoek uitgesteld. Hieronder staat het eerdere bronregistratiemoment;
inventory.json beschrijft uitsluitend die oorspronkelijke registratie.

Toegepast op apiosk-production (jgjoiyqdyypouskftzeq).
Migratie: 20261013172106_onboard_nl_benefits_sources.
Dezelfde migratie staat in gateway/supabase/migrations en app/supabase/migrations.

GEREGISTREERD

Samenwerkende Catalogi: 1 GET /sru endpoint, nl.municipal.services.search.
Dienst Toeslagen: 5 GET HTML-endpoints voor zorgtoeslag, huurtoeslag,
kinderopvangtoeslag, kindgebonden budget en de zorgtoeslagtabellen.
Nibud: 5 endpoints: Uitgaven v6 Referentiebedragen (POST), Gemeentes (GET),
Versie (GET), pensioenuitgaven Resultaat (POST), Afloscapaciteit Resultaat (POST).

Elke nieuwe endpoint heeft categorie, tags, zoektermen, use cases, sectoren,
input/output entities, taakomschrijving, documentatie, authenticatievereisten,
een capability, fact vocabulary, draft execution contract en discovery offer.
De drie nieuwe bronnen zijn inactive, discoverable=false en is_verified=false.
Alle 11 nieuwe endpoints zijn inactive; alle 11 contracts en capabilities draft.
Prijs 0 is uitsluitend een verplichte databaseplaceholder bij deze inactieve
registraties; de productieprijs en eventuele upstreamkosten zijn niet vastgesteld.

BESTAANDE BRONNEN

KOOP was al actief met twee endpoints en vijf capabilities:
- government.publication.search
- government.publication.detail
- government.staatscourant.search
- government.bwb.search
- government.cvdr.search

Deze endpoints kregen aanvullende tags, zoektermen, categorie en beschrijvingen.
Bestaande uitvoercontracten, actieve status en prijs (USD 0.085066) zijn behouden.
De voorstellen nl.legal.search en nl.municipal.regulations verwijzen in metadata
naar de bestaande BWB/CVDR-capabilities; geen dubbele uitvoerbare capabilities.

NOG NIET UITVOERBAAR

Samenwerkende Catalogi: XML-normalisatie, vaste collectieafbakening, prijsreview
en een volledige Apiosk-uitvoertest ontbreken. Eén gratis upstream-probe leverde
736 zoekresultaten met 2 records op; dat bewijst geen Apiosk-uitvoering.

Dienst Toeslagen: HTML-extractie, versie/jaarbewaking, prijsreview en uitvoering
moeten nog worden geverifieerd. De pagina's zijn geen reken-API.
Er is geen nl.benefits.eligibility-capability als uitvoerbare dienst geregistreerd.

Nibud: subscription key, commerciële prijs, doorleveringslicentie, private
credential binding en live response-validatie ontbreken. Geen account gestart,
geen mail verstuurd, geen betaalde calls gedaan en geen sleutels opgeslagen.
De publieke schemas verschillen van voorbeelden qua casing, numerieke types
en bij Uitgaven de ongedefinieerde verplichte bouwenergielabel-property.
De bewaarde schemas moeten vóór activering met Nibud worden uitgeklaard.
De pensioeninvoer is uit de operatietabel overgenomen; volledige schema-
conformiteit, inclusief verplichte top-level arrays, is nog niet geverifieerd.

Gemeentelijke websites: de aanvullende connectorvereisten en onderwerpen staan
bij Samenwerkende Catalogi. Dit is een landelijke ingang naar officiële links,
geen reeds geïmplementeerde crawler voor alle gemeentelijke HTML/PDF-pagina's.
De KOOP-eventfeed is als documentatie vastgelegd; synchronisatie is niet gebouwd.

VERIFICATIE

evidence/probes.json: URLs, HTTP-status, responsehashes en tijdstip van zes
gratis KOOP-requests. BWB, CVDR, officiële publicaties en Samenwerkende Catalogi
gaven geldige SRU XML zonder diagnostics en ieder twee records terug.
evidence/*-explain.xml: ondersteunde BWB/CVDR-zoekvelden.
evidence/nibud-uitgaven-v6.openapi.json: officiële OpenAPI-export.
evidence/production-readback.json: productie-readback per endpoint/capability.

De live tabelkolommen zijn vergeleken met de testfixture: geen afwijkingen.
node docs/source-onboarding/nl-benefits/test.mjs controleert in PGlite de echte
productiekolommen/constraints en relevante triggers, 3 bronnen/11 endpoints,
metadata, inactive/draft-status, behoud van KOOP-status/prijs, dubbele import en
atomische rollback. De Rust check_source_bundle-check accepteert alle 11
draft-contractdefinities. Er zijn geen nieuwe live extraction samples of
Apiosk buyer-flow-tests; dit is registratiebewijs, geen activeringsbewijs.

build.mjs genereert de drie manifests en de migratie deterministisch.
Een herhaalde productie-import stopt op bestaande bron-slugs; niet blind herhalen.
Aanpassingen na deze toegepaste migratie horen in een nieuwe migratie.

Primaire documentatie:
https://data.overheid.nl/sites/default/files/dataset/d0cca537-44ea-48cf-9880-fa21e1a7058f/resources/HandleidingSRU2.0.pdf
https://developer.nibud.nl/products2
https://developer.nibud.nl/getting-started
https://developer.nibud.nl/technical-uitgaven
https://developer.nibud.nl/technical-pensioen-uitgaven
https://developer.nibud.nl/technical-afloscapaciteit
https://www.belastingdienst.nl/wps/wcm/connect/nl/toeslagen/toeslagen
