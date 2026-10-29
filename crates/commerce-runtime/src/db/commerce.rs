//! Database access for the commerce track (webshop integrations + products).
//! One responsibility per method; business logic lives in `crate::commerce`.
//!
//! `webshop_integrations` is written by the provider dashboard (its own migration
//! on the shared Supabase project); the gateway only READS it here, with the
//! service role, to resolve a public integration id to its domain + wallet.
//! `commerce_products` is gateway-owned (migration 060).

use anyhow::Result;
use uuid::Uuid;

use super::Database;

/// A webshop integration as the resolve endpoint needs it: the anti-abuse guards
/// (domain + URL pattern) plus the wallet to snapshot onto the product.
pub struct WebshopIntegration {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub domain: String,
    pub product_url_pattern: String,
    pub wallet_address: String,
    pub status: String,
}

/// Everything the commerce charge paywall needs about a product: its price (USDC
/// minor units) and the wallet snapshot to advertise the split against.
pub struct CommerceProductCharge {
    pub price_amount: i64,
    pub destination_wallet: String,
}

impl Database {
    /// Resolve a public integration id to its guard config + wallet, or `None` if
    /// unknown. Reads `webshop_integrations` with the gateway's service role
    /// (bypasses RLS). Never trusts anything from the browser payload but the id.
    pub async fn lookup_webshop_integration(&self, id: Uuid) -> Result<Option<WebshopIntegration>> {
        let row: Option<(Uuid, Uuid, String, String, String, String)> = sqlx::query_as(
            r#"SELECT id, owner_id, domain, product_url_pattern, wallet_address, status
                 FROM webshop_integrations
                WHERE id = $1
                LIMIT 1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(
            |(id, owner_id, domain, product_url_pattern, wallet_address, status)| {
                WebshopIntegration {
                    id,
                    owner_id,
                    domain,
                    product_url_pattern,
                    wallet_address,
                    status,
                }
            },
        ))
    }

    /// Upsert a commerce product for `(integration_id, url)`. Idempotent: a
    /// re-scrape updates the name/price/wallet snapshot and returns the existing
    /// id, so a page loaded a thousand times yields one product row.
    pub async fn upsert_commerce_product(
        &self,
        integration_id: Uuid,
        owner_id: Uuid,
        url: &str,
        name: &str,
        price_amount: i64,
        destination_wallet: &str,
    ) -> Result<Uuid> {
        let row: (Uuid,) = sqlx::query_as(
            r#"INSERT INTO commerce_products
                   (integration_id, owner_id, url, name, price_amount, destination_wallet)
                 VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (integration_id, url) DO UPDATE
                   SET name = EXCLUDED.name,
                       price_amount = EXCLUDED.price_amount,
                       destination_wallet = EXCLUDED.destination_wallet,
                       updated_at = now()
              RETURNING id"#,
        )
        .bind(integration_id)
        .bind(owner_id)
        .bind(url)
        .bind(name)
        .bind(price_amount)
        .bind(destination_wallet)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.0)
    }

    /// Load a commerce product's price + destination wallet for the charge paywall,
    /// or `None` if the id is unknown.
    pub async fn get_commerce_product_charge(
        &self,
        id: Uuid,
    ) -> Result<Option<CommerceProductCharge>> {
        let row: Option<(i64, String)> = sqlx::query_as(
            "SELECT price_amount, destination_wallet FROM commerce_products WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(
            row.map(|(price_amount, destination_wallet)| CommerceProductCharge {
                price_amount,
                destination_wallet,
            }),
        )
    }
}
