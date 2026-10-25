use apiosk_gateway_v2::{display_money, store::Store};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires isolated support-db (TEST_DATABASE_URL, default port 54329); never uses a live database"]
async fn every_account_reads_euro_and_the_rate_is_cached_until_invalidated() {
    let pool = PgPoolOptions::new().max_connections(1)
        .connect(&std::env::var("TEST_DATABASE_URL").unwrap_or_else(|_| "postgresql://postgres@127.0.0.1:54329/postgres?sslmode=disable".into())).await.unwrap();
    sqlx::raw_sql("CREATE SCHEMA identity; CREATE SCHEMA billing;
        CREATE TABLE identity.user_preferences(user_id uuid PRIMARY KEY,display_currency text);
        CREATE TABLE billing.fx_rates(quote_currency text,as_of_date date,rate numeric);
        ALTER TABLE identity.user_preferences ENABLE ROW LEVEL SECURITY;
        ALTER TABLE billing.fx_rates ENABLE ROW LEVEL SECURITY;
        GRANT USAGE ON SCHEMA billing TO gateway_v2_runtime;
        INSERT INTO identity.user_preferences VALUES ('00000000-0000-4000-8000-000000000001','USD'),('00000000-0000-4000-8000-000000000002','GBP');
        INSERT INTO billing.fx_rates VALUES ('EUR',CURRENT_DATE-2,0.91),('EUR',CURRENT_DATE-1,0.92),('EUR',CURRENT_DATE+1,0.99),('GBP',CURRENT_DATE-1,0.80);")
        .execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!("../supabase/migrations/20260913121701_gateway_display_currency.sql")).execute(&pool).await.unwrap();
    display_money::clear().await;
    sqlx::query("SET ROLE gateway_v2_runtime").execute(&pool).await.unwrap();
    let store = Store { pool: pool.clone() };
    // A stored dollar or pound preference is ignored: amounts read in euro.
    for owner in ["00000000-0000-4000-8000-000000000001", "00000000-0000-4000-8000-000000000002"] {
        let display = store.money_display(Uuid::parse_str(owner).unwrap()).await;
        assert_eq!(display["currency"], "EUR");
        assert_eq!(display["rate"], "0.92");
    }
    assert_eq!(store.money_display(Uuid::new_v4()).await["currency"], "EUR", "new accounts read euro");

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
