# Elf workflows: productiecontrole van begin tot einde
Datum: 21 september 2026

## Uitkomst

Alle elf workflows zijn met echte productieaanvragen doorlopen: **acht volledig uitgevoerd, drie met een gedeeltelijk antwoord door ontbrekende bronresultaten**. De tests vonden meerdere fouten die tijdens deze controle zijn opgelost, gecommit, gepusht en op productie gezet.

Een volledige uitvoering betekent hier: chatstart, ontbrekende gegevens verzamelen, passende offerte, één toestemming, de geselecteerde bronaanvragen, een onderbouwd antwoord en heropenen van de opgeslagen taak. Het betekent niet dat bronnen volledige dekking garanderen of dat bijvoorbeeld een sanctiecontrole een certificering is.

## Resultaten per workflow

| Workflow | Laatste taak | Bronaanvragen / leveranciers | Resultaat |
|---|---|---:|---|
| Supplier payment check | [Open](https://app.apiosk.com/gateway-v2?task=8dfe5a16-014b-41f8-aa45-7fcd28c2770d) | 6 / 5 | Volledig. Synthetische PDF lokaal uitgelezen; alle zes aanvragen geleverd; tien verdictregels. Rekeninghouder expliciet niet geverifieerd. |
| Find my next customers | [Open](https://app.apiosk.com/gateway-v2?task=888d3a7a-7b9b-48e9-ae2b-17714454dbb9) | 2 / 2 | Gedeeltelijk. Exa-kandidaten zichtbaar; personeelsomvang onbekend. StableEnrich bevestigt de betaling niet; geen heraankoop. |
| New customer onboarding | [Open](https://app.apiosk.com/gateway-v2?task=068c2e33-4319-4607-b646-27a17c2f77e2) | 4 / 3 | Volledig. Na alleen de bedrijfsnaam vroeg de AI uitsluitend resterende gegevens. Expliciet geweigerde btw-controle niet gekocht. |
| Competitor and pricing comparison | [Open](https://app.apiosk.com/gateway-v2?task=779e3a08-8c02-4b62-a697-b11261e07116) | 2 / 1 | Volledig. Prijzen per teamlid en facturatieperiode, introductiekorting en opzegvoorwaarden vergeleken. Onbekende Enterprise-prijzen en ontbrekende Slack-overeenkomst expliciet vermeld. Vernieuwde analyse en herladen geslaagd; enkele bronfragmenten blijven taalkundig onhandig. |
| Find relevant tenders | [Open](https://app.apiosk.com/gateway-v2?task=d135debe-bc5e-482d-9925-11ee5967f5bc) | 3 / 3 | Volledig. Concrete berichten met links; geen passend Amsterdam-resultaat expliciet vermeld. Historische aankondigingen niet als huidige open inschrijvingen voorgesteld. |
| Find potential subsidies | [Open](https://app.apiosk.com/gateway-v2?task=ead8f7f6-1b34-4379-8a80-4482df7361a0) | 2 / 2 | Volledig. Frankrijk-route gebruikt EU Funding & Tenders en Linkup; RVO is niet aangeroepen. France 2030, energiebesparingscertificaten en Ademe genoemd met bronnen. Open status, deadlines en toelating expliciet onbekend. Analyseherstel en herladen op de laatste release geslaagd. |
| Property pre-check | [Open](https://app.apiosk.com/gateway-v2?task=497a9ac5-f7ab-403e-934d-0627b4526d2a) | 6 / 4 | Volledig. Correct adres gekozen; BAG, plankaart, bodemsignalen en publicaties geleverd. Geen bodemsignaal niet als schone grond uitgelegd. |
| Who is behind this company? | [Open](https://app.apiosk.com/gateway-v2?task=fbe8087f-7344-4517-97a0-6f427731de78) | 4 / 2 | Gedeeltelijk. Drie Companies House-resultaten en deelanalyse beschikbaar. Uitkomst van de OpenSanctions-aanvraag onbekend; geen nieuw verzoek gestuurd. Geen GLEIF-aankoop zonder LEI. |
| Company financial health | [Open](https://app.apiosk.com/gateway-v2?task=528a8468-b7b1-4a95-affe-86ccc720caef) | 3 / 2 | Gedeeltelijk. KVK-profiel en jaarrekening geleverd, boekjaar 2020 expliciet genoemd. Reuters-zoekopdracht faalde en is terugbetaald. |
| Used business vehicle pre-check | [Open](https://app.apiosk.com/gateway-v2?task=5685efdc-7800-46d0-b8dd-87776672c5b8) | 4 / 4 | Volledig. RDW, verkoperregistratie, advertentiepagina en domein geleverd. Heropenen via Chat history en herladen van de opgeslagen URL gecontroleerd. |
| Company news and events | [Open](https://app.apiosk.com/gateway-v2?task=0f28c036-5659-464a-8003-26445da024af) | 4 / 2 | Volledig. CityFALCON en Reuters-resultaten voor Apple; insidertransacties expliciet uitgesloten. Herstel gebruikte de vier bestaande aanvragen. |

De aantallen zijn daadwerkelijk geselecteerde aanvragen in deze testgevallen, niet een verplicht aantal controles voor iedere workflow. De tests raakten achttien verschillende bronleveranciers. Andere landen, optionele takken en alle mogelijke combinaties van de zesendertig beschikbare bronnen zijn hiermee niet afzonderlijk gecertificeerd.

## Interface

- Elf workflows staan in één kolom. Alle elf rijen hadden dezelfde linkerpositie en breedte; elke rij had een verschillend SVG-icoon.
- Elke rij toont diensten/bronnen en uitklapbare stappen en controles.
- De workflowlinks openen Ask en starten een gesprek.
- Getest: ontbrekende waarden opvragen, bestaande waarden bewaren, optionele btw/LEI/RVO/insideraanvragen weglaten.
- Adresselectie gebruikt nu de generieke tekst “Which result do you mean?”.
- Opgeslagen taak-URL's herstellen de chat; het rootadres opent een nieuwe chat. Eerdere chats blijven via Chat history bereikbaar.

## Kosten en open uitkomsten

De gebruiker gaf expliciet toestemming voor maximaal **€5 totaal**.

- Som van alle goedgekeurde testplafonds, inclusief hertests en twee pogingen zonder bronuitvoering: **€4,267711**.
- Definitief afgeschreven: **3.020.534 USD micro**, ongeveer **€2,778891** op basis van de getoonde App-koers.
- StableEnrich: **180.000 USD micro**, in de App **€0,1656** als betaling in onderzoek.
- De afzonderlijke OpenSanctions-aanvraag bij de eigendomscheck heeft geen definitief resultaat of gekoppeld betaalbewijs. De uitkomst is niet als gratis, terugbetaald of geslaagd aangemerkt.
- De eerste VIES-fout en beide mislukte Reuters-zoekopdrachten voor Mollie zijn terugbetaald. VIES werkte bij de leveranciershertest; Reuters werkte voor Apple-nieuws.
- Herladen, bronherstel en opnieuw analyseren voegden geen bronaanvragen toe. De laatste afrekening bevestigt onveranderd 48 uitvoeringsrecords over alle testpogingen en dezelfde bedragen. Onzekere aanvragen zijn niet opnieuw gekocht.

De database registreert bedragen in USD micro; dat is niet hetzelfde als EUR micro. Het conservatieve EUR-plafond blijft de budgetgrens.

## Opgeloste fouten en wijzigingen

### Gateway-repository

Runtimewijzigingen:

| Commit | Wijziging |
|---|---|
| `2e768af` | Embedded databasepools begrensd; gezamenlijke applicatiecapaciteit van maximaal 78 naar 32 verbindingen bij een databasegrens van 60. |
| `fee4fb9` | JSON-modus voor analyse: de modelprovider weigerde het strikte JSON-schema. Bestaande bron- en waardevalidatie behouden. |
| `5950ad2` | Acceptatiefixture aangepast aan JSON-modus. |
| `a0fea40` | Herstel van betaalde, opgeslagen bronresultaten na offerteverval; uitvoeringscatalogus zonder directory-statistieken; alleen relevante bronbeperkingen. |
| `6992d9b` | Korte exacte citaten uit webteksten; compacte workflowhistorie; expliciet opnieuw analyseren zonder heraankoop. Een vergelijking van 864 KB veroorzaakte eerder een databasegroottefout. |
| `baf04a3` | Actuele contract- en prijscontrole beperkt tot de geselecteerde diensten; duidelijkere instructies voor prijsfragmenten. |
| `929209a` | Afzonderlijk begrensde, alleen-lezen catalogusquery bij opstarten; voorkomt crashherhaling bij databasebelasting. |
| `48c8491` | Expliciete deelanalyse van beschikbare resultaten terwijl een andere bronuitkomst onbekend blijft. |
| `9c1425b` | Een mislukte vernieuwde analyse bewaart het eerdere gevalideerde antwoord; optionele null-citaten afgehandeld; concretere herstelmeldingen voor de modelvalidator. |
| `9dfda66` | De begrensde correctiepoging krijgt het afgekeurde antwoord terug, zodat de AI gericht de bronverwijzing kan herstellen. Ongewijzigde bronvalidatie en maximaal twee modelpogingen. |

Productie-app: `apiosk-gateway-v2` op Fly, runtimecommit `9dfda66`, image `deployment-01M31NEYYRVT9DC75S6RF03VVP`. Rolling deploy, machine-, health- en DNS-controles geslaagd.

### App-repository

- `08d7e11`: herstelknop na offerteverval en generieke resultaatselectie.
- `5b0f583`: de herstelknop leest iedere beschikbare bronstatus één keer. Een onbekende eerste aanvraag blokkeert andere resultaten niet. Geen toestemming of betaalde uitvoering in deze herstelreeks.
- Netlify-productiedeploy: `6ab0f4f6f08a7116a259a8e3`, https://app.apiosk.com; status `ready`, gepubliceerd om 09:12:40 UTC.

### Bestaande infrastructuur

- Eén normale Supabase-herstart na de oorspronkelijke storing. De database werd om 07:57:51 UTC weer schrijfbaar nadat WAL automatisch was gerecycled.
- Geen gegevens verwijderd, geen handmatige schijfvergroting, geen read-only-beveiliging uitgezet, geen nieuwe migratie of gewijzigde toegangsrechten.
- Supabase rapporteerde later zelf ongeveer 12 GB schijfcapaciteit; er is geen resize-opdracht uitgevoerd.
- De bestaande KVK-worker `apiosk-kvk-worker-1`, machine `811d471a97e918`, is omstreeks 09:21 UTC herstart. Daarna werd de sinds 18 september wachtende Mollie-aanvraag uitgevoerd. Geen extra machine, egressroute of gewijzigd worker-image.
- MCP- en Website-repositories zijn tijdens deze test niet aangepast of gedeployd.
- Bestaande lokale latencywijzigingen in App en Gateway zijn met een aparte back-up/stash behouden en na het bijwerken teruggezet. Eén overlap in `src/engine/execute.rs` is samengevoegd met behoud van zowel timingmetingen als de nieuwe analysebescherming; `cargo check --offline --all-targets` op deze lokale combinatie is geslaagd.

## Verificatie

- Gateway: **159 tests geslaagd, nul mislukt, 26 expliciet genegeerd** in de volledige reguliere suite.
- Afzonderlijke integratietests met een tijdelijke lokale database: leverancierscheck plus alle tien overige workflows geslaagd. Inclusief offerte/toestemming, bronfouten, annuleren, ambiguïteit, historie, vervallen toestemming, herstel in andere volgorde, deelanalyse en geen extra bronaanvraag.
- Nieuwe regressietest: mislukte analysevernieuwing bewaart de eerdere geverifieerde bevindingen zonder bronkosten.
- App: de 35 gerichte conversatietests, productiebuild, TypeScript-controles en gerichte lintcontrole zijn geslaagd. Volledige suite: **2.218 geslaagd, vier bestaande baselinefouten** in `tests/standalone.test.ts` en `tests/server/workspace-selection.test.tsx`. De al aanwezige lokale wijzigingen in deze bestanden zijn bewaard en niet in deze release meegenomen.
- Live productie: echte bronaanvragen, resultaten, betaalbewijzen, heropenen en gerichte hertests zoals in de tabel. De lokale fixtures worden niet als productie-E2E-bewijs gebruikt.
- De releasecontrole is na de laatste deploy geslaagd: Gateway ready, taaklezing vereist authenticatie, MCP met vijf tools en kaart `gateway-v2-card-v46`, goedkeurings-CORS uitsluitend voor de App.

## Resterende beperkingen

StableEnrich en één OpenSanctions-uitkomst moeten aan de bron-/betalingskant definitief worden bevestigd. Reuters leverde geen resultaat voor de specifieke Mollie-zoekopdracht. De App presenteert deze als ontbrekend of onbekend; er is geen onterechte negatieve sanctieconclusie.

Antwoorden kunnen nog registercodes en Engelstalige bronbeperkingen naast Nederlandse tekst bevatten. Niet elke bron bevat actuele prijzen, deadlines of recente jaarrekeningen. De controle bewijst de geteste routes en herstelpaden; het is geen langdurige beschikbaarheids- of belastingstest.

## Bewijsbestanden

Lokale gegevens onder `gateway/output/eleven-workflows/e2e/`:

- `test-plan.json`: toestemmingsplafonds, alle testtaak-ID's en afrekening.
- `receipt-reconciliation.json`: uitsluitend de testtaken, bronstatussen en geldbedragen.
- `final-receipt-reconciliation.json`: eindcontrole na het laatste analyseherstel, met acht complete en drie gedeeltelijke laatste taken.
- `synthetic-test-invoice.pdf`: expliciete testfactuur, geen echte betaalopdracht; de voorbeeld-IBAN is niet als Mollie-rekeninghouder bevestigd.
- `progress.md`: tussentijdse bevindingen.
