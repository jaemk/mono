//! Rooms, players, and the action pipeline. Postgres holds every bit of game
//! state; each action locks the game row, applies the rules engine, and saves
//! state, events, and stats in one transaction that also notifies the room.

use crate::game::{self, Action, Game, Outcome, Reject, Stat};
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

/// How long a page load or socket heartbeat counts a member as present.
pub const PRESENCE_SECS: i64 = 25;

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
        let status = if r.code == "too_late" || r.code == "stale" {
            StatusCode::CONFLICT
        } else {
            StatusCode::BAD_REQUEST
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

pub async fn create_room(db: &DbPool, host: &str) -> Result<Room> {
    for _ in 0..10 {
        let code = new_code();
        let mut tx = db.begin().await?;
        let id: Option<i64> = sqlx::query_scalar(
            "INSERT INTO rooms (code, host_player_id) VALUES ($1, $2)
             ON CONFLICT (code) DO NOTHING RETURNING id",
        )
        .bind(&code)
        .bind(host)
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
    let row =
        sqlx::query("SELECT id, code, host_player_id, last_winner FROM rooms WHERE code = $1")
            .bind(&code)
            .fetch_optional(db)
            .await?
            .ok_or_else(ApiError::not_found)?;
    Ok(Room {
        id: row.get("id"),
        code: row.get("code"),
        host: row.get("host_player_id"),
        last_winner: row.get("last_winner"),
    })
}

async fn lock_room(tx: &mut Tx<'_>, room_id: i64) -> Result<Room> {
    let row = sqlx::query(
        "SELECT id, code, host_player_id, last_winner FROM rooms WHERE id = $1 FOR UPDATE",
    )
    .bind(room_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    Ok(Room {
        id: row.get("id"),
        code: row.get("code"),
        host: row.get("host_player_id"),
        last_winner: row.get("last_winner"),
    })
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
        let mut deck = game::deck();
        deck.shuffle(&mut rng);
        let first = match room
            .last_winner
            .as_ref()
            .and_then(|w| players.iter().position(|p| p == w))
        {
            Some(winner) => winner + 1,
            None => rng.random_range(0..players.len()),
        };
        let game = Game::new(players, deck, first, now_ms());
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
/// `turn_seq` it was chosen against (see [`Game::apply`]).
pub enum ClientAction {
    Start,
    Game(Action, Option<u64>),
}

impl ClientAction {
    pub fn parse(mut value: Value) -> Result<Self> {
        if value.get("type").and_then(Value::as_str) == Some("start") {
            return Ok(Self::Start);
        }
        let turn_seq = match value.as_object_mut().and_then(|o| o.remove("turn_seq")) {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                v.as_u64()
                    .ok_or_else(|| ApiError::invalid("turn_seq must be a whole number"))?,
            ),
        };
        serde_json::from_value(value)
            .map(|action| Self::Game(action, turn_seq))
            .map_err(|e| ApiError::invalid(format!("unknown action: {e}")))
    }
}

/// Apply one client action for a member of the room.
pub async fn act(db: &DbPool, room: &Room, player: &str, action: ClientAction) -> Result<()> {
    require_member(db, room.id, player).await?;
    let mut tx = db.begin().await?;
    // always room then game, so concurrent paths lock in the same order
    let room = lock_room(&mut tx, room.id).await?;
    match action {
        ClientAction::Start => start_game(&mut tx, &room, player).await?,
        ClientAction::Game(action, turn_seq) => {
            let (game_id, mut game) = lock_game(&mut tx, room.id)
                .await?
                .ok_or_else(|| ApiError::invalid("no game is in progress"))?;
            let out = {
                let mut rng = rand::rng();
                game.apply(player, action, turn_seq, now_ms(), &mut rng)?
            };
            save(&mut tx, &room, game_id, &game, &out).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Advance timers on every unfinished game.
pub async fn tick_all(db: &DbPool) -> Result<()> {
    let ids: Vec<i64> = sqlx::query_scalar("SELECT room_id FROM games WHERE status <> 'scored'")
        .fetch_all(db)
        .await?;
    for room_id in ids {
        let mut tx = db.begin().await?;
        let room = lock_room(&mut tx, room_id).await?;
        let Some((game_id, mut game)) = lock_game(&mut tx, room_id).await? else {
            continue;
        };
        let present: Vec<String> = sqlx::query_scalar(
            "SELECT player_id FROM room_members WHERE room_id = $1 AND present_until > now()",
        )
        .bind(room_id)
        .fetch_all(&mut *tx)
        .await?;
        let out = game.tick(now_ms(), &|p| present.iter().any(|x| x == p));
        if out.changed {
            save(&mut tx, &room, game_id, &game, &out).await?;
        }
        tx.commit().await?;
    }
    Ok(())
}

pub async fn delete_stale_rooms(db: &DbPool) -> Result<u64> {
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

/// Everything a member's page shows, redacted for that member.
pub async fn view(db: &DbPool, room: &Room, viewer: &str, base_url: &str) -> Result<Value> {
    let room = room_by_code(db, &room.code).await?;
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
            let mut view = state.view(viewer, now_ms());
            view["id"] = json!(game_id);
            view["version"] = json!(row.get::<i64, _>("version"));
            let events: Vec<Value> = sqlx::query(
                "SELECT player_id, kind, payload FROM game_events
                 WHERE game_id = $1 ORDER BY id DESC LIMIT 40",
            )
            .bind(game_id)
            .fetch_all(db)
            .await?
            .iter()
            .map(|r| {
                json!({
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
        },
        "me": viewer,
        "members": members,
        "game": game,
        "events": events,
        "stats": stats,
    }))
}
