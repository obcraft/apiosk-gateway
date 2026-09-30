Apiosk: hoofdindeling en de waarde van overheidsdata + x402
Onderzocht op 30 september 2026. Voorstel, geen productie-aanpassing.

ADVIES

Gebruik vier inhoudelijke hoofdtabs: Country APIs, Open Government & Datasets,
Enrichment en Search & Tools. Land is daarnaast een gedeeld filter. Een pagina
voor Nederland toont daardoor zowel KVK als CBS, PDOK en passende verrijking.
Een API, dataset, aanbieder en betaalmethode zijn verschillende eigenschappen.
x402 krijgt een badge/filter; het is geen bewijs van inhoudelijke kwaliteit.

Een bron heeft één primaire plek en mag via dezelfde identiteit elders worden
getoond. Binnen StableEnrich, Pulse en Bazaar wordt ieder product of endpoint
afzonderlijk geclassificeerd. De bronkaart mag meerdere onderwerpen tonen.
GLEIF is een wereldwijde referentiebron, geen nationaal overheidsregister.
Een datasetcatalogus levert metadata; de gevonden dataset is een tweede stap.

DE BESTAANDE 37 BRONNEN

Country APIs (5):
  KVK, Companies House, GLEIF, VIES, RDW.

Open Government & Datasets (12):
  Bodemloket, CBS, data.overheid.nl, EU Funding & Tenders, EUR-Lex Cellar,
  Eurostat, KOOP, PDOK, PDOK Ruimtelijke Plannen, RVO, TED, TenderNed.

Enrichment (10):
  Ask Merlin, Bloomberg News, CityFALCON, EODHD, OpenSanctions, Overheid.io,
  Pulse Network, Reuters News, Sirenic, StableEnrich.

Search & Tools (10):
  AgentBodega, Browserbase, CrowdPull, Domain Intelligence, Exa, IBAN Check,
  Linkup, PixelForge, Tavily, WebberSites.

Sirenic, Overheid.io en Ask Merlin blijven ook zichtbaar bij relevante landen
en controles. Hun API is de toegangsroute; vermeld de oorspronkelijke bron.
StableEnrich heeft ook zoektools; Pulse heeft zeer uiteenlopende producten.
Deze hoofdcategorieën zijn navigatievoorstellen, geen uitvoeringsrechten.

Bij slechts drie tabs: voeg Search & Tools samen met Enrichment. Vier bewaart
het onderscheid tussen data toevoegen en informatie ophalen/verwerken.

NIEUWE LANDENBRONNEN

country-source-mapping.json deelt alle 76 onderzoeksrecords in. Daarvan zijn
57 nationale officiële register-/publicatierecords voor 42 landen; KVK en
Companies House bestaan al in de 37. Andere bestaande overlap is GLEIF, VIES
en OpenSanctions. Hergebruik deze vijf bronidentiteiten.
De nieuwe nationale kandidaten krijgen hun plek onder Country APIs, per land,
met het bestaande onderscheid tussen specificatie, toegang en uitvoering.
Registerdirectories zijn verwijzingen, geen uitvoerbare bedrijfslookup.
Deze uitbreiding onderzoekt bedrijfsregisters; ze bewijst niet dat ieder land
al statistiek, vastgoed, aanbestedingen en andere open-government-data heeft.

WAT DE DRIE VERRIJKINGSKANALEN WERKELIJK ZIJN

StableEnrich: bundelt commerciële data- en zoekproducten. FullEnrich zoekt
bedrijven; CompanyEnrich verrijkt via een domein; Exa/Firecrawl halen webdata.
Dat kan ontbrekende bedrijfskenmerken of webcontext toevoegen. De documentatie
waarschuwt voor naam-/domeinverwarring en levert sommige omzet- en personeels-
gegevens als bandbreedtes. Verifieer de koppeling met de juridische entiteit.
Bron: https://stableenrich.dev/llms.txt

Pulse Network: specialistische data en analyse. FilingsPulse publiceert zowel
een deterministische SEC-XBRL-kwartaalroute als AI-samenvattingen; RiskPulse en
TradePulse bieden onder meer landen- en handelsanalyses. De officiële site
presenteert deze producten zelf; kwaliteit en EU-dekking zijn hier niet via
een betaalde respons bewezen. Vergelijk een analyse met wat Apiosk uit dezelfde
officiële gegevens al kan maken. Een endpoint met eigen primaire data kan wél
waarde toevoegen. Bron: https://pulse.theaslangroupllc.com/

Coinbase Bazaar: openbare ontdekking van diensten van andere aanbieders. Het
is geen uniforme dataleverancier. Een gevonden StableEnrich- of Pulse-endpoint
wordt geen extra onafhankelijke bron doordat het ook in Bazaar staat. Zoek
hier naar ontbrekende mogelijkheden en vergelijk de feitelijke aanbieders.
Bron: https://docs.cdp.coinbase.com/x402/buyer/discover-services

Sirenic is daarnaast een specialistische registeraggregator. De huidige
agent-card beschrijft landelijke registers via lokale kopieën en live routes,
plus GLEIF voor andere dekking. Kies voor dezelfde identiteit de directe bron
óf een geschikte Sirenic-route; koop beide alleen bij een expliciete behoefte
aan onafhankelijke bevestiging die de onderliggende bronnen echt leveren.
Een aggregator kan onderhoud, normalisatie en toegang vereenvoudigen, ook als
de oorspronkelijke data gratis is. Bron:
https://api.sirenic.eu/.well-known/agent-card.json

WAAR COMBINEREN WAARDE KAN TOEVOEGEN

