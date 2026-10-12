# Apiosk Gateway v2

Nieuw zelfstandig Rust-project voor de chatbotflow: vraag → voorstel en totaalprijs → toestemming → deterministische uitvoering → bronresultaten → doorvragen met bewaarde context.

De gateway exposeert drie MCP-tools via de bestaande MCP-repository: `apiosk_sources`, `apiosk_discover` en `apiosk_execute`. De bestaande App is aangesloten met een optionele v2-vraagflow en een aparte toestemmingspagina. De bestaande betaaladministratie en treasury-executor blijven verantwoordelijk voor betalingen. V2 heeft geen tweede saldo-administratie.

## Status

De betaalroute is verder aangescherpt met afzonderlijke ledgerbetaalstatus, exacte kosten/feecontrole, saldotekortafhandeling en weergave van refunds. Zie [betalingen en grenzen van de oude route](docs/payments.md).

De zes implementatiefasen staan in [de oplevermatrix](docs/verification.md). Lokale ketentests, daadwerkelijke MCP-HTTP-integratie, App-tests en een alleen-lezen snapshot van de productiecatalogus zijn gecontroleerd. Dit is nog geen productiedeployment of bewijs van gebruik in geïnstalleerde ChatGPT-/Claude-clients. De toen geconfigureerde Gemini 3.8 Flash-parser heeft een echte modeltest met uitsluitend synthetische invoer doorstaan. Uitvoering staat standaard uit.

Ask interpreteert vragen naar capabilities met de in code vastgelegde `openai/gpt-6-luna`. Intentie, planning en analyse van één bronaanbieder behouden `qwen/qwen3-30b-a3b-instruct-2507`. Analyse die meerdere bronaanbieders combineert gebruikt uitsluitend voor die stap `openai/gpt-6.1-sol`, met een begrensd uitvoerschema en verwijzingen naar exact opgeslagen bronvelden. Gespreksreacties hebben hun eigen Qwen3 235B-stap. Modelinstellingen in omgevingsvariabelen veranderen deze productiegrenzen niet. Alle stappen behouden bronvalidatie; analyse kan geen bronnen kiezen, uitvoeren of kopen.

De Supabase-snapshot van 7 september 2026 bevatte 133 bruikbare, actieve en gevalideerde uitvoeringscontracten; alle 133 doorstonden de lokale contractvalidatie. De catalogus bevat meer listings/capabilities dan daadwerkelijk ondersteunde uitvoeringscontracten. Een beschikbare listing telt daarom niet automatisch als uitvoerbare dienst.

## Indeling

| Onderdeel | Verantwoordelijkheid |
| --- | --- |
| `src/protocol.rs`, `contracts/` | Versie 2, JSON-envelopes, tools en hostinstructies |
| `src/state.rs`, `store.rs`, `maintenance.rs` | Ondertekende state, accountbinding, revisies, requestreceipts en retentie |
| `src/catalog.rs`, `catalog.sql`, `crates/catalog-core` | Supabase-contracten, schema's, mapping, bronextractie en bestaande prijsregels |
| `src/parser.rs`, `resolver.rs` | Eén intentparse; begrensde deterministische afhankelijkheden en prijsselectie |
| `src/execution.rs`, `engine/` | Toestemming, cap, duurzame dispatch, herstel en bronresultaten |
| `src/auth.rs`, `server.rs`, `main.rs` | Accountauthenticatie, HTTP, CORS, limieten en procesconfiguratie |

Zie [ChatGPT, Claude en browserkoppeling](docs/chatbot-setup.md), [architectuur en grenzen](docs/architecture.md), [lokale setup en uitrol](docs/runbook.md) en [hergebruik van bestaande code](docs/source-reuse.md).

`src/catalog.sql` en `src/catalog/registered.sql` zijn alleen-lezen queries die
in de gatewaybinary worden opgenomen, geen migraties voor de Supabase SQL-editor.
De tweede query verwacht `$1` als een `uuid[]` met geselecteerde dienst-/endpoint-ID's.
De directe aankoop op basis van geregistreerde invoervelden vereist geen nieuwe
databasemigratie; bouw en deploy de gewijzigde gateway en App. Echte migraties
staan uitsluitend onder `supabase/migrations/`.

## Controleren

Haal na het clonen eerst de vastgelegde submodules op:

```sh
git submodule update --init --recursive
```

De Fly-image bouwt zowel gateway v2 als de ingebouwde provider-/betaalexecutor
uit `legacy-executor`. Zonder die submodule faalt `fly deploy` met een ontbrekende
`/executor/Cargo.toml`. De executor draait binnen dezelfde v2-image; er is geen
aparte gateway-v1-deployment nodig.

Vanuit deze directory, met Rust/Cargo en Node geïnstalleerd en de App/MCP-dependencies aanwezig:

```sh
node scripts/test-homepage-starters.mjs
node scripts/test-homepage-starters.mjs --check-only --live --report /tmp/homepage-starters-live.json
cargo test --workspace --offline
cargo clippy --workspace --all-targets --offline -- -D warnings
node scripts/check-source-reuse.mjs
node scripts/sync-contracts.mjs --check
node scripts/sync-migration.mjs --check
node scripts/test-integration.mjs
cargo build --release --offline
```

