# Oplevering per fase — 7 september 2026

Aanvulling betaling: zie [betalingen](payments.md) voor de nieuwste aanpassingen en de expliciete verschillen tussen de aanvullende ontwerpbijlage en de bestaande gateway. De aanvullende tests controleren betaalstatus, saldotekort, ledgerreceipt, late refund en het uitschakelen van een nieuwe betaalidentiteit bij een automatische v2-retry.

De zes oorspronkelijke opdrachten onder `chatbot-flow/` zijn achtereenvolgens vertaald naar onderstaande implementatie. Implementatiebewijs, gecontroleerde fixtures en daadwerkelijk hostgebruik worden afzonderlijk vermeld. Niet alle oorspronkelijke eindacceptatiecriteria zijn hiermee bewezen.

| Fase | Geïmplementeerd | Bewijs / resterende grens |
| --- | --- | --- |
| 1. Contract en hostgedrag | Twee tools, één response-envelope, strikt inputcontract, toestemmingsscheiding, hostinstructies, opt-in legacycompatibiliteit | Rust-serde-tests, MCP-tests en werkelijke MCP-adapter naar Rust HTTP. Geen claim van native hostkaarten. |
| 2. State en taakcontext | Account/verbinding, HMAC, CAS, compacte focus, provenance, retentie en geauthenticeerd herstel | State-tests en SQLx-ketentest: andere eigenaar, signaturewijziging, stale state, resultaatreads en recovery. Geen bewezen duurzaam geheugen binnen een geïnstalleerde chatbot. |
| 3. Catalogus en bindings | Live Supabase-contractloader, sample/schema-checks, semantische input/outputmapping, bestaande integerprijsregels, quarantine | Alleen-lezen productiecatalogussnapshot: 133 contracten gevonden, 133 toegelaten, 0 in quarantine binnen die query. 84 bestaande core-tests. Geen providers aangeroepen tijdens catalogusvalidatie. |
| 4. Intent en resolver | Maximaal één modelrequest, cache/single-flight, vaste graphlimieten, prijsrangschikking, gedeelde dependencies, geen gegokte identifiers | Zes resolvertests en parsercallteller in ketentest. Een echte modelaanroep met synthetische invoer is inmiddels geslaagd; representatieve kwaliteit en latency zijn nog niet gemeten. Niet-gemapte bron-/tijdvereisten worden niet afgezwakt. |
| 5. Uitvoering en betaling | Exact voorstel/cap, menselijke App-autorisatie, deterministische executorbrug, korte transacties, journal, idempotency, reconciliatie, partial results | Lokale SQLx/HTTP-test: toestemming, cap, ambiguïteit, retry, onbekende uitkomst, ledgerherstel, RLS/kolomrechten. Echte betalingen, productieconcurrentie en treasurysettlement niet getest. |
| 6. App en MCP | Featureflags, nieuwe App-vraagflow, toestemmingsroute, selectie/input/poll/stop/resultaatlezen, twee MCP-tools en taakherstel | App build/componenttests en volledige suites; echte MCP-module door Rust HTTP/Auth/SQLx. Geïnstalleerde ChatGPT- en Claude-clientflows zijn nog niet uitgevoerd. |

## Testresultaten

- Rust workspace: 6 protocol/state/billing-tests, 6 resolvertests en 84 hergebruikte core-tests geslaagd. Catalogus- en DB-ketentests zijn expliciet ignored in de gewone suite en worden afzonderlijk gestart.
- Lokale duurzame ketentest geslaagd, inclusief echte MCP-adapter → Rust HTTP/Auth → SQLx → fictieve provider. De testdatabase gebruikt PGlite met één wire-verbinding; dit bewijst geen multi-connection loadgedrag.
- Gecontroleerde keten: lookup kost fictief 23 atomic, accounts 115 atomic, totaal/cap 138 atomic USDC. Geen aankoop vóór toestemming. De vervolgdatavraag gebruikt de bestaande identifier en plant alleen de accounts-call. Selectie, reads en polling voegen geen parsercall toe.
- In de uitgebreide ketentest worden meerdere losse scenario's uitgevoerd, inclusief annuleren en gedeeltelijke brondata. De laatste waarneming duurde circa 0,42 seconde met vijf fixtureparsercalls en negen fixtureprovidercalls. Dit zijn geen productie-p50/p95 of echte model-/providerlatencies.
- MCP: 119 tests geslaagd, inclusief behoud van elf tools zonder v2-flag.
- App: volledige suite van 130 testbestanden / 1.825 tests opnieuw geslaagd na de aanvullende betaalintegratie. TypeScript/Vite-build en ESLint voor de nieuwe UI-bestanden geslaagd.
- Geoptimaliseerde Rust release-binary succesvol gebouwd met `cargo build --release --offline`; Clippy over alle workspace-targets slaagt zonder waarschuwingen.
- Bestaande gateway: 19 gerichte `v1_routes::do_job`-tests geslaagd. Dit is geen volledige v1-regressiesuite.
- Core-bronvergelijking, toolcontractsync en migratiesync beschikbaar met `--check`. De twee expliciet goedgekeurde productiemigraties zijn toegepast; hun lokale versies komen overeen met de databasehistorie.

