use axum::{
    routing::{get, post},
    Router,
};
use tower_http::services::{ServeDir, ServeFile};
use tracing::info;

use crate::{handlers, sweep, Resources, State};

pub fn router<S>(_state: S) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    State: axum::extract::FromRef<S>,
{
    Router::new()
        // API routes
        .route("/api/upload/defaults", get(handlers::api_upload_defaults))
        .route("/api/upload/init", post(handlers::api_upload_init))
        .route("/api/upload", post(handlers::api_upload_file))
        .route("/api/upload/delete", post(handlers::api_upload_delete))
        .route("/api/download/init", post(handlers::api_download_init))
        .route("/api/download", post(handlers::api_download))
        .route(
            "/api/download/confirm",
            post(handlers::api_download_confirm),
        )
        // Static assets (JS, CSS)
        .nest_service("/static", ServeDir::new("crates/transfer/web/static"))
        // Pages
        .route_service(
            "/download",
            ServeFile::new("crates/transfer/web/download.html"),
        )
        .route_service("/delete", ServeFile::new("crates/transfer/web/delete.html"))
        // Root — upload page (also catches /)
        .route_service("/", ServeFile::new("crates/transfer/web/index.html"))
}

pub async fn init(config: crate::Config) -> anyhow::Result<State> {
    let db = common::db::init_pool(&config.database_url).await?;
    info!(" ** Established transfer database connection pool **");

    let s3 = crate::storage::create_client(&config.s3_endpoint, &config.s3_region).await;
    info!(
        " ** Created transfer S3 client (endpoint: {}) **",
        config.s3_endpoint
    );

    let state = std::sync::Arc::new(Resources { db, s3, config });
    sweep::start(state.clone());
    Ok(state)
}
