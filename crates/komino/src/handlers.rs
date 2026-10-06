use crate::game::{Action, Secret, Settings};
use crate::lag::{self, Rtt, Seen};
use crate::models::{self, Acted, ApiError, ClientAction, Player, Result, Room, Snapshot};
use crate::{sealed, State};
use axum::body::Bytes;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State as AxumState,
    },
    http::{header, StatusCode},
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

fn cookie_id(state: &State, jar: &CookieJar) -> Option<String> {
    jar.get(PLAYER_COOKIE)
        .and_then(|c| verify_player(c.value(), &state.config.signing_key))
}

/// The requesting player, issuing a fresh identity when the cookie is
/// missing, forged, or unknown.
async fn identify(state: &State, jar: CookieJar) -> Result<(Player, CookieJar)> {
    let id = cookie_id(state, &jar);
    let (player, fresh) = models::ensure_player(&state.db, id.as_deref()).await?;
    let jar = if fresh {
        jar.add(player_cookie(&player.id, &state.config.signing_key))
    } else {
        jar
    };
    Ok((player, jar))
}

async fn room_view(state: &State, room: &Room, player: &str) -> Result<Value> {
    models::view(&state.db, room, Some(player), &state.config.real_hostname).await
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

/// The spa shell for `/komino` and `/komino/r/<code>`. Issues the cookie on
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
    Ok((jar, Json(json!({ "id": player.id, "name": player.name }))).into_response())
}

#[derive(Deserialize)]
pub struct RenameRequest {
    pub name: String,
}

pub async fn rename(
    AxumState(state): AxumState<State>,
    jar: CookieJar,
    Json(req): Json<RenameRequest>,
) -> Result<Response> {
    let (player, jar) = identify(&state, jar).await?;
    let name = models::set_name(&state.db, &player.id, &req.name).await?;
    Ok((jar, Json(json!({ "id": player.id, "name": name }))).into_response())
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
    let player =
        cookie_id(&state, &jar).ok_or_else(|| ApiError::forbidden("load the room page first"))?;
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
    let player =
        cookie_id(&state, &jar).ok_or_else(|| ApiError::forbidden("load the watch page first"))?;
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
}
