use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::ConnectOptions;
use sqlx::PgPool;
use std::str::FromStr;

pub type DbPool = PgPool;

/// Connections a pool opens unless the app asks for more.
pub const DEFAULT_MAX_CONNECTIONS: u32 = 5;

pub async fn init_pool(url: &str) -> Result<DbPool, sqlx::Error> {
    init_pool_sized(url, DEFAULT_MAX_CONNECTIONS).await
}

pub async fn init_pool_sized(url: &str, max_connections: u32) -> Result<DbPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .connect_with(connect_options(url)?)
        .await
}

/// Options for `url`, for a pool or a dedicated connection.
pub fn connect_options(url: &str) -> Result<PgConnectOptions, sqlx::Error> {
    Ok(PgConnectOptions::from_str(url)?.log_statements(log::LevelFilter::Debug))
}
