# Hergebruik zonder tweede betaalimplementatie

`crates/catalog-core` bouwt zelfstandig en deelt nog twee identieke pure modules met `legacy-executor`: `facts.rs` en `facts/path.rs`. `node scripts/check-source-reuse.mjs` vergelijkt die bestanden byte voor byte en draait daarna de prijscompatibiliteitstest.

De adapter-, contract- en schemamodules zijn inmiddels afzonderlijk doorontwikkeld. Hun eigen contracttests bewaken dat gedrag; deze modules mogen niet automatisch over elkaar worden gekopieerd. De prijsmodules hebben verschillende publieke helpers, maar moeten dezelfde financiële bedragen opleveren. `tests/source_reuse.rs` vergelijkt alle prijsdelen voor beide providerklassen, reguliere en workflowtarieven, ongeldige tarieven en afrondingsgrenzen.

Treasury, x402, balance-reservations, saldo-afschrijving en refunds zijn niet gekopieerd. De v2-executor maakt een geselecteerd endpoint aan en gebruikt de bestaande App `use-selection`/`agent-gateway`-uitvoeringsroute met een stabiele idempotency-key. De bestaande gateway kreeg alleen een optionele strengere `deterministic`-modus voor named candidates.

De canonical tool-JSON staat in `gateway/contracts/tool-schemas.json`; `sync-contracts.mjs` kopieert die naar `mcp/src/gateway-v2-contracts.json`. De canonical v2-migratie wordt met `sync-migration.mjs` naar de bestaande App-migratiereeks gekopieerd. Beide scripts hebben `--check` voor CI en wijzigen geen remote systemen.
