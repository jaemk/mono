use crate::bot::Level;
use crate::game::{Action, Secret, Settings};
use crate::lag::{self, Rtt, Seen};
use crate::models::{self, Acted, ApiError, ClientAction, Player, Result, Room, Snapshot};
use crate::{accounts, sealed, stats, State};
use axum::body::Bytes;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State as AxumState,
    },
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;

pub const PLAYER_COOKIE: &str = "komino_player";
pub const SESSION_COOKIE: &str = "komino_session";
const COOKIE_DAYS: i64 = 365;
const HEARTBEAT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// `<id>.<hmac>`, so a hand edited cookie can't claim another player.
pub fn sign_player(id: &str, key: &str) -> String {
    format!("{id}.{}", common::crypto::hmac_sign(id, key.as_bytes()))
}

pub fn verify_player(value: &str, key: &str) -> Option<String> {
    let (id, sig) = value.split_once('.')?;
    common::crypto::hmac_verify(id, sig, key.as_bytes()).then(|| id.to_string())
}

fn player_cookie(id: &str, key: &str) -> Cookie<'static> {
    Cookie::build((PLAYER_COOKIE, sign_player(id, key)))
        .secure(true)
        .http_only(true)
        .same_site(SameSite::Lax)
        .max_age(time::Duration::days(COOKIE_DAYS))
        .path("/komino")
        .build()
}

/// The session token cookie (ACCT-6). Same attributes as the player cookie,
/// but it lives only as long as the session.
fn session_cookie(token: &str) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, token.to_string()))
        .secure(true)
        .http_only(true)
        .same_site(SameSite::Lax)
        .max_age(time::Duration::days(accounts::SESSION_DAYS))
        .path("/komino")
        .build()
}

/// A cookie to pass to [`CookieJar::remove`], matching the path it was set
/// on.
fn expired(name: &'static str) -> Cookie<'static> {
    Cookie::build(name).path("/komino").build()
}

fn cookie_id(state: &State, jar: &CookieJar) -> Option<String> {
    jar.get(PLAYER_COOKIE)
        .and_then(|c| verify_player(c.value(), &state.config.signing_key))
}

/// The account behind the session cookie, if it is live. A dead one is
/// dropped from the jar.
async fn session(state: &State, jar: CookieJar) -> Result<(Option<Player>, CookieJar)> {
    let Some(token) = jar.get(SESSION_COOKIE).map(|c| c.value().to_string()) else {
        return Ok((None, jar));
    };
    Ok(match accounts::session_player(&state.db, &token).await? {
        Some((player, true)) => (Some(player), jar.add(session_cookie(&token))),
        Some((player, false)) => (Some(player), jar),
        None => (None, jar.remove(expired(SESSION_COOKIE))),
    })
}

/// The requesting player: a live session's account, else the anonymous
/// player cookie's player, issuing a fresh anonymous identity when that
/// cookie is missing, forged, or unknown, or names an account (ACCT-7).
async fn identify(state: &State, jar: CookieJar) -> Result<(Player, CookieJar)> {
    let (account, jar) = session(state, jar).await?;
    if let Some(player) = account {
        return Ok((player, jar));
    }
    let id = cookie_id(state, &jar);
    let (player, fresh) = models::ensure_player(&state.db, id.as_deref()).await?;
    let jar = if fresh {
        jar.add(player_cookie(&player.id, &state.config.signing_key))
    } else {
        jar
    };
    Ok((player, jar))
}

/// The requesting player for a socket, which can't set cookies: the same
/// rules as [`identify`], but never a new player.
async fn socket_player(state: &State, jar: CookieJar) -> Result<Option<String>> {
    if let (Some(player), _) = session(state, jar.clone()).await? {
        return Ok(Some(player.id));
    }
    match cookie_id(state, &jar) {
        Some(id) if models::is_anonymous(&state.db, &id).await? => Ok(Some(id)),
        _ => Ok(None),
    }
}

fn me_json(player: &Player) -> Value {
    json!({ "id": player.id, "name": player.name, "username": player.username })
}

/// The client's address as the fly proxy saw it, which it sets on every
/// request it forwards.
pub const CLIENT_IP_HEADER: &str = "fly-client-ip";

fn client_addr(headers: &HeaderMap) -> Option<&str> {
    headers.get(CLIENT_IP_HEADER).and_then(|v| v.to_str().ok())
}

/// Count a password check against the client's address (ACCT-22), and say
/// who the client is for the per-address lock (ACCT-9) and its session's
/// device (ACCT-19).
fn limit(state: &State, headers: &HeaderMap) -> Result<accounts::Client> {
    state.limiter.check(client_addr(headers))?;
    let agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok());
    Ok(accounts::Client::new(client_addr(headers), agent))
}

