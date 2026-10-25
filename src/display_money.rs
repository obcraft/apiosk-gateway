//! Display currency only; quote authorization and ledger amounts stay in micro USD.
//!
//! Every account reads amounts in euro. The one display input is the latest
//! published euro rate per UTC day, cached here. A caller that knows the rate
//! changed can invalidate it at once through `rates_changed` or `clear`;
//! absent that, a rate never outlives its TTL. A failed read is never cached.
use crate::store::Store;
use chrono::NaiveDate;
use serde_json::{json, Value};
use sqlx::Row;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use uuid::Uuid;

/// The only currency amounts are shown in.
const DISPLAY_CURRENCY: &str = "EUR";
/// Rates are published at most once per UTC day; a few minutes of staleness is safe.
const RATE_TTL: Duration = Duration::from_secs(300);

#[derive(Clone)]
struct Rate {
    rate: String,
    as_of_date: NaiveDate,
}

struct DisplayCache {
    rates: moka::future::Cache<(String, NaiveDate), Option<Rate>>,
}

fn cache() -> &'static DisplayCache {
    static CACHE: OnceLock<DisplayCache> = OnceLock::new();
    CACHE.get_or_init(|| DisplayCache {
        rates: moka::future::Cache::builder().max_capacity(256).time_to_live(RATE_TTL).build(),
    })
}

/// A rate was published, corrected or withdrawn.
pub async fn rates_changed() {
    cache().rates.invalidate_all();
}

/// Forget every cached display input.
pub async fn clear() {
    cache().rates.invalidate_all();
}

/// Reads `key` through `cache`, loading it once for concurrent callers. A failed load is never cached.
async fn cached<K, T, F>(cache: &moka::future::Cache<K, T>, key: K, load: impl FnOnce() -> F) -> Result<T, Arc<sqlx::Error>>
where
    K: std::hash::Hash + Eq + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    F: std::future::Future<Output = Result<T, sqlx::Error>>,
{
    cache.try_get_with(key, load()).await
}

impl Store {
    /// The same for every account; `_owner` keeps callers unchanged.
    pub async fn money_display(&self, _owner: Uuid) -> Value {
        let today = chrono::Utc::now().date_naive();
        let display: Result<Value, Arc<sqlx::Error>> = async {
            let rate = cached(&cache().rates, (DISPLAY_CURRENCY.to_owned(), today), || self.display_rate(DISPLAY_CURRENCY, today)).await?;
            Ok(json!({"preferred_currency":DISPLAY_CURRENCY,"rate":rate.as_ref().map(|r| &r.rate),"as_of_date":rate.as_ref().map(|r| r.as_of_date)}))
        }.await;
        match display {
            Ok(value) => resolve_display(value),
            Err(error) => {
                tracing::warn!(error = %error, "display exchange rate unavailable");
                json!({"base_currency":"USD","currency":"USD","rate":"1","fallback_reason":"display_currency_unavailable"})
            }
        }
    }

    /// The latest rate published on or before `day`; a later-dated row is not yet valid.
    async fn display_rate(&self, currency: &str, day: NaiveDate) -> Result<Option<Rate>, sqlx::Error> {
        let _db = crate::telemetry::db("money_display");
        let row = sqlx::query("SELECT rate::text AS rate,as_of_date FROM billing.fx_rates WHERE quote_currency=$1 AND as_of_date<=$2 ORDER BY as_of_date DESC LIMIT 1")
            .bind(currency).bind(day).fetch_optional(&self.pool).await?;
        Ok(row.map(|row| Rate { rate: row.get("rate"), as_of_date: row.get("as_of_date") }))
    }
}

fn resolve_display(value: Value) -> Value {
    let preferred = value["preferred_currency"].as_str().unwrap_or("EUR");
    let valid_currency = preferred.len() == 3 && preferred.bytes().all(|b| b.is_ascii_uppercase());
    let rate = value["rate"].as_str().unwrap_or("");
    // Match the bounded decimal representation consumed by the exact client formatter.
    let parts: Vec<_> = rate.split('.').collect();
    let valid_rate = parts.len() <= 2 && parts.iter().all(|p| !p.is_empty() && p.len() <= 18 && p.bytes().all(|b| b.is_ascii_digit()))
        && rate.bytes().any(|b| matches!(b, b'1'..=b'9'));
    let available = valid_currency && (preferred == "USD" || valid_rate);
    json!({
        "base_currency":"USD", "preferred_currency":preferred,
        "currency":if available { preferred } else { "USD" },
        "rate":if preferred == "USD" || !available { "1" } else { rate },
        "as_of_date":if available && preferred != "USD" { value["as_of_date"].clone() } else { Value::Null },
        "fallback_reason":if available { Value::Null } else { json!("exchange_rate_unavailable") }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_currency_uses_published_decimal_rate_and_date() {
        let value = resolve_display(json!({"preferred_currency":"EUR","rate":"0.92000000","as_of_date":"2026-08-21"}));
        assert_eq!(value["currency"], "EUR");
        assert_eq!(value["rate"], "0.92000000");
        assert_eq!(value["as_of_date"], "2026-08-21");
        assert!(value["fallback_reason"].is_null());
        assert_eq!(resolve_display(json!({"preferred_currency":"USD"}))["rate"], "1");
        assert_eq!(resolve_display(json!({"preferred_currency":"JPY","rate":"150.123"}))["currency"], "JPY");
    }

    #[test]
    fn missing_or_invalid_rate_never_relabels_unconverted_dollars() {
        for rate in [Value::Null, json!("0"), json!("NaN"), json!("-0.9"), json!("1e2"), json!("1.2.3")] {
            let value = resolve_display(json!({"preferred_currency":"EUR","rate":rate}));
            assert_eq!(value["preferred_currency"], "EUR");
            assert_eq!(value["currency"], "USD");
            assert_eq!(value["rate"], "1");
            assert_eq!(value["fallback_reason"], "exchange_rate_unavailable");
        }
    }

    #[tokio::test]
    async fn concurrent_reads_of_the_same_key_load_once_and_a_failed_load_is_never_cached() {
        let cache: moka::future::Cache<u8, &'static str> = moka::future::Cache::new(4);
        let loads = &std::sync::atomic::AtomicUsize::new(0);
        let load = move || async move {
            loads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok::<_, sqlx::Error>("value")
        };
        assert_eq!(cached(&cache, 1, load).await.unwrap(), "value");
        assert_eq!(cached(&cache, 1, load).await.unwrap(), "value");
        assert_eq!(loads.load(std::sync::atomic::Ordering::SeqCst), 1, "a fresh cached read does not reload");
        let failing = || async { Err(sqlx::Error::RowNotFound) };
        assert!(cached(&cache, 2, failing).await.is_err());
        assert_eq!(cached(&cache, 2, load).await.unwrap(), "value", "a failed read is never cached");
    }

    #[tokio::test]
    async fn invalidation_forces_the_next_read_to_reload() {
        cache().rates.insert((DISPLAY_CURRENCY.to_string(), chrono::Utc::now().date_naive()), None).await;
        rates_changed().await;
        assert_eq!(cache().rates.iter().count(), 0);

        cache().rates.insert((DISPLAY_CURRENCY.to_string(), chrono::Utc::now().date_naive()), None).await;
        clear().await;
        assert_eq!(cache().rates.iter().count(), 0);
    }
}
