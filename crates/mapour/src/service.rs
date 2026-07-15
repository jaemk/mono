use axum::{
    routing::{delete, get, post},
    Router,
};
use std::sync::Arc;
use std::time::Duration;
use tower_http::services::{ServeDir, ServeFile};
use tracing::{debug, error, info};

use crate::handlers;
use crate::storage;
use crate::State;

pub fn router<S>(_state: S) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    State: axum::extract::FromRef<S>,
{
    Router::new()
        .route_service("/", ServeFile::new("crates/mapour/assets/index.html"))
        .route("/status", get(handlers::status))
        // auth
        .route("/api/register", post(handlers::register))
        .route("/api/login", post(handlers::login))
        .route("/api/logout", post(handlers::logout))
        .route("/api/me", get(handlers::me))
        .route("/api/claim", post(handlers::claim))
        .route("/api/verify-email", get(handlers::verify_email))
        // orgs
        .route("/api/orgs", post(handlers::create_org))
        .route("/api/orgs/{pid}", get(handlers::get_org))
        .route("/api/orgs/{pid}/join", post(handlers::join_org))
        // members
        .route("/api/orgs/{pid}/members", get(handlers::list_members))
        .route(
            "/api/orgs/{pid}/members/{member_id}/role",
            post(handlers::set_member_role),
        )
        .route(
            "/api/orgs/{pid}/members/{member_id}",
            delete(handlers::remove_member),
        )
        // invites
        .route(
            "/api/orgs/{pid}/invites",
            get(handlers::list_invites).post(handlers::create_invite),
        )
        .route(
            "/api/orgs/{pid}/invites/{invite_id}",
            delete(handlers::delete_invite),
        )
        .route("/api/invites/accept", post(handlers::accept_invite))
        // join requests
        .route(
            "/api/orgs/{pid}/requests",
            get(handlers::list_join_requests),
        )
        .route(
            "/api/orgs/{pid}/requests/{request_id}/{decision}",
            post(handlers::decide_join_request),
        )
        // categories
        .route("/api/orgs/{pid}/categories", get(handlers::list_categories))
        .route(
            "/api/orgs/{pid}/categories/{category_id}",
            post(handlers::update_category),
        )
        // pins
        .route(
            "/api/orgs/{pid}/pins",
            get(handlers::list_pins).post(handlers::create_pin),
        )
        .route(
            "/api/orgs/{pid}/pins/{pin_id}",
            post(handlers::update_pin).delete(handlers::delete_pin),
        )
        .route(
            "/api/orgs/{pid}/pins/{pin_id}/photo",
            post(handlers::upload_photo),
        )
        // photos
        .route(
            "/api/orgs/{pid}/photos/pending",
            get(handlers::pending_photos),
        )
        .route(
            "/api/orgs/{pid}/photos/{photo_id}",
            get(handlers::get_photo).delete(handlers::delete_photo),
        )
        .route(
            "/api/orgs/{pid}/photos/{photo_id}/approve",
            post(handlers::approve_photo),
        )
        .nest_service("/static", ServeDir::new("crates/mapour/assets/static"))
        .route_service(
            "/favicon.svg",
            ServeFile::new("crates/mapour/assets/favicon.svg"),
        )
}

/// Advisory-lock id for the org-expiry sweeper.
/// Stable numeric encoding of "mapour_s" (8 ASCII bytes, big-endian).
const MAPOUR_SWEEP_LOCK_ID: i64 = 0x6d61706f75725f73_u64 as i64;

/// Periodically delete anonymous maps that have outlived their ttl, along
/// with their photo objects in S3.
pub fn init_expiry_sweeper(state: State) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;

            let mut conn = match state.db.acquire().await {
                Ok(c) => c,
                Err(e) => {
                    error!("Error acquiring connection for mapour sweeper lock: {}", e);
                    continue;
                }
            };

            use sqlx::Row;
            let locked: bool = match sqlx::query("select pg_try_advisory_lock($1)")
                .bind(MAPOUR_SWEEP_LOCK_ID)
                .fetch_one(&mut *conn)
                .await
            {
                Ok(row) => row.get(0),
                Err(e) => {
                    error!("Error acquiring advisory lock for mapour sweeper: {}", e);
                    continue;
                }
            };
            if !locked {
                debug!("Could not acquire mapour_sweep advisory lock, skipping tick");
                continue;
            }

            if let Err(e) = sweep_expired_orgs(&state).await {
                error!("Error sweeping expired mapour orgs: {}", e);
            }

            let _ = sqlx::query("select pg_advisory_unlock($1)")
                .bind(MAPOUR_SWEEP_LOCK_ID)
                .execute(&mut *conn)
                .await;
        }
    });
}

/// Delete all orgs whose `expires_at` has passed.  Photo objects are removed
/// from S3 best-effort first; the org row delete cascades everything else.
pub async fn sweep_expired_orgs(state: &State) -> anyhow::Result<usize> {
    let expired: Vec<i64> = sqlx::query_scalar(
        "select id from orgs where expires_at is not null and expires_at <= now()",
    )
    .fetch_all(&state.db)
    .await?;
    for org_id in &expired {
        let keys = crate::models::org_photo_keys(&state.db, *org_id).await?;
        storage::delete_objects_best_effort(&state.s3, &state.config.s3_bucket, &keys).await;
        sqlx::query("delete from orgs where id = $1")
            .bind(org_id)
            .execute(&state.db)
            .await?;
        info!("Deleted expired mapour org id={org_id}");
    }
    Ok(expired.len())
}

pub async fn init(config: crate::Config) -> anyhow::Result<State> {
    let db = common::db::init_pool(&config.database_url).await?;
    info!(" ** Established mapour database connection pool **");

    let s3 = storage::create_client(&config.s3_endpoint, &config.s3_region).await;
    info!(
        " ** Created mapour S3 client (endpoint: {}) **",
        config.s3_endpoint
    );

    let state = Arc::new(crate::Resources::new(db, config, s3));
    init_expiry_sweeper(state.clone());
    Ok(state)
}