fn session_token(jar: &CookieJar) -> Option<String> {
    jar.get(SESSION_COOKIE).map(|c| c.value().to_string())
}

/// The account and its new recovery code, shown once (ACCT-18).
fn signed_json(signed: &accounts::Signed) -> Value {
    let mut body = me_json(&signed.player);
    body["recovery_code"] = json!(signed.recovery_code);
    body
}

async fn room_view(state: &State, room: &Room, player: &str) -> Result<Value> {
    models::view(&state.db, room, Some(player), &state.config.real_hostname).await
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

/// The spa shell for `/komino`, `/komino/account`, and `/komino/r/<code>`. Issues the cookie on
/// first visit so the websocket that follows already carries it.
pub async fn index(AxumState(state): AxumState<State>, jar: CookieJar) -> Response {
    let jar = match identify(&state, jar).await {
        Ok((_, jar)) => jar,
        Err(e) => return e.into_response(),
    };
    match tokio::fs::read_to_string("crates/komino/assets/index.html").await {
        Ok(html) => (jar, [(header::CONTENT_TYPE, "text/html")], html).into_response(),
        Err(e) => {
            tracing::error!("komino index read error: {e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, "content error").into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Api
// ---------------------------------------------------------------------------

pub async fn me(AxumState(state): AxumState<State>, jar: CookieJar) -> Result<Response> {
    let (player, jar) = identify(&state, jar).await?;
    Ok((jar, Json(me_json(&player))).into_response())
}

#[derive(Deserialize)]
pub struct RenameRequest {
    pub name: String,
}

/// Set the default display name. An account's blank name goes back to its
/// username (ACCT-11).
pub async fn rename(
    AxumState(state): AxumState<State>,
    jar: CookieJar,
    Json(req): Json<RenameRequest>,
) -> Result<Response> {
    let (mut player, jar) = identify(&state, jar).await?;
    let raw = match (&player.username, req.name.trim()) {
        (Some(username), "") => username.clone(),
        _ => req.name,
    };
    player.name = models::set_name(&state.db, &player.id, &raw).await?;
    Ok((jar, Json(me_json(&player))).into_response())
}

#[derive(Deserialize)]
pub struct SignupRequest {
    pub username: String,
    pub password: String,
    pub name: Option<String>,
}

/// Make the current anonymous player an account and sign it in (ACCT-2).
/// The player cookie is dropped: it can't reach an account (ACCT-7).
pub async fn signup(
    AxumState(state): AxumState<State>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(req): Json<SignupRequest>,
) -> Result<Response> {
    let client = limit(&state, &headers)?;
    let (player, jar) = identify(&state, jar).await?;
    let signed = accounts::signup(
        &state.db,
        &player,
        &req.username,
        &req.password,
        req.name.as_deref(),
        &client,
    )
    .await?;
    let jar = jar
        .remove(expired(PLAYER_COOKIE))
        .add(session_cookie(&signed.token));
    Ok((jar, no_store(), Json(signed_json(&signed))).into_response())
}

/// A response that holds a secret, so no cache keeps it.
fn no_store() -> [(header::HeaderName, &'static str); 1] {
    [(header::CACHE_CONTROL, "no-store")]
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

/// Sign in to an account (ACCT-5). The anonymous player cookie is kept, so
/// signing out returns this browser to that player.
pub async fn login(
    AxumState(state): AxumState<State>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(req): Json<LoginRequest>,
) -> Result<Response> {
    let client = limit(&state, &headers)?;
    let (account, token) =
        accounts::login(&state.db, &req.username, &req.password, &client).await?;
    // a session this browser held before is replaced, not left behind
    if let Some(old) = jar.get(SESSION_COOKIE) {
        accounts::logout(&state.db, old.value()).await?;
    }
    let jar = jar.add(session_cookie(&token));
    Ok((jar, Json(me_json(&account))).into_response())
}

/// End this browser's session (ACCT-8).
pub async fn logout(AxumState(state): AxumState<State>, jar: CookieJar) -> Result<Response> {
    if let Some(token) = jar.get(SESSION_COOKIE) {
        accounts::logout(&state.db, token.value()).await?;
    }
    let jar = jar.remove(expired(SESSION_COOKIE));
    Ok((jar, Json(json!({ "ok": true }))).into_response())
}

#[derive(Deserialize)]
pub struct PasswordRequest {
    pub current: String,
    pub new: String,
}

/// Change the signed in account's password (ACCT-15). Its other sessions
/// end, and this browser gets a new one.
pub async fn change_password(
    AxumState(state): AxumState<State>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(req): Json<PasswordRequest>,
) -> Result<Response> {
    let client = limit(&state, &headers)?;
    let (player, jar) = identify(&state, jar).await?;
    let token =
        accounts::change_password(&state.db, &player, &req.current, &req.new, &client).await?;
    let jar = jar.add(session_cookie(&token));
    Ok((jar, Json(me_json(&player))).into_response())
}

#[derive(Deserialize)]
pub struct UsernameRequest {
    pub username: String,
    pub password: String,
}

/// Change the signed in account's username (ACCT-20).
pub async fn change_username(
    AxumState(state): AxumState<State>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(req): Json<UsernameRequest>,
) -> Result<Response> {
    let client = limit(&state, &headers)?;
    let (player, jar) = identify(&state, jar).await?;
    let account =
        accounts::change_username(&state.db, &player, &req.username, &req.password, &client)
            .await?;
    Ok((jar, Json(me_json(&account))).into_response())
}

#[derive(Deserialize)]
pub struct RecoverRequest {
    pub username: String,
    pub code: String,
    pub password: String,
}

/// Set a new password with a recovery code and sign in (ACCT-18). A
/// session this browser held before is replaced, as at sign in.
pub async fn recover(
    AxumState(state): AxumState<State>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(req): Json<RecoverRequest>,
) -> Result<Response> {
    let client = limit(&state, &headers)?;
    let signed =
        accounts::recover(&state.db, &req.username, &req.code, &req.password, &client).await?;
    if let Some(old) = jar.get(SESSION_COOKIE) {
        accounts::logout(&state.db, old.value()).await?;
    }
    let jar = jar.add(session_cookie(&signed.token));
    Ok((jar, no_store(), Json(signed_json(&signed))).into_response())
}

#[derive(Deserialize)]
pub struct PasswordOnly {
    pub password: String,
}

/// Replace the signed in account's recovery code (ACCT-18).
pub async fn new_recovery(
    AxumState(state): AxumState<State>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(req): Json<PasswordOnly>,
) -> Result<Response> {
    let client = limit(&state, &headers)?;
    let (player, jar) = identify(&state, jar).await?;
    let code = accounts::new_recovery(&state.db, &player, &req.password, &client).await?;
    Ok((jar, no_store(), Json(json!({ "recovery_code": code }))).into_response())
}

/// Sign the account out everywhere but this browser (ACCT-19).
pub async fn sign_out_others(
    AxumState(state): AxumState<State>,
    jar: CookieJar,
) -> Result<Response> {
    let (player, jar) = identify(&state, jar).await?;
    let token = session_token(&jar).unwrap_or_default();
    let ended = accounts::end_other_sessions(&state.db, &player, &token).await?;
    Ok((jar, Json(json!({ "ended": ended }))).into_response())
}

#[derive(Deserialize)]
pub struct DeviceRequest {
    pub id: i64,
}

/// Sign the account out on one other device (ACCT-19).
pub async fn sign_out_device(
    AxumState(state): AxumState<State>,
    jar: CookieJar,
    Json(req): Json<DeviceRequest>,
) -> Result<Response> {
    let (player, jar) = identify(&state, jar).await?;
    let token = session_token(&jar).unwrap_or_default();
    accounts::end_session(&state.db, &player, req.id, &token).await?;
    Ok((jar, Json(json!({ "ok": true }))).into_response())
}

/// The account page's details (ACCT-17), for any player.
pub async fn account(AxumState(state): AxumState<State>, jar: CookieJar) -> Result<Response> {
    let (player, jar) = identify(&state, jar).await?;
    let token = session_token(&jar);
    let body = accounts::account_info(&state.db, &player, token.as_deref()).await?;
    Ok((jar, Json(body)).into_response())
}

/// The requesting player's lifetime stats (STAT-11).
pub async fn account_stats(AxumState(state): AxumState<State>, jar: CookieJar) -> Result<Response> {
    let (player, jar) = identify(&state, jar).await?;
    let body = stats::player_stats(&state.db, &player.id).await?;
    Ok((jar, Json(body)).into_response())
}

/// Delete the signed in account (ACCT-16).
pub async fn delete_account(
    AxumState(state): AxumState<State>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(req): Json<PasswordOnly>,
) -> Result<Response> {
    let client = limit(&state, &headers)?;
    let (player, jar) = identify(&state, jar).await?;
    accounts::delete_account(&state.db, &player, &req.password, &client).await?;
    let jar = jar.remove(expired(SESSION_COOKIE));
    Ok((jar, Json(json!({ "ok": true }))).into_response())
}

/// Set or clear this member's name for one room (ACCT-12).
pub async fn room_name(
    AxumState(state): AxumState<State>,
    Path(code): Path<String>,
    jar: CookieJar,
    Json(req): Json<RenameRequest>,
) -> Result<Response> {
    let (player, jar) = identify(&state, jar).await?;
    let room = models::room_by_code(&state.db, &code).await?;
    models::set_room_name(&state.db, &room, &player.id, &req.name).await?;
    let view = room_view(&state, &room, &player.id).await?;
    Ok((jar, Json(view)).into_response())
}

/// The settings for a new room (SET-2): an empty body takes every default.
fn parse_settings(body: &[u8]) -> Result<Settings> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Settings::default());
    }
    serde_json::from_slice(body).map_err(|e| ApiError::invalid(format!("bad settings: {e}")))
}

pub async fn create_room(
    AxumState(state): AxumState<State>,
    jar: CookieJar,
    body: Bytes,
) -> Result<Response> {
    let settings = parse_settings(&body)?;
    let (player, jar) = identify(&state, jar).await?;
    let room = models::create_room(&state.db, &player.id, settings).await?;
    let view = room_view(&state, &room, &player.id).await?;
    Ok((jar, Json(view)).into_response())
}

pub async fn join_room(
    AxumState(state): AxumState<State>,
    Path(code): Path<String>,
    jar: CookieJar,
) -> Result<Response> {
    let (player, jar) = identify(&state, jar).await?;
    let room = models::room_by_code(&state.db, &code).await?;
    models::join(&state.db, &room, &player.id).await?;
    let view = room_view(&state, &room, &player.id).await?;
    Ok((jar, Json(view)).into_response())
}

pub async fn get_room(
    AxumState(state): AxumState<State>,
    Path(code): Path<String>,
    jar: CookieJar,
) -> Result<Response> {
    let (player, jar) = identify(&state, jar).await?;
    let room = models::room_by_code(&state.db, &code).await?;
    // membership is checked against the same load the view comes from
    let snap = Snapshot::load(&state.db, room.id).await?;
    snap.require_member(&player.id)?;
    let view = snap.render(Some(&player.id), &state.config.real_hostname);
    Ok((jar, Json(view)).into_response())
}

pub async fn leave_room(
    AxumState(state): AxumState<State>,
    Path(code): Path<String>,
    jar: CookieJar,
) -> Result<Response> {
    let (player, jar) = identify(&state, jar).await?;
    let room = models::room_by_code(&state.db, &code).await?;
    models::require_member(&state.db, room.id, &player.id).await?;
    models::leave(&state.db, &room, &player.id).await?;
    Ok((jar, Json(json!({ "ok": true }))).into_response())
}

#[derive(Deserialize)]
pub struct TargetRequest {
    pub player: String,
}

pub async fn remove_member(
    AxumState(state): AxumState<State>,
    Path(code): Path<String>,
    jar: CookieJar,
    Json(req): Json<TargetRequest>,
) -> Result<Response> {
    let (player, jar) = identify(&state, jar).await?;
    let room = models::room_by_code(&state.db, &code).await?;
    models::remove(&state.db, &room, &player.id, &req.player).await?;
    let view = room_view(&state, &room, &player.id).await?;
    Ok((jar, Json(view)).into_response())
}

/// A new bot's level (BOT-5): `{"level": "easy" | "normal" | "hard"}`, or an
/// empty body for normal.
fn parse_level(body: &[u8]) -> Result<Level> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Level::default());
    }
    let v: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| ApiError::invalid(format!("bad body: {e}")))?;
    match v.get("level") {
        None | Some(serde_json::Value::Null) => Ok(Level::default()),
        Some(l) => l
            .as_str()
            .and_then(Level::parse)
            .ok_or_else(|| ApiError::invalid("level must be easy, normal, or hard")),
    }
}

