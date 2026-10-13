//! Accounts and sessions. An anonymous player becomes an account by choosing
//! a username and password; from then on only a session, whose token lives
//! in its own cookie and is stored here hashed, reaches that player.

use crate::models::{self, ApiError, Player, Result};
use axum::http::StatusCode;
use common::db::DbPool;
use serde_json::{json, Value};
use sqlx::Row;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

pub const MIN_USERNAME_CHARS: usize = 3;
pub const MAX_USERNAME_CHARS: usize = 24;
pub const MIN_PASSWORD_CHARS: usize = 8;
pub const MAX_PASSWORD_BYTES: usize = 256;

/// How long a session lasts since it was last used (ACCT-6).
pub const SESSION_DAYS: i64 = 30;
/// A session is extended at most once a day: when less than this is left.
const SESSION_REFRESH_DAYS: i64 = 29;

/// Wrong passwords in a row from one address before the account locks for
/// that address (ACCT-9).
pub const MAX_FAILED_LOGINS: i32 = 5;
pub const LOCK_SECS: i64 = 300;
/// How long an address's wrong password count is kept without a new one.
const LOCK_KEPT_HOURS: i32 = 24;

/// The most of a user agent a session keeps (ACCT-19).
pub const MAX_AGENT_CHARS: usize = 200;
/// A session's last use is written at most this often.
const LAST_USED_SECS: f64 = 3_600.0;

/// Password checks one address may start at once, and how fast it gets
/// more after (ACCT-22).
pub const AUTH_BURST: f64 = 30.0;
pub const AUTH_REFILL_SECS: f64 = 2.0;
/// Addresses the limiter remembers before it forgets the ones back to full.
const LIMITER_KEYS: usize = 10_000;
/// How long a password check waits for a free hashing slot (ACCT-22).
const HASH_WAIT: Duration = Duration::from_secs(10);

/// Characters in a recovery code, 5 bits each (ACCT-18).
pub const RECOVERY_CHARS: usize = 25;
/// Lower case, without `i`, `l`, `o`, and `u`, which read as other things.
const RECOVERY_ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// Rooms the account page lists (ACCT-17).
pub const ROOMS_SHOWN: i64 = 50;

/// Hashing slots for the whole machine: half its cores, so password checks
/// can't take every core from the games (ACCT-22).
static HASHING: LazyLock<Semaphore> = LazyLock::new(|| {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    Semaphore::new(cores.div_ceil(2))
});

/// Hashed once, so an unknown username costs a login as much as a wrong
/// password and the two can't be told apart by timing.
static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| {
    common::crypto::hash_password(b"komino-no-such-account").expect("a dummy password hash")
});

fn unauthorized() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "wrong username or password",
    )
}

fn locked() -> ApiError {
    ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "locked",
        "too many wrong passwords, try again in a few minutes",
    )
}

/// Usernames are case-insensitive, so they are kept lower case.
pub fn normalize_username(raw: &str) -> String {
    raw.trim().to_lowercase()
}

pub fn validate_username(raw: &str) -> Result<String> {
    let name = normalize_username(raw);
    let n = name.chars().count();
    if !(MIN_USERNAME_CHARS..=MAX_USERNAME_CHARS).contains(&n) {
        return Err(ApiError::invalid(format!(
            "username must be {MIN_USERNAME_CHARS}-{MAX_USERNAME_CHARS} characters"
        )));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_-.".contains(c))
    {
        return Err(ApiError::invalid(
            "username may only use letters, digits, '_', '-', and '.'",
        ));
    }
    Ok(name)
}

pub fn validate_password(pw: &str) -> Result<()> {
    if pw.chars().count() < MIN_PASSWORD_CHARS {
        return Err(ApiError::invalid(format!(
            "password must be at least {MIN_PASSWORD_CHARS} characters"
        )));
    }
    if pw.len() > MAX_PASSWORD_BYTES {
        return Err(ApiError::invalid(format!(
            "password must be at most {MAX_PASSWORD_BYTES} bytes"
        )));
    }
    Ok(())
}

/// What the session table stores for a cookie token.
pub fn token_hash(token: &str) -> String {
    hex::encode(common::crypto::sha256(token.as_bytes()))
}

fn internal(what: &str) -> ApiError {
    tracing::error!("komino {what} error");
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "error", what.to_string())
}