De homepagecontrole vergelijkt alle gepubliceerde vragen met `../app/src/lib/ui/ask-examples.ts` en test intake, bronselectie, invoerbinding, dekking van alle gevraagde velden en totaalprijs met opgeslagen broncontracten. Een ontbrekende App-checkout, gewijzigde vraag of onvolledig plan laat de controle falen. Zonder `--live` gebruikt modelinterpretatie een lokale HTTP-fixture.

Voor publicatie is ook `--live` vereist: iedere exacte vraag moet via de productie-preview een volledig geprijsd plan opleveren. Herstel of verwijder vragen die niet slagen; vul de lijst niet aan om een vast aantal te halen. Rol bij wijzigingen aan beide repositories eerst de Gateway uit en controleer daarna de preview voordat de App wordt gepubliceerd. De releaserunner `scripts/test-release-gate.mjs` voert deze live controle eerst uit. De preview maakt geen taak aan, koopt geen brondata en wordt nooit goedgekeurd. Rapporten bewaren geen goedkeuringstokens.

De algemene planner koppelt gevraagde begrippen aan exacte velden uit toegelaten uitvoercontracten en gevalideerde, geëxtraheerde bronvoorbeelden. De AI ziet velddefinities, geen voorbeeldwaarden als actueel antwoord. Iedere koppeling blijft gebonden aan aanvraag, catalogusversie, onderwerp en daadwerkelijk gekozen endpoint. Onvolledige modelplannen krijgen binnen de bestaande herstelronde feedback over ontbrekende uitvoer of verloren invoer. Beperkingen worden vóór goedkeuring getoond; de engine blijft de volledige dekking controleren vóór een prijsvoorstel.

`docs/evidence/general-planning-20261012.json` bevat twaalf live modelcontroles met de lokaal aangepaste Gateway en de productiecatalogus: de zes oorspronkelijke homepagevragen plus zes nieuwe vragen in Nederlands en Engels. De bijbehorende modelantwoorden worden offline opnieuw door bronselectie, dekking en prijsberekening gehaald. Dit bewijst planning met de lokale wijziging, niet productie-uitrol of betaalde uitvoering.

De integratierunner start en stopt zelf een tijdelijke PGlite-database op loopbackpoort 54330. Providers, model en gebruikersidentiteiten in die test zijn expliciete fixtures. De test gebruikt echte SQLx-queries, migraties, rollen, HTTP-authenticatie en de echte MCP-adapter. Hij doet geen externe betalingen. `PGTEST_PORT` kan de lokale testpoort aanpassen.

## Starten

Vanuit de gezamenlijke `Production`-directory start één commando de App,
MCP en Gateway/API en opent daarna de App:

```sh
./dev
```

De versiebeheerbare launcher staat in `scripts/dev-local.mjs`; vanuit deze
gateway-directory werkt ook `node scripts/dev-local.mjs`. Vereist Node 22.12+,
Rust/Cargo en geïnstalleerde App/MCP-dependencies. Bestaande `gateway/.env`,
`app/.env.local` en optionele `mcp/.env.v2.local` worden ingelezen zonder ze te
wijzigen. Gateway/API draait op `127.0.0.1:8082`, MCP op `127.0.0.1:3000/mcp`
en App op `localhost:5276`. Supabase API/Auth/database en de uitvoeringsfuncties
blijven de bestaande geconfigureerde omgeving gebruiken. De lokale Gateway
gebruikt dus dezelfde data; dit is geen geïsoleerde database.

De launcher zet betaalde uitvoering, KVK-workers, automatische vertaling en
recurring orders standaard uit. `./dev --execute` staat expliciet goedgekeurde
uitvoering via de geconfigureerde Supabase-functies toe; de workers blijven uit.
OAuth blijft de geconfigureerde `agent-gateway` gebruiken, inclusief diens
`APP_ORIGIN` voor de toestemmingspagina. Een volledig lokale OAuth-/betaalketen
vereist daarnaast lokale Edge Functions en de provider-/betaalexecutor.

`Ctrl+C` stopt alle drie processen, ook als één service onderweg uitvalt.
Bezette poorten worden gemeld; bestaande processen worden nooit afgesloten.
Logs staan in `.local/dev-stack/`. Gebruik `./dev --check` voor configuratie en
poortcontrole, `./dev --smoke` voor starten/controleren/stoppen, of `./dev --no-open`
om de browser gesloten te laten. De eerste Rust-build kan enkele minuten duren.

Gebruik [.env.example](.env.example), vul serverconfiguratie in en volg eerst [de runbook](docs/runbook.md). De binary past geen migraties toe. Met de private tabellen en runtime-role beschikbaar:

```sh
cargo run --release
```

Opt-in koppelingen:

```text
# MCP-serveromgeving
APIOSK_GATEWAY_V2_URL=http://127.0.0.1:8082

# App Vite-omgeving; opnieuw bouwen na wijzigen
VITE_GATEWAY_V2_URL=http://127.0.0.1:8082
```

Gebruik een HTTPS-adres buiten dezelfde lokale machine. Zonder deze variabelen blijven de bestaande App-vraagflow en elf legacy MCP-tools actief. Er zijn geen productievariabelen omgezet.