pub async fn add_bot(
    AxumState(state): AxumState<State>,
    Path(code): Path<String>,
    jar: CookieJar,
    body: Bytes,
) -> Result<Response> {
    let level = parse_level(&body)?;
    let (player, jar) = identify(&state, jar).await?;
    let room = models::room_by_code(&state.db, &code).await?;
    models::add_bot(&state.db, &room, &player.id, level).await?;
    let view = room_view(&state, &room, &player.id).await?;
    Ok((jar, Json(view)).into_response())
}

pub async fn unban_member(
    AxumState(state): AxumState<State>,
    Path(code): Path<String>,
    jar: CookieJar,
    Json(req): Json<TargetRequest>,
) -> Result<Response> {
    let (player, jar) = identify(&state, jar).await?;
    let room = models::room_by_code(&state.db, &code).await?;
    models::unban(&state.db, &room, &player.id, &req.player).await?;
    let view = room_view(&state, &room, &player.id).await?;
    Ok((jar, Json(view)).into_response())
}

/// The public view of a room, for the watch page's first render. Watching
/// does not join the room.
pub async fn watch_room(
    AxumState(state): AxumState<State>,
    Path(code): Path<String>,
    jar: CookieJar,
) -> Result<Response> {
    let (player, jar) = identify(&state, jar).await?;
    let room = models::room_by_code(&state.db, &code).await?;
    let snap = Snapshot::load(&state.db, room.id).await?;
    snap.require_not_removed(&player.id)?;
    let view = snap.render(None, &state.config.real_hostname);
    Ok((jar, Json(view)).into_response())
}