fn busy() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "busy",
        "too many sign ins right now, try again in a moment",
    )
}

/// Hashing is slow on purpose, so it runs off the async workers, and only a
/// few at a time (ACCT-22).
async fn hashing<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    let _slot = tokio::time::timeout(HASH_WAIT, HASHING.acquire())
        .await
        .map_err(|_| busy())?
        .map_err(|_| internal("password hash"))?;
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| internal("password hash"))
}

async fn hash_password(pw: String) -> Result<String> {
    hashing(move || common::crypto::hash_password(pw.as_bytes()))
        .await?
        .map_err(|_| internal("password hash"))
}

async fn verify_password(pw: String, hash: String) -> Result<bool> {
    hashing(move || common::crypto::verify_password(pw.as_bytes(), &hash)).await
}

/// Password checks per client address (ACCT-22): a bucket of
/// [`AUTH_BURST`] that refills one every [`AUTH_REFILL_SECS`]. Each machine
/// keeps its own.
#[derive(Default)]
pub struct Limiter {
    buckets: Mutex<HashMap<String, (f64, Instant)>>,
}

impl Limiter {
    /// Take one check for `key` at `now`, false when its bucket is empty.
    pub fn take(&self, key: &str, now: Instant) -> bool {
        let mut buckets = self.buckets.lock().unwrap();
        let level = |(left, at): (f64, Instant)| {
            (left + now.saturating_duration_since(at).as_secs_f64() / AUTH_REFILL_SECS)
                .min(AUTH_BURST)
        };
        if buckets.len() >= LIMITER_KEYS {
            // a full bucket is the same as a forgotten one
            buckets.retain(|_, b| level(*b) < AUTH_BURST);
        }
        let left = buckets.get(key).map_or(AUTH_BURST, |b| level(*b));
        if left < 1.0 {
            return false;
        }
        buckets.insert(key.to_string(), (left - 1.0, now));
        true
    }

    /// [`Limiter::take`] for a request from `addr`, as an error when it is
    /// refused. A request without an address (not through the proxy) is
    /// left to the hashing slots.
    pub fn check(&self, addr: Option<&str>) -> Result<()> {
        match addr.map(limit_key) {
            Some(key) if !self.take(&key, Instant::now()) => Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "slow_down",
                "too many attempts, try again in a minute",
            )),
            _ => Ok(()),
        }
    }
}

/// The bucket a client address draws from (ACCT-22). An IPv6 client
/// usually holds a whole /64, so the /64 shares one bucket.
pub fn limit_key(addr: &str) -> String {
    use std::net::IpAddr;
    match addr.trim().parse::<IpAddr>() {
        Ok(IpAddr::V6(v6)) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = v6.segments();
                format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
            }
        },
        Ok(IpAddr::V4(v4)) => v4.to_string(),
        Err(_) => addr.trim().to_string(),
    }
}

/// Who is asking: the address a wrong password locks the account for
/// (ACCT-9), and the device a new session is shown as (ACCT-19).
#[derive(Clone, Debug, Default)]
pub struct Client {
    /// The client address as [`limit_key`] gives it, empty without one.
    pub addr: String,
    /// The user agent, cut to [`MAX_AGENT_CHARS`].
    pub agent: String,
}

impl Client {
    pub fn new(addr: Option<&str>, agent: Option<&str>) -> Client {
        Client {
            addr: addr.map(limit_key).unwrap_or_default(),
            agent: agent
                .unwrap_or("")
                .chars()
                .filter(|c| !c.is_control())
                .take(MAX_AGENT_CHARS)
                .collect(),
        }
    }
}

