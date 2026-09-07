# Hergebruik zonder tweede betaalimplementatie

`crates/catalog-core` bevat een ongewijzigde kopie van zes pure bestanden uit de bestaande gateway: `facts.rs`, `facts/adapter.rs`, `facts/contract.rs`, `facts/path.rs`, `facts/schema.rs` en `fees.rs`. De relevante contractfixtures zijn meegeleverd. De 84 bijbehorende tests draaien in de nieuwe workspace.

Dit houdt de nieuwe Rust-binary zelfstandig bouwbaar en bewaart de bestaande extractie- en prijsregels. Het is bewuste tijdelijke bronduplicatie. `node scripts/check-source-reuse.mjs` vergelijkt de zes bestanden byte voor byte; voer die check uit bij wijzigingen in een van beide projecten. Een later gedeeld package moet aan beide kanten gecontroleerd worden ingevoerd. Kopieer niet automatisch over een afwijking heen.

Treasury, x402, balance-reservations, saldo-afschrijving en refunds zijn niet gekopieerd. De v2-executor maakt een geselecteerd endpoint aan en gebruikt de bestaande App `use-selection`/`agent-gateway`-uitvoeringsroute met een stabiele idempotency-key. De bestaande gateway kreeg alleen een optionele strengere `deterministic`-modus voor named candidates.

De canonical tool-JSON staat in `gateway-v2/contracts/tool-schemas.json`; `sync-contracts.mjs` kopieert die naar `mcp/src/gateway-v2-contracts.json`. De canonical v2-migratie wordt met `sync-migration.mjs` naar de bestaande App-migratiereeks gekopieerd. Beide scripts hebben `--check` voor CI en wijzigen geen remote systemen.
