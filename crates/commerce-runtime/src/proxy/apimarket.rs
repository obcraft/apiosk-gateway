//! The reviewed Crunchbase routes use the platform key directly from Vault.
//! No key is copied into public listing metadata or returned to a caller.
use sqlx::PgPool;

pub(super) const SOURCE: &str = "apimarket-magicapi-crunchbase-bbcca0a6";
const ORIGIN: &str = "https://prod.api.market/api/v1/magicapi/crunchbase";

fn valid_route(origin: &str, method: &str, path: &str) -> bool {
    origin.trim_end_matches('/') == ORIGIN
        && method == "POST"
        && matches!(path, "/autocompletes" | "/searches/organizations")
}

pub(super) async fn credentials(pool: &PgPool, origin: &str, method: &str, path: &str) -> Result<Vec<(String, String)>, &'static str> {
    if !valid_route(origin, method, path) { return Err("unsupported_crunchbase_route"); }
    let key: Option<String> = sqlx::query_scalar("SELECT decrypted_secret FROM vault.decrypted_secrets WHERE name='APIMARKET_API_KEY'")
        .fetch_optional(pool).await.map_err(|_| "apimarket_key_unavailable")?;
    let key = key.filter(|key| !key.is_empty() && key.len() <= 4096 && key.bytes().all(|b| (33..=126).contains(&b)))
        .ok_or("apimarket_key_unavailable")?;
    Ok(vec![("x-api-market-key".into(), key), ("User-Agent".into(), "ApioskGateway/1.0".into())])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vault_key_is_only_available_for_reviewed_provider_routes() {
        for path in ["/autocompletes", "/searches/organizations"] { assert!(valid_route(ORIGIN, "POST", path)); }
        for origin in ["https://evil.example", "https://prod.api.market.evil.example/api/v1/magicapi/crunchbase", "http://prod.api.market/api/v1/magicapi/crunchbase", "https://prod.api.market/api/v1/magicapi/other"] {
            assert!(!valid_route(origin, "POST", "/autocompletes"));
        }
        for path in ["/entities/organizations", "/../other", "https://evil.example", "/autocompletes?url=https://evil.example"] {
            assert!(!valid_route(ORIGIN, "POST", path));
        }
        assert!(!valid_route(ORIGIN, "GET", "/autocompletes"));
    }
}
