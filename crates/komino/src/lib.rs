pub mod bot;
pub mod config;
pub mod game;
pub mod handlers;
pub mod lag;
pub mod models;
pub mod sealed;
pub mod service;
pub mod test_utils;

pub use config::Config;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, OnceCell};

pub type State = Arc<Resources>;

pub struct Resources {
    pub db: common::db::DbPool,
    pub config: Config,
    pub hub: Hub,
    /// Static ECDH key that seals private card values (SEAL-3).
    pub server_key: sealed::ServerKey,
}

/// One room's snapshot for the newest change this machine heard of, loaded
/// by the first socket that asks.
type SnapshotCell = Arc<OnceCell<Arc<models::Snapshot>>>;

/// This machine's websocket fanout. Each room gets a broadcast channel that
/// carries a bare "something changed" ping. Each ping starts a fresh
/// snapshot cell, so the room's sockets share one load of the change and
/// render their own view from it (RT-21).
#[derive(Default)]
pub struct Hub {
    rooms: Mutex<HashMap<i64, (broadcast::Sender<()>, SnapshotCell)>>,
    sockets: Mutex<HashMap<(i64, String), usize>>,
}

impl Hub {
    pub fn subscribe(&self, room_id: i64) -> broadcast::Receiver<()> {
        let mut rooms = self.rooms.lock().unwrap();
        rooms
            .entry(room_id)
            .or_insert_with(|| (broadcast::channel(16).0, SnapshotCell::default()))
            .0
            .subscribe()
    }

    pub fn ping(&self, room_id: i64) {
        let mut rooms = self.rooms.lock().unwrap();
        if let Some((tx, cell)) = rooms.get_mut(&room_id) {
            // a load started from here on sees the change behind this ping
            *cell = SnapshotCell::default();
            if tx.send(()).is_err() {
                // nobody listening on this machine any more
                rooms.remove(&room_id);
            }
        }
    }

    /// The room's snapshot as of the newest ping, loading it if no socket
    /// has yet.
    pub async fn snapshot(
        &self,
        db: &common::db::DbPool,
        room_id: i64,
    ) -> models::Result<Arc<models::Snapshot>> {
        let cell = match self.rooms.lock().unwrap().get(&room_id) {
            Some((_, cell)) => cell.clone(),
            // no subscribers to share with
            None => SnapshotCell::default(),
        };
        cell.get_or_try_init(|| async { models::Snapshot::load(db, room_id).await.map(Arc::new) })
            .await
            .cloned()
    }

    /// Count a socket in; returns how many this player now has in the room.
    pub fn connect(&self, room_id: i64, player: &str) -> usize {
        let mut sockets = self.sockets.lock().unwrap();
        let n = sockets.entry((room_id, player.to_string())).or_default();
        *n += 1;
        *n
    }

    /// Count a socket out; returns how many this player still has.
    pub fn disconnect(&self, room_id: i64, player: &str) -> usize {
        let mut sockets = self.sockets.lock().unwrap();
        let key = (room_id, player.to_string());
        let left = sockets.get(&key).copied().unwrap_or(1).saturating_sub(1);
        if left == 0 {
            sockets.remove(&key);
        } else {
            sockets.insert(key, left);
        }
        left
    }
}