/// A new recovery code: [`RECOVERY_CHARS`] random characters in groups of
/// five (ACCT-18).
pub fn new_recovery_code() -> Result<String> {
    let bytes =
        common::crypto::rand_bytes(RECOVERY_CHARS).map_err(|_| internal("recovery code"))?;
    // 256 is a multiple of 32, so the low five bits are uniform
    let chars: Vec<char> = bytes
        .iter()
        .map(|b| RECOVERY_ALPHABET[usize::from(b & 31)] as char)
        .collect();
    Ok(chars
        .chunks(5)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-"))
}

/// A recovery code as typed: case, spaces, and dashes don't matter, and the
/// letters it leaves out read as the digits they look like.
pub fn normalize_recovery_code(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| match c.to_ascii_lowercase() {
            'i' | 'l' => '1',
            'o' => '0',
            c => c,
        })
        .collect()
}

/// What the players table stores for a recovery code. The code is random
/// enough that a plain hash keeps it safe.
pub fn recovery_hash(code: &str) -> String {
    hex::encode(common::crypto::sha256(
        normalize_recovery_code(code).as_bytes(),
    ))
}

/// Start a session for `player`, returning the cookie token. Only the
/// token's hash is stored, so a read of the table can't sign anyone in.
async fn new_session<'e, E>(db: E, player: &str, client: &Client) -> Result<String>
where
    E: sqlx::PgExecutor<'e>,
{
    let token = hex::encode(common::crypto::rand_bytes(32).map_err(|_| internal("session"))?);
    sqlx::query(
        "INSERT INTO sessions (token_hash, player_id, expires, agent)
         VALUES ($1, $2, now() + make_interval(days => $3), $4)",
    )
    .bind(token_hash(&token))
    .bind(player)
    .bind(SESSION_DAYS as i32)
    .bind(&client.agent)
    .execute(db)
    .await?;
    Ok(token)
}

/// A new account's or recovered account's sign in: the account, its
/// session token, and its new recovery code (ACCT-18).
pub struct Signed {
    pub player: Player,
    pub token: String,
    pub recovery_code: String,
}

/// Turn the anonymous `player` into an account (ACCT-2), keeping their
/// rooms and stats. Their default name becomes `name`, or the username when
/// none is given (ACCT-11).
pub async fn signup(
    db: &DbPool,
    player: &Player,
    username: &str,
    password: &str,
    name: Option<&str>,
    client: &Client,
) -> Result<Signed> {
    if player.username.is_some() {
        return Err(ApiError::invalid(
            "sign out before creating another account",
        ));
    }
    let username = validate_username(username)?;
    validate_password(password)?;
    let name = match name.map(str::trim) {
        None | Some("") => username.clone(),
        Some(n) => models::validate_name(n)?,
    };
    let hash = hash_password(password.to_string()).await?;
    let recovery_code = new_recovery_code()?;
    let mut tx = db.begin().await?;
    let updated = sqlx::query(
        "UPDATE players SET username = $2, password_hash = $3, name = $4, recovery_hash = $5
         WHERE id = $1 AND username IS NULL AND NOT bot",
    )
    .bind(&player.id)
    .bind(&username)
    .bind(&hash)
    .bind(&name)
    .bind(recovery_hash(&recovery_code))
    .execute(&mut *tx)
    .await;
    match updated.map_err(taken_or) {
        Ok(r) if r.rows_affected() == 1 => {}
        Ok(_) => return Err(ApiError::invalid("this player can't become an account")),
        Err(e) => return Err(e),
    }
    models::notify_rooms_of(&mut tx, &player.id).await?;
    let token = new_session(&mut *tx, &player.id, client).await?;
    tx.commit().await?;
    Ok(Signed {
        player: Player {
            id: player.id.clone(),
            name,
            username: Some(username),
        },
        token,
        recovery_code,
    })
}

