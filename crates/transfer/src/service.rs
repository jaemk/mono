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
        // Upload API
        .route("/api/upload/defaults", get(handlers::api_upload_defaults))
        .route("/api/upload/init", post(handlers::api_upload_init))
        .route("/api/upload", post(handlers::api_upload_file))
        .route("/api/upload/delete", post(handlers::api_upload_delete))
        // Download API
        .route("/api/download/init", post(handlers::api_download_init))
        .route("/api/download", post(handlers::api_download))
        .route(
            "/api/download/confirm",
            post(handlers::api_download_confirm),
        )
        // Auth API
        .route("/api/auth/register", post(handlers::api_auth_register))
        .route(
            "/api/auth/verify-code",
            post(handlers::api_auth_verify_code),
        )
        .route(
            "/api/auth/resend-code",
            post(handlers::api_auth_resend_code),
        )
        .route("/api/auth/login", post(handlers::api_auth_login))
        .route("/api/auth/logout", post(handlers::api_auth_logout))
        .route("/api/auth/me", get(handlers::api_auth_me))
        .route("/api/auth/google", post(handlers::api_auth_google))
        // My Transfers API
        .route("/api/my/transfers", get(handlers::api_my_transfers))
        .route("/api/my/delete", post(handlers::api_my_delete))
        // Settings API
        .route(
            "/api/settings/add-password",
            post(handlers::api_settings_add_password),
        )
        // Static assets
        .nest_service("/static", ServeDir::new("crates/transfer/web/static"))
        // New pages
        .route_service("/login", ServeFile::new("crates/transfer/web/login.html"))
        .route_service(
            "/register",
            ServeFile::new("crates/transfer/web/register.html"),
        )
        .route_service("/my", ServeFile::new("crates/transfer/web/my.html"))
        .route_service(
            "/settings",
            ServeFile::new("crates/transfer/web/settings.html"),
        )
        // Existing pages
        .route_service(
            "/download",
            ServeFile::new("crates/transfer/web/download.html"),
        )
        .route_service("/delete", ServeFile::new("crates/transfer/web/delete.html"))
        // Root
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

    let http = reqwest::Client::new();

    let state = std::sync::Arc::new(Resources {
        db,
        s3,
        http,
        config,
    });
    sweep::start(state.clone());
    Ok(state)
}