/// The server's static ECDH public key (SEAL-3).
pub async fn server_key(AxumState(state): AxumState<State>) -> Json<Value> {
    Json(json!({ "kid": state.server_key.kid, "public_key": state.server_key.public_key }))
}

#[derive(Deserialize)]
pub struct RevealRequest {
    pub client_key: String,
    pub what: String,
    pub id: Option<String>,
}

/// A private card value, sealed to the client's key (SEAL-6, SEAL-7).
pub async fn reveal(
    AxumState(state): AxumState<State>,
    Path(code): Path<String>,
    jar: CookieJar,
    Json(req): Json<RevealRequest>,
) -> Result<Response> {
    let (player, jar) = identify(&state, jar).await?;
    let room = models::room_by_code(&state.db, &code).await?;
    let client = sealed::parse_public(&req.client_key)
        .ok_or_else(|| ApiError::invalid("client_key must be a P-256 public key"))?;
    let what = match (req.what.as_str(), req.id) {
        ("opening", _) => Secret::Opening,
        ("drawn", _) => Secret::Drawn,
        ("peek", Some(id)) => Secret::Peek(id),
        ("peek", None) => return Err(ApiError::invalid("a peek reveal needs its id")),
        _ => return Err(ApiError::invalid("what must be opening, drawn, or peek")),
    };
    let key_hash = hex::encode(common::crypto::sha256(&sealed::public_point(&client)));
    let secret = models::reveal(&state.db, &room, &player.id, &what, &key_hash).await?;
    let body = state
        .server_key
        .seal(&client, room.code.as_bytes(), secret.to_string().as_bytes())
        .map_err(|e| {
            tracing::error!("komino seal error: {e}");
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "error", "could not seal")
        })?;
    Ok((jar, [(header::CACHE_CONTROL, "no-store")], Json(body)).into_response())
}