/// A taken username as `409 taken`, any other database error as itself.
fn taken_or(e: sqlx::Error) -> ApiError {
    match e {
        sqlx::Error::Database(e) if e.is_unique_violation() => {
            ApiError::new(StatusCode::CONFLICT, "taken", "that username is taken")
        }
        e => e.into(),
    }
}

/// Check `password` against account `id` from `client`. A wrong one counts
/// toward locking the account for the client's address after
/// [`MAX_FAILED_LOGINS`] in a row, and a right one resets that address's
/// count (ACCT-9). False for a wrong password or an id that is not an
/// account.
async fn check_password(db: &DbPool, id: &str, password: &str, client: &Client) -> Result<bool> {
    let row = sqlx::query(
        "SELECT p.password_hash, EXISTS (
             SELECT 1 FROM login_locks l
             WHERE l.player_id = p.id AND l.addr = $2 AND l.locked_until > now()
         ) AS locked
         FROM players p WHERE p.id = $1 AND p.username IS NOT NULL",
    )
    .bind(id)
    .bind(&client.addr)
    .fetch_optional(db)
    .await?;
    let Some(row) = row else {
        return Ok(false);
    };
    if row.get::<bool, _>("locked") {
        return Err(locked());
    }
    let hash: String = row.get("password_hash");
    let stale = common::crypto::password_needs_rehash(&hash);
    if !verify_password(password.to_string(), hash).await? {
        sqlx::query(
            "INSERT INTO login_locks (player_id, addr, failures) VALUES ($1, $2, 1)
             ON CONFLICT (player_id, addr) DO UPDATE SET
                 failures = CASE WHEN login_locks.failures + 1 >= $3
                     THEN 0 ELSE login_locks.failures + 1 END,
                 locked_until = CASE WHEN login_locks.failures + 1 >= $3
                     THEN now() + make_interval(secs => $4) ELSE login_locks.locked_until END,
                 updated = now()",
        )
        .bind(id)
        .bind(&client.addr)
        .bind(MAX_FAILED_LOGINS)
        .bind(LOCK_SECS as f64)
        .execute(db)
        .await?;
        return Ok(false);
    }
    sqlx::query("DELETE FROM login_locks WHERE player_id = $1 AND addr = $2")
        .bind(id)
        .bind(&client.addr)
        .execute(db)
        .await?;
    sqlx::query("UPDATE players SET last_seen = now() WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    if stale {
        // the password is known only now, so this is when it moves to
        // today's rounds (ACCT-23)
        let hash = hash_password(password.to_string()).await?;
        sqlx::query("UPDATE players SET password_hash = $2 WHERE id = $1")
            .bind(id)
            .bind(&hash)
            .execute(db)
            .await?;
    }
    Ok(true)
}

/// Check a username and password and start a session (ACCT-5). Wrong
/// passwords lock the account for a while from this address (ACCT-9).
pub async fn login(
    db: &DbPool,
    username: &str,
    password: &str,
    client: &Client,
) -> Result<(Player, String)> {
    let username = normalize_username(username);
    let row = sqlx::query("SELECT id, name FROM players WHERE username = $1")
        .bind(&username)
        .fetch_optional(db)
        .await?;
    let Some(row) = row else {
        // the dummy hash is first made inside the slot too, never on a worker
        let pw = password.to_string();
        hashing(move || common::crypto::verify_password(pw.as_bytes(), &DUMMY_HASH)).await?;
        return Err(unauthorized());
    };
    let id: String = row.get("id");
    if !check_password(db, &id, password, client).await? {
        return Err(unauthorized());
    }
    let token = new_session(db, &id, client).await?;
    let account = Player {
        id,
        name: row.get("name"),
        username: Some(username),
    };
    Ok((account, token))
}

fn sign_in_first() -> ApiError {
    ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", "sign in first")
}

