use axum::{
    routing::{get, post},
    Router,
};
use sqlx::postgres::{PgConnection, PgListener};
use sqlx::Connection;
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
        .route("/api/key", get(handlers::server_key))
        .route("/api/rooms/{code}/reveal", post(handlers::reveal))
        .route("/api/rooms", post(handlers::create_room))
        .route("/api/rooms/{code}", get(handlers::get_room))
        .route("/api/rooms/{code}/join", post(handlers::join_room))
        .route("/api/rooms/{code}/watch", get(handlers::watch_room))
        .route("/api/rooms/{code}/leave", post(handlers::leave_room))
        .route("/api/rooms/{code}/remove", post(handlers::remove_member))
        .route("/api/rooms/{code}/unban", post(handlers::unban_member))
        .route("/api/rooms/{code}/bots", post(handlers::add_bot))
        .route("/api/rooms/{code}/action", post(handlers::action))
        .nest_service("/static", ServeDir::new("crates/komino/assets/static"))
}

/// Pool size: every socket's view load, action, and heartbeat draws from it.
/// The listener and the sweeper's leader lock hold their own connections.
const POOL_SIZE: u32 = 20;

/// Relay room notifications from postgres to this machine's sockets, so a
/// change made through any machine reaches every connected player. Listens
/// on its own connection, outside the pool.
pub fn init_listener(state: State) {
    tokio::spawn(async move {
        loop {
            match PgListener::connect(&state.config.database_url).await {
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

/// How often a machine that isn't the sweeper tries to become it.
const LEADER_RETRY: Duration = Duration::from_secs(5);
/// How often the sweeper checks that its lock connection is still alive.
const LEADER_CHECK_TICKS: u64 = 15;

/// Fire ready, away, scoring, and match window deadlines, and drop rooms
/// idle for 30 days. One machine at a time sweeps: it takes the advisory
/// lock once on a dedicated connection and keeps it while that connection
/// lives. Each due game is still re-checked under its row lock, so a deadline
/// applies once even if two machines briefly both sweep.
pub fn init_sweeper(state: State) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = lead(&state).await {
                error!("komino sweeper error: {e}");
            }
            tokio::time::sleep(LEADER_RETRY).await;
        }
    });
}

/// Wait to hold the sweeper lock, then sweep until the lock connection
/// fails.
async fn lead(state: &State) -> Result<(), sqlx::Error> {
    let opts = common::db::connect_options(&state.config.database_url)?;
    let mut conn = PgConnection::connect_with(&opts).await?;
    loop {
        let locked: bool = sqlx::query_scalar("select pg_try_advisory_lock($1)")
            .bind(KOMINO_TICK_LOCK_ID)
            .fetch_one(&mut conn)
            .await?;
        if locked {
            break;
        }
        debug!("komino sweeper lock held elsewhere");
        tokio::time::sleep(LEADER_RETRY).await;
    }
    info!("komino sweeper lock taken");
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    let mut ticks: u64 = 0;
    loop {
        interval.tick().await;
        ticks += 1;
        if ticks.is_multiple_of(LEADER_CHECK_TICKS) {
            // the lock dies with its connection; stop sweeping if it did
            sqlx::query("select 1").execute(&mut conn).await?;
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
    }
}

pub async fn init(config: crate::Config) -> anyhow::Result<State> {
    let server_key = crate::sealed::ServerKey::from_hex(&config.ecdh_key)?;
    let db = common::db::init_pool_sized(&config.database_url, POOL_SIZE).await?;
    info!(" ** Established komino database connection pool **");
    let state = Arc::new(crate::Resources {
        db,
        config,
        hub: Default::default(),
        server_key,
    });
    init_listener(state.clone());
    init_sweeper(state.clone());
    Ok(state)
}
