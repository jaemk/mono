use std::time::Duration;
use tracing::{debug, error, info};

use crate::{models, storage, State};

const TRANSFER_SWEEP_LOCK_ID: i64 = 0x74726e735f737700_u64 as i64;

pub fn start(state: State) {
    let interval = Duration::from_secs(state.config.expired_cleanup_interval_secs);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;

            let mut conn = match state.db.acquire().await {
                Ok(c) => c,
                Err(e) => {
                    error!("transfer sweeper: failed to acquire connection: {e}");
                    continue;
                }
            };

            use sqlx::Row;
            let locked: bool = match sqlx::query("select pg_try_advisory_lock($1)")
                .bind(TRANSFER_SWEEP_LOCK_ID)
                .fetch_one(&mut *conn)
                .await
            {
                Ok(row) => row.get(0),
                Err(e) => {
                    error!("transfer sweeper: advisory lock error: {e}");
                    continue;
                }
            };

            if !locked {
                debug!("transfer sweeper: lock held by another instance, skipping");
                continue;
            }

            let now = chrono::Utc::now();
            let init_upload_cutoff =
                now - chrono::Duration::seconds(state.config.upload_timeout_secs);

            match models::delete_expired_init_uploads(&state.db, init_upload_cutoff).await {
                Ok(n) if n > 0 => info!("transfer sweeper: deleted {n} expired init_upload rows"),
                Ok(_) => {}
                Err(e) => error!("transfer sweeper: error deleting expired init_uploads: {e}"),
            }

            let download_cutoff =
                now - chrono::Duration::seconds(state.config.download_timeout_secs);
            match models::delete_expired_init_downloads(&state.db, download_cutoff).await {
                Ok(n) if n > 0 => {
                    info!("transfer sweeper: deleted {n} expired init_download rows")
                }
                Ok(_) => {}
                Err(e) => {
                    error!("transfer sweeper: error deleting expired init_downloads: {e}")
                }
            }

            match models::delete_expired_pending_registrations(&state.db, now).await {
                Ok(n) if n > 0 => {
                    info!("transfer sweeper: deleted {n} expired pending_registration rows")
                }
                Ok(_) => {}
                Err(e) => error!(
                    "transfer sweeper: error deleting expired pending_registrations: {e}"
                ),
            }

            match models::delete_expired_sessions(&state.db, now).await {
                Ok(n) if n > 0 => info!("transfer sweeper: deleted {n} expired session rows"),
                Ok(_) => {}
                Err(e) => error!("transfer sweeper: error deleting expired sessions: {e}"),
            }

            match models::get_expired_uploads(&state.db, now).await {
                Ok(expired) => {
                    for row in expired {
                        match storage::delete_object(
                            &state.s3,
                            &state.config.s3_bucket,
                            &row.storage_uri,
                        )
                        .await
                        {
                            Ok(()) => match models::soft_delete_upload(&state.db, row.id).await {
                                Ok(()) => {
                                    info!("transfer sweeper: deleted expired upload id={}", row.id)
                                }
                                Err(e) => error!(
                                    "transfer sweeper: db soft-delete failed for id={}: {e}",
                                    row.id
                                ),
                            },
                            Err(e) => {
                                error!("transfer sweeper: S3 delete failed for id={}: {e}", row.id);
                            }
                        }
                    }
                }
                Err(e) => error!("transfer sweeper: error querying expired uploads: {e}"),
            }

            let _ = sqlx::query("select pg_advisory_unlock($1)")
                .bind(TRANSFER_SWEEP_LOCK_ID)
                .execute(&mut *conn)
                .await;
        }
    });
}
