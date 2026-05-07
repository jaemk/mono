use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::ConnectOptions;
use sqlx::PgPool;
use std::str::FromStr;

pub type DbPool = PgPool;

pub async fn init_pool(url: &str) -> Result<DbPool, sqlx::Error> {
    let opts = PgConnectOptions::from_str(url)?.log_statements(log::LevelFilter::Debug);
    PgPoolOptions::new()
        .max_connections(5)
        .connect_with(opts)
        .await
}
