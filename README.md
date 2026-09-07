# Apiosk Gateway v2

Nieuw zelfstandig Rust-project voor de chatbotflow: vraag → voorstel en totaalprijs → toestemming → deterministische uitvoering → bronresultaten → doorvragen met bewaarde context.

De gateway exposeert twee MCP-tools via de bestaande MCP-repository: `apiosk_discover` en `apiosk_execute`. De bestaande App is aangesloten met een optionele v2-vraagflow en een aparte toestemmingspagina. De bestaande betaaladministratie en treasury-executor blijven verantwoordelijk voor betalingen. V2 heeft geen tweede saldo-administratie en geen antwoordmodel.

## Status

De betaalroute is verder aangescherpt met afzonderlijke ledgerbetaalstatus, exacte kosten/feecontrole, saldotekortafhandeling en weergave van refunds. Zie [betalingen en grenzen van de oude route](docs/payments.md).

De zes implementatiefasen staan in [de oplevermatrix](docs/verification.md). Lokale ketentests, daadwerkelijke MCP-HTTP-integratie, App-tests en een alleen-lezen snapshot van de productiecatalogus zijn gecontroleerd. Dit is nog geen productiedeployment of bewijs van gebruik in geïnstalleerde ChatGPT-/Claude-clients. De geconfigureerde Gemini 3.8 Flash-parser heeft een echte modeltest met uitsluitend synthetische invoer doorstaan. Uitvoering staat standaard uit.

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

## Controleren

Vanuit deze directory, met Rust/Cargo en Node geïnstalleerd en de App/MCP-dependencies aanwezig:

```sh
cargo test --workspace --offline
cargo clippy --workspace --all-targets --offline -- -D warnings
node scripts/check-source-reuse.mjs
node scripts/sync-contracts.mjs --check
node scripts/sync-migration.mjs --check
node scripts/test-integration.mjs
cargo build --release --offline
```

De integratierunner start en stopt zelf een tijdelijke PGlite-database op loopbackpoort 54330. Providers, model en gebruikersidentiteiten in die test zijn expliciete fixtures. De test gebruikt echte SQLx-queries, migraties, rollen, HTTP-authenticatie en de echte MCP-adapter. Hij doet geen externe betalingen. `PGTEST_PORT` kan de lokale testpoort aanpassen.

## Starten

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
