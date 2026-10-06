//! Rooms, players, and the action pipeline. Postgres holds every bit of game
//! state; each action locks the game row, applies the rules engine, and saves
//! state, events, and stats in one transaction that also notifies the room.

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
}

const ROOM_COLUMNS: &str =
    "id, code, host_player_id, last_winner, hand_size, away_grace_secs, turn_limit_secs, reveal_secs";

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
        },
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
                                turn_limit_secs, reveal_secs)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (code) DO NOTHING RETURNING id",
        )
        .bind(&code)
        .bind(host)
        .bind(settings.hand_size as i32)
        .bind(settings.away_grace_secs as i32)
        .bind(settings.turn_limit_secs.map(|s| s as i32))
        .bind(settings.reveal_secs.map(|s| s as i32))
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
    let row = sqlx::query(&format!("SELECT {ROOM_COLUMNS} FROM rooms WHERE code = $1"))
        .bind(&code)
        .fetch_optional(db)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(room_from_row(&row))
}

async fn lock_room(tx: &mut Tx<'_>, room_id: i64) -> Result<Room> {
    let row = sqlx::query(&format!(
        "SELECT {ROOM_COLUMNS} FROM rooms WHERE id = $1 FOR UPDATE"
    ))
    .bind(room_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    Ok(room_from_row(&row))
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
    match row {
        Some(r) if r.get::<bool, _>("removed") => Err(ApiError::removed()),
        Some(r) if !r.get::<bool, _>("gone") => Ok(()),
        _ => Err(ApiError::forbidden("join the room first")),
    }
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
    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM room_observers WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    notify(&mut tx, room_id).await?;
    tx.commit().await?;
    Ok(())
}

/// Add a member, or bring a returning one back. Removed players stay out.
pub async fn join(db: &DbPool, room: &Room, player: &str) -> Result<()> {
    let mut tx = db.begin().await?;
    let removed: Option<bool> = sqlx::query_scalar(
        "SELECT removed FROM room_members WHERE room_id = $1 AND player_id = $2",
    )
    .bind(room.id)
    .bind(player)
    .fetch_optional(&mut *tx)
    .await?;
    if removed == Some(true) {
        return Err(ApiError::removed());
    }
    sqlx::query(
        "INSERT INTO room_members (room_id, player_id, present_until)
         VALUES ($1, $2, now() + make_interval(secs => $3))
         ON CONFLICT (room_id, player_id)
         DO UPDATE SET left_at = NULL, present_until = excluded.present_until",
    )
    .bind(room.id)
    .bind(player)
    .bind(PRESENCE_SECS as f64)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE rooms SET last_active = now() WHERE id = $1")
        .bind(room.id)
        .execute(&mut *tx)
        .await?;
    notify(&mut tx, room.id).await?;
    tx.commit().await?;
    Ok(())
}

/// Count a member present for another [`PRESENCE_SECS`], or away now.
pub async fn set_presence(db: &DbPool, room_id: i64, player: &str, present: bool) -> Result<()> {
    let secs = if present { PRESENCE_SECS } else { 0 };
    let mut tx = db.begin().await?;
    let was: Option<bool> = sqlx::query_scalar(
        "SELECT coalesce(present_until > now(), false) FROM room_members
         WHERE room_id = $1 AND player_id = $2",
    )
    .bind(room_id)
    .bind(player)
    .fetch_optional(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE room_members SET present_until = now() + make_interval(secs => $3)
         WHERE room_id = $1 AND player_id = $2",
    )
    .bind(room_id)
    .bind(player)
    .bind(secs as f64)
    .execute(&mut *tx)
    .await?;
    if was.is_some_and(|was| was != present) {
        notify(&mut tx, room_id).await?;
    }
    tx.commit().await?;
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
        // longest-standing present member first, then anyone still here
        let next: Option<String> = sqlx::query_scalar(
            "SELECT player_id FROM room_members
             WHERE room_id = $1 AND player_id <> $2 AND left_at IS NULL AND NOT removed
             ORDER BY coalesce(present_until > now(), false) DESC, joined
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
    let n = sqlx::query(
        "UPDATE room_members SET removed = true, present_until = NULL
         WHERE room_id = $1 AND player_id = $2",
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

pub async fn unban(db: &DbPool, room: &Room, host: &str, target: &str) -> Result<()> {
    if room.host != host {
        return Err(ApiError::forbidden("only the host can unban players"));
    }
    let mut tx = db.begin().await?;
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

/// Persist a game change: state, events, stats, and a notify, all inside the
/// caller's transaction.
async fn save(
    tx: &mut Tx<'_>,
    room: &Room,
    game_id: i64,
    game: &Game,
    out: &Outcome,
) -> Result<()> {
    let status = game.status.as_str();
    let version: i64 = sqlx::query_scalar(
        "UPDATE games SET state = $1, status = $2, version = version + 1,
             ended = CASE WHEN $2 = 'scored' THEN now() ELSE ended END
         WHERE id = $3 RETURNING version",
    )
    .bind(serde_json::to_value(game)?)
    .bind(status)
    .bind(game_id)
    .fetch_one(&mut **tx)
    .await?;
    for e in &out.events {
        sqlx::query(
            "INSERT INTO game_events (game_id, version, player_id, kind, payload)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(game_id)
        .bind(version)
        .bind(&e.player)
        .bind(e.kind)
        .bind(&e.payload)
        .execute(&mut **tx)
        .await?;
    }
    for (player, stat, n) in &out.stats {
        // the column comes from a fixed enum, never from input
        let col = stat.column();
        sqlx::query(&format!(
            "INSERT INTO room_stats (room_id, player_id, {col}) VALUES ($1, $2, $3)
             ON CONFLICT (room_id, player_id) DO UPDATE SET {col} = room_stats.{col} + excluded.{col}"
        ))
        .bind(room.id)
        .bind(player)
        .bind(n)
        .execute(&mut **tx)
        .await?;
    }
    let winner = game.winners().first().map(|w| w.to_string());
    sqlx::query(
        "UPDATE rooms SET last_active = now(), last_winner = coalesce($2, last_winner) WHERE id = $1",
    )
    .bind(room.id)
    .bind(winner)
    .execute(&mut **tx)
    .await?;
    notify(tx, room.id).await
}

async fn start_game(tx: &mut Tx<'_>, room: &Room, player: &str) -> Result<()> {
    if room.host != player {
        return Err(ApiError::forbidden("only the host can start a game"));
    }
    if lock_game(tx, room.id).await?.is_some() {
        return Err(ApiError::invalid("a game is already in progress"));
    }
    let players: Vec<String> = sqlx::query_scalar(
        "SELECT player_id FROM room_members
         WHERE room_id = $1 AND left_at IS NULL AND NOT removed AND present_until > now()
         ORDER BY joined LIMIT $2",
    )
    .bind(room.id)
    .bind(game::MAX_SEATS as i64)
    .fetch_all(&mut **tx)
    .await?;
    if players.len() < 2 {
        return Err(ApiError::invalid("at least 2 present players are needed"));
    }
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
        let game = Game::new(players, deck, first, now_ms(), room.settings);
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
    require_member(db, room.id, player).await?;
    let mut tx = db.begin().await?;
    // always room then game, so concurrent paths lock in the same order
    let room = lock_room(&mut tx, room.id).await?;
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
    tick_room(db, room_id).await?;
    let state: Option<Value> =
        sqlx::query_scalar("SELECT state FROM games WHERE room_id = $1 ORDER BY id DESC LIMIT 1")
            .bind(room_id)
            .fetch_optional(db)
            .await?;
    let game: Option<Game> = state.map(serde_json::from_value).transpose()?;
    match game.and_then(|g| g.claim_result(id)) {
        Some(result) => Ok(result?),
        None => Err(ApiError::from(Reject::new(
            "too_late",
            "that match could not be settled",
        ))),
    }
}

/// Advance timers on every unfinished game.
pub async fn tick_all(db: &DbPool) -> Result<()> {
    let ids: Vec<i64> = sqlx::query_scalar("SELECT room_id FROM games WHERE status <> 'scored'")
        .fetch_all(db)
        .await?;
    for room_id in ids {
        tick_room(db, room_id).await?;
    }
    Ok(())
}

/// Advance the timers of one room's unfinished game, if it has one.
pub async fn tick_room(db: &DbPool, room_id: i64) -> Result<()> {
    let mut tx = db.begin().await?;
    let room = lock_room(&mut tx, room_id).await?;
    let Some((game_id, mut game)) = lock_game(&mut tx, room_id).await? else {
        return Ok(());
    };
    let present: Vec<String> = sqlx::query_scalar(
        "SELECT player_id FROM room_members WHERE room_id = $1 AND present_until > now()",
    )
    .bind(room_id)
    .fetch_all(&mut *tx)
    .await?;
    let out = {
        let mut rng = rand::rng();
        game.tick(now_ms(), &|p| present.iter().any(|x| x == p), &mut rng)
    };
    if out.changed {
        save(&mut tx, &room, game_id, &game, &out).await?;
    }
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Sealed reveals
// ---------------------------------------------------------------------------

/// Bind a client key to `player` on first use and refuse it once its window
/// has passed or when another player bound it first.
async fn check_client_key(tx: &mut Tx<'_>, player: &str, key_hash: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO client_keys (key_hash, player_id) VALUES ($1, $2)
         ON CONFLICT (key_hash) DO NOTHING",
    )
    .bind(key_hash)
    .bind(player)
    .execute(&mut **tx)
    .await?;
    let row = sqlx::query(
        "SELECT player_id, first_seen < now() - make_interval(secs => $2) AS expired
         FROM client_keys WHERE key_hash = $1",
    )
    .bind(key_hash)
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
    require_member(db, room.id, player).await?;
    let mut tx = db.begin().await?;
    check_client_key(&mut tx, player, key_hash).await?;
    // lock room then game like every action, so a concurrent swap, ready, or
    // discard can't commit between this check and the reveal
    lock_room(&mut tx, room.id).await?;
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
    Ok(
        sqlx::query("DELETE FROM rooms WHERE last_active < now() - interval '30 days'")
            .execute(db)
            .await?
            .rows_affected(),
    )
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

/// Everything a member's page shows, redacted for that member. A `None`
/// viewer is an observer, who sees only public state.
pub async fn view(db: &DbPool, room: &Room, viewer: Option<&str>, base_url: &str) -> Result<Value> {
    let room = room_by_code(db, &room.code).await?;
    let observers: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM room_observers WHERE room_id = $1 AND until > now()",
    )
    .bind(room.id)
    .fetch_one(db)
    .await?;
    let members: Vec<Value> = sqlx::query(
        "SELECT m.player_id, p.name, m.removed, m.left_at IS NOT NULL AS gone,
                coalesce(m.present_until > now(), false) AS present
         FROM room_members m JOIN players p ON p.id = m.player_id
         WHERE m.room_id = $1 ORDER BY m.joined",
    )
    .bind(room.id)
    .fetch_all(db)
    .await?
    .iter()
    .map(|r| {
        json!({
            "id": r.get::<String, _>("player_id"),
            "name": r.get::<String, _>("name"),
            "removed": r.get::<bool, _>("removed"),
            "left": r.get::<bool, _>("gone"),
            "present": r.get::<bool, _>("present"),
        })
    })
    .collect();

    let game_row = sqlx::query(
        "SELECT id, version, state FROM games WHERE room_id = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(room.id)
    .fetch_optional(db)
    .await?;
    let (game, events) = match game_row {
        Some(row) => {
            let game_id: i64 = row.get("id");
            let state: Game = serde_json::from_value(row.get::<Value, _>("state"))?;
            let mut view = match viewer {
                Some(viewer) => state.view(viewer, now_ms()),
                None => state.observer_view(now_ms()),
            };
            view["id"] = json!(game_id);
            view["version"] = json!(row.get::<i64, _>("version"));
            let events: Vec<Value> = sqlx::query(
                "SELECT id, player_id, kind, payload FROM game_events
                 WHERE game_id = $1 ORDER BY id DESC LIMIT 40",
            )
            .bind(game_id)
            .fetch_all(db)
            .await?
            .iter()
            .map(|r| {
                // ids only grow, so a client can tell which events are new
                json!({
                    "id": r.get::<i64, _>("id"),
                    "player": r.get::<Option<String>, _>("player_id"),
                    "kind": r.get::<String, _>("kind"),
                    "payload": r.get::<Value, _>("payload"),
                })
            })
            .collect();
            (view, events)
        }
        None => (Value::Null, vec![]),
    };

    let stats: Vec<Value> = sqlx::query(
        "SELECT s.*, p.name FROM room_stats s JOIN players p ON p.id = s.player_id
         WHERE s.room_id = $1 ORDER BY s.wins DESC, s.games_played DESC, p.name",
    )
    .bind(room.id)
    .fetch_all(db)
    .await?
    .iter()
    .map(|r| {
        let mut row = json!({
            "player": r.get::<String, _>("player_id"),
            "name": r.get::<String, _>("name"),
        });
        for stat in Stat::ALL {
            row[stat.column()] = json!(r.get::<i32, _>(stat.column()));
        }
        row
    })
    .collect();

    Ok(json!({
        "room": {
            "code": room.code,
            "host": room.host,
            "url": format!("{base_url}/komino/r/{}", room.code),
            "watch_url": format!("{base_url}/komino/r/{}/watch", room.code),
            "settings": room.settings,
        },
        "me": viewer,
        "observer": viewer.is_none(),
        // lets the client read deadlines against the server clock
        "server_now": now_ms(),
        "observers": observers,
        "members": members,
        "game": game,
        "events": events,
        "stats": stats,
    }))
}
