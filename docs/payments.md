# Betalingen: bestaande gateway, aangescherpt voor v2

De gebruikersinstructie is de bestaande gatewaybetaalroute te behouden en die passend te maken voor v2. De bijlage van 7 september 2026 is gebruikt als ontwerpcontract en om verschillen met de bestaande implementatie zichtbaar te maken.

## Wat blijft leidend

Het bestaande Apiosk-saldo en de App-RPC's reserveren, verrekenen en geven bedragen terug. De oorspronkelijke gateway voert providerverkeer, treasury-signing, x402 en settlementregistratie uit. V2 maakt geen tweede ledger, verstuurt geen wallettransfer en vraagt de gebruiker geen eigen providerkey of walletsignature. Er wordt geen echte betaling uitgevoerd tijdens deze implementatietests.

Voor de koper blijft het tarief: providerprijs plus 15%, per uitvoering naar boven afgerond in gehele micro-eenheden. De bestaande `fees.rs` is leidend. V2 controleert de afrekening opnieuw tegen die regel voordat vervolgstappen worden vrijgegeven. Een ontbrekende uitsplitsing of extra/dubbele fee blijft onopgelost en wordt niet als correcte afrekening geaccepteerd.

De oude gateway heeft ook een managed-providerfee aan de aanbiederskant. Die is niet dezelfde fee als de 15% kopersopslag en is in deze aansluiting niet gewijzigd. Daarom labelt de betaalprojectie de kostenbasis als `existing_gateway_provider_tariff`. Een catalogustarief of managed-providerlistprijs wordt niet gepresenteerd als onafhankelijk bewezen inkoopkosten of als het exacte bedrag dat een resell-provider onchain ontving.

## Nieuwe aanscherpingen

- `billing` staat naast `status` en `result` in de bestaande v2-envelope. Een beschikbaar resultaat is geen bewijs van definitieve externe settlement.
- Betaalbedragen zijn USD-microstrings; `funding_source` is `apiosk_balance`. De bestaande `proposal.currency=USDC`/atomicvelden blijven compatibel met de oude quote; er wordt geen nieuwe wisselkoers toegepast.
- De projectie leest de bestaande `balance_charges` en `user_balances` met accountbinding. Zij schrijft daar niets. Een autorisatiecap of v2-dispatchrecord wordt nooit als een echte saldoreservering geboekt.
- `quote_cap` is de offerte; `max_authorized` is de nog geldige opgeslagen toestemming en is vóór toestemming nul. Kosten gelden per huidige quote; individuele regels bevatten execution-ID en de echte charge-ID als `receipt_ref`.
- `reserved`, `pending_reconciliation`, `captured`, `released` en `refunded` worden uit de bestaande ledger afgeleid. `not_started` geeft expliciet aan dat nog geen aankoop is begonnen. Een onbekende dispatch zonder ledgerbewijs blijft onopgelost.
- Het bestaande beschikbare saldo is al verminderd met actieve reserveringen. V2 trekt die niet nogmaals af. Later bevestigde refunds zijn bij taakherstel direct zichtbaar, ook wanneer het bronresultaat bewaard blijft.
- Een expliciet geweigerde aanvraag vóór reservering, zonder execution-ID en zonder ledgerregel, eindigt met nul besteed. Daardoor wordt onvoldoende saldo niet meer een eindeloze poll. Onbekende fouten na verzending houden hun allocatie vast; een onzekere `charged_micro_usd: 0` maakt geen budget vrij.
- Voor v2 zet de bestaande App-executor zijn automatische tweede poging met een nieuwe gateway-idempotency-key uit. Legacyclients behouden hun bestaande gedrag. Iedere v2-actie blijft aan dezelfde uitvoeringsidentiteit gekoppeld.
- App en MCP presenteren betaling uit het interne saldo, microbedragen, status, fee en betaalbonnen. Secrets en treasurycredentials worden niet opgenomen.

## Verschillen tussen de bijlage en de bestaande gateway

De bijlage beschrijft ook een uitgebreidere kostenadministratie: variabele token-/creditmeting, kostentoerekening voor abonnementen, een gewone API-route zonder x402-walletbetaling, een volledig append-only dubbelboekhoudingsmodel en automatische compensatie na een vaste onderzoekstermijn. Dat is niet allemaal aanwezig in de bestaande gateway.

Specifiek: de oude `proxy/settlement.rs` gebruikt voor managed listings zijn hosted-providerbetaalroute. Het enkel toevoegen van `deterministic: true` verandert dat niet in een nieuwe credit-/API-keybetaalroute. Een aparte normale API-route uit de bijlage is daarom niet als geïmplementeerd aangemerkt. Daarvoor zijn geversioneerde kostenbasis, providerusagebewijs en expliciete routemetadata nodig; de huidige v2-loader beperkt zich tot bestaande gevalideerde read/per-call-contracten.

Ook een maximale onderzoekstermijn met automatische gebruikerscompensatie is nog een expliciet operationeel/financieel implementatiepunt. V2 geeft onbekende treasuryverplichtingen niet op eigen initiatief vrij. De bestaande financiële reconciliatie blijft eigenaar. Een latere providerrefund of creditinkoop veroorzaakt vanuit v2 geen nieuwe gebruikersboeking.

## Verificatie

Laatste controle: 96 gewone Rust-tests, de afzonderlijke SQLx/MCP-ketentest, 103 bestaande gateway-proxytests en 113 MCP-tests geslaagd. De volledige App-suite telt nu 1.825 geslaagde tests; de aanpassing aan de oude één-prijsregel is beperkt tot de standaard gesloten v2-betaalbon. TypeScript/Vite-build, Clippy en de geoptimaliseerde Rust-releasebuild slagen.

De lokale SQLx-ketentest controleert saldotekort zonder provideruitvoering, nulafboeking, kosten/fee/charge-ID uit de ledger, geen dubbele saldoaftrek, late refundweergave, onbekende uitkomst, herstel en accountisolatie. Pure Rust-tests controleren $0,20 + $0,03 = $0,23, microafronding en weigering van ontbrekende/dubbele fees. App-tests controleren één uitvoeringspoging voor v2 en volledige vrijgave bij bewezen pre-payment failure, plus afzonderlijke betaalstatusweergave.

```sh
# gateway-v2
cargo test --workspace --offline
node scripts/test-integration.mjs
cargo clippy --workspace --all-targets --offline -- -D warnings

# app
NODE_OPTIONS=--no-experimental-webstorage npm test
npm run build

# gateway: bestaande betaal-/proxycode, geen live probe
cargo test --offline --bin apiosk-gateway proxy::

# mcp
npm test
```

De bestaande proxytests: 103 geslaagd; de live betaalprobe is ignored. De v2 SQLx-/MCP-ketentest slaagt met fixtures. Deze controles bewijzen geen echte providerbetaling, treasuryliquiditeit of productie-settlement. Voor uitrol moet de bijgewerkte `supabase/runtime-role.sql` worden toegepast voor uitsluitend aanvullende leesrechten op saldo en kostenkolommen; daarnaast moeten de aangepaste App Edge Functions, v2-binary en clients samen worden uitgerold zoals in de [runbook](runbook.md).
