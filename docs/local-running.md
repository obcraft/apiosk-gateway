# Lokale omgeving

Op 7 september 2026 gestart en via HTTP gecontroleerd:

- App: http://localhost:5276/
- Gateway v2: http://127.0.0.1:8082/health
- Actieve database: het bestaande Supabase-project, via een aparte login `gateway_v2_server` en de beperkte rol `gateway_v2_runtime`.
- Lokale PostgreSQL 17.11 op 127.0.0.1:55432 blijft beschikbaar als ontwikkelfixture.

De App gebruikt `VITE_GATEWAY_V2_URL=http://127.0.0.1:8082`. Gateway `APP_ORIGIN` is `http://localhost:5276`. Beide services gaven HTTP 200; de door Vite geleverde configuratie bevat de lokale gateway-URL.

De gateway leest nu rechtstreeks dezelfde Supabase-catalogus als v1: bij herstart 133 gevalideerde contracten, 0 in quarantine. Bestaande accounts, agenttokens en de ledger blijven in hetzelfde project. Er is geen kopie van gebruikers of saldi nodig. De oude lokale catalogussnapshot wordt niet meer door de actieve gateway gebruikt.

`.env` bevat de lokale databaseconfiguratie, signing key en aangeleverde intentmodelkey, met bestandsrechten 0600. `.env`, `.local/` en de catalogusexport zijn git-ignored. `V2_EXECUTION_ENABLED=1`: de lokale gateway kan na menselijke goedkeuring van het exacte plan en prijsplafond via de bestaande online betaalgateway uitvoeren. De keys zijn aangesloten en de serverroute is gecontroleerd; er is tijdens deze aansluiting geen echte aankoop gedaan. Het huidige intentmodel is `google/gemma-3-12b-it`. De eerdere echte modeltest met uitsluitend synthetische invoer gebruikte Gemini 3.8 Flash (circa 4,2 seconden). Een modeltest met de productiecatalogus is niet uitgevoerd.

## Opnieuw starten

Vanuit `gateway-v2`, als PostgreSQL niet meer draait:

```sh
/opt/homebrew/Cellar/postgresql@17/17.11/bin/pg_ctl -D .local/postgres -l .local/postgres.log -o '-h 127.0.0.1 -p 55432 -k /Users/olivier/Documents/Apiosk/Production/gateway-v2/.local/socket' start
```

Start vervolgens de gateway vanuit `gateway-v2`:

```sh
./target/release/apiosk-gateway-v2
```

Start de App vanuit `app`:

```sh
npm run dev -- --host localhost
```

`node scripts/setup-local.mjs` bereidt uitsluitend deze lokale database voor. Het gebruikt een lokale Unix-socket en databasebeheeraccount en kopieert geen productie-accounts. De lokale catalogus is een snapshot; veranderende productieprijzen worden niet automatisch naar deze ontwikkelomgeving gesynchroniseerd.

De oorspronkelijke Vite-instantie op poort 5276 is vervangen omdat die 404 gaf. Homebrew had een linkconflict met bestaande libpq-tools; de server draait daarom via zijn expliciete versiemap, met afzonderlijke versiegebonden share/lib-paden. Bestaande libpq-clientlinks zijn behouden.

## Integratie-update

MCP v2 draait op http://localhost:3002/mcp, uitsluitend op loopback. Start vanuit `mcp` met `npm run dev:v2`; de genegeerde `.env.v2.local` bevat een eigen OAuth-secret. App WebMCP publiceert dezelfde twee tools en dezelfde canonieke instructies. De echte HTTP-metadata en OAuth-weigering zijn gecontroleerd.

De private v2-tabellen, runtime-role en aparte database-login zijn aangesloten. `python3 scripts/configure-shared-db.py --apply` is na toestemming uitgevoerd. TLS gebruikt `verify-full` en het officiële Supabase CA-certificaat onder `.local/certs/`; de URL bevat het certificaatpad. De vorige lokale configuratie is bewaard als genegeerde `.env.local-db.backup`. De runtime-login kan v2-state lezen en schrijven, maar geen saldi wijzigen of providersecretkolommen lezen.

Zie [chatbot-setup](chatbot-setup.md) voor cloudconnectors en de resterende betaal-/uitrolcontroles.