## Reproduceerbare opdrachten

Vanuit `gateway-v2`:

```sh
cargo test --workspace --offline
cargo clippy --workspace --all-targets --offline -- -D warnings
node scripts/test-integration.mjs
node scripts/sync-contracts.mjs --check
node scripts/sync-migration.mjs --check
```

De productiecatalogussnapshot is lokaal opgeslagen onder het genegeerde `test-artifacts/live-catalog.json`. Gebruik een nieuwe alleen-lezen export van `src/catalog.sql` om de actuele toestand te controleren; de snapshot is geen permanente productiewaarheid.

```sh
CATALOG_SNAPSHOT=test-artifacts/live-catalog.json cargo test --offline --test catalog_live -- --ignored --nocapture
```

Vanuit `mcp`: `npm test`. Vanuit `app`: `NODE_OPTIONS=--no-experimental-webstorage npm test` en `npm run build`. Vanuit `gateway`: `cargo test --offline v1_routes::do_job`.

## Nog te bewijzen vóór volledige vrijgave

1. Echte intentparser op representatieve Nederlandse en Engelse vragen: schema-validiteit, referenties, behoud van bron/tijdvereisten, tokens, p50/p95 en aantal noodzakelijke vervolgvragen.
2. Aanvullende betrouwbare catalogusmapping voor tijd/filings, wanneer 'laatste twee jaarrekeningen' als ondersteunde use-case wordt vrijgegeven. De huidige implementatie meldt dit als niet ondersteund; de fixture `accounts` is daarvoor geen bewijs.
3. Migratie en runtime-role in staging, productie-equivalente multi-connection concurrency/crash-tests en aansluiting op de echte App-ledger/treasury met een vooraf begrensde toegestane aankoop.
4. Geïnstalleerde ChatGPT- en Claude-clients: verbinden, vraag, prijsvoorstel, App-toestemming, resultaat met bron, doorvragen en contextverlies/herstel. Leg toolcalls, kosten en stateversies per host vast.

De goedgekeurde private v2-tabellen en runtime-role zijn toegevoegd. Er zijn geen productievariabelen gewijzigd, providerbetalingen uitgevoerd of connectorpublicaties gedaan. De vier reeds aanwezige App-wijzigingen in `single-answer.ts`, `agent-gateway/index.ts`, `ask-turn/index.ts` en `ask-turn-flow.test.ts` zijn behouden; deze taak wijzigde die bestanden niet.

## Integratie-update van 7 september 2026

- Rust: 96 tests; Clippy met warnings-as-errors geslaagd; releasebuild geslaagd.
- SQLx/MCP-fixtureketen: geslaagd; 560 ms voor de verzamelde fixtures, 5 parser- en 9 providerfixtureaanroepen. Dit is geen productielatentiebenchmark.
- MCP: 119 tests; aanvullende echte lokale HTTP-controle bevestigt initialize, twee tools, contractresource, metadata en OAuth-401. SDK-tests met hostnamen ChatGPT/Claude zijn protocoltests, geen tests van de geïnstalleerde apps.
- App: 131 testbestanden, 1.830 tests geslaagd; TypeScript/Vite-build en gerichte lintcontroles geslaagd. Onderbroken aankoop vereist herstel; goedgekeurde agenttaak wordt niet opnieuw goedgekeurd of door de App uitgevoerd. Daarna zijn alle 10 gerichte App/WebMCP-tests opnieuw geslaagd, inclusief de extra test die aankoopknoppen verbergt bij uitgeschakelde uitvoering; build en volledige lintcontrole slagen.
- Browser: ingelogde lokale App toont de v2-vraagflow; WebMCP registreert precies de twee v2-tools met gedeelde schema's/instructies.
- OpenRouter: één echte Gemini 3.8 Flash-aanroep met een volledig synthetische catalogus en fictieve bedrijfsnaam, geldig intentresultaat in 4.180 ms. Geen database-/provideraanroep of treasurytransactie in deze test.
- Bronhergebruik: zes gekopieerde catalogus-/prijsbestanden zijn bytegelijk aan de oorspronkelijke gateway.
- Supabase: private state en runtime-role toegepast. De rol kan geen saldi wijzigen, auth.users lezen of alle API-kolommen lezen. Security advisors rapporteerde geen gateway_v2-vermelding; dat is geen auditverklaring over de hele bestaande database.

