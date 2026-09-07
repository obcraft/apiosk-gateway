# Oplevering per fase — 7 september 2026

Aanvulling betaling: zie [betalingen](payments.md) voor de nieuwste aanpassingen en de expliciete verschillen tussen de aanvullende ontwerpbijlage en de bestaande gateway. De aanvullende tests controleren betaalstatus, saldotekort, ledgerreceipt, late refund en het uitschakelen van een nieuwe betaalidentiteit bij een automatische v2-retry.

De zes oorspronkelijke opdrachten onder `chatbot-flow/` zijn achtereenvolgens vertaald naar onderstaande implementatie. Implementatiebewijs, gecontroleerde fixtures en daadwerkelijk hostgebruik worden afzonderlijk vermeld. Niet alle oorspronkelijke eindacceptatiecriteria zijn hiermee bewezen.

| Fase | Geïmplementeerd | Bewijs / resterende grens |
| --- | --- | --- |
| 1. Contract en hostgedrag | Twee tools, één response-envelope, strikt inputcontract, toestemmingsscheiding, hostinstructies, opt-in legacycompatibiliteit | Rust-serde-tests, MCP-tests en werkelijke MCP-adapter naar Rust HTTP. Geen claim van native hostkaarten. |
| 2. State en taakcontext | Account/verbinding, HMAC, CAS, compacte focus, provenance, retentie en geauthenticeerd herstel | State-tests en SQLx-ketentest: andere eigenaar, signaturewijziging, stale state, resultaatreads en recovery. Geen bewezen duurzaam geheugen binnen een geïnstalleerde chatbot. |
| 3. Catalogus en bindings | Live Supabase-contractloader, sample/schema-checks, semantische input/outputmapping, bestaande integerprijsregels, quarantine | Alleen-lezen productiecatalogussnapshot: 133 contracten gevonden, 133 toegelaten, 0 in quarantine binnen die query. 84 bestaande core-tests. Geen providers aangeroepen tijdens catalogusvalidatie. |
| 4. Intent en resolver | Maximaal één modelrequest, cache/single-flight, vaste graphlimieten, prijsrangschikking, gedeelde dependencies, geen gegokte identifiers | Zes resolvertests en parsercallteller in ketentest. Werkelijke modelkwaliteit/latency niet getest omdat er geen modelkey beschikbaar was. Niet-gemapte bron-/tijdvereisten worden niet afgezwakt. |
| 5. Uitvoering en betaling | Exact voorstel/cap, menselijke App-autorisatie, deterministische executorbrug, korte transacties, journal, idempotency, reconciliatie, partial results | Lokale SQLx/HTTP-test: toestemming, cap, ambiguïteit, retry, onbekende uitkomst, ledgerherstel, RLS/kolomrechten. Echte betalingen, productieconcurrentie en treasurysettlement niet getest. |
| 6. App en MCP | Featureflags, nieuwe App-vraagflow, toestemmingsroute, selectie/input/poll/stop/resultaatlezen, twee MCP-tools en taakherstel | App build/componenttests en volledige suites; echte MCP-module door Rust HTTP/Auth/SQLx. Geïnstalleerde ChatGPT- en Claude-clientflows zijn nog niet uitgevoerd. |

## Testresultaten

- Rust workspace: 6 protocol/state/billing-tests, 6 resolvertests en 84 hergebruikte core-tests geslaagd. Catalogus- en DB-ketentests zijn expliciet ignored in de gewone suite en worden afzonderlijk gestart.
- Lokale duurzame ketentest geslaagd, inclusief echte MCP-adapter → Rust HTTP/Auth → SQLx → fictieve provider. De testdatabase gebruikt PGlite met één wire-verbinding; dit bewijst geen multi-connection loadgedrag.
- Gecontroleerde keten: lookup kost fictief 23 atomic, accounts 115 atomic, totaal/cap 138 atomic USDC. Geen aankoop vóór toestemming. De vervolgdatavraag gebruikt de bestaande identifier en plant alleen de accounts-call. Selectie, reads en polling voegen geen parsercall toe.
- In de uitgebreide ketentest worden meerdere losse scenario's uitgevoerd, inclusief annuleren en gedeeltelijke brondata. De laatste waarneming duurde circa 0,42 seconde met vijf fixtureparsercalls en negen fixtureprovidercalls. Dit zijn geen productie-p50/p95 of echte model-/providerlatencies.
- MCP: 113 tests geslaagd, inclusief behoud van elf tools zonder v2-flag.
- App: volledige suite van 130 testbestanden / 1.825 tests opnieuw geslaagd na de aanvullende betaalintegratie. TypeScript/Vite-build en ESLint voor de nieuwe UI-bestanden geslaagd.
- Geoptimaliseerde Rust release-binary succesvol gebouwd met `cargo build --release --offline`; Clippy over alle workspace-targets slaagt zonder waarschuwingen.
- Bestaande gateway: 19 gerichte `v1_routes::do_job`-tests geslaagd. Dit is geen volledige v1-regressiesuite.
- Core-bronvergelijking, toolcontractsync en migratiesync beschikbaar met `--check`. Geen automatische productiemigratie.

## Reproduceerbare opdrachten

Vanuit `gateway-v2`:

```sh
cargo test --workspace --offline
cargo clippy --workspace --all-targets --offline -- -D warnings
node scripts/test-integration.mjs
node scripts/check-source-reuse.mjs
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

Er zijn geen productievariabelen gewijzigd, providerbetalingen uitgevoerd of connectorpublicaties gedaan. De vier reeds aanwezige App-wijzigingen in `single-answer.ts`, `agent-gateway/index.ts`, `ask-turn/index.ts` en `ask-turn-flow.test.ts` zijn behouden; deze taak wijzigde die bestanden niet.
