use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::ConnectOptions;
use sqlx::PgPool;
use std::str::FromStr;

pub type DbPool = PgPool;

pub async fn init_pool(url: &str) -> Result<DbPool, sqlx::Error> {
    let mut opts = PgConnectOptions::from_str(url)?;
    opts.log_statements(log::LevelFilter::Debug);
    PgPoolOptions::new()
        .max_connections(5)
        .connect_with(opts)
        .await
}