1. Leveranciersdossier — aanbevolen eerste product
   Landelijk register: juiste juridische entiteit en status.
   VIES en relevante publicaties: beschikbare registratie-/publicatiefeiten.
   StableEnrich of een bestaande zoekbron: website, bedrijfskenmerken, nieuws.
   OpenSanctions: aanvullende kandidaatmatches met bronverwijzing.
   Resultaat: herleidbaar dossier, geen automatische goedkeuring of risicogarantie.
   Risico: merk, groep en juridische dochter mogen niet worden samengevoegd.

2. Aanbesteding + bedrijf — aanbevolen tweede product
   TED/TenderNed: aankondiging, opdrachtgever, documentverwijzingen en data.
   Landelijk register: identiteit van gevonden organisaties, waar ID's beschikbaar zijn.
   Enrichment: website, dienstverlening, nieuws en marktcontext.
   Resultaat: bruikbaar overzicht van een kans en de betrokken organisaties.
   Een aankondiging is geen gunning; een gunning is geen geboekte omzet.
   TED ondersteunt hergebruik van publicaties:
   https://docs.ted.europa.eu/api/latest/search.html

3. Marktverkenning per land — zinvol met duidelijke grenzen
   Eurostat/CBS: vergelijkbare indicatoren en sector-/regiostatistiek.
   Registerzoekfunctie: bereikbare bedrijfsidentiteiten binnen de zoekdekking.
   StableEnrich: commerciële kenmerken of een aanvullende bedrijfsshortlist.
   Eventueel Pulse: specifiek ontbrekende, onderbouwde marktcontext.
   Resultaat: officiële marktcijfers naast een herkenbaar onvolledige shortlist.
   Een commerciële zoeklijst mag geen officiële marktpopulatie voorstellen.
   Eurostat heeft expliciete dataset-, land-, periode- en overige dimensies:
   https://ec.europa.eu/eurostat/web/user-guides/data-browser/api-data-access/api-getting-started/api

4. Locatieonderzoek — bruikbaar als voorbereiding
   PDOK/BAG + CBS + relevante publicaties: adres, object en gebiedsgegevens.
   Maps-/webverrijking: omgeving, zichtbare bedrijven en actuele voorzieningen.
   Dit kan locatiecontext toevoegen; vergunningen, eigendom en juridische
   gebruiksmogelijkheden moeten uit de passende officiële stukken blijken.

WANNEER HET OVERBODIG IS

- Een adres of registerstatus nogmaals kopen zonder extra dekking of actualiteit.
- KVK, een KVK-doorverkoper en een Bazaar-vermelding als drie bevestigingen tellen.
- Een generieke AI-samenvatting kopen die Apiosk zelf uit de opgehaalde data kan maken.
- Een personeels- of omzetbandbreedte presenteren als exact register- of jaarrekeningcijfer.
- Iedere vraag standaard langs alle drie de x402-kanalen sturen.

INTEGRATIEONTWERP

Vraag -> benodigde feiten en land -> passende geaccepteerde contracten ->
ontbrekende velden -> gerichte verrijking -> één totaalprijs en akkoord ->
uitvoering op de backend -> opgeslagen resultaat met herkomst per veld.

De parser mag geen bron kiezen op alleen een tabnaam. Gebruik land, onderwerp,
identifiers, periode, gevraagde velden en daadwerkelijke contractdekking.
Classificeer officiële feiten, schattingen en AI-analyse afzonderlijk.
Gebruik bestaande bron- en betaalpaden; start geen tweede approval-flow voor x402.
Dedupliceer zowel dezelfde endpoints als dezelfde onderliggende databronnen.
Een directe route en aggregatorroute zijn alternatieven totdat een aantoonbaar
aanvullend gegeven beide rechtvaardigt.

Te veranderen: catalogusmetadata; gedeelde filters en telling vóór paginering;
landpagina's; classificatie per netwerkdienst; ontbrekende-veldenlogica;
identiteitskoppeling; bronherkomst; toelating en prijsbepaling per operatie.
De bestaande Integrations voor CRM/mail/andere apps blijft een afzonderlijke
functie. Bronintegratie en een resultaat naar een app sturen zijn andere stappen.

HUIDIGE STAND EN BEWIJS

Read-only productiecontrole: 37 zichtbare hoofdbronnen en 112 diensten, met
76 Pulse-diensten onder één bron. Alle 161 geïmporteerde Bazaar-diensten staan
pending/active=false/discoverable=false. Er bestaat ook afzonderlijke Ask-v2
code voor externe Bazaar-discovery, prijsopgave en aankoop; verborgen imports
betekenen dus niet dat die externe route ontbreekt. Geen betaalde route getest.

Bewaar het verschil tussen documentatie, cataloguszichtbaarheid en uitvoering.
De officiële providerdocumentatie is gelezen; genoemde samengestelde producten
zijn ontwerpvoorstellen. Er zijn geen nieuwe bronnen geactiveerd of UI's aangepast.

Volgende inhoudelijke validatie: vergelijk op dezelfde 20 openbare bedrijven
in NL, FR, FI, CZ en NO drie varianten: alleen officiële data, gerichte verrijking,
en een aggregator als alternatief. Meet juiste identiteit, extra bruikbare velden,
controleerbare verwijzingen, actualiteit, duplicatie, totale kosten en snelheid.
Een betaalde benchmark vereist vooraf een concrete begrensde prijsopgave.

BESTANDEN

proposal.json                Vier niches, alle 37 toewijzingen, routering en wijzigingen.
country-source-mapping.json  Toewijzing van de 76 landen-/verrijkingsrecords.
catalog-snapshot.json        Alleen bronmetadata uit de read-only productiecontrole.
README.txt                   Dit onderzoeksadvies.
