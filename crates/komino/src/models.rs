//! Rooms, players, and the action pipeline. Postgres holds every bit of game
//! state; each action locks the game row, applies the rules engine, and saves
//! state, events, and stats in one transaction that also notifies the room.

use crate::bot;
use crate::game::{self, Action, Game, Outcome, Reject, Secret, Settings, Stat};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use common::db::DbPool;
use rand::RngExt;
use serde_json::{json, Value};
use sqlx::{Postgres, Row, Transaction};
use std::sync::LazyLock;

/// Postgres notify channel; the payload is the room id.
pub const NOTIFY_CHANNEL: &str = "komino";

/// Codes avoid 0/O/1/I/L.
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const CODE_LEN: usize = 6;
pub const MAX_NAME_CHARS: usize = 24;

/// How long a page load or socket heartbeat counts a member as present, and
/// how long an observer lease lasts between heartbeats.
pub const PRESENCE_SECS: i64 = 25;

/// Observer sockets allowed per room, across every machine.
pub const MAX_OBSERVERS: i64 = 4;

/// How long a client ECDH key may be used after its first reveal (SEAL-5).
pub const CLIENT_KEY_SECS: i64 = 300;

pub type Tx<'a> = Transaction<'a, Postgres>;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid", message)
    }
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "no such room")
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }
    pub fn removed() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "removed",
            "you were removed from this room",
        )
    }
    pub fn full() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "full",
            format!("this room already has {MAX_OBSERVERS} observers"),
        )
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!("komino db error: {e:?}");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "error", "database error")
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        tracing::error!("komino state error: {e:?}");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "error", "bad game state")
    }
}

impl From<Reject> for ApiError {
    fn from(r: Reject) -> Self {
        let status = match r.code {
            "too_late" | "stale" => StatusCode::CONFLICT,
            "forbidden" => StatusCode::FORBIDDEN,
            _ => StatusCode::BAD_REQUEST,
        };
        Self::new(status, r.code, r.message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "code": self.code, "message": self.message })),
        )
            .into_response()
    }
}

pub type Result<T> = std::result::Result<T, ApiError>;

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// ---------------------------------------------------------------------------
// Players
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Player {
    pub id: String,
    pub name: String,
}

