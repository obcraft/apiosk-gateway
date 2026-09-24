//! Account display preferences only; quote authorization and ledger amounts stay in micro USD.
use crate::store::Store;
use serde_json::{json, Value};
use uuid::Uuid;

impl Store {
    pub async fn money_display(&self, owner: Uuid) -> Value {
        let _db = crate::telemetry::db("money_display");
        let row: Result<Value, sqlx::Error> = async {
            let mut tx = self.pool.begin().await?;
            // Transaction-local RLS identity cannot leak to another pooled request.
            sqlx::query("SELECT set_config('apiosk.display_owner',$1,true)")
                .bind(owner.to_string()).execute(&mut *tx).await?;
            let value = sqlx::query_scalar(
            "WITH preference AS (
                SELECT coalesce((SELECT display_currency FROM identity.user_preferences WHERE user_id=$1),'EUR') AS currency
             ) SELECT jsonb_build_object('preferred_currency',p.currency,'rate',fx.rate::text,'as_of_date',fx.as_of_date)
             FROM preference p LEFT JOIN LATERAL (
                SELECT rate,as_of_date FROM billing.fx_rates WHERE quote_currency=p.currency
                  AND as_of_date <= (now() AT TIME ZONE 'UTC')::date
                ORDER BY as_of_date DESC LIMIT 1
             ) fx ON true",
            ).bind(owner).fetch_one(&mut *tx).await?;
            tx.commit().await?;
            Ok(value)
        }.await;
        match row {
            Ok(value) => resolve_display(value),
            Err(error) => {
                tracing::warn!(error = %error, "account display currency unavailable");
                json!({"base_currency":"USD","currency":"USD","rate":"1","fallback_reason":"display_currency_unavailable"})
            }
        }
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
}