Nog niet bewezen: gebruik van v2 in de geïnstalleerde ChatGPT/Claude-cloudclients, volledige live brondekking, productiebetalingen/settlement en een vergelijkende kwaliteits-/snelheidsbenchmark tegen v1. De aparte database-login en de lokale omschakeling zijn inmiddels met toestemming uitgevoerd en via volledig geverifieerde TLS gecontroleerd.


## Aansluiting van serverrol en treasury

- De lokale v2-server gebruikt dezelfde Supabase-database als v1 via `gateway_v2_server`; volledig geverifieerde TLS met het officiële Supabase CA-certificaat. Catalogus bij herstart: 133 toegelaten contracten, 0 quarantine.
- `TREASURY_PRIVATE_KEY` en `SUPABASE_SERVICE_ROLE_KEY` zijn via stdin gekoppeld aan Fly-app `apioskgateway`. Beide zijn na herstart met hashes vergeleken met de aangeleverde lokale keys: gelijk. De treasury-key was al gelijk; de canonieke service-role-key is toegevoegd.
- Walletformaat secp256k1 gecontroleerd; alleen-lezen Base-mainnetcontrole (chain 8453): 33.812561 USDC en 0.00089863955886449 ETH op het controlemoment. Dit is geen liquiditeitsgarantie voor latere aankopen.
- Online betaalgateway: release 714, App-gezondheidscheck geslaagd. De bestaande backend voert x402-signing en settlement uit; v2 krijgt geen directe saldowijzigingsrechten.
- Supabase `use-selection` versie 58 en `agent-gateway` versie 48 actief. De uitrol is opgebouwd uit de eerder online bestanden, met uitsluitend de v2-doorgeefvlag, uitschakeling van een tweede betaalidentiteit bij retry en geen samenvattingsmodel voor v2. Andere lokale agent/discoverywijzigingen zijn niet mee uitgerold. JWT-/agentauthenticatie-instellingen zijn behouden. Oude pakketten zijn lokaal genegeerd bewaard onder `.local/rollback/`.
- Live interne preflight met servicekey en een onuitvoerbaar verzoek zonder endpoint: HTTP 400 `deterministic_candidate_required`. Dit bewijst dat treasury-authenticatie werkt en de v2-guard vóór uitvoering actief is; het bewijst geen providerbetaling.
- 19 gerichte gateway-jobtests, 12 x402-reselltests (1 live betaaltest ignored) en 34 App-betaaltests geslaagd. Bestaande Rust-waarschuwingen blijven aanwezig.
- Lokale `V2_EXECUTION_ENABLED=1`; aankopen vereisen de bestaande accountcontrole en menselijke goedkeuring van het exacte plan en plafond. Er is geen echte aankoop of treasurytransfer uitgevoerd in deze aansluiting.

## Aparte Ask-chatlaag

> Uitgefaseerd: `/v2/chat` antwoordt nu `410 ask_moved`. De App-Ask gebruikt `/v2/ask-v2/*` en opent opgeslagen taken op `/?task=<id>`. Onderstaande tekst beschrijft de eerdere situatie.

De App gebruikte `/v2/chat` voor een extra Gemini 3.8 Flash-gespreksstap. Begroetingen en verduidelijkingen krijgen een modelantwoord en opgeslagen gesprekscontext (laatste zes beurten). Een concrete datavraag gaat daarna met de oorspronkelijke gebruikersinvoer naar dezelfde bestaande intentparser, resolver, prijs- en toestemmingsroute. De chatlaag mag geen identifiers invullen of betalingen goedkeuren. MCP en WebMCP blijven `/v2/discover` gebruiken zonder deze extra modelstap; de bestaande parserprompt en intentstructuur zijn behouden. De nieuwe App-route weigert agentcredentials.

Gecontroleerd: App-componenttest voor modelantwoord en vervolgstate; Rust-validation van de beperkte chatrespons; lokale duurzame ketentest voor begroeting, herstel, vervolgplan en verwijderen van oude koopacties zonder provideruitvoering. In de echte ingelogde Ask UI kwam op `Hallo` een OpenRouter-modelantwoord terug zonder foutmelding of aankoop. Dit is geen bewijs voor een betaalde bronquery.

De echte Ask UI is vervolgens gecontroleerd met een Nederlandse hulpvraag en `Ik wil de bedrijfsgegevens van ASML opzoeken.`: modelreactie gevolgd door twee geplande stappen (`company.registry.search`, `company.registry.profile`) met plafond 0.097826 USDC. Geen goedkeuring aangeklikt en geen providerbetaling gedaan. De eerste korte begroeting was taalambigu; de chatprompt kiest nu Nederlands voor een losse `Hallo` zolang het gesprek geen andere taal vastlegt. Na de laatste wijziging: 98 Rust-workspace-tests, 12 gerichte App/WebMCP-tests, Clippy, App-build, lint en duurzame fixtureketen geslaagd.

De bestaande New-knop start nu een afzonderlijke v2-chat zonder vorige task-state. De App-test controleert dat de volgende vraag `state: null` meestuurt.
