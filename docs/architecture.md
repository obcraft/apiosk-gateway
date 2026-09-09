# Architectuur en uitvoeringsgrenzen

## Eén eigenaar per verantwoordelijkheid

De chatbot bewaakt het gesprek en schrijft het antwoord. Een nieuwe datavraag gaat eenmaal naar de intentparser. De parser levert uitsluitend getypeerde onderwerpen, doelen, constraints en antwoordbewerkingen. Nieuwe identifiers moeten letterlijk uit de vraag komen of via bestaande entiteitsreferenties worden aangewezen. De parser selecteert geen provider, verzint geen facts en voert geen tools uit. Ongeldige output wordt niet via een tweede modelcall gerepareerd.

Als de resolver de getypeerde aanvraag niet kan plannen, beoordeelt de plannings-AI de oorspronkelijke vraag, gesprekscontext, bronidentiteiten en relevante gevalideerde input/output-contracten. Zij kan bronaliassen naar bestaande bronnen vertalen en een gecorrigeerd intent teruggeven; de compiler valideert opnieuw alle stappen, afhankelijkheden en prijzen. Expliciete periodes en andere voorwaarden blijven behouden. Een niet-uitvoerbare wens leidt tot een opgeslagen `needs_input`-vervolgvraag met een concreet alternatief. Dat alternatief wordt pas na een antwoord van de gebruiker gepland. App en MCP gebruiken dezelfde beoordeling. Herladen van de taak herhaalt geen modelcall of aankoop.

De resolver kiest op basis van gevalideerde Supabase-contracten een plan. Hij hergebruikt niet-verlopen facts voor dezelfde entiteit, weigert conflicterende waarden, dedupliceert gedeelde stappen en zoekt begrensd door de afhankelijkheden. Prijsberekening gebruikt de bestaande integer micro-USDC-regels uit `gateway/src/fees.rs`. Een plan bevat contractsnapshots, een hash, een quote-ID, een cap en een vervaltijd.

De executor koopt alleen een volledig gebonden endpoint uit dat voorstel. Vóór iedere dispatch controleert hij opnieuw catalogusversie, prijs, account, verbinding, revisie, toestemming en resterende cap. De HTTP-call gaat naar de bestaande App-executor, die saldo en treasury beheert. Het nieuwe `deterministic`-veld dwingt in de bestaande gateway een named candidate met uitsluitend aangeleverde inputs af, zonder extra resolveraankoop of intentmodel. Beide kanten moeten samen worden uitgerold.

## Gesprek en herstel

De envelope bevat een taakreferentie, revisie, focus, vervaltijd en HMAC. De server bewaart de volledige facts en bronresultaten in het private `gateway_v2`-schema. Een envelope is accountgebonden; writes moeten bovendien bij dezelfde agentverbinding horen. De eigenaar kan in de App een agenttaak bekijken en toestemming geven, maar de App neemt diens uitvoeringsverbinding niet over.

`context_delta` voegt expliciete waarden toe. Clients mogen facts, revisies of signatures niet herschrijven. Een stale of verlopen envelope levert actuele state terug zonder uitvoering. Via `apiosk_execute({recover_task_ref})` vertaalt de MCP-adapter taakherstel naar een geauthenticeerde GET. Contextverlies wordt zo hersteld zonder een derde tool. Native hostgeheugen wordt niet verondersteld.

Een requestreceipt dedupliceert dezelfde request-ID en inhoud. Een afzonderlijk uitvoeringsjournal bindt action-ID en idempotency-ID aan account, taak, contractsnapshot, entiteit en inputs. Dispatch wordt gejournaliseerd voordat netwerk-I/O begint. Database-transacties blijven kort en houden geen locks tijdens providerverkeer.

## Betaling en concurrentie

Alleen een door Supabase Auth geverifieerde App-sessie kan het exacte, actuele voorstel autoriseren. Een agenttoken, `approved: true`, statetoken of toolcall telt niet als toestemming. Eén toestemming dekt vervolgstappen binnen hetzelfde voorstel en de cap. De bestaande agentlimieten en saldocontroles blijven aanvullend gelden.

Taak- en autorisatierijen worden gelockt voor dispatch. Per taak is maximaal één onopgeloste uitvoering toegestaan. Gealloceerde kosten omvatten ook onbekende uitkomsten; daardoor ontstaat na een timeout geen vrij budget waarmee onbedoeld opnieuw kan worden gekocht. De v2-allocatie is een toestemmingscontrole, geen financieel saldo.

Bij timeout of onbekende uitkomst wordt niet automatisch opnieuw verstuurd. Polling leest `balance_charges` en `call_runs`. Een bewezen terugbetaling eindigt als fout met nul werkelijk besteed; een geslaagde ledger/run-combinatie herstelt het resultaat. Een onduidelijke charge blijft zichtbaar als lopend/onopgelost en vereist bestaande financiële reconciliatie. Er is geen claim van end-to-end exactly-once bij een externe provider.

