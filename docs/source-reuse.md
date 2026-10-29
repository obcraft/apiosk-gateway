# Hergebruik zonder tweede betaalimplementatie

`crates/catalog-core` bouwt zelfstandig en deelt nog twee identieke pure modules met `crates/commerce-runtime`: `facts.rs` en `facts/path.rs`. `node scripts/check-source-reuse.mjs` vergelijkt die bestanden byte voor byte en draait daarna de prijscompatibiliteitstest. Beide crates staan in dezelfde repository en workspace.

De adapter-, contract- en schemamodules zijn inmiddels afzonderlijk doorontwikkeld. Hun eigen contracttests bewaken dat gedrag; deze modules mogen niet automatisch over elkaar worden gekopieerd. De prijsmodules hebben verschillende publieke helpers, maar moeten dezelfde financiële bedragen opleveren. `tests/source_reuse.rs` vergelijkt alle prijsdelen voor beide providerklassen, reguliere en workflowtarieven, ongeldige tarieven en afrondingsgrenzen.

Treasury- en x402-handlers zijn ondergebracht in `commerce-runtime` en worden in dezelfde gatewaybinary gelinkt. De v2-executor maakt een geselecteerd endpoint aan en gebruikt de bestaande App `use-selection`/`agent-gateway`-uitvoeringsroute met een stabiele idempotency-key. Die route bewaart de bestaande balansreservering, saldo-afschrijving en refunds en roept de gateway aan; de provider- en betaalafhandeling gebeurt vervolgens in hetzelfde serverproces. De strengere `deterministic`-modus voor named candidates blijft behouden. Er is geen tweede betaaladministratie of apart executorproces.

Bright Data en KVK hebben elk één canonieke crate in de workspace. Zowel de v2-code als `commerce-runtime` verwijst naar diezelfde crate. `node scripts/check-brightdata-crate-sync.mjs` en `python3 scripts/check-kvk-crate-sync.py` controleren de daadwerkelijke Cargo-paddependencies; er worden geen broncodekopieën gesynchroniseerd.

De canonical tool-JSON staat in `gateway/contracts/tool-schemas.json`; `sync-contracts.mjs` kopieert die naar `mcp/src/gateway-v2-contracts.json`. De canonical v2-migratie wordt met `sync-migration.mjs` naar de bestaande App-migratiereeks gekopieerd. Beide scripts hebben `--check` voor CI en wijzigen geen remote systemen.