/// The same actions the websocket takes, over plain http.
pub async fn action(
    AxumState(state): AxumState<State>,
    Path(code): Path<String>,
    jar: CookieJar,
    Json(body): Json<Value>,
) -> Result<Response> {
    let (player, jar) = identify(&state, jar).await?;
    let room = models::room_by_code(&state.db, &code).await?;
    // over http nothing bounds a reported reaction, so a match is timed from
    // when its discard landed
    let acted = models::act(
        &state.db,
        &room,
        &player.id,
        ClientAction::parse(body)?,
        None,
    )
    .await?;
    if let Acted::Claimed { id, deadline } = acted {
        models::claim_result(&state.db, room.id, &id, deadline).await?;
    }
    let view = room_view(&state, &room, &player.id).await?;
    Ok((jar, Json(view)).into_response())
}

// ---------------------------------------------------------------------------
// Websocket
// ---------------------------------------------------------------------------

pub async fn ws(
    AxumState(state): AxumState<State>,
    Path(code): Path<String>,
    jar: CookieJar,
    upgrade: WebSocketUpgrade,
) -> Result<Response> {
    let player = socket_player(&state, jar)
        .await?
        .ok_or_else(|| ApiError::forbidden("load the room page first"))?;
    let room = models::room_by_code(&state.db, &code).await?;
    models::require_member(&state.db, room.id, &player).await?;
    Ok(upgrade.on_upgrade(move |socket| socket_loop(state, room, Who::Member(player), socket)))
}

