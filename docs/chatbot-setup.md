# ChatGPT, Claude en App aansluiten op v2

## Eén contract

De externe MCP-server en WebMCP in de App publiceren in v2-modus precies `apiosk_discover` en `apiosk_execute`. De canonieke schema's en instructies staan in `contracts/`; `node scripts/sync-contracts.mjs --check` controleert beide clientkopieën. MCP initialize, serverkaart en instructieresource gebruiken dezelfde tekst. De App laat de persoon een voorstel goedkeuren; de chatbot schrijft het antwoord uit de brongegevens.

Voor een nieuwe vraag: discover. Voor ontbrekende invoer, een keuze, vervolgactie, resultaatread of voortgang: execute met de aangeboden actie. Bij kwijtgeraakte state: execute met alleen `recover_task_ref`. Een ander request-ID mag een betaalactie niet veranderen in een tweede aankoop. Accountverbinding en toestemming voor een specifiek voorstel zijn aparte controles.

## Lokaal

- App: http://localhost:5276/
- Gateway: http://127.0.0.1:8082/health
- MCP: http://localhost:3002/mcp

Start de gateway vanuit `gateway-v2` met `./target/release/apiosk-gateway-v2`. Start de App vanuit `app` met `npm run dev -- --host localhost`. Start MCP vanuit `mcp` met `npm run dev:v2`; dit leest de genegeerde `.env.v2.local`. De MCP OAuth-secret is een aparte serversecret. Treasury-, Supabase service-role- en modelkeys horen nooit in de MCP-clientconfiguratie.

Voor Claude Code op deze machine:

```sh
claude mcp add --transport http apiosk-v2 http://localhost:3002/mcp
```

Gebruik de normale OAuth-aanmelding. De server publiceert protected-resource metadata en ondersteunt PKCE via de bestaande Apiosk-aanmeldroute. Een lokale HTTP-endpoint is geen bereikbare cloudconnector voor Claude of een gewone publieke ChatGPT-verbinding.

## ChatGPT en Claude cloud

Rol dezelfde MCP-build uit achter een HTTPS-endpoint. Configureer daar `APIOSK_GATEWAY_V2_URL` naar de bereikbare v2-backend, `APIOSK_MCP_PUBLIC_BASE_URL` naar de MCP-origin en een eigen `APIOSK_MCP_OAUTH_SECRET`. De gateway heeft een publiek bereikbare App-origin nodig voor het goedkeuringsscherm. Stel `VITE_GATEWAY_V2_MCP_URL` in de App in op precies de uitgerolde v2-MCP-URL inclusief `/mcp`.

ChatGPT: zet developer mode aan waar je account/workspace dit toestaat, voeg de MCP-endpoint toe via Plugins, verbind je Apiosk-account en controleer dat precies twee tools verschijnen. Voor privéontwikkeling ondersteunt ChatGPT ook Secure MCP Tunnel. Vernieuw connector-metadata na een toolwijziging en begin een nieuwe chat. Zie [OpenAI verbinden en testen](https://developers.openai.com/plugins/deploy/connect-chatgpt) en [OAuth voor tools](https://developers.openai.com/plugins/build/auth).

Claude: open Customize → Connectors → Add custom connector, vul de remote MCP-URL in en verbind je Apiosk-account. Zie [Anthropic: custom connectors](https://support.claude.com/en/articles/11175166-get-started-with-custom-connectors-using-remote-mcp). Beschikbaarheid en organisatiebeleid kunnen verschillen.

De App stuurt een v2-gebruiker niet stilzwijgend naar de bestaande v1-connector. Zonder geconfigureerde remote v2-URL toont de App dat cloudverbinding nog moet worden ingericht. Een bestaande accountverbinding bewijst niet dat een geïnstalleerde chatbot de nieuwe toolversie heeft geladen.

## Dezelfde bronnen, gerichte uitvoering

V2 leest dezelfde publieke Supabase-catalogustabellen als v1. De App Sources-pagina behoudt de bestaande broninventaris. De v2-planner gebruikt alleen actieve, gevalideerde read/per-call-uitvoeringscontracten; meer listings tonen betekent niet dat elke listing veilig uitvoerbaar is. Niet-ondersteunde voorwaarden worden zichtbaar teruggegeven.

Efficiëntie komt uit een gecachte catalogus, één intentparse per nieuwe vraag, deterministische vervolgstappen, gedeelde afhankelijkheden en hergebruik van bestaande resultaten. Iedere betaalstap controleert opnieuw het actuele contract en de prijs. Er is nog geen vergelijkende productiebenchmark die aantoont hoeveel sneller of beter v2 dan v1 is.

## Aantoonbaar getest en resterende releasecontroles

`node scripts/check-v2-http.mjs` in MCP controleert het echte lokale HTTP-protocol: initialize, twee tools, dezelfde instructies en resource, OAuth-metadata en weigering zonder account. De SQLx/MCP-fixturetest controleert goedkeuring, idempotentie, resultaten en herstel. De browsercontrole bevestigt de twee v2-WebMCP-tools in de ingelogde App. De echte Gemini 3.8 Flash-test gebruikt uitsluitend een synthetische catalogus; geen productiegegevens of providerbetalingen.

Voor een productiebetaling moeten de deterministische wijzigingen in de oorspronkelijke betaalgateway en App Edge Functions samen uitgerold en gecontroleerd zijn. Treasury-signing blijft in die betaalgateway. De lokale aanwezigheid van `TREASURY_PRIVATE_KEY` in v2 configureert niet automatisch de gedeployde betaalexecutor. `V2_EXECUTION_ENABLED=0` blijft gelden tot de volledige betaalroute is geverifieerd.

Gebruik in beide geïnstalleerde chatbots dezelfde acceptatiescenario's: een nieuwe vraag met verplichte bron/periode; ontbrekend ID; meerdere matches; weigering zonder App-toestemming; een goedgekeurde begrensde taak; verbroken verbinding gevolgd door recovery; resultaat herlezen zonder aankoop; annuleren; verlopen accountverbinding. Registreer resultaat, feitelijke kosten, brondekking en eventuele ontbrekende stap. Protocoltests alleen zijn geen bewijs dat die geïnstalleerde clients al zijn getest.
