use apiosk_gateway_v2::{display_money, store::Store};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires isolated support-db (TEST_DATABASE_URL, default port 54329); never uses a live database"]
async fn display_currency_is_owner_scoped_cached_and_invalidated_on_change() {
    let pool = PgPoolOptions::new().max_connections(1)
        .connect(&std::env::var("TEST_DATABASE_URL").unwrap_or_else(|_| "postgresql://postgres@127.0.0.1:54329/postgres?sslmode=disable".into())).await.unwrap();
    sqlx::raw_sql("CREATE SCHEMA identity; CREATE SCHEMA billing;
        CREATE TABLE identity.user_preferences(user_id uuid PRIMARY KEY,display_currency text);
        CREATE TABLE billing.fx_rates(quote_currency text,as_of_date date,rate numeric);
        ALTER TABLE identity.user_preferences ENABLE ROW LEVEL SECURITY;
        ALTER TABLE billing.fx_rates ENABLE ROW LEVEL SECURITY;
        GRANT USAGE ON SCHEMA billing TO gateway_v2_runtime;
        INSERT INTO identity.user_preferences VALUES ('00000000-0000-4000-8000-000000000001','EUR'),('00000000-0000-4000-8000-000000000002','GBP');
        INSERT INTO billing.fx_rates VALUES ('EUR',CURRENT_DATE-2,0.91),('EUR',CURRENT_DATE-1,0.92),('EUR',CURRENT_DATE+1,0.99),('GBP',CURRENT_DATE-1,0.80);")
        .execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!("../supabase/migrations/20260913121701_gateway_display_currency.sql")).execute(&pool).await.unwrap();
    display_money::clear().await;
    sqlx::query("SET ROLE gateway_v2_runtime").execute(&pool).await.unwrap();
    let store = Store { pool: pool.clone() };
    let first = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
    let eur = store.money_display(first).await;
    assert_eq!(eur["currency"], "EUR");
    assert_eq!(eur["rate"], "0.92");
    let gbp = store.money_display(Uuid::parse_str("00000000-0000-4000-8000-000000000002").unwrap()).await;
    assert_eq!(gbp["currency"], "GBP");
    assert_eq!(gbp["rate"], "0.80");
    let visible: i64 = sqlx::query_scalar("SELECT count(user_id) FROM identity.user_preferences").fetch_one(&pool).await.unwrap();
    assert_eq!(visible, 0, "transaction-local owner must not leak through the pool");
    assert_eq!(store.money_display(Uuid::new_v4()).await["currency"], "EUR", "new accounts use the existing EUR default");

    // A changed preference is served from cache until explicitly invalidated,
    // and only that account's entry is reloaded.
    sqlx::query("RESET ROLE").execute(&pool).await.unwrap();
    sqlx::query("UPDATE identity.user_preferences SET display_currency='GBP' WHERE user_id=$1").bind(first).execute(&pool).await.unwrap();
    sqlx::query("SET ROLE gateway_v2_runtime").execute(&pool).await.unwrap();
    assert_eq!(store.money_display(first).await["currency"], "EUR", "cached until invalidated");
    display_money::preference_changed(first).await;
    let changed = store.money_display(first).await;
    assert_eq!(changed["currency"], "GBP");
    assert_eq!(changed["rate"], "0.80");

    sqlx::query("RESET ROLE").execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM billing.fx_rates WHERE quote_currency='EUR'").execute(&pool).await.unwrap();
    sqlx::query("SET ROLE gateway_v2_runtime").execute(&pool).await.unwrap();
    assert_eq!(store.money_display(Uuid::new_v4()).await["rate"], "0.92", "the dated rate is cached until invalidated");
    display_money::rates_changed().await;
    let missing = store.money_display(Uuid::new_v4()).await;
    assert_eq!(missing["currency"], "USD");
    assert_eq!(missing["fallback_reason"], "exchange_rate_unavailable");

    sqlx::query("RESET ROLE").execute(&pool).await.unwrap();
    sqlx::query("DROP TABLE billing.fx_rates").execute(&pool).await.unwrap();
    display_money::clear().await;
    assert_eq!(store.money_display(Uuid::new_v4()).await["fallback_reason"], "display_currency_unavailable");
    sqlx::raw_sql("CREATE TABLE billing.fx_rates(quote_currency text,as_of_date date,rate numeric); INSERT INTO billing.fx_rates VALUES ('EUR',CURRENT_DATE,0.93);")
        .execute(&pool).await.unwrap();
    display_money::clear().await;
    assert_eq!(store.money_display(Uuid::new_v4()).await["rate"], "0.93", "a failed read is never cached");
}