Een bronresultaat moet ook in de taakstate zijn gekoppeld voordat een nieuwe intent het herstelplan mag vervangen. Compare-and-swap voorkomt dat parallelle updates stilzwijgend context overschrijven. Deze mechanismen zijn lokaal functioneel getest; druktests met meerdere echte PostgreSQL-verbindingen zijn nog een releasegate.

## Catalogus en antwoorddekking

De loader leest `capability_execution_contracts`, `fact_types`, `capabilities`, `api_endpoints` en `apis`. Alleen actieve, gevalideerde read/per-call-contracten bij actieve, geverifieerde, zichtbare en niet-geschorste providers worden toegelaten. Sample-output en mappings worden bij het laden gecontroleerd. Ongeldige of niet-ondersteunde contracten komen in quarantine.

Ondersteund: de bestaande contractextractors, één subject-role, simpele inputbinding zonder wrap, expliciete bron/jurisdictieconstraints, getypeerde afhankelijkheden en één providerdispatch per stap. Meerdere identiteitshits vereisen selectie. Ontbrekende velden, upstream-truncatie, onbruikbare schema's en overschrijding van de factcapaciteit leveren gedeeltelijke dekking op; ruwe beschikbare resultaten blijven leesbaar.

Nog niet geïmplementeerd: generieke pagination/fan-out, multi-role/wrap/precondition-uitvoering, selectie van 'de laatste twee' op betrouwbare filingtijd, generieke PDF/OCR-extractie, conversie van verslagbasis/valuta en een conflict-oplosinterface voor reeds opgeslagen tegengestelde facts. Zulke vereisten worden geweigerd of als ontbrekende dekking teruggegeven. Er wordt geen andere betaalde bron stilzwijgend gekozen. Antwoordbewerkingen behoren aan de host; opgeslagen resultaten kunnen zonder nieuwe aankoop gelezen worden.

Bronverwijzingen bevatten provider, beschikbare catalogus-URL, endpoint, contractversie, ophaaltijd en daadwerkelijk berekende kosten. Een catalogus-URL kan documentatie zijn; dit is niet automatisch de canonieke URL van een specifieke filing. Facts verwijzen tevens naar resultaat en extractiepad. Financiële eindconclusies moeten uit deze data volgen; afwezigheid is geen nul.

## Limieten en schaalbaarheid

| Grens | Huidige instelling |
| --- | --- |
| Nieuwe vraag | Maximaal 4.000 bytes; één modelcall, 25 s timeout, geen automatische retry |
| Parsercache | 1.024 items, 10 minuten; key omvat account, context, vraag, model en catalogusversie |
| Cataloguscache | 60 s met single-flight; vóór een aankoop opnieuw gevalideerd |
| Resolver | Maximaal 12 stappen, diepte 5, 256 expansies |
| Requests | Maximaal 60 nieuwe requestreceipts per account per minuut |
| HTTP | 64 KiB input; 32 gelijktijdige discover/execute-requests per proces |
| Databasepool | 16 verbindingen, 5 s acquire-timeout, 10 s statement-timeout |
| State | 100 entiteiten, 1.000 facts, 100 resultaten, 1 MiB taakdocument |
| Resultaten | 4 MiB upstream; 32 KiB inline; grotere resultaten via 8.000-karakterpagina's |
| Retentie | Taak/receipts 30 dagen; onopgeloste uitvoeringen blijven behouden |

Meerdere processen kunnen dezelfde database en signing key gebruiken. Caches zijn lokaal en mogen verdwijnen; financiële en gesprekswaarheid staan in de database. Dit is een begrensde basis, geen bewezen maximale throughput. Productie-p50/p95, modelkwaliteit, tokengebruik en multi-instance capaciteit moeten met echte stagingdata worden gemeten.

## Beveiligingsgrens

Supabase Auth verifieert menselijke sessies; bestaande token-RPC's verifiëren agentverbindingen. Geen decode-only JWT-identiteit, geen opgeslagen bearer tokens. De runtime-role krijgt alleen specifieke catalogus-/ledgerkolommen, private state en een geselecteerde endpointinsert; geen providercredentials, saldo-updates of treasuryrechten. Het private schema heeft RLS en geen toegang voor `anon`/`authenticated`.

De server is een vertrouwde accountoverschrijdende dienst: ownership wordt op elke taakroute en query afgedwongen. De runtime-role heeft daarom brede toegang binnen het private schema en mag nooit aan een browser worden gegeven. Niet-lokale databaseverbindingen gebruiken certificaatverificatie; externe HTTP-URLs vereisen HTTPS en redirects zijn uit. CORS beperkt browsertoegang tot de ingestelde App-origin. Providertekst blijft onbetrouwbare data.