fn wrong_password() -> ApiError {
    ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", "wrong password")
}

/// Change an account's password, given its current one (ACCT-15). Every
/// session of the account ends, and the one returned replaces this
/// browser's.
pub async fn change_password(
    db: &DbPool,
    player: &Player,
    current: &str,
    new: &str,
    client: &Client,
) -> Result<String> {
    if player.username.is_none() {
        return Err(sign_in_first());
    }
    validate_password(new)?;
    if !check_password(db, &player.id, current, client).await? {
        return Err(wrong_password());
    }
    let hash = hash_password(new.to_string()).await?;
    let mut tx = db.begin().await?;
    sqlx::query("UPDATE players SET password_hash = $2 WHERE id = $1")
        .bind(&player.id)
        .bind(&hash)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM sessions WHERE player_id = $1")
        .bind(&player.id)
        .execute(&mut *tx)
        .await?;
    let token = new_session(&mut *tx, &player.id, client).await?;
    tx.commit().await?;
    Ok(token)
}

/// Delete an account for good, given its password (ACCT-16): it leaves every
/// room, and its sessions, memberships, and stats go with it. Its partners
/// keep their games with it as games with someone no longer here (STAT-10).
pub async fn delete_account(
    db: &DbPool,
    player: &Player,
    password: &str,
    client: &Client,
) -> Result<()> {
    if player.username.is_none() {
        return Err(sign_in_first());
    }
    if !check_password(db, &player.id, password, client).await? {
        return Err(wrong_password());
    }
    let mut tx = db.begin().await?;
    models::leave_everywhere(&mut tx, &player.id).await?;
    sqlx::query(&format!(
        "WITH gone AS (SELECT $1::text AS id) {}",
        models::FOLD_PARTNERS_GONE
    ))
    .bind(&player.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM players WHERE id = $1")
        .bind(&player.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

fn wrong_recovery() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "wrong username or recovery code",
    )
}

/// Set a new password with the account's recovery code (ACCT-18). The code
/// is used up and replaced, every session ends, every lock lifts, and this
/// browser signs in.
pub async fn recover(
    db: &DbPool,
    username: &str,
    code: &str,
    password: &str,
    client: &Client,
) -> Result<Signed> {
    validate_password(password)?;
    let username = normalize_username(username);
    let row = sqlx::query("SELECT id, name, recovery_hash FROM players WHERE username = $1")
        .bind(&username)
        .fetch_optional(db)
        .await?;
    let Some(row) = row else {
        return Err(wrong_recovery());
    };
    let id: String = row.get("id");
    let stored: Option<String> = row.get("recovery_hash");
    let given = recovery_hash(code);
    // compared as hashes, so the time taken says nothing about the code
    if stored.as_deref() != Some(given.as_str()) {
        return Err(wrong_recovery());
    }
    let hash = hash_password(password.to_string()).await?;
    let recovery_code = new_recovery_code()?;
    let mut tx = db.begin().await?;
    // the code must still be the one checked, so it is used at most once
    let updated = sqlx::query(
        "UPDATE players SET password_hash = $2, recovery_hash = $3, last_seen = now()
         WHERE id = $1 AND recovery_hash = $4",
    )
    .bind(&id)
    .bind(&hash)
    .bind(recovery_hash(&recovery_code))
    .bind(&given)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(wrong_recovery());
    }
    sqlx::query("DELETE FROM login_locks WHERE player_id = $1")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM sessions WHERE player_id = $1")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    let token = new_session(&mut *tx, &id, client).await?;
    tx.commit().await?;
    Ok(Signed {
        player: Player {
            id,
            name: row.get("name"),
            username: Some(username),
        },
        token,
        recovery_code,
    })
}

