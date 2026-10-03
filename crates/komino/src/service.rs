use axum::{
    routing::{get, post},
    Router,
};
use sqlx::postgres::PgListener;
use std::sync::Arc;
use std::time::Duration;
use tower_http::services::ServeDir;
use tracing::{debug, error, info};

use crate::handlers;
use crate::models;
use crate::State;

pub fn router<S>(_state: S) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    State: axum::extract::FromRef<S>,
{
    Router::new()
        .route("/", get(handlers::index))
        .route("/r/{code}", get(handlers::index))
        .route("/r/{code}/ws", get(handlers::ws))
        .route("/r/{code}/watch", get(handlers::index))
        .route("/r/{code}/watch/ws", get(handlers::watch_ws))
        .route("/api/me", get(handlers::me).post(handlers::rename))
        .route("/api/rooms", post(handlers::create_room))
        .route("/api/rooms/{code}", get(handlers::get_room))
        .route("/api/rooms/{code}/join", post(handlers::join_room))
        .route("/api/rooms/{code}/watch", get(handlers::watch_room))
        .route("/api/rooms/{code}/leave", post(handlers::leave_room))
        .route("/api/rooms/{code}/remove", post(handlers::remove_member))
        .route("/api/rooms/{code}/unban", post(handlers::unban_member))
        .route("/api/rooms/{code}/action", post(handlers::action))
        .nest_service("/static", ServeDir::new("crates/komino/assets/static"))
}

/// Relay room notifications from postgres to this machine's sockets, so a
/// change made through any machine reaches every connected player.
pub fn init_listener(state: State) {
    tokio::spawn(async move {
        loop {
            match PgListener::connect_with(&state.db).await {
                Ok(mut listener) => {
                    if let Err(e) = listener.listen(models::NOTIFY_CHANNEL).await {
                        error!("komino listen failed: {e}");
                    } else {
                        loop {
                            match listener.recv().await {
                                Ok(n) => {
                                    if let Ok(room_id) = n.payload().parse::<i64>() {
                                        state.hub.ping(room_id);
                                    }
                                }
                                Err(e) => {
                                    error!("komino listener error: {e}");
                                    break;
                                }
                            }
                        }
                    }
                }
                Err(e) => error!("komino listener connect failed: {e}"),
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
}

/// Advisory-lock id for the timer sweeper: "komino_t" as big-endian bytes.
const KOMINO_TICK_LOCK_ID: i64 = 0x6b6f6d696e6f5f74_u64 as i64;

/// Fire ready, away, and scoring deadlines, and drop rooms idle for 30 days.
/// One machine at a time holds the lock; each game is still re-checked under
/// its row lock so a deadline applies once.
pub fn init_sweeper(state: State) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        let mut ticks: u64 = 0;
        loop {
            interval.tick().await;
            ticks += 1;
            let mut conn = match state.db.acquire().await {
                Ok(c) => c,
                Err(e) => {
                    error!("komino sweeper connection error: {e}");
                    continue;
                }
            };
            let locked: bool = match sqlx::query_scalar("select pg_try_advisory_lock($1)")
                .bind(KOMINO_TICK_LOCK_ID)
                .fetch_one(&mut *conn)
                .await
            {
                Ok(locked) => locked,
                Err(e) => {
                    error!("komino sweeper lock error: {e}");
                    continue;
                }
            };
            if !locked {
                debug!("komino sweeper lock held elsewhere, skipping tick");
                continue;
            }
            if let Err(e) = models::tick_all(&state.db).await {
                error!("komino tick error: {e:?}");
            }
            if ticks % 3600 == 1 {
                match models::delete_stale_rooms(&state.db).await {
                    Ok(0) => {}
                    Ok(n) => info!("deleted {n} idle komino rooms"),
                    Err(e) => error!("komino room cleanup error: {e:?}"),
                }
            }
            let _ = sqlx::query("select pg_advisory_unlock($1)")
                .bind(KOMINO_TICK_LOCK_ID)
                .execute(&mut *conn)
                .await;
        }
    });
}

pub async fn init(config: crate::Config) -> anyhow::Result<State> {
    let db = common::db::init_pool(&config.database_url).await?;
    info!(" ** Established komino database connection pool **");
    let state = Arc::new(crate::Resources {
        db,
        config,
        hub: Default::default(),
    });
    init_listener(state.clone());
    init_sweeper(state.clone());
    Ok(state)
}
