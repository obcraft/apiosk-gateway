# Lokale setup, uitrol en herstel

## Werkdirectories

Gebruik `gateway-v2` voor de nieuwe Rust-server, `mcp` voor de bestaande connectorserver en `app` voor de App en Supabase Edge Functions. `gateway` blijft de bestaande treasury/execution-service. Er is geen tweede financiële administratie toegevoegd.

## Configuratie

Kopieer `.env.example` naar `.env` en vul waarden via de bestaande secretvoorziening in. Commit geen credentials. `DATABASE_URL` hoort bij een dedicated login met de rol `gateway_v2_runtime`. De NOLOGIN-groepsrol zelf is geen verbindingsaccount. `SUPABASE_PUBLISHABLE_KEY` is de openbare projectkey; de server gebruikt geen service-role key om een menselijke gebruiker te imiteren. `STATE_SIGNING_KEY` moet minstens 32 willekeurige bytes bevatten en gelijk zijn voor alle v2-instances.

Gebruik voor een langlevende SQLx-server een directe of session-poolerverbinding. De binary dwingt buiten loopback `verify-full` TLS af. Configureer zo nodig het passende CA-certificaat in de databaseconnectie. Stel de poolgrootte per replica af op het totale PostgreSQL-budget; de huidige binary gebruikt 16 verbindingen per proces. Zie [Supabase: verbindingen](https://supabase.com/docs/guides/database/connecting-to-postgres).

`INTENT_API_URL`, `INTENT_API_KEY` en `INTENT_MODEL` horen bij de ene intentparser. Er is geen automatische model-fallback. Zonder key blijft een nieuwe vraag expliciet niet beschikbaar; resultaatreads en reeds bestaande state vereisen geen nieuw model. De ingestelde voorbeeldmodelnaam is configuratie, geen gemeten kwaliteitsgarantie.

`APP_ORIGIN` moet de App-origin exact bevatten. `SUPABASE_URL` wijst naar het project waarvan ook catalogus, accounts en ledger afkomstig zijn. `V2_EXECUTION_ENABLED=0` is de standaard. Zet die pas aan wanneer dezelfde omgeving ook de deterministische App/v1-executorroute bevat.

## Database en releasevolgorde

De nieuwe migratie is via de Supabase CLI aangemaakt. De canonieke kopie staat onder `gateway-v2/supabase/migrations/`; `scripts/sync-migration.mjs` synchroniseert exact dezelfde SQL naar de bestaande App-migratiereeks. Pas de migratie niet onafhankelijk in beide histories toe. De bestandsversies zijn afgestemd op de daadwerkelijke gedeelde Supabase-historie: state `20260907094100`, runtime access `20260907095555`. Beide zijn op 7 september 2026 toegepast; de runtime access-migratie na expliciete toestemming. `--check` detecteert drift.

Voer een eerste rollout uit in een gecontroleerde stagingomgeving:

1. Synchroniseer contracten en migratie. Bekijk vanuit `app` met `npx supabase db push --linked --dry-run` welke migraties klaarstaan. Controleer het gekoppelde project en alle eerdere pending migraties. De v2-binary voert zelf nooit DDL uit.
2. Pas via de bestaande App-migratieprocedure de nieuwe private state-migratie toe. Laat een DBA daarna de runtime access-migratie (canonieke SQL ook in `gateway-v2/supabase/runtime-role.sql`) toepassen en de dedicated login aan `gateway_v2_runtime` koppelen. Dit bestand bevat geen wachtwoord. Bestaande applicatierollen krijgen geen nieuwe v2-rechten.
3. Rol de kleine aanpassing in `gateway/src/v1_routes/do_job.rs` uit. Rol vervolgens de App Edge Functions uit die `_shared/request.ts` gebruiken: `use-selection` en `agent-gateway`. De nieuwe `deterministic`-flag moet overal worden doorgegeven voordat v2 aankopen toestaat. Alleen frontenddeploy is daarvoor onvoldoende.
4. Start de v2-binary met uitvoeringen uitgeschakeld. Controleer startup/catalogus, authenticatie, een echte intentparse, prijsvoorstel en weigering zonder toestemming.
5. Stel `APIOSK_GATEWAY_V2_URL` in op de MCP-server en `VITE_GATEWAY_V2_URL` op de App-build. Laat de bestaande MCP-OAuth/tokenafhandeling intact. De App-route `/?task=<uuid>` (Ask-taakweergave) biedt toestemming voor een agenttaak; oude `/gateway-v2?task=<uuid>`-links sturen daarheen door. Voor remote clients is een bereikbaar HTTPS-adres nodig; hun localhost is niet deze ontwikkelmachine.
6. Voer in staging een toegestaan, begrensd aankoopscenario uit, controleer de bestaande ledger/treasury en test procesonderbreking, gelijktijdige retries, ingetrokken verbinding en quote-expiry. Zet uitvoering alleen voor die gecontroleerde omgeving aan. Verifieer daarna de geïnstalleerde ChatGPT- en Claude-clients afzonderlijk.

Productiedeployment, publicatie en echte betalingen zijn in deze oplevering niet uitgevoerd. De lokale tests gebruiken tijdelijke testidentiteiten; zet de fixture-authserver nooit in een normale runtimeconfiguratie.

## Herstellen van taken

Bij contextverlies kan de host `apiosk_execute` met uitsluitend `recover_task_ref` en eventueel een request-ID gebruiken. De gateway geeft de huidige accountgebonden envelope terug. Bij `state_conflict` neemt de host de actuele state over en beoordeelt opnieuw welke actie nog nodig is. Hij maakt geen vervangende betaalde actie aan.

Bij een bekende actie blijven `action_id` en `idempotency_key` bij een netwerkretry gelijk. Een ontbrekende requestresponse kan betekenen dat de uitvoering wel is gestart. Lees dan de taak en gebruik de aangeboden poll/herstelactie. Een oude discover-request die midden in de parsercall is onderbroken wordt niet automatisch opnieuw geparseerd; herstel de taak of begin bewust een nieuwe vraag met een nieuwe request-ID.

`running` na onbekende betaaluitkomst houdt de allocatie vast. Polling koopt niets opnieuw. Als charge en run niet eenduidig bewijzen wat is gebeurd, moet de bestaande financiële reconciliatie dit oplossen. Wijzig de v2-journalstatus niet handmatig om alsnog opnieuw te kunnen dispatchen. Cancel stopt volgende stappen; een al verstuurde providerrequest wordt daarmee niet teruggedraaid.

Signing-keyrotatie maakt oude envelopes ongeldig. Geauthenticeerd taakherstel levert een nieuwe signature op. Alle actieve instances moeten dezelfde keyversie gebruiken tijdens een gecontroleerde omwisseling. Taakdata verloopt na 30 dagen; onopgeloste financiële uitvoering wordt door retentie niet weggegooid.

## Terugschakelen

Schakel v2-uitvoering uit, verwijder `APIOSK_GATEWAY_V2_URL` uit de MCP-configuratie en verwijder `VITE_GATEWAY_V2_URL` uit de App-buildconfiguratie; herstart/herbouw die services. Daarmee keren de elf bestaande MCP-tools en de bestaande App-flow terug. De kleine v1-flag blijft achterwaarts compatibel omdat de standaard `false` is.

Bewaar v2-tabellen, resultaten en journal tijdens rollback voor reconciliatie. Verwijder geen onbekende uitvoeringen of bestaande ledgerregels. Verifieer eerst dat lopende requests zijn afgerond voordat de v2-server wordt gestopt.
