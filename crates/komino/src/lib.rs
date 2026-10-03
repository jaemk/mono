pub mod config;
pub mod game;
pub mod handlers;
pub mod models;
pub mod sealed;
pub mod service;
pub mod test_utils;

pub use config::Config;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

pub type State = Arc<Resources>;

pub struct Resources {
    pub db: common::db::DbPool,
    pub config: Config,
    pub hub: Hub,
    /// Static ECDH key that seals private card values (SEAL-3).
    pub server_key: sealed::ServerKey,
}

/// This machine's websocket fanout. Each room gets a broadcast channel that
/// carries a bare "something changed" ping; sockets reload their own view.
#[derive(Default)]
pub struct Hub {
    rooms: Mutex<HashMap<i64, broadcast::Sender<()>>>,
    sockets: Mutex<HashMap<(i64, String), usize>>,
}

impl Hub {
    pub fn subscribe(&self, room_id: i64) -> broadcast::Receiver<()> {
        let mut rooms = self.rooms.lock().unwrap();
        rooms
            .entry(room_id)
            .or_insert_with(|| broadcast::channel(16).0)
            .subscribe()
    }

    pub fn ping(&self, room_id: i64) {
        let mut rooms = self.rooms.lock().unwrap();
        if let Some(tx) = rooms.get(&room_id) {
            if tx.send(()).is_err() {
                // nobody listening on this machine any more
                rooms.remove(&room_id);
            }
        }
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