/// Replace the account's recovery code, given its password (ACCT-18). The
/// old code stops working.
pub async fn new_recovery(
    db: &DbPool,
    player: &Player,
    password: &str,
    client: &Client,
) -> Result<String> {
    if player.username.is_none() {
        return Err(sign_in_first());
    }
    if !check_password(db, &player.id, password, client).await? {
        return Err(wrong_password());
    }
    let code = new_recovery_code()?;
    sqlx::query("UPDATE players SET recovery_hash = $2 WHERE id = $1")
        .bind(&player.id)
        .bind(recovery_hash(&code))
        .execute(db)
        .await?;
    Ok(code)
}

/// End every session of the account but `token`'s (ACCT-19). Returns how
/// many ended.
pub async fn end_other_sessions(db: &DbPool, player: &Player, token: &str) -> Result<u64> {
    if player.username.is_none() {
        return Err(sign_in_first());
    }
    let ended = sqlx::query("DELETE FROM sessions WHERE player_id = $1 AND token_hash <> $2")
        .bind(&player.id)
        .bind(token_hash(token))
        .execute(db)
        .await?
        .rows_affected();
    Ok(ended)
}

/// End one other session of the account, by its id on the account page
/// (ACCT-19). This browser's own session is ended by signing out instead.
pub async fn end_session(db: &DbPool, player: &Player, id: i64, token: &str) -> Result<()> {
    if player.username.is_none() {
        return Err(sign_in_first());
    }
    let ended =
        sqlx::query("DELETE FROM sessions WHERE player_id = $1 AND id = $2 AND token_hash <> $3")
            .bind(&player.id)
            .bind(id)
            .bind(token_hash(token))
            .execute(db)
            .await?
            .rows_affected();
    if ended == 0 {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such device",
        ));
    }
    Ok(())
}

/// Change the account's username, given its password (ACCT-20). A default
/// name that was the old username follows it.
pub async fn change_username(
    db: &DbPool,
    player: &Player,
    username: &str,
    password: &str,
    client: &Client,
) -> Result<Player> {
    let Some(old) = &player.username else {
        return Err(sign_in_first());
    };
    let username = validate_username(username)?;
    if !check_password(db, &player.id, password, client).await? {
        return Err(wrong_password());
    }
    if &username == old {
        return Ok(player.clone());
    }
    let mut tx = db.begin().await?;
    let name: String = sqlx::query_scalar(
        "UPDATE players SET username = $2,
             name = CASE WHEN name = username THEN $2 ELSE name END
         WHERE id = $1 RETURNING name",
    )
    .bind(&player.id)
    .bind(&username)
    .fetch_one(&mut *tx)
    .await
    .map_err(taken_or)?;
    models::notify_rooms_of(&mut tx, &player.id).await?;
    tx.commit().await?;
    Ok(Player {
        id: player.id.clone(),
        name,
        username: Some(username),
    })
}