/// The watch socket. The observer slot is claimed after the upgrade so a
/// full room can say so in a message the page can read.
pub async fn watch_ws(
    AxumState(state): AxumState<State>,
    Path(code): Path<String>,
    jar: CookieJar,
    upgrade: WebSocketUpgrade,
) -> Result<Response> {
    let player = socket_player(&state, jar)
        .await?
        .ok_or_else(|| ApiError::forbidden("load the watch page first"))?;
    let room = models::room_by_code(&state.db, &code).await?;
    models::require_not_removed(&state.db, room.id, &player).await?;
    Ok(upgrade.on_upgrade(move |socket| watch_loop(state, room, player, socket)))
}

async fn watch_loop(state: State, room: Room, player: String, mut socket: WebSocket) {
    match models::claim_observer(&state.db, room.id).await {
        Ok(lease) => {
            let who = Who::Observer {
                lease: lease.clone(),
                player,
            };
            socket_loop(state.clone(), room.clone(), who, socket).await;
            if let Err(e) = models::release_observer(&state.db, room.id, &lease).await {
                tracing::warn!("komino observer release error: {e:?}");
            }
        }
        Err(e) => {
            let msg = json!({ "type": e.code, "message": e.message });
            let _ = socket.send(Message::Text(msg.to_string().into())).await;
            let _ = socket.send(Message::Close(None)).await;
        }
    }
}

/// Who is on the other end of a room socket.
enum Who {
    Member(String),
    /// An observer's lease id, and the cookie player behind it, so a removal
    /// while watching still closes the socket.
    Observer {
        lease: String,
        player: String,
    },
}

/// The message for a failed access check: a refusal ends the socket as
/// `removed`, while a server error is reported and the socket stays open.
fn access_error(e: &ApiError) -> Value {
    if e.status.is_server_error() {
        json!({ "type": "error", "code": e.code, "message": e.message })
    } else {
        json!({ "type": "removed", "code": e.code })
    }
}

/// The socket's view rendered from `snap`, or why it can't have one.
fn view_msg(state: &State, who: &Who, snap: &Snapshot) -> Value {
    let base = &state.config.real_hostname;
    let (access, viewer) = match who {
        Who::Member(player) => (snap.require_member(player), Some(player.as_str())),
        Who::Observer { player, .. } => (snap.require_not_removed(player), None),
    };
    match access {
        Err(e) => access_error(&e),
        Ok(()) => json!({ "type": "view", "view": snap.render(viewer, base) }),
    }
}

/// Send the socket's current view, noting when it first carried each
/// discard (RT-18). After a ping the room's sockets on this machine share one
/// snapshot load (RT-21); otherwise this socket loads its own. False when the
/// socket should close.
async fn send_view(
    state: &State,
    room: &Room,
    who: &Who,
    socket: &mut WebSocket,
    seen: &mut Seen,
    shared: bool,
) -> bool {
    let snap = if shared {
        state.hub.snapshot(&state.db, room.id).await
    } else {
        Snapshot::load(&state.db, room.id).await.map(Arc::new)
    };
    let msg = match snap {
        Ok(snap) => view_msg(state, who, &snap),
        Err(e) => json!({ "type": "error", "code": e.code, "message": e.message }),
    };
    let closing = msg["type"] == "removed";
    let seq = msg["view"]["game"]["discard_seq"].as_u64();
    let ok = socket
        .send(Message::Text(msg.to_string().into()))
        .await
        .is_ok();
    if let Some(seq) = seq {
        seen.sent(seq, std::time::Instant::now());
    }
    ok && !closing
}

fn result_msg(reference: Option<Value>, result: Result<()>) -> Value {
    match result {
        Ok(()) => json!({ "type": "result", "ref": reference, "ok": true }),
        Err(e) => json!({
            "type": "result", "ref": reference, "ok": false, "code": e.code, "message": e.message,
        }),
    }
}

/// What a socket knows about its own latency when a message arrives.
struct Timing<'a> {
    received: std::time::Instant,
    rtt: Option<lag::RoundTrip>,
    seen: &'a Seen,
}

