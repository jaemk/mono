use crate::models::{self, ApiError, ClientAction, Player, Result, Room};
use crate::State;
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
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

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

async fn observer_view(state: &State, room: &Room) -> Result<Value> {
    models::view(&state.db, room, None, &state.config.real_hostname).await
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

pub async fn create_room(AxumState(state): AxumState<State>, jar: CookieJar) -> Result<Response> {
    let (player, jar) = identify(&state, jar).await?;
    let room = models::create_room(&state.db, &player.id).await?;
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
    models::require_member(&state.db, room.id, &player.id).await?;
    let view = room_view(&state, &room, &player.id).await?;
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
    models::require_not_removed(&state.db, room.id, &player.id).await?;
    let view = observer_view(&state, &room).await?;
    Ok((jar, Json(view)).into_response())
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
    models::act(&state.db, &room, &player.id, ClientAction::parse(body)?).await?;
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

/// Send the socket's current view. False when the socket should close.
async fn send_view(state: &State, room: &Room, who: &Who, socket: &mut WebSocket) -> bool {
    let msg = match who {
        Who::Member(player) => match models::require_member(&state.db, room.id, player).await {
            Err(e) => json!({ "type": "removed", "code": e.code }),
            Ok(()) => match room_view(state, room, player).await {
                Ok(view) => json!({ "type": "view", "view": view }),
                Err(e) => json!({ "type": "error", "code": e.code, "message": e.message }),
            },
        },
        Who::Observer { player, .. } => {
            match models::require_not_removed(&state.db, room.id, player).await {
                Err(e) => json!({ "type": "removed", "code": e.code }),
                Ok(()) => match observer_view(state, room).await {
                    Ok(view) => json!({ "type": "view", "view": view }),
                    Err(e) => json!({ "type": "error", "code": e.code, "message": e.message }),
                },
            }
        }
    };
    let closing = msg["type"] == "removed";
    socket
        .send(Message::Text(msg.to_string().into()))
        .await
        .is_ok()
        && !closing
}

async fn handle_text(state: &State, room: &Room, who: &Who, text: &str) -> Value {
    let parsed: std::result::Result<Value, _> = serde_json::from_str(text);
    let (reference, result) = match parsed {
        Ok(mut body) => {
            let reference = body.get_mut("ref").map(Value::take);
            let result = match (who, ClientAction::parse(body)) {
                (Who::Observer { .. }, _) => Err(ApiError::forbidden("observers cannot act")),
                (Who::Member(player), Ok(action)) => {
                    models::act(&state.db, room, player, action).await
                }
                (Who::Member(_), Err(e)) => Err(e),
            };
            (reference, result)
        }
        Err(_) => (None, Err(ApiError::invalid("messages must be json"))),
    };
    match result {
        Ok(()) => json!({ "type": "result", "ref": reference, "ok": true }),
        Err(e) => json!({
            "type": "result", "ref": reference, "ok": false, "code": e.code, "message": e.message,
        }),
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
    if send_view(&state, &room, &who, &mut socket).await {
        loop {
            tokio::select! {
                msg = socket.recv() => match msg {
                    Some(Ok(Message::Text(text))) => {
                        let reply = handle_text(&state, &room, &who, text.as_str()).await;
                        if socket.send(Message::Text(reply.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {}
                },
                ping = pings.recv() => match ping {
                    Ok(()) | Err(RecvError::Lagged(_)) => {
                        if !send_view(&state, &room, &who, &mut socket).await {
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
    fn player_cookie_attributes() {
        let c = player_cookie("abc", "key");
        assert_eq!(c.name(), PLAYER_COOKIE);
        assert_eq!(c.http_only(), Some(true));
        assert_eq!(c.secure(), Some(true));
        assert_eq!(c.path(), Some("/komino"));
        assert_eq!(c.max_age(), Some(time::Duration::days(365)));
    }
}