/// What the account page shows besides stats (ACCT-17): live sessions and
/// their devices, `token`'s marked as this one, whether there is a recovery
/// code, and the rooms the player is in with their games and wins there,
/// most recently active first.
pub async fn account_info(db: &DbPool, player: &Player, token: Option<&str>) -> Result<Value> {
    let row = sqlx::query(
        "SELECT
             (SELECT coalesce(jsonb_agg(jsonb_build_object(
                  'id', s.id, 'agent', s.agent, 'created', s.created,
                  'last_used', s.last_used, 'current', s.token_hash = $3
              ) ORDER BY s.token_hash = $3 DESC, s.last_used DESC), '[]')
              FROM sessions s WHERE s.player_id = $1 AND s.expires > now()) AS devices,
             coalesce((SELECT recovery_hash IS NOT NULL FROM players WHERE id = $1), false)
                 AS recovery,
             (SELECT coalesce(jsonb_agg(jsonb_build_object(
                  'code', r.code,
                  'host', r.host_player_id = $1,
                  'members', (SELECT count(*) FROM room_members o
                              WHERE o.room_id = r.id AND o.left_at IS NULL AND NOT o.removed),
                  'playing', EXISTS (SELECT 1 FROM games g
                                     WHERE g.room_id = r.id AND g.status <> 'scored'),
                  'games', coalesce(st.games_played, 0),
                  'wins', coalesce(st.wins, 0),
                  'last_active', r.last_active
              ) ORDER BY r.last_active DESC), '[]')
              FROM (SELECT r.* FROM rooms r
                        JOIN room_members m ON m.room_id = r.id AND m.player_id = $1
                    WHERE m.left_at IS NULL AND NOT m.removed
                    ORDER BY r.last_active DESC LIMIT $2) r
                  LEFT JOIN room_stats st ON st.room_id = r.id AND st.player_id = $1) AS rooms",
    )
    .bind(&player.id)
    .bind(ROOMS_SHOWN)
    .bind(token.map(token_hash).unwrap_or_default())
    .fetch_one(db)
    .await?;
    let devices: Value = row.get("devices");
    Ok(json!({
        "id": player.id,
        "name": player.name,
        "username": player.username,
        "sessions": devices.as_array().map_or(0, Vec::len),
        "devices": devices,
        "recovery": row.get::<bool, _>("recovery"),
        "rooms": row.get::<Value, _>("rooms"),
    }))
}

/// The account behind a live session token, and whether the session was
/// just extended so its cookie should be sent again (ACCT-6). Its last use
/// is kept to the hour (ACCT-19).
pub async fn session_player(db: &DbPool, token: &str) -> Result<Option<(Player, bool)>> {
    let row = sqlx::query(
        "WITH s AS (
             SELECT player_id, expires < now() + make_interval(days => $2) AS stale,
                 last_used < now() - make_interval(secs => $4) AS idle
             FROM sessions WHERE token_hash = $1 AND expires > now()
         ), u AS (
             UPDATE sessions SET last_used = now(),
                 expires = CASE WHEN (SELECT stale FROM s)
                     THEN now() + make_interval(days => $3) ELSE expires END
             WHERE token_hash = $1 AND ((SELECT stale FROM s) OR (SELECT idle FROM s))
         )
         SELECT p.id, p.name, p.username, s.stale
         FROM s JOIN players p ON p.id = s.player_id",
    )
    .bind(token_hash(token))
    .bind(SESSION_REFRESH_DAYS as i32)
    .bind(SESSION_DAYS as i32)
    .bind(LAST_USED_SECS)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|row| {
        (
            Player {
                id: row.get("id"),
                name: row.get("name"),
                username: row.get("username"),
            },
            row.get("stale"),
        )
    }))
}

/// Forget wrong password counts and locks gone quiet (ACCT-9), with the
/// hourly cleanup.
pub async fn forget_login_locks(db: &DbPool) -> Result<u64> {
    let forgotten = sqlx::query(
        "DELETE FROM login_locks
         WHERE updated < now() - make_interval(hours => $1)
             AND (locked_until IS NULL OR locked_until <= now())",
    )
    .bind(LOCK_KEPT_HOURS)
    .execute(db)
    .await?
    .rows_affected();
    Ok(forgotten)
}

