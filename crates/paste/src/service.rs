use axum::{
    http::{header, Method},
    routing::{get, post},
    Router,
};
use std::sync::Arc;
use std::time::Duration;
use tera::Tera;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};
use tracing::{debug, error, info, warn};

use crate::handlers;
use crate::models;
use crate::Resources;
use crate::State;

/// Capacity of the paste-deletion channel.  If the worker falls behind and
/// the channel fills up the sweeper skips the excess entries — they will be
/// picked up again on the next sweep tick.
const DELETION_CHANNEL_CAPACITY: usize = 10_000;

pub fn router<S>(_state: S) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    State: axum::extract::FromRef<S>,
{
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([
            header::CONTENT_TYPE,
            header::HeaderName::from_static("x-paste-encryption-key"),
        ]);

    Router::new()
        .route("/", get(handlers::home))
        .route("/status", get(handlers::status))
        .route("/new", post(handlers::new_paste))
        .route("/raw/{key}", get(handlers::view_paste_raw))
        .route("/json/{key}", get(handlers::view_paste_json))
        .route(
            "/{key}",
            get(handlers::view_paste).post(handlers::view_paste),
        )
        .nest_service("/static", ServeDir::new("crates/paste/assets/static"))
        .route_service(
            "/favicon.ico",
            ServeFile::new("crates/paste/assets/favicon.ico"),
        )
        .route_service(
            "/robots.txt",
            ServeFile::new("crates/paste/assets/robots.txt"),
        )
        .layer(cors)
}

/// Spawns the background task that processes deletion requests from the
/// channel.  Each request is handled by [`models::Paste::attempt_deletion`]
/// which holds a DB transaction open until the S3 deletion is confirmed.
pub fn init_deletion_worker(
    state: State,
    mut rx: tokio::sync::mpsc::Receiver<models::DeletionRequest>,
) {
    tokio::spawn(async move {
        while let Some(req) = rx.recv().await {
            let paste_id = req.id;
            match models::Paste::attempt_deletion(&state.db, &state.s3, &state.config, &req).await {
                Ok(()) => info!("Deleted paste id={paste_id}"),
                Err(e) => warn!("Failed to delete paste id={paste_id}: {e}"),
            }
        }
        error!("Paste deletion worker channel closed unexpectedly");
    });
}

/// Advisory-lock id for the paste sweeper.
/// Stable numeric encoding of "paste_sw" (first 8 ASCII bytes, big-endian).
const PASTE_SWEEP_LOCK_ID: i64 = 0x70617374655f7377_u64 as i64;

/// Spawns the background task that periodically scans for expired / stale
/// pastes and enqueues them on the deletion channel.
pub fn init_sweeper(state: State) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(20));
        loop {
            interval.tick().await;

            // Acquire a dedicated connection for the advisory lock so that the
            // session-level lock is tied to a single connection.
            let mut conn = match state.db.acquire().await {
                Ok(c) => c,
                Err(e) => {
                    error!("Error acquiring connection for paste sweeper lock: {}", e);
                    continue;
                }
            };

            // Try to acquire the advisory lock non-blocking.  If another
            // instance already holds it we simply skip this tick.
            use sqlx::Row;
            let locked: bool = match sqlx::query("select pg_try_advisory_lock($1)")
                .bind(PASTE_SWEEP_LOCK_ID)
                .fetch_one(&mut *conn)
                .await
            {
                Ok(row) => row.get(0),
                Err(e) => {
                    error!("Error acquiring advisory lock for paste sweeper: {}", e);
                    continue;
                }
            };

            if !locked {
                debug!("Could not acquire paste_sweep advisory lock, skipping tick");
                continue;
            }

            let result = async {
                let cutoff = chrono::Utc::now()
                    .checked_sub_signed(chrono::Duration::seconds(
                        state.config.max_paste_age_seconds,
                    ))
                    .ok_or_else(|| anyhow::anyhow!("Error calculating stale cutoff date"))?;
                models::Paste::queue_outdated_for_deletion(
                    &state.db,
                    &state.deletion_tx,
                    cutoff,
                    chrono::Utc::now(),
                )
                .await
            }
            .await;

            match result {
                Ok(count) => {
                    if count > 0 {
                        info!(" ** Queued {} stale pastes for deletion **", count);
                    } else {
                        debug!(" ** No stale pastes found **");
                    }
                }
                Err(e) => error!("Error scanning for stale pastes: {}", e),
            }

            // Release the advisory lock.
            let _ = sqlx::query("select pg_advisory_unlock($1)")
                .bind(PASTE_SWEEP_LOCK_ID)
                .execute(&mut *conn)
                .await;
        }
    });
}

pub async fn init(config: crate::Config) -> anyhow::Result<State> {
    let db_pool = common::db::init_pool(&config.database_url).await?;
    info!(" ** Established paste database connection pool **");

    let s3 = crate::storage::create_client(&config.s3_endpoint, &config.s3_region).await;
    info!(
        " ** Created paste S3 client (endpoint: {}) **",
        config.s3_endpoint
    );

    let mut tera = Tera::new("crates/paste/templates/**/*")?;
    tera.autoescape_on(vec!["html"]);

    let (deletion_tx, deletion_rx) = tokio::sync::mpsc::channel(DELETION_CHANNEL_CAPACITY);

    let state = Arc::new(Resources::new(tera, db_pool, config, s3, deletion_tx));
    init_deletion_worker(state.clone(), deletion_rx);
    init_sweeper(state.clone());
    Ok(state)
}