impl Timing<'_> {
    /// A match's reaction time, when this socket sent its discard (RT-18).
    fn reaction_ms(&self, action: &ClientAction) -> Option<i64> {
        let ClientAction::Game {
            action: Action::Match { seq, .. },
            reaction_ms,
            ..
        } = action
        else {
            return None;
        };
        let elapsed = self.seen.since(*seq, self.received)?;
        Some(lag::reaction_ms(*reaction_ms, elapsed, self.rtt))
    }
}

/// Apply a socket message. The reply comes back at once, or for a claimed
/// match through `later` once the claim settles.
async fn handle_text(
    state: &State,
    room: &Room,
    who: &Who,
    text: &str,
    timing: Timing<'_>,
    later: &mpsc::UnboundedSender<Value>,
) -> Option<Value> {
    let parsed: std::result::Result<Value, _> = serde_json::from_str(text);
    let (reference, result) = match parsed {
        Ok(mut body) => {
            let reference = body.get_mut("ref").map(Value::take);
            let result = match (who, ClientAction::parse(body)) {
                (Who::Observer { .. }, _) => Err(ApiError::forbidden("observers cannot act")),
                (Who::Member(player), Ok(action)) => {
                    let reaction = timing.reaction_ms(&action);
                    models::act(&state.db, room, player, action, reaction).await
                }
                (Who::Member(_), Err(e)) => Err(e),
            };
            (reference, result)
        }
        Err(_) => (None, Err(ApiError::invalid("messages must be json"))),
    };
    match result {
        Ok(Acted::Claimed { id, deadline }) => {
            // wait off the socket loop so views keep flowing meanwhile
            let (db, room_id, later) = (state.db.clone(), room.id, later.clone());
            tokio::spawn(async move {
                let result = models::claim_result(&db, room_id, &id, deadline).await;
                let _ = later.send(result_msg(reference, result));
            });
            None
        }
        Ok(Acted::Done) => Some(result_msg(reference, Ok(()))),
        Err(e) => Some(result_msg(reference, Err(e))),
    }
}

/// Refresh a member's presence or an observer's lease. False when an
/// observer's lease is gone (expired and reclaimed), so its slot may already
/// belong to someone else and the socket has to close.
async fn heartbeat(state: &State, room: &Room, who: &Who) -> Result<bool> {
    match who {
        Who::Member(player) => models::set_presence(&state.db, room.id, player, true)
            .await
            .map(|()| true),
        Who::Observer { lease, .. } => models::refresh_observer(&state.db, lease).await,
    }
}

