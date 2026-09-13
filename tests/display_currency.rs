use apiosk_gateway_v2::store::Store;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires isolated support-db on 54329; never uses a live database"]
async fn display_currency_is_owner_scoped_and_uses_only_published_rates() {
    let pool = PgPoolOptions::new().max_connections(1)
        .connect("postgresql://postgres@127.0.0.1:54329/postgres?sslmode=disable").await.unwrap();
    sqlx::raw_sql("CREATE SCHEMA identity; CREATE SCHEMA billing;
        CREATE TABLE identity.user_preferences(user_id uuid PRIMARY KEY,display_currency text);
        CREATE TABLE billing.fx_rates(quote_currency text,as_of_date date,rate numeric);
        ALTER TABLE identity.user_preferences ENABLE ROW LEVEL SECURITY;
        ALTER TABLE billing.fx_rates ENABLE ROW LEVEL SECURITY;
        GRANT USAGE ON SCHEMA billing TO gateway_v2_runtime;
        INSERT INTO identity.user_preferences VALUES ('00000000-0000-4000-8000-000000000001','EUR'),('00000000-0000-4000-8000-000000000002','GBP');
        INSERT INTO billing.fx_rates VALUES ('EUR',CURRENT_DATE-2,0.91),('EUR',CURRENT_DATE-1,0.92),('EUR',CURRENT_DATE+1,0.99),('GBP',CURRENT_DATE-1,0.80);")
        .execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!("../supabase/migrations/20260913125000_gateway_display_currency.sql")).execute(&pool).await.unwrap();
    sqlx::query("SET ROLE gateway_v2_runtime").execute(&pool).await.unwrap();
    let store = Store { pool: pool.clone() };
    let eur = store.money_display(Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap()).await;
    assert_eq!(eur["currency"], "EUR");
    assert_eq!(eur["rate"], "0.92");
    let gbp = store.money_display(Uuid::parse_str("00000000-0000-4000-8000-000000000002").unwrap()).await;
    assert_eq!(gbp["currency"], "GBP");
    assert_eq!(gbp["rate"], "0.80");
    let visible: i64 = sqlx::query_scalar("SELECT count(user_id) FROM identity.user_preferences").fetch_one(&pool).await.unwrap();
    assert_eq!(visible, 0, "transaction-local owner must not leak through the pool");
    assert_eq!(store.money_display(Uuid::new_v4()).await["currency"], "EUR", "new accounts use the existing EUR default");
    sqlx::query("RESET ROLE").execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM billing.fx_rates WHERE quote_currency='EUR'").execute(&pool).await.unwrap();
    sqlx::query("SET ROLE gateway_v2_runtime").execute(&pool).await.unwrap();
    let missing = store.money_display(Uuid::new_v4()).await;
    assert_eq!(missing["currency"], "USD");
    assert_eq!(missing["fallback_reason"], "exchange_rate_unavailable");
    sqlx::query("RESET ROLE").execute(&pool).await.unwrap();
    sqlx::query("DROP TABLE billing.fx_rates").execute(&pool).await.unwrap();
    assert_eq!(store.money_display(Uuid::new_v4()).await["fallback_reason"], "display_currency_unavailable");
}