/// Names are rendered as text only, so the limits are length and no control
/// characters.
pub fn validate_name(raw: &str) -> Result<String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(ApiError::invalid("name must not be blank"));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(ApiError::invalid(format!(
            "name must be at most {MAX_NAME_CHARS} characters"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(ApiError::invalid(
            "name must not contain control characters",
        ));
    }
    Ok(name.to_string())
}

/// The player for a verified cookie id, or a brand new player when the id is
/// missing or unknown.
pub async fn ensure_player(db: &DbPool, id: Option<&str>) -> Result<(Player, bool)> {
    if let Some(id) = id {
        let row = sqlx::query("UPDATE players SET last_seen = now() WHERE id = $1 RETURNING name")
            .bind(id)
            .fetch_optional(db)
            .await?;
        if let Some(row) = row {
            return Ok((
                Player {
                    id: id.to_string(),
                    name: row.get("name"),
                },
                false,
            ));
        }
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    let name = format!("player-{}", rand::rng().random_range(1000..10000));
    sqlx::query("INSERT INTO players (id, name) VALUES ($1, $2)")
        .bind(&id)
        .bind(&name)
        .execute(db)
        .await?;
    Ok((Player { id, name }, true))
}

/// Rename a player and refresh every room they are in.
pub async fn set_name(db: &DbPool, player: &str, raw: &str) -> Result<String> {
    let name = validate_name(raw)?;
    let mut tx = db.begin().await?;
    sqlx::query("UPDATE players SET name = $1 WHERE id = $2")
        .bind(&name)
        .bind(player)
        .execute(&mut *tx)
        .await?;
    let rooms: Vec<i64> = sqlx::query_scalar(
        "SELECT room_id FROM room_members WHERE player_id = $1 AND left_at IS NULL AND NOT removed",
    )
    .bind(player)
    .fetch_all(&mut *tx)
    .await?;
    for room in rooms {
        notify(&mut tx, room).await?;
    }
    tx.commit().await?;
    Ok(name)
}

// ---------------------------------------------------------------------------
// Rooms and members
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Room {
    pub id: i64,
    pub code: String,
    pub host: String,
    pub last_winner: Option<String>,
    pub settings: Settings,
    /// One entry per scored game, oldest first (SET-14): `{m, over, totals,
    /// scores}`, totals and scores keyed by player id.
    pub history: Vec<Value>,
}

/// Games kept in a room's history.
const HISTORY_KEPT: usize = 50;

/// The room columns every room read takes, prefixed `r.`.
const ROOM_COLUMNS: &str =
    "r.id, r.code, r.host_player_id, r.last_winner, r.hand_size, r.away_grace_secs,
     r.turn_limit_secs, r.reveal_secs, r.show_misses, r.target_score, r.caller_penalty,
     r.exact_reset, r.memory_marks, r.history";

fn room_from_row(row: &sqlx::postgres::PgRow) -> Room {
    Room {
        id: row.get("id"),
        code: row.get("code"),
        host: row.get("host_player_id"),
        last_winner: row.get("last_winner"),
        settings: Settings {
            hand_size: row.get::<i32, _>("hand_size") as usize,
            away_grace_secs: row.get::<i32, _>("away_grace_secs").into(),
            turn_limit_secs: row.get::<Option<i32>, _>("turn_limit_secs").map(Into::into),
            reveal_secs: row.get::<Option<i32>, _>("reveal_secs").map(Into::into),
            show_misses: row.get("show_misses"),
            target_score: row.get::<Option<i32>, _>("target_score").map(Into::into),
            caller_penalty: row.get::<i32, _>("caller_penalty").into(),
            exact_reset: row.get("exact_reset"),
            memory_marks: row.get("memory_marks"),
        },
        history: json_array(row.get("history")),
    }
}

impl Room {
    /// The match the next game belongs to and each player's running total
    /// coming into it (SET-14). A finished match starts the next from zero;
    /// a player new to a running match starts level with its highest total,
    /// so sitting out the early games is no advantage.
    fn carry(&self) -> (u32, impl Fn(&str) -> i32 + '_) {
        let last = self.history.last();
        let m = last.and_then(|e| e["m"].as_u64()).unwrap_or(0) as u32;
        let over = last.is_none_or(|e| e["over"] == true);
        let totals = last.filter(|_| !over).map(|e| &e["totals"]);
        let highest = totals
            .and_then(Value::as_object)
            .and_then(|t| t.values().filter_map(Value::as_i64).max())
            .unwrap_or(0) as i32;
        let carry = move |player: &str| match totals {
            Some(t) => t[player].as_i64().map(|n| n as i32).unwrap_or(highest),
            None => 0,
        };
        (if over { m + 1 } else { m }, carry)
    }

    /// The games of the newest match, oldest first, for the room view.
    fn match_history(&self) -> Vec<&Value> {
        let m = self.history.last().map(|e| &e["m"]);
        self.history.iter().filter(|e| Some(&e["m"]) == m).collect()
    }

    /// The history with `game`'s result appended, when it was just scored.
    fn history_after(&self, game: &Game) -> Value {
        let by_player = |f: &dyn Fn(&game::Seat) -> Option<i32>| -> Value {
            game.seats
                .iter()
                .filter_map(|s| f(s).map(|n| (s.player.clone(), json!(n))))
                .collect::<serde_json::Map<_, _>>()
                .into()
        };
        let mut history = self.history.clone();
        history.push(json!({
            "m": game.match_no,
            "over": game.match_over,
            "totals": by_player(&|s| s.total),
            "scores": by_player(&|s| s.score),
        }));
        let extra = history.len().saturating_sub(HISTORY_KEPT);
        history.drain(..extra);
        Value::Array(history)
    }
}

fn new_code() -> String {
    let mut rng = rand::rng();
    (0..CODE_LEN)
        .map(|_| CODE_ALPHABET[rng.random_range(0..CODE_ALPHABET.len())] as char)
        .collect()
}

/// Codes are case-insensitive on input; anything outside the alphabet can't
/// name a room.
pub fn normalize_code(raw: &str) -> Option<String> {
    let code = raw.trim().to_ascii_uppercase();
    (code.len() == CODE_LEN && code.bytes().all(|b| CODE_ALPHABET.contains(&b))).then_some(code)
}

pub async fn notify(tx: &mut Tx<'_>, room_id: i64) -> Result<()> {
    sqlx::query("SELECT pg_notify($1, $2)")
        .bind(NOTIFY_CHANNEL)
        .bind(room_id.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub async fn create_room(db: &DbPool, host: &str, settings: Settings) -> Result<Room> {
    settings.validate().map_err(ApiError::invalid)?;
    for _ in 0..10 {
        let code = new_code();
        let mut tx = db.begin().await?;
        let id: Option<i64> = sqlx::query_scalar(
            "INSERT INTO rooms (code, host_player_id, hand_size, away_grace_secs,
                                turn_limit_secs, reveal_secs, show_misses, target_score,
                                caller_penalty, exact_reset, memory_marks)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
             ON CONFLICT (code) DO NOTHING RETURNING id",
        )
        .bind(&code)
        .bind(host)
        .bind(settings.hand_size as i32)
        .bind(settings.away_grace_secs as i32)
        .bind(settings.turn_limit_secs.map(|s| s as i32))
        .bind(settings.reveal_secs.map(|s| s as i32))
        .bind(settings.show_misses)
        .bind(settings.target_score.map(|s| s as i32))
        .bind(settings.caller_penalty as i32)
        .bind(settings.exact_reset)
        .bind(settings.memory_marks)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(id) = id else { continue };
        sqlx::query(
            "INSERT INTO room_members (room_id, player_id, present_until)
             VALUES ($1, $2, now() + make_interval(secs => $3))",
        )
        .bind(id)
        .bind(host)
        .bind(PRESENCE_SECS as f64)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(Room {
            id,
            code,
            host: host.to_string(),
            last_winner: None,
            settings,
            history: vec![],
        });
    }
    Err(ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "error",
        "could not allocate a room code",
    ))
}

pub async fn room_by_code(db: &DbPool, raw: &str) -> Result<Room> {
    let code = normalize_code(raw).ok_or_else(ApiError::not_found)?;
    let row = sqlx::query(&format!(
        "SELECT {ROOM_COLUMNS} FROM rooms r WHERE r.code = $1"
    ))
    .bind(&code)
    .fetch_optional(db)
    .await?
    .ok_or_else(ApiError::not_found)?;
    Ok(room_from_row(&row))
}

async fn lock_room(tx: &mut Tx<'_>, room_id: i64) -> Result<Room> {
    let row = sqlx::query(&format!(
        "SELECT {ROOM_COLUMNS} FROM rooms r WHERE r.id = $1 FOR UPDATE"
    ))
    .bind(room_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    Ok(room_from_row(&row))
}

/// Lock the room row and read `player`'s membership in the same round trip,
/// then require them to be a current member.
async fn lock_room_as(tx: &mut Tx<'_>, room_id: i64, player: &str) -> Result<Room> {
    let row = sqlx::query(&format!(
        "SELECT {ROOM_COLUMNS}, m.removed, m.left_at IS NOT NULL AS gone
         FROM rooms r LEFT JOIN room_members m ON m.room_id = r.id AND m.player_id = $2
         WHERE r.id = $1 FOR UPDATE OF r"
    ))
    .bind(room_id)
    .bind(player)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let removed: Option<bool> = row.get("removed");
    check_member(removed.map(|r| (r, row.get("gone"))))?;
    Ok(room_from_row(&row))
}

/// Membership as `(removed, left)`, `None` for someone who never joined.
fn check_member(membership: Option<(bool, bool)>) -> Result<()> {
    match membership {
        Some((true, _)) => Err(ApiError::removed()),
        Some((false, false)) => Ok(()),
        _ => Err(ApiError::forbidden("join the room first")),
    }
}

/// A current member: joined, not left, not removed.
pub async fn require_member(db: &DbPool, room_id: i64, player: &str) -> Result<()> {
    let row = sqlx::query(
        "SELECT removed, left_at IS NOT NULL AS gone FROM room_members
         WHERE room_id = $1 AND player_id = $2",
    )
    .bind(room_id)
    .bind(player)
    .fetch_optional(db)
    .await?;
    check_member(row.map(|r| (r.get("removed"), r.get("gone"))))
}

/// Anyone but a player the host removed may watch a room.
pub async fn require_not_removed(db: &DbPool, room_id: i64, player: &str) -> Result<()> {
    let removed: Option<bool> = sqlx::query_scalar(
        "SELECT removed FROM room_members WHERE room_id = $1 AND player_id = $2",
    )
    .bind(room_id)
    .bind(player)
    .fetch_optional(db)
    .await?;
    match removed {
        Some(true) => Err(ApiError::removed()),
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Observers
// ---------------------------------------------------------------------------

/// Take one of the room's observer slots, returning the lease id. The room
/// row lock serializes claims from every machine.
pub async fn claim_observer(db: &DbPool, room_id: i64) -> Result<String> {
    let mut tx = db.begin().await?;
    lock_room(&mut tx, room_id).await?;
    sqlx::query("DELETE FROM room_observers WHERE room_id = $1 AND until <= now()")
        .bind(room_id)
        .execute(&mut *tx)
        .await?;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM room_observers WHERE room_id = $1")
        .bind(room_id)
        .fetch_one(&mut *tx)
        .await?;
    if n >= MAX_OBSERVERS {
        return Err(ApiError::full());
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    sqlx::query(
        "INSERT INTO room_observers (id, room_id, until)
         VALUES ($1, $2, now() + make_interval(secs => $3))",
    )
    .bind(&id)
    .bind(room_id)
    .bind(PRESENCE_SECS as f64)
    .execute(&mut *tx)
    .await?;
    notify(&mut tx, room_id).await?;
    tx.commit().await?;
    Ok(id)
}

/// Extend a live observer lease by another [`PRESENCE_SECS`]. False when the
/// lease expired or was deleted, since its slot may have been claimed again.
pub async fn refresh_observer(db: &DbPool, id: &str) -> Result<bool> {
    let n = sqlx::query(
        "UPDATE room_observers SET until = now() + make_interval(secs => $2)
         WHERE id = $1 AND until > now()",
    )
    .bind(id)
    .bind(PRESENCE_SECS as f64)
    .execute(db)
    .await?
    .rows_affected();
    Ok(n == 1)
}

pub async fn release_observer(db: &DbPool, room_id: i64, id: &str) -> Result<()> {
    sqlx::query(
        "WITH d AS (DELETE FROM room_observers WHERE id = $1)
         SELECT pg_notify($3, $2::text)",
    )
    .bind(id)
    .bind(room_id)
    .bind(NOTIFY_CHANNEL)
    .execute(db)
    .await?;
    Ok(())
}

/// Add a member, or bring a returning one back. Removed players stay out.
/// One statement: the upsert skips a removed member, and only a join touches
/// the room and notifies. A join into a room whose host has left takes the
/// host role, so a room emptied by its host is not stuck without one
/// (ROOM-20).
pub async fn join(db: &DbPool, room: &Room, player: &str) -> Result<()> {
    let joined = sqlx::query(
        "WITH j AS (
             INSERT INTO room_members (room_id, player_id, present_until)
             VALUES ($1, $2, now() + make_interval(secs => $3))
             ON CONFLICT (room_id, player_id)
             DO UPDATE SET left_at = NULL, present_until = excluded.present_until
             WHERE NOT room_members.removed
             RETURNING room_id
         ), r AS (
             UPDATE rooms SET last_active = now(),
                 host_player_id = CASE WHEN EXISTS (
                     SELECT 1 FROM room_members h
                     WHERE h.room_id = rooms.id AND h.player_id = rooms.host_player_id
                       AND h.left_at IS NULL AND NOT h.removed
                 ) THEN host_player_id ELSE $2 END
             WHERE id IN (SELECT room_id FROM j)
         )
         SELECT pg_notify($4, $1::text) FROM j",
    )
    .bind(room.id)
    .bind(player)
    .bind(PRESENCE_SECS as f64)
    .bind(NOTIFY_CHANNEL)
    .fetch_optional(db)
    .await?;
    match joined {
        Some(_) => Ok(()),
        None => Err(ApiError::removed()),
    }
}

/// Count a member present for another [`PRESENCE_SECS`], or away now, in one
/// statement that notifies only when presence flips.
pub async fn set_presence(db: &DbPool, room_id: i64, player: &str, present: bool) -> Result<()> {
    let secs = if present { PRESENCE_SECS } else { 0 };
    sqlx::query(
        "WITH old AS (
             SELECT coalesce(present_until > now(), false) AS was FROM room_members
             WHERE room_id = $1 AND player_id = $2
         ), u AS (
             UPDATE room_members SET present_until = now() + make_interval(secs => $3)
             WHERE room_id = $1 AND player_id = $2
         )
         SELECT pg_notify($5, $1::text) FROM old WHERE old.was <> $4",
    )
    .bind(room_id)
    .bind(player)
    .bind(secs as f64)
    .bind(present)
    .bind(NOTIFY_CHANNEL)
    .execute(db)
    .await?;
    Ok(())
}

/// Forfeit `player` from the room's running game, if they are in it.
async fn forfeit(tx: &mut Tx<'_>, room: &Room, player: &str) -> Result<()> {
    if let Some((game_id, mut game)) = lock_game(tx, room.id).await? {
        let out = game.forfeit(player, now_ms());
        if out.changed {
            save(tx, room, game_id, &game, &out).await?;
        }
    }
    Ok(())
}

pub async fn leave(db: &DbPool, room: &Room, player: &str) -> Result<()> {
    let mut tx = db.begin().await?;
    let room = lock_room(&mut tx, room.id).await?;
    forfeit(&mut tx, &room, player).await?;
    sqlx::query(
        "UPDATE room_members SET left_at = now(), present_until = NULL
         WHERE room_id = $1 AND player_id = $2",
    )
    .bind(room.id)
    .bind(player)
    .execute(&mut *tx)
    .await?;
    if room.host == player {
        // longest-standing present member first, then anyone still here;
        // never a bot (BOT-2)
        let next: Option<String> = sqlx::query_scalar(
            "SELECT m.player_id FROM room_members m JOIN players p ON p.id = m.player_id
             WHERE m.room_id = $1 AND m.player_id <> $2 AND m.left_at IS NULL AND NOT m.removed
               AND NOT p.bot
             ORDER BY coalesce(m.present_until > now(), false) DESC, m.joined
             LIMIT 1",
        )
        .bind(room.id)
        .bind(player)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(next) = next {
            sqlx::query("UPDATE rooms SET host_player_id = $1 WHERE id = $2")
                .bind(next)
                .bind(room.id)
                .execute(&mut *tx)
                .await?;
        }
    }
    notify(&mut tx, room.id).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn remove(db: &DbPool, room: &Room, host: &str, target: &str) -> Result<()> {
    let mut tx = db.begin().await?;
    let room = lock_room(&mut tx, room.id).await?;
    if room.host != host {
        return Err(ApiError::forbidden("only the host can remove players"));
    }
    if target == host {
        return Err(ApiError::invalid("the host cannot remove themselves"));
    }
    forfeit(&mut tx, &room, target).await?;
    // a removed bot just leaves, with nothing to unban (BOT-2)
    let n = sqlx::query(
        "UPDATE room_members m SET removed = NOT p.bot, present_until = NULL,
             left_at = CASE WHEN p.bot THEN now() ELSE m.left_at END
         FROM players p
         WHERE m.room_id = $1 AND m.player_id = $2 AND p.id = m.player_id",
    )
    .bind(room.id)
    .bind(target)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(ApiError::invalid("that player is not in this room"));
    }
    notify(&mut tx, room.id).await?;
    tx.commit().await?;
    Ok(())
}

/// Names bots are given, with a number when one is taken.
const BOT_NAMES: [&str; 8] = ["ava", "ben", "cleo", "dex", "ezra", "fay", "gus", "hana"];

/// Add a computer player at `level` to the room (BOT-1, BOT-5). Bots are
/// always present, so the next game seats them like anyone here.
pub async fn add_bot(db: &DbPool, room: &Room, host: &str, level: bot::Level) -> Result<()> {
    let mut tx = db.begin().await?;
    let room = lock_room(&mut tx, room.id).await?;
    if room.host != host {
        return Err(ApiError::forbidden("only the host can add bots"));
    }
    let members: Vec<(String, bool)> = sqlx::query_as(
        "SELECT p.name, p.bot FROM room_members m JOIN players p ON p.id = m.player_id
         WHERE m.room_id = $1 AND m.left_at IS NULL AND NOT m.removed",
    )
    .bind(room.id)
    .fetch_all(&mut *tx)
    .await?;
    let bots = members.iter().filter(|(_, bot)| *bot).count();
    let names: Vec<String> = members.into_iter().map(|(n, _)| n).collect();
    if bots >= game::MAX_SEATS - 1 {
        return Err(ApiError::invalid(format!(
            "a room can have at most {} bots",
            game::MAX_SEATS - 1
        )));
    }
    let name = (0..)
        .map(|i| {
            let base = BOT_NAMES[i % BOT_NAMES.len()];
            match i / BOT_NAMES.len() {
                0 => format!("bot {base}"),
                n => format!("bot {base} {}", n + 1),
            }
        })
        .find(|n| !names.contains(n))
        .expect("an unused bot name");
    let id = format!("bot-{}", uuid::Uuid::new_v4().simple());
    sqlx::query("INSERT INTO players (id, name, bot, bot_level) VALUES ($1, $2, true, $3)")
        .bind(&id)
        .bind(&name)
        .bind(level.as_str())
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO room_members (room_id, player_id, present_until)
         VALUES ($1, $2, 'infinity')",
    )
    .bind(room.id)
    .bind(&id)
    .execute(&mut *tx)
    .await?;
    notify(&mut tx, room.id).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn unban(db: &DbPool, room: &Room, host: &str, target: &str) -> Result<()> {
    let mut tx = db.begin().await?;
    let room = lock_room(&mut tx, room.id).await?;
    if room.host != host {
        return Err(ApiError::forbidden("only the host can unban players"));
    }
    // an unbanned player rejoins through the share link like anyone else
    sqlx::query(
        "UPDATE room_members SET removed = false, left_at = now()
         WHERE room_id = $1 AND player_id = $2 AND removed",
    )
    .bind(room.id)
    .bind(target)
    .execute(&mut *tx)
    .await?;
    notify(&mut tx, room.id).await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Games
// ---------------------------------------------------------------------------

async fn lock_game(tx: &mut Tx<'_>, room_id: i64) -> Result<Option<(i64, Game)>> {
    let row = sqlx::query(
        "SELECT id, state FROM games WHERE room_id = $1 AND status <> 'scored' FOR UPDATE",
    )
    .bind(room_id)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        Some(row) => Ok(Some((
            row.get("id"),
            serde_json::from_value(row.get::<Value, _>("state"))?,
        ))),
        None => Ok(None),
    }
}

/// The single statement behind [`save`]: game state, its events, the stat
/// deltas, the room's activity, and the notify. The stat columns come from
/// a fixed enum, never from input.
static SAVE_SQL: LazyLock<String> = LazyLock::new(|| {
    let cols: Vec<&str> = Stat::ALL.iter().map(Stat::column).collect();
    let list = cols.join(", ");
    let typed = cols
        .iter()
        .map(|c| format!("{c} int"))
        .collect::<Vec<_>>()
        .join(", ");
    let from_s = cols
        .iter()
        .map(|c| format!("s.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sums = cols
        .iter()
        .map(|c| format!("{c} = room_stats.{c} + excluded.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "WITH g AS (
             UPDATE games SET state = $1, status = $2, version = version + 1,
                 ended = CASE WHEN $2 = 'scored' THEN now() ELSE ended END
             WHERE id = $3 RETURNING version
         ), ev AS (
             INSERT INTO game_events (game_id, version, player_id, kind, payload)
             SELECT $3, g.version, e.player, e.kind, e.payload
             FROM g, ROWS FROM (jsonb_to_recordset($4) AS (player text, kind text, payload jsonb))
                 WITH ORDINALITY AS e(player, kind, payload, n)
             ORDER BY e.n
         ), st AS (
             INSERT INTO room_stats (room_id, player_id, {list})
             SELECT $5, s.player, {from_s}
             FROM jsonb_to_recordset($6) AS s(player text, {typed})
             ON CONFLICT (room_id, player_id) DO UPDATE SET {sums}
         ), r AS (
             UPDATE rooms SET last_active = now(), last_winner = coalesce($7, last_winner),
                 history = coalesce($9, history)
             WHERE id = $5
         )
         SELECT pg_notify($8, $5::text) FROM g"
    )
});

/// One row per player with every stat column, for [`SAVE_SQL`].
fn stat_rows(stats: &[(String, Stat, i32)]) -> Value {
    let mut rows: Vec<(String, serde_json::Map<String, Value>)> = Vec::new();
    for (player, stat, n) in stats {
        let i = match rows.iter().position(|(p, _)| p == player) {
            Some(i) => i,
            None => {
                let zero = Stat::ALL
                    .iter()
                    .map(|s| (s.column().to_string(), json!(0)))
                    .collect();
                rows.push((player.clone(), zero));
                rows.len() - 1
            }
        };
        let cell = rows[i].1.get_mut(stat.column()).expect("every stat column");
        *cell = json!(cell.as_i64().unwrap_or(0) + i64::from(*n));
    }
    Value::Array(
        rows.into_iter()
            .map(|(player, mut cols)| {
                cols.insert("player".into(), json!(player));
                Value::Object(cols)
            })
            .collect(),
    )
}

/// Persist a game change: state, events, stats, and a notify, in one
/// statement inside the caller's transaction.
async fn save(
    tx: &mut Tx<'_>,
    room: &Room,
    game_id: i64,
    game: &Game,
    out: &Outcome,
) -> Result<()> {
    let events: Vec<Value> = out
        .events
        .iter()
        .map(|e| json!({ "player": e.player, "kind": e.kind, "payload": e.payload }))
        .collect();
    let winner = game.winners().first().map(|w| w.to_string());
    // the game just scored adds its result to the room's history (SET-14)
    let history = out
        .events
        .iter()
        .any(|e| e.kind == "scored")
        .then(|| room.history_after(game));
    sqlx::query(&SAVE_SQL)
        .bind(serde_json::to_value(game)?)
        .bind(game.status.as_str())
        .bind(game_id)
        .bind(Value::Array(events))
        .bind(room.id)
        .bind(stat_rows(&out.stats))
        .bind(winner)
        .bind(NOTIFY_CHANNEL)
        .bind(history)
        .fetch_one(&mut **tx)
        .await?;
    Ok(())
}

async fn start_game(tx: &mut Tx<'_>, room: &Room, player: &str) -> Result<()> {
    if room.host != player {
        return Err(ApiError::forbidden("only the host can start a game"));
    }
    if lock_game(tx, room.id).await?.is_some() {
        return Err(ApiError::invalid("a game is already in progress"));
    }
    // each seat's player, and its level when it is a bot
    let seated: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT m.player_id, CASE WHEN p.bot THEN p.bot_level END
         FROM room_members m JOIN players p ON p.id = m.player_id
         WHERE m.room_id = $1 AND m.left_at IS NULL AND NOT m.removed AND m.present_until > now()
         ORDER BY m.joined LIMIT $2",
    )
    .bind(room.id)
    .bind(game::MAX_SEATS as i64)
    .fetch_all(&mut **tx)
    .await?;
    if seated.len() < 2 {
        return Err(ApiError::invalid("at least 2 present players are needed"));
    }
    let bots: Vec<(String, bot::Level)> = seated
        .iter()
        .filter_map(|(p, level)| {
            let level = level.as_deref()?;
            Some((p.clone(), bot::Level::parse(level).unwrap_or_default()))
        })
        .collect();
    let players: Vec<String> = seated.into_iter().map(|(p, _)| p).collect();
    let (game, out) = {
        use rand::seq::SliceRandom;
        let mut rng = rand::rng();
        let mut deck = game::deck_for(players.len(), room.settings.hand_size);
        deck.shuffle(&mut rng);
        let first = match room
            .last_winner
            .as_ref()
            .and_then(|w| players.iter().position(|p| p == w))
        {
            Some(winner) => winner + 1,
            None => rng.random_range(0..players.len()),
        };
        let mut game = Game::new(players, deck, first, now_ms(), room.settings);
        let (match_no, carry) = room.carry();
        game.carry_in(match_no, carry);
        game.seat_bots(|p| bots.iter().find(|(b, _)| b == p).map(|(_, l)| *l));
        let mut out = Outcome::default();
        out.events.push(game::Event {
            player: Some(player.to_string()),
            kind: "start",
            payload: json!({}),
        });
        (game, out)
    };
    let game_id: i64 = sqlx::query_scalar(
        "INSERT INTO games (room_id, status, state, version) VALUES ($1, $2, $3, 0) RETURNING id",
    )
    .bind(room.id)
    .bind(game.status.as_str())
    .bind(serde_json::to_value(&game)?)
    .fetch_one(&mut **tx)
    .await?;
    save(tx, room, game_id, &game, &out).await
}

/// What a client can send: starting a game, or a move in it with the
/// `turn_seq` it was chosen against (see [`Game::apply`]) and, for a match,
/// the reaction time the client measured (RT-18).
pub enum ClientAction {
    Start,
    Game {
        action: Action,
        turn_seq: Option<u64>,
        reaction_ms: Option<u64>,
    },
}

/// Take a whole number field out of an action body.
fn take_whole(value: &mut Value, field: &str) -> Result<Option<u64>> {
    match value.as_object_mut().and_then(|o| o.remove(field)) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| ApiError::invalid(format!("{field} must be a whole number"))),
    }
}

impl ClientAction {
    pub fn parse(mut value: Value) -> Result<Self> {
        if value.get("type").and_then(Value::as_str) == Some("start") {
            return Ok(Self::Start);
        }
        let turn_seq = take_whole(&mut value, "turn_seq")?;
        let reaction_ms = take_whole(&mut value, "reaction_ms")?;
        serde_json::from_value(value)
            .map(|action| Self::Game {
                action,
                turn_seq,
                reaction_ms,
            })
            .map_err(|e| ApiError::invalid(format!("unknown action: {e}")))
    }
}

/// What an applied action left to wait for.
#[derive(Debug, PartialEq, Eq)]
pub enum Acted {
    Done,
    /// A match was claimed; it settles when its window closes at `deadline`
    /// (RT-16), see [`claim_result`].
    Claimed {
        id: String,
        deadline: i64,
    },
}

/// Apply one client action for a member of the room. `reaction_ms` is a
/// match's reaction time as the server measured and bounded it (RT-18);
/// `None` times it from when the discard landed.
pub async fn act(
    db: &DbPool,
    room: &Room,
    player: &str,
    action: ClientAction,
    reaction_ms: Option<i64>,
) -> Result<Acted> {
    let mut tx = db.begin().await?;
    // always room then game, so concurrent paths lock in the same order
    let room = lock_room_as(&mut tx, room.id, player).await?;
    let mut acted = Acted::Done;
    match action {
        ClientAction::Start => start_game(&mut tx, &room, player).await?,
        ClientAction::Game {
            action, turn_seq, ..
        } => {
            let (game_id, mut game) = lock_game(&mut tx, room.id)
                .await?
                .ok_or_else(|| ApiError::invalid("no game is in progress"))?;
            let out = {
                let mut rng = rand::rng();
                game.apply_timed(player, action, turn_seq, reaction_ms, now_ms(), &mut rng)?
            };
            if let Some((id, deadline)) = &out.claim {
                acted = Acted::Claimed {
                    id: id.clone(),
                    deadline: *deadline,
                };
            }
            save(&mut tx, &room, game_id, &game, &out).await?;
        }
    }
    tx.commit().await?;
    Ok(acted)
}

/// Wait for a claimed match's window to close, settle it, and return how it
/// ended (RT-16). Every claimer in a window wakes at the same deadline; the
/// first to lock the game settles all of them.
pub async fn claim_result(db: &DbPool, room_id: i64, id: &str, deadline: i64) -> Result<()> {
    let wait = (deadline - now_ms()).max(0) as u64;
    tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
    let game = match tick_room(db, room_id).await? {
        Some(game) => Some(game),
        // the game finished before this waiter woke
        None => {
            let state: Option<Value> = sqlx::query_scalar(
                "SELECT state FROM games WHERE room_id = $1 ORDER BY id DESC LIMIT 1",
            )
            .bind(room_id)
            .fetch_optional(db)
            .await?;
            state.map(serde_json::from_value).transpose()?
        }
    };
    match game.and_then(|g| g.claim_result(id)) {
        Some(result) => Ok(result?),
        None => Err(ApiError::from(Reject::new(
            "too_late",
            "that match could not be settled",
        ))),
    }
}

/// Advance timers on every unfinished game that has something due. One
/// read, without locks, tries each game's tick on a copy; only the games it
/// would change are locked and ticked for real (RT-14).
pub async fn tick_all(db: &DbPool) -> Result<()> {
    let rows = sqlx::query(
        "SELECT g.room_id, g.state,
                coalesce(array(SELECT m.player_id FROM room_members m
                               WHERE m.room_id = g.room_id AND m.present_until > now()),
                         '{}') AS present
         FROM games g WHERE g.status <> 'scored'",
    )
    .fetch_all(db)
    .await?;
    let now = now_ms();
    let mut due = vec![];
    for row in rows {
        let mut game: Game = serde_json::from_value(row.get::<Value, _>("state"))?;
        let present: Vec<String> = row.get("present");
        let changed = {
            let mut rng = rand::rng();
            game.tick(now, &|p| present.iter().any(|x| x == p), &mut rng)
                .changed
        };
        if changed {
            due.push(row.get::<i64, _>("room_id"));
        }
    }
    for room_id in due {
        tick_room(db, room_id).await?;
    }
    Ok(())
}

/// Advance the timers of one room's unfinished game, if it has one, and
/// return the game as it stands after.
pub async fn tick_room(db: &DbPool, room_id: i64) -> Result<Option<Game>> {
    let mut tx = db.begin().await?;
    let room = lock_room(&mut tx, room_id).await?;
    // the game row and who is present, in one round trip
    let row = sqlx::query(
        "SELECT id, state,
                coalesce(array(SELECT m.player_id FROM room_members m
                               WHERE m.room_id = $1 AND m.present_until > now()),
                         '{}') AS present
         FROM games WHERE room_id = $1 AND status <> 'scored' FOR UPDATE",
    )
    .bind(room_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let game_id: i64 = row.get("id");
    let mut game: Game = serde_json::from_value(row.get::<Value, _>("state"))?;
    let present: Vec<String> = row.get("present");
    let out = {
        let mut rng = rand::rng();
        game.tick(now_ms(), &|p| present.iter().any(|x| x == p), &mut rng)
    };
    if out.changed {
        save(&mut tx, &room, game_id, &game, &out).await?;
        tx.commit().await?;
    }
    Ok(Some(game))
}

// ---------------------------------------------------------------------------
// Sealed reveals
// ---------------------------------------------------------------------------

/// Bind a client key to `player` on first use and refuse it once its window
/// has passed or when another player bound it first. One statement: a fresh
/// key comes back from the insert, a known one from the table.
async fn check_client_key(tx: &mut Tx<'_>, player: &str, key_hash: &str) -> Result<()> {
    let row = sqlx::query(
        "WITH ins AS (
             INSERT INTO client_keys (key_hash, player_id) VALUES ($1, $2)
             ON CONFLICT (key_hash) DO NOTHING
             RETURNING player_id, false AS expired
         )
         SELECT player_id, expired FROM ins
         UNION ALL
         SELECT player_id, first_seen < now() - make_interval(secs => $3)
         FROM client_keys WHERE key_hash = $1 AND NOT EXISTS (SELECT 1 FROM ins)",
    )
    .bind(key_hash)
    .bind(player)
    .bind(CLIENT_KEY_SECS as f64)
    .fetch_one(&mut **tx)
    .await?;
    if row.get::<String, _>("player_id") != player {
        return Err(ApiError::forbidden("that key belongs to another player"));
    }
    if row.get::<bool, _>("expired") {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "key_expired",
            "the client key expired; make a new one",
        ));
    }
    Ok(())
}

/// The private value `player` asked for, after every check (SEAL-5,
/// SEAL-6). The caller seals it to the client's key.
pub async fn reveal(
    db: &DbPool,
    room: &Room,
    player: &str,
    what: &Secret,
    key_hash: &str,
) -> Result<Value> {
    let mut tx = db.begin().await?;
    // lock room then game like every action, so a concurrent swap, ready, or
    // discard can't commit between this check and the reveal
    lock_room_as(&mut tx, room.id, player).await?;
    check_client_key(&mut tx, player, key_hash).await?;
    let (game_id, game) = lock_game(&mut tx, room.id)
        .await?
        .ok_or_else(|| ApiError::forbidden("no game is in progress"))?;
    let secret = game.secret(player, what, now_ms())?;
    if let Secret::Peek(id) = what {
        let fresh = sqlx::query(
            "INSERT INTO reveal_fetches (reveal_id, game_id) VALUES ($1, $2)
             ON CONFLICT (reveal_id) DO NOTHING",
        )
        .bind(id)
        .bind(game_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if fresh == 0 {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "already_revealed",
                "that peek was already revealed",
            ));
        }
    }
    tx.commit().await?;
    Ok(secret)
}

pub async fn delete_stale_rooms(db: &DbPool) -> Result<u64> {
    sqlx::query("DELETE FROM client_keys WHERE first_seen < now() - interval '1 day'")
        .execute(db)
        .await?;
    // leases left behind by a machine that died mid-socket
    sqlx::query("DELETE FROM room_observers WHERE until <= now()")
        .execute(db)
        .await?;
    let rooms = sqlx::query("DELETE FROM rooms WHERE last_active < now() - interval '30 days'")
        .execute(db)
        .await?
        .rows_affected();
    // a bot lives only as long as a room it is in (BOT-1)
    sqlx::query(
        "DELETE FROM players p WHERE p.bot
         AND NOT EXISTS (SELECT 1 FROM room_members m WHERE m.player_id = p.id)",
    )
    .execute(db)
    .await?;
    Ok(rooms)
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

/// Everything a room's pages show, before redaction, loaded in one query.
/// A machine loads it once per change and renders it for each of its
/// sockets (RT-21).
pub struct Snapshot {
    room: Room,
    observers: i64,
    /// `{id, name, removed, left, present}` in join order.
    members: Vec<Value>,
    /// The newest game: id, version, and state.
    game: Option<(i64, i64, Game)>,
    /// The newest 40 events, newest first.
    events: Value,
    stats: Vec<Value>,
}

/// The single query behind [`Snapshot::load`]. The stat columns come from a
/// fixed enum, never from input.
static SNAPSHOT_SQL: LazyLock<String> = LazyLock::new(|| {
    let stats = Stat::ALL
        .iter()
        .map(|s| format!("'{0}', s.{0}", s.column()))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "SELECT {ROOM_COLUMNS},
                (SELECT count(*) FROM room_observers o
                 WHERE o.room_id = r.id AND o.until > now()) AS observers,
                (SELECT coalesce(jsonb_agg(jsonb_build_object(
                     'id', m.player_id, 'name', p.name, 'removed', m.removed,
                     'left', m.left_at IS NOT NULL,
                     'present', coalesce(m.present_until > now(), false), 'bot', p.bot,
                     'bot_level', CASE WHEN p.bot THEN p.bot_level END
                 ) ORDER BY m.joined), '[]')
                 FROM room_members m JOIN players p ON p.id = m.player_id
                 WHERE m.room_id = r.id) AS members,
                g.id AS game_id, g.version, g.state,
                (SELECT coalesce(jsonb_agg(jsonb_build_object(
                     'id', e.id, 'player', e.player_id, 'kind', e.kind, 'payload', e.payload
                 ) ORDER BY e.id DESC), '[]')
                 FROM (SELECT * FROM game_events WHERE game_id = g.id
                       ORDER BY id DESC LIMIT 40) e) AS events,
                (SELECT coalesce(jsonb_agg(jsonb_build_object(
                     'player', s.player_id, 'name', p.name, {stats}
                 ) ORDER BY s.wins DESC, s.games_played DESC, p.name), '[]')
                 FROM room_stats s JOIN players p ON p.id = s.player_id
                 WHERE s.room_id = r.id) AS stats
         FROM rooms r
         LEFT JOIN LATERAL (
             SELECT id, version, state FROM games WHERE room_id = r.id ORDER BY id DESC LIMIT 1
         ) g ON true
         WHERE r.id = $1"
    )
});

fn json_array(v: Value) -> Vec<Value> {
    match v {
        Value::Array(items) => items,
        _ => vec![],
    }
}

impl Snapshot {
    pub async fn load(db: &DbPool, room_id: i64) -> Result<Self> {
        let row = sqlx::query(&SNAPSHOT_SQL)
            .bind(room_id)
            .fetch_optional(db)
            .await?
            .ok_or_else(ApiError::not_found)?;
        let game = match row.get::<Option<i64>, _>("game_id") {
            Some(id) => Some((
                id,
                row.get::<i64, _>("version"),
                serde_json::from_value(row.get::<Value, _>("state"))?,
            )),
            None => None,
        };
        Ok(Self {
            room: room_from_row(&row),
            observers: row.get("observers"),
            members: json_array(row.get("members")),
            game,
            events: row.get("events"),
            stats: json_array(row.get("stats")),
        })
    }

    fn membership(&self, player: &str) -> Option<(bool, bool)> {
        self.members
            .iter()
            .find(|m| m["id"] == player)
            .map(|m| (m["removed"] == true, m["left"] == true))
    }

    /// [`require_member`] against this snapshot.
    pub fn require_member(&self, player: &str) -> Result<()> {
        check_member(self.membership(player))
    }

    /// [`require_not_removed`] against this snapshot.
    pub fn require_not_removed(&self, player: &str) -> Result<()> {
        match self.membership(player) {
            Some((true, _)) => Err(ApiError::removed()),
            _ => Ok(()),
        }
    }

    /// The view for `viewer`, redacted for them. A `None` viewer is an
    /// observer, who sees only public state.
    pub fn render(&self, viewer: Option<&str>, base_url: &str) -> Value {
        let room = &self.room;
        let now = now_ms();
        let game = match &self.game {
            Some((id, version, state)) => {
                let mut view = match viewer {
                    Some(viewer) => state.view(viewer, now),
                    None => state.observer_view(now),
                };
                view["id"] = json!(id);
                view["version"] = json!(version);
                view
            }
            None => Value::Null,
        };
        // ids only grow, so a client can tell which events are new
        let events = if self.game.is_some() {
            self.events.clone()
        } else {
            json!([])
        };
        json!({
            "room": {
                "code": room.code,
                "host": room.host,
                "url": format!("{base_url}/komino/r/{}", room.code),
                "watch_url": format!("{base_url}/komino/r/{}/watch", room.code),
                "settings": room.settings,
                "history": room.match_history(),
            },
            "me": viewer,
            "observer": viewer.is_none(),
            // lets the client read deadlines against the server clock
            "server_now": now,
            "observers": self.observers,
            "members": self.members,
            "game": game,
            "events": events,
            "stats": self.stats,
        })
    }
}

/// Everything a member's page shows, redacted for that member. A `None`
/// viewer is an observer, who sees only public state.
pub async fn view(db: &DbPool, room: &Room, viewer: Option<&str>, base_url: &str) -> Result<Value> {
    Ok(Snapshot::load(db, room.id).await?.render(viewer, base_url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_deltas_fold_into_one_row_per_player() {
        let stats = vec![
            ("a".to_string(), Stat::CardsInteracted, 1),
            ("b".to_string(), Stat::Matches, 1),
            ("a".to_string(), Stat::CardsInteracted, 2),
            ("a".to_string(), Stat::Wins, 1),
        ];
        let rows = stat_rows(&stats);
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["player"], "a");
        assert_eq!(rows[0]["cards_interacted"], 3);
        assert_eq!(rows[0]["wins"], 1);
        assert_eq!(rows[0]["matches"], 0);
        assert_eq!(rows[1]["player"], "b");
        assert_eq!(rows[1]["matches"], 1);
        // every column is present, so the insert can name them all
        for stat in Stat::ALL {
            assert!(rows[1].get(stat.column()).is_some());
        }
        assert_eq!(stat_rows(&[]), json!([]));
    }

    #[test]
    fn membership_is_checked_the_same_from_either_source() {
        assert!(check_member(Some((false, false))).is_ok());
        assert_eq!(
            check_member(Some((true, false))).unwrap_err().code,
            "removed"
        );
        assert_eq!(
            check_member(Some((true, true))).unwrap_err().code,
            "removed"
        );
        assert_eq!(
            check_member(Some((false, true))).unwrap_err().code,
            "forbidden"
        );
        assert_eq!(check_member(None).unwrap_err().code, "forbidden");
    }
}