/// End one session (ACCT-8).
pub async fn logout(db: &DbPool, token: &str) -> Result<()> {
    sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
        .bind(token_hash(token))
        .execute(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames_are_lower_case_and_plain() {
        assert_eq!(
            validate_username("  Ada_Lovelace ").unwrap(),
            "ada_lovelace"
        );
        assert_eq!(validate_username("a.b-c").unwrap(), "a.b-c");
        for bad in [
            "ab",
            &"a".repeat(25),
            "ada lovelace",
            "ädä",
            "ada!",
            "",
            "   ",
        ] {
            assert_eq!(
                validate_username(bad).unwrap_err().code,
                "invalid",
                "{bad:?}"
            );
        }
        assert!(validate_username(&"a".repeat(24)).is_ok());
    }

    #[test]
    fn passwords_have_a_length_range() {
        assert!(validate_password("1234567").is_err());
        assert!(validate_password("12345678").is_ok());
        assert!(validate_password(&"x".repeat(256)).is_ok());
        assert!(validate_password(&"x".repeat(257)).is_err());
    }

    #[test]
    fn recovery_codes_are_grouped_and_forgiving() {
        let code = new_recovery_code().unwrap();
        assert_eq!(code.len(), RECOVERY_CHARS + 4);
        assert_eq!(code.split('-').count(), 5);
        assert!(code
            .chars()
            .all(|c| c == '-' || RECOVERY_ALPHABET.contains(&(c as u8))));
        assert_ne!(code, new_recovery_code().unwrap());
        // typed back with other spacing and case, it is the same code
        let typed = code.to_uppercase().replace('-', " ");
        assert_eq!(recovery_hash(&typed), recovery_hash(&code));
        assert_eq!(normalize_recovery_code("Il-O0 x"), "1100x");
        assert_ne!(recovery_hash(&code), recovery_hash("00000-00000"));
    }

    #[test]
    fn the_limiter_allows_a_burst_then_refills() {
        let limiter = Limiter::default();
        let start = Instant::now();
        for _ in 0..AUTH_BURST as usize {
            assert!(limiter.take("a", start));
        }
        assert!(!limiter.take("a", start));
        // another address has its own bucket
        assert!(limiter.take("b", start));
        let refill = Duration::from_secs_f64(AUTH_REFILL_SECS);
        assert!(!limiter.take("a", start + refill / 2));
        assert!(limiter.take("a", start + refill));
        assert!(!limiter.take("a", start + refill));
        // a long wait refills only up to the burst
        let later = start + refill * 100;
        for _ in 0..AUTH_BURST as usize {
            assert!(limiter.take("a", later));
        }
        assert!(!limiter.take("a", later));
    }

    #[test]
    fn an_ipv6_slash_64_shares_one_bucket() {
        assert_eq!(limit_key("1.2.3.4"), "1.2.3.4");
        assert_eq!(limit_key(" 1.2.3.4 "), "1.2.3.4");
        assert_eq!(limit_key("::ffff:1.2.3.4"), "1.2.3.4");
        assert_eq!(
            limit_key("2001:db8:1:2:aaaa::1"),
            limit_key("2001:db8:1:2:bbbb:cccc:dddd:eeee")
        );
        assert_eq!(limit_key("2001:db8:1:2::1"), "2001:db8:1:2::/64");
        assert_ne!(limit_key("2001:db8:1:2::1"), limit_key("2001:db8:1:3::1"));
        assert_eq!(limit_key("not an address"), "not an address");
        let limiter = Limiter::default();
        for i in 0..AUTH_BURST as usize {
            assert!(limiter.check(Some(&format!("2001:db8::{i:x}"))).is_ok());
        }
        assert_eq!(
            limiter.check(Some("2001:db8::ffff")).unwrap_err().code,
            "slow_down"
        );
    }

    #[test]
    fn the_limiter_forgets_full_buckets_when_crowded() {
        let limiter = Limiter::default();
        let start = Instant::now();
        assert!(limiter.take("busy", start));
        for i in 0..LIMITER_KEYS {
            assert!(limiter.take(&format!("k{i}"), start));
        }
        let later = start + Duration::from_secs_f64(AUTH_REFILL_SECS * 2.0);
        assert!(limiter.take("new", later));
        // every bucket refilled by then, so all were forgotten
        assert_eq!(limiter.buckets.lock().unwrap().len(), 1);
        assert!(limiter.check(None).is_ok());
    }

    #[test]
    fn tokens_are_stored_hashed() {
        let h = token_hash("abc");
        assert_eq!(h.len(), 64);
        assert_ne!(h, "abc");
        assert_eq!(h, token_hash("abc"));
    }
}