async fn socket_loop(state: State, room: Room, who: Who, mut socket: WebSocket) {
    let mut pings = state.hub.subscribe(room.id);
    if let Who::Member(player) = &who {
        state.hub.connect(room.id, player);
    }
    if let Err(e) = heartbeat(&state, &room, &who).await {
        tracing::warn!("komino presence error: {e:?}");
    }
    let mut beat = tokio::time::interval(HEARTBEAT);
    let mut probe = tokio::time::interval(lag::PING_EVERY);
    let mut rtt = Rtt::default();
    let mut seen = Seen::default();
    let (later, mut settled) = mpsc::unbounded_channel::<Value>();
    if send_view(&state, &room, &who, &mut socket, &mut seen, false).await {
        loop {
            tokio::select! {
                msg = socket.recv() => match msg {
                    Some(Ok(Message::Text(text))) => {
                        let timing = Timing {
                            received: std::time::Instant::now(),
                            rtt: rtt.estimate(),
                            seen: &seen,
                        };
                        let reply =
                            handle_text(&state, &room, &who, text.as_str(), timing, &later).await;
                        if let Some(reply) = reply {
                            if socket.send(Message::Text(reply.to_string().into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Pong(payload))) => {
                        rtt.pong(&payload, std::time::Instant::now());
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {}
                },
                reply = settled.recv() => {
                    let Some(reply) = reply else { break };
                    if socket.send(Message::Text(reply.to_string().into())).await.is_err() {
                        break;
                    }
                }
                _ = probe.tick() => {
                    // browsers answer pings on their own, no page code involved
                    let payload = rtt.ping(std::time::Instant::now());
                    if socket.send(Message::Ping(payload.into())).await.is_err() {
                        break;
                    }
                }
                ping = pings.recv() => match ping {
                    Ok(()) | Err(RecvError::Lagged(_)) => {
                        if !send_view(&state, &room, &who, &mut socket, &mut seen, true).await {
                            break;
                        }
                    }
                    Err(RecvError::Closed) => break,
                },
                _ = beat.tick() => match heartbeat(&state, &room, &who).await {
                    Ok(true) => {}
                    // the page reconnects and claims a slot again, or hears the room is full
                    Ok(false) => break,
                    // a transient db error; the lease outlives a missed beat
                    Err(e) => tracing::warn!("komino heartbeat error: {e:?}"),
                },
            }
        }
    }
    // a clean close, whichever side ended it; ignored if the peer is gone
    let _ = socket.send(Message::Close(None)).await;
    // read until the peer's close, so a frame it sent meanwhile (a pong) is
    // consumed instead of resetting the connection
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        while let Some(Ok(msg)) = socket.recv().await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
        }
    })
    .await;
    if let Who::Member(player) = &who {
        if state.hub.disconnect(room.id, player) == 0 {
            let _ = models::set_presence(&state.db, room.id, player, false).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_cookie_round_trips_and_rejects_forgery() {
        let signed = sign_player("abc", "key");
        assert_eq!(verify_player(&signed, "key").as_deref(), Some("abc"));
        assert_eq!(verify_player(&signed, "other-key"), None);
        let forged = signed.replacen("abc", "abd", 1);
        assert_eq!(verify_player(&forged, "key"), None);
        assert_eq!(verify_player("abc", "key"), None);
    }

    #[test]
    fn only_refusals_close_a_socket_as_removed() {
        let removed = access_error(&ApiError::removed());
        assert_eq!(removed, json!({ "type": "removed", "code": "removed" }));
        let left = access_error(&ApiError::forbidden("join the room first"));
        assert_eq!(left["type"], "removed");
        // a database failure is retryable, not a removal
        let db = access_error(&ApiError::from(sqlx::Error::PoolTimedOut));
        assert_eq!(db["type"], "error");
        assert_eq!(db["code"], "error");
    }

    #[test]
    fn bot_levels_parse_from_an_optional_body() {
        assert_eq!(parse_level(b"").unwrap(), Level::Normal);
        assert_eq!(parse_level(b"{}").unwrap(), Level::Normal);
        assert_eq!(parse_level(br#"{"level": null}"#).unwrap(), Level::Normal);
        assert_eq!(parse_level(br#"{"level": "easy"}"#).unwrap(), Level::Easy);
        assert_eq!(parse_level(br#"{"level": "hard"}"#).unwrap(), Level::Hard);
        for bad in [&br#"{"level": "expert"}"#[..], br#"{"level": 3}"#, b"{"] {
            assert_eq!(parse_level(bad).unwrap_err().code, "invalid");
        }
    }

    #[test]
    fn settings_default_per_field_and_reject_bad_json() {
        assert_eq!(parse_settings(b"").unwrap(), Settings::default());
        assert_eq!(parse_settings(b" \n").unwrap(), Settings::default());
        let s = parse_settings(br#"{"hand_size": 6, "turn_limit_secs": 60}"#).unwrap();
        assert_eq!(
            s,
            Settings {
                hand_size: 6,
                turn_limit_secs: Some(60),
                ..Settings::default()
            }
        );
        let s = parse_settings(br#"{"reveal_secs": null}"#).unwrap();
        assert_eq!(s.reveal_secs, None);
        assert!(s.show_misses);
        let s = parse_settings(br#"{"show_misses": false}"#).unwrap();
        assert!(!s.show_misses);
        assert_eq!(
            parse_settings(br#"{"show_misses": null}"#)
                .unwrap_err()
                .code,
            "invalid"
        );
        assert_eq!(parse_settings(b"{").unwrap_err().code, "invalid");
        assert_eq!(
            parse_settings(br#"{"hand_size": -1}"#).unwrap_err().code,
            "invalid"
        );
    }

    #[test]
    fn player_cookie_attributes() {
        let c = player_cookie("abc", "key");
        assert_eq!(c.name(), PLAYER_COOKIE);
        assert_eq!(c.http_only(), Some(true));
        assert_eq!(c.secure(), Some(true));
        assert_eq!(c.path(), Some("/komino"));
        assert_eq!(c.max_age(), Some(time::Duration::days(365)));
    }

    #[test]
    fn session_cookie_attributes() {
        let c = session_cookie("tok");
        assert_eq!(c.name(), SESSION_COOKIE);
        assert_eq!(c.value(), "tok");
        assert_eq!(c.http_only(), Some(true));
        assert_eq!(c.secure(), Some(true));
        assert_eq!(c.same_site(), Some(SameSite::Lax));
        assert_eq!(c.path(), Some("/komino"));
        assert_eq!(c.max_age(), Some(time::Duration::days(30)));
    }
}
