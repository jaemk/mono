use axum::http::StatusCode;
use axum_test::TestServer;
use komino::game::{Game, Reveal, Stage, Status};
use komino::{service, Config, State};
use serde_json::{json, Value};

fn set_workspace_root() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    std::env::set_current_dir(workspace_root).ok();
}

async fn get_state() -> State {
    set_workspace_root();
    let state = service::init(Config::load())
        .await
        .expect("failed to initialize komino state");
    komino::test_utils::clean_komino_db(&state.db).await;
    state
}

/// A fresh cookie-holding browser over the shared state.
fn client(state: &State) -> TestServer {
    let router = service::router(state.clone()).with_state(state.clone());
    TestServer::builder().save_cookies().build(router)
}

async fn me(server: &TestServer) -> String {
    let resp = server.get("/api/me").await;
    resp.assert_status_ok();
    resp.json::<Value>()["id"].as_str().unwrap().to_string()
}

async fn create_room(server: &TestServer) -> String {
    let resp = server.post("/api/rooms").await;
    resp.assert_status_ok();
    resp.json::<Value>()["room"]["code"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn act(server: &TestServer, code: &str, body: Value) -> axum_test::TestResponse {
    server
        .post(&format!("/api/rooms/{code}/action"))
        .json(&body)
        .await
}

/// Host plus one guest in a fresh room.
async fn room_of_two(state: &State) -> (TestServer, TestServer, String) {
    let host = client(state);
    let guest = client(state);
    let code = create_room(&host).await;
    guest
        .post(&format!("/api/rooms/{code}/join"))
        .await
        .assert_status_ok();
    (host, guest, code)
}

/// A cookie-holding browser over a real socket, for websocket routes.
fn ws_client(state: &State) -> TestServer {
    let router = service::router(state.clone()).with_state(state.clone());
    TestServer::builder()
        .http_transport()
        .save_cookies()
        .build(router)
}

/// The next json message on a socket, failing if none arrives in 5s.
async fn next(socket: &mut axum_test::TestWebSocket) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(5), socket.receive_json())
        .await
        .expect("socket went quiet")
}

/// Read socket messages until one matches.
async fn next_matching(socket: &mut axum_test::TestWebSocket, f: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..20 {
        let msg = next(socket).await;
        if f(&msg) {
            return msg;
        }
    }
    panic!("no matching socket message");
}

async fn observer_count(state: &State, code: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM room_observers o JOIN rooms r ON r.id = o.room_id
         WHERE r.code = $1 AND o.until > now()",
    )
    .bind(code)
    .fetch_one(&state.db)
    .await
    .unwrap()
}

/// A client's ECDH key pair, as the browser makes one (SEAL-5).
struct ClientKey {
    secret: p256::SecretKey,
    public_b64: String,
}

impl ClientKey {
    fn new() -> Self {
        use base64::Engine;
        let secret = loop {
            let bytes = common::crypto::rand_bytes(32).unwrap();
            if let Ok(s) = p256::SecretKey::from_slice(&bytes) {
                break s;
            }
        };
        let public_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(komino::sealed::public_point(&secret.public_key()));
        Self { secret, public_b64 }
    }

    /// How the server records the key in `client_keys`.
    fn hash(&self) -> String {
        hex::encode(common::crypto::sha256(&komino::sealed::public_point(
            &self.secret.public_key(),
        )))
    }

    /// Decrypt a sealed reveal response for room `code`.
    fn open(&self, state: &State, code: &str, resp: axum_test::TestResponse) -> Value {
        resp.assert_status_ok();
        let body: Value = resp.json();
        let sealed = komino::sealed::Sealed {
            kid: body["kid"].as_str().unwrap().into(),
            salt: body["salt"].as_str().unwrap().into(),
            iv: body["iv"].as_str().unwrap().into(),
            ct: body["ct"].as_str().unwrap().into(),
        };
        let plain = komino::sealed::open(
            &self.secret,
            &state.server_key.public_key,
            code.as_bytes(),
            &sealed,
        )
        .expect("could not open the sealed reveal");
        serde_json::from_slice(&plain).unwrap()
    }
}

async fn reveal(
    server: &TestServer,
    code: &str,
    key: &ClientKey,
    what: &str,
    id: Option<&str>,
) -> axum_test::TestResponse {
    server
        .post(&format!("/api/rooms/{code}/reveal"))
        .json(&json!({ "client_key": key.public_b64, "what": what, "id": id }))
        .await
}

/// Replace the running game's state wholesale.
async fn set_game(state: &State, code: &str, f: impl FnOnce(&mut Game)) {
    let row: Value = sqlx::query_scalar(
        "SELECT g.state FROM games g JOIN rooms r ON r.id = g.room_id
         WHERE r.code = $1 AND g.status <> 'scored'",
    )
    .bind(code)
    .fetch_one(&state.db)
    .await
    .unwrap();
    let mut game: Game = serde_json::from_value(row).unwrap();
    f(&mut game);
    sqlx::query(
        "UPDATE games g SET state = $1, status = $2 FROM rooms r
         WHERE r.id = g.room_id AND r.code = $3 AND g.status <> 'scored'",
    )
    .bind(serde_json::to_value(&game).unwrap())
    .bind(game.status.as_str())
    .bind(code)
    .execute(&state.db)
    .await
    .unwrap();
}

#[tokio::test]
async fn test_index_issues_a_player_cookie() {
    let state = get_state().await;
    let server = client(&state);
    let resp = server.get("/").await;
    resp.assert_status_ok();
    assert!(resp.text().contains("komino"));
    assert!(resp.maybe_cookie("komino_player").is_some());
    // the same browser keeps its identity
    let a = me(&server).await;
    let b = me(&server).await;
    assert_eq!(a, b);
}

#[tokio::test]
async fn test_forged_cookie_gets_a_fresh_identity() {
    let state = get_state().await;
    let server = client(&state);
    let real = me(&server).await;
    let other = TestServer::new(service::router(state.clone()).with_state(state.clone()));
    let resp = other
        .get("/api/me")
        .add_cookie(axum_extra::extract::cookie::Cookie::new(
            "komino_player",
            format!("{real}.deadbeef"),
        ))
        .await;
    assert_ne!(resp.json::<Value>()["id"].as_str().unwrap(), real);
}

#[tokio::test]
async fn test_rename_validates() {
    let state = get_state().await;
    let server = client(&state);
    let resp = server
        .post("/api/me")
        .json(&json!({ "name": "  ada  " }))
        .await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>()["name"], "ada");
    server
        .post("/api/me")
        .json(&json!({ "name": "   " }))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    server
        .post("/api/me")
        .json(&json!({ "name": "x".repeat(25) }))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_create_and_join_by_code() {
    let state = get_state().await;
    let (host, guest, code) = room_of_two(&state).await;
    assert_eq!(code.len(), 6);
    let view: Value = guest
        .get(&format!("/api/rooms/{}", code.to_lowercase()))
        .await
        .json();
    assert_eq!(view["room"]["host"], me(&host).await);
    assert!(view["room"]["url"]
        .as_str()
        .unwrap()
        .ends_with(&format!("/komino/r/{code}")));
    assert_eq!(view["members"].as_array().unwrap().len(), 2);

    // a stranger can't read the room without joining
    let stranger = client(&state);
    stranger
        .get(&format!("/api/rooms/{code}"))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    stranger
        .post("/api/rooms/ZZZZZZ/join")
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_only_host_starts_and_needs_two_present() {
    let state = get_state().await;
    let host = client(&state);
    let code = create_room(&host).await;
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    let guest = client(&state);
    guest
        .post(&format!("/api/rooms/{code}/join"))
        .await
        .assert_status_ok();
    act(&guest, &code, json!({ "type": "start" }))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    let resp = act(&host, &code, json!({ "type": "start" })).await;
    resp.assert_status_ok();
    let view: Value = resp.json();
    assert_eq!(view["game"]["status"], "peeking");
    assert_eq!(view["game"]["seats"].as_array().unwrap().len(), 2);
    // one game at a time
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_views_are_redacted_per_player() {
    let state = get_state().await;
    let (host, guest, code) = room_of_two(&state).await;
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    let host_id = me(&host).await;
    let view: Value = guest.get(&format!("/api/rooms/{code}")).await.json();
    let seats = view["game"]["seats"].as_array().unwrap();
    let host_seat = seats.iter().find(|s| s["player"] == host_id).unwrap();
    for slot in host_seat["slots"].as_array().unwrap() {
        assert!(slot.get("v").is_none(), "guest saw a host card: {view}");
    }
    let text = view.to_string();
    assert!(!text.contains("\"deck\""));
    // no private value at all travels in a view, not even the guest's own
    assert!(!view["game"].to_string().contains("\"v\""), "{view}");
    // the guest reveals their own opening peek, sealed
    let key = ClientKey::new();
    let cards = key.open(
        &state,
        &code,
        reveal(&guest, &code, &key, "opening", None).await,
    );
    let guest_seat = seats.iter().position(|s| s["player"] != host_id).unwrap();
    assert_eq!(cards["cards"].as_array().unwrap().len(), 2);
    assert_eq!(cards["cards"][0]["seat"], guest_seat);
    assert_eq!(cards["cards"][0]["slot"], 2);
}

#[tokio::test]
async fn test_turn_flow_over_http_and_stats() {
    let state = get_state().await;
    let (host, guest, code) = room_of_two(&state).await;
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    set_game(&state, &code, |g| {
        g.turn = 0;
        g.deck = vec![1, 2, 3];
    })
    .await;
    act(&host, &code, json!({ "type": "ready" }))
        .await
        .assert_status_ok();
    let view: Value = act(&guest, &code, json!({ "type": "ready" })).await.json();
    let seq = view["game"]["turn_seq"].as_u64().unwrap();
    act(&guest, &code, json!({ "type": "draw", "turn_seq": seq }))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    // a turn action without its token is refused
    let resp = act(&host, &code, json!({ "type": "draw" })).await;
    resp.assert_status(StatusCode::CONFLICT);
    assert_eq!(resp.json::<Value>()["code"], "stale");
    let view: Value = act(&host, &code, json!({ "type": "draw", "turn_seq": seq }))
        .await
        .json();
    // the drawn card is only ever revealed sealed
    assert!(view["game"]["stage"]["card"].is_null());
    let key = ClientKey::new();
    let drawn = key.open(
        &state,
        &code,
        reveal(&host, &code, &key, "drawn", None).await,
    );
    assert_eq!(drawn, json!({ "card": 3 }));
    // the spent token can't be replayed
    act(&host, &code, json!({ "type": "draw", "turn_seq": seq }))
        .await
        .assert_status(StatusCode::CONFLICT);
    let seq = view["game"]["turn_seq"].as_u64().unwrap();
    let view: Value = act(
        &host,
        &code,
        json!({ "type": "swap", "slot": 0, "turn_seq": seq }),
    )
    .await
    .json();
    assert_eq!(view["game"]["turn"], 1);
    let host_id = me(&host).await;
    let stats = view["stats"].as_array().unwrap();
    let row = stats.iter().find(|s| s["player"] == host_id).unwrap();
    assert_eq!(row["cards_interacted"], 3);
    let kinds: Vec<&str> = view["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"swap"));
    assert!(kinds.contains(&"draw"));
}

#[tokio::test]
async fn test_concurrent_matches_exactly_one_wins() {
    let state = get_state().await;
    let (host, guest, code) = room_of_two(&state).await;
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    set_game(&state, &code, |g| {
        g.status = Status::Playing;
        g.stage = Stage::Start;
        g.ready_deadline = None;
        g.discard = vec![5];
        g.discard_seq = 3;
        g.matchable = true;
        g.seats[0].slots = vec![Some(5), Some(1), Some(1), Some(1)];
        g.seats[1].slots = vec![Some(5), Some(2), Some(2), Some(2)];
    })
    .await;
    let m = |seat: usize| json!({ "type": "match", "seq": 3, "seat": seat, "slot": 0 });
    let (a, b) = tokio::join!(act(&host, &code, m(0)), act(&guest, &code, m(1)));
    let mut statuses = [a.status_code(), b.status_code()];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::CONFLICT]);
    let loser = if a.status_code() == StatusCode::CONFLICT {
        a
    } else {
        b
    };
    assert_eq!(loser.json::<Value>()["code"], "too_late");

    let view: Value = host.get(&format!("/api/rooms/{code}")).await.json();
    let emptied = view["game"]["seats"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["slots"][0].is_null())
        .count();
    assert_eq!(emptied, 1);
    let total_matches: i64 = view["stats"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["matches"].as_i64().unwrap())
        .sum();
    assert_eq!(total_matches, 1);
}

#[tokio::test]
async fn test_host_removes_a_player_who_forfeits_and_cannot_rejoin() {
    let state = get_state().await;
    let host = client(&state);
    let code = create_room(&host).await;
    let a = client(&state);
    let b = client(&state);
    for c in [&a, &b] {
        c.post(&format!("/api/rooms/{code}/join"))
            .await
            .assert_status_ok();
    }
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    let b_id = me(&b).await;

    // only the host can remove
    a.post(&format!("/api/rooms/{code}/remove"))
        .json(&json!({ "player": b_id }))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    let view: Value = host
        .post(&format!("/api/rooms/{code}/remove"))
        .json(&json!({ "player": b_id }))
        .await
        .json();
    let seat = view["game"]["seats"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["player"] == b_id)
        .unwrap()
        .clone();
    assert_eq!(seat["forfeited"], true);
    assert_eq!(seat["slots"].as_array().unwrap().len(), 0);

    b.get(&format!("/api/rooms/{code}"))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    let resp = b.post(&format!("/api/rooms/{code}/join")).await;
    resp.assert_status(StatusCode::FORBIDDEN);
    assert_eq!(resp.json::<Value>()["code"], "removed");

    host.post(&format!("/api/rooms/{code}/unban"))
        .json(&json!({ "player": b_id }))
        .await
        .assert_status_ok();
    b.post(&format!("/api/rooms/{code}/join"))
        .await
        .assert_status_ok();
}

#[tokio::test]
async fn test_leaving_host_hands_off_and_ends_a_two_player_game() {
    let state = get_state().await;
    let (host, guest, code) = room_of_two(&state).await;
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    host.post(&format!("/api/rooms/{code}/leave"))
        .await
        .assert_status_ok();
    let view: Value = guest.get(&format!("/api/rooms/{code}")).await.json();
    let guest_id = me(&guest).await;
    assert_eq!(view["room"]["host"], guest_id);
    assert_eq!(view["game"]["status"], "scored");
    let row = view["stats"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["player"] == guest_id)
        .unwrap()
        .clone();
    assert_eq!(row["wins"], 1);
    assert_eq!(row["games_played"], 1);

    // the leaver can come back through the share link, stats intact
    host.post(&format!("/api/rooms/{code}/join"))
        .await
        .assert_status_ok();
    let view: Value = host.get(&format!("/api/rooms/{code}")).await.json();
    let host_id = me(&host).await;
    let row = view["stats"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["player"] == host_id)
        .unwrap()
        .clone();
    assert_eq!(row["forfeits"], 1);
}

/// An action through one connection reaches another player's socket through
/// postgres notify, and socket actions get a result back.
#[tokio::test]
async fn test_websocket_pushes_views_and_takes_actions() {
    let state = get_state().await;
    let router = || service::router(state.clone()).with_state(state.clone());
    let host = TestServer::builder()
        .http_transport()
        .save_cookies()
        .build(router());
    let guest = client(&state);
    let code = create_room(&host).await;
    guest
        .post(&format!("/api/rooms/{code}/join"))
        .await
        .assert_status_ok();

    let mut socket = host
        .get_websocket(&format!("/r/{code}/ws"))
        .await
        .into_websocket()
        .await;
    let first: Value = socket.receive_json().await;
    assert_eq!(first["type"], "view");
    assert_eq!(first["view"]["room"]["code"], code);

    socket
        .send_json(&json!({ "type": "start", "ref": 1 }))
        .await;
    let mut saw_result = false;
    let mut saw_game = false;
    for _ in 0..6 {
        let msg: Value =
            tokio::time::timeout(std::time::Duration::from_secs(5), socket.receive_json())
                .await
                .expect("socket went quiet");
        if msg["type"] == "result" {
            assert_eq!(msg["ok"], true);
            assert_eq!(msg["ref"], 1);
            saw_result = true;
        }
        if msg["type"] == "view" && msg["view"]["game"]["status"] == "peeking" {
            saw_game = true;
        }
        if saw_result && saw_game {
            break;
        }
    }
    assert!(saw_result && saw_game);

    // the guest acts over http; the host's socket hears about it
    act(&guest, &code, json!({ "type": "ready" }))
        .await
        .assert_status_ok();
    let guest_id = me(&guest).await;
    let mut heard = false;
    for _ in 0..6 {
        let msg: Value =
            tokio::time::timeout(std::time::Duration::from_secs(5), socket.receive_json())
                .await
                .expect("socket went quiet");
        let seats = msg["view"]["game"]["seats"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if seats
            .iter()
            .any(|s| s["player"] == guest_id && s["ready"] == true)
        {
            heard = true;
            break;
        }
    }
    assert!(heard, "the host socket never saw the guest get ready");

    // a bad action gets a typed rejection, not a dropped socket
    socket.send_json(&json!({ "type": "draw", "ref": 2 })).await;
    loop {
        let msg: Value =
            tokio::time::timeout(std::time::Duration::from_secs(5), socket.receive_json())
                .await
                .expect("socket went quiet");
        if msg["type"] == "result" {
            assert_eq!(msg["ok"], false);
            assert_eq!(msg["ref"], 2);
            break;
        }
    }
}

#[tokio::test]
async fn test_tick_fires_scoring_after_the_delay() {
    let state = get_state().await;
    let (host, _guest, code) = room_of_two(&state).await;
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    set_game(&state, &code, |g| {
        g.status = Status::Scoring;
        g.score_at = Some(0);
    })
    .await;
    komino::models::tick_all(&state.db).await.unwrap();
    let view: Value = host.get(&format!("/api/rooms/{code}")).await.json();
    assert_eq!(view["game"]["status"], "scored");
    // every card is face up once scored
    for seat in view["game"]["seats"].as_array().unwrap() {
        for slot in seat["slots"].as_array().unwrap() {
            assert!(slot.get("v").is_some());
        }
    }
}

#[tokio::test]
async fn test_pages_and_static_assets_are_served() {
    let state = get_state().await;
    let server = client(&state);
    for path in ["/r/ABCDEF", "/r/ABCDEF/watch"] {
        let resp = server.get(path).await;
        resp.assert_status_ok();
        assert!(resp.text().contains("/komino/static/app.js"));
    }
    let js = server.get("/static/app.js").await;
    js.assert_status_ok();
    assert!(js.text().contains("createKomino"));
    server.get("/static/app.css").await.assert_status_ok();
    server
        .get("/static/nope.js")
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_unknown_and_malformed_codes_are_not_found() {
    let state = get_state().await;
    let server = client(&state);
    for path in [
        "/api/rooms/ZZZZZZ",
        "/api/rooms/bad!!!",
        "/api/rooms/ABC",
        "/api/rooms/ZZZZZZ/watch",
    ] {
        let resp = server.get(path).await;
        resp.assert_status(StatusCode::NOT_FOUND);
        assert_eq!(resp.json::<Value>()["code"], "not_found");
    }
    server
        .post("/api/rooms/ZZZZZZ/leave")
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_member_routes_require_membership() {
    let state = get_state().await;
    let (host, _guest, code) = room_of_two(&state).await;
    let host_id = me(&host).await;
    let stranger = client(&state);
    stranger
        .post(&format!("/api/rooms/{code}/leave"))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    stranger
        .post(&format!("/api/rooms/{code}/remove"))
        .json(&json!({ "player": host_id }))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    let resp = act(&stranger, &code, json!({ "type": "start" })).await;
    resp.assert_status(StatusCode::FORBIDDEN);
    assert_eq!(resp.json::<Value>()["code"], "forbidden");

    // sockets need the cookie the page issues, and membership
    let router = service::router(state.clone()).with_state(state.clone());
    let cookieless = TestServer::builder().http_transport().build(router);
    cookieless
        .get_websocket(&format!("/r/{code}/ws"))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    cookieless
        .get_websocket(&format!("/r/{code}/watch/ws"))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    let outsider = ws_client(&state);
    me(&outsider).await;
    outsider
        .get_websocket(&format!("/r/{code}/ws"))
        .await
        .assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_host_controls_reject_bad_targets() {
    let state = get_state().await;
    let (host, guest, code) = room_of_two(&state).await;
    let host_id = me(&host).await;
    let guest_id = me(&guest).await;
    host.post(&format!("/api/rooms/{code}/remove"))
        .json(&json!({ "player": host_id }))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    host.post(&format!("/api/rooms/{code}/remove"))
        .json(&json!({ "player": "nobody" }))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    guest
        .post(&format!("/api/rooms/{code}/unban"))
        .json(&json!({ "player": host_id }))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    // removed players show in the host's member list, marked removed
    let view: Value = host
        .post(&format!("/api/rooms/{code}/remove"))
        .json(&json!({ "player": guest_id }))
        .await
        .json();
    let row = view["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == guest_id)
        .unwrap()
        .clone();
    assert_eq!(row["removed"], true);
}

#[tokio::test]
async fn test_actions_reject_malformed_bodies() {
    let state = get_state().await;
    let (host, _guest, code) = room_of_two(&state).await;
    let resp = act(&host, &code, json!({ "type": "ready" })).await;
    resp.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(resp.json::<Value>()["message"], "no game is in progress");
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    for body in [
        json!({ "type": "nope" }),
        json!({ "type": "draw", "turn_seq": "x" }),
        json!({ "type": "swap", "turn_seq": 1 }),
        json!([1, 2]),
    ] {
        let resp = act(&host, &code, body).await;
        resp.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(resp.json::<Value>()["code"], "invalid");
    }
}

#[tokio::test]
async fn test_rename_reaches_the_room() {
    let state = get_state().await;
    let (host, guest, code) = room_of_two(&state).await;
    host.post("/api/me")
        .json(&json!({ "name": "ada" }))
        .await
        .assert_status_ok();
    host.post("/api/me")
        .json(&json!({ "name": "a\u{7}b" }))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    let view: Value = guest.get(&format!("/api/rooms/{code}")).await.json();
    let host_id = me(&host).await;
    let row = view["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == host_id)
        .unwrap()
        .clone();
    assert_eq!(row["name"], "ada");
}

#[tokio::test]
async fn test_watch_view_is_public_and_does_not_join() {
    let state = get_state().await;
    let (host, _guest, code) = room_of_two(&state).await;
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    let watcher = client(&state);
    let resp = watcher.get(&format!("/api/rooms/{code}/watch")).await;
    resp.assert_status_ok();
    // the first visit gets an identity cookie like any page
    assert!(resp.maybe_cookie("komino_player").is_some());
    let view: Value = resp.json();
    assert_eq!(view["observer"], true);
    assert_eq!(view["me"], Value::Null);
    assert_eq!(view["game"]["me"], Value::Null);
    assert_eq!(view["game"]["status"], "peeking");
    assert!(view["room"]["watch_url"]
        .as_str()
        .unwrap()
        .ends_with(&format!("/komino/r/{code}/watch")));
    // nobody's opening peek reaches an observer
    assert!(
        !view["game"].to_string().contains("\"v\""),
        "observer saw a value: {view}"
    );
    assert_eq!(view["members"].as_array().unwrap().len(), 2);
    // watching did not make them a member
    watcher
        .get(&format!("/api/rooms/{code}"))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    act(&watcher, &code, json!({ "type": "ready" }))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    // members see themselves, not an observer view
    let view: Value = host.get(&format!("/api/rooms/{code}")).await.json();
    assert_eq!(view["observer"], false);
    assert_eq!(view["observers"], 0);
}

#[tokio::test]
async fn test_removed_player_cannot_watch() {
    let state = get_state().await;
    let host = client(&state);
    let code = create_room(&host).await;
    let banned = ws_client(&state);
    banned
        .post(&format!("/api/rooms/{code}/join"))
        .await
        .assert_status_ok();
    host.post(&format!("/api/rooms/{code}/remove"))
        .json(&json!({ "player": me(&banned).await }))
        .await
        .assert_status_ok();
    let resp = banned.get(&format!("/api/rooms/{code}/watch")).await;
    resp.assert_status(StatusCode::FORBIDDEN);
    assert_eq!(resp.json::<Value>()["code"], "removed");
    banned
        .get_websocket(&format!("/r/{code}/watch/ws"))
        .await
        .assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_observer_socket_gets_views_and_cannot_act() {
    let state = get_state().await;
    let (host, guest, code) = room_of_two(&state).await;
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    let watcher = ws_client(&state);
    watcher
        .get(&format!("/api/rooms/{code}/watch"))
        .await
        .assert_status_ok();
    let mut socket = watcher
        .get_websocket(&format!("/r/{code}/watch/ws"))
        .await
        .into_websocket()
        .await;
    let first = next(&mut socket).await;
    assert_eq!(first["type"], "view");
    assert_eq!(first["view"]["observer"], true);
    assert_eq!(observer_count(&state, &code).await, 1);
    let view: Value = host.get(&format!("/api/rooms/{code}")).await.json();
    assert_eq!(view["observers"], 1);

    socket
        .send_json(&json!({ "type": "ready", "ref": 7 }))
        .await;
    let result = next_matching(&mut socket, |m| m["type"] == "result").await;
    assert_eq!(result["ok"], false);
    assert_eq!(result["ref"], 7);
    assert_eq!(result["code"], "forbidden");

    // a player's action reaches the observer, still without values
    act(&guest, &code, json!({ "type": "ready" }))
        .await
        .assert_status_ok();
    let guest_id = me(&guest).await;
    let msg = next_matching(&mut socket, |m| {
        m["view"]["game"]["seats"].as_array().is_some_and(|s| {
            s.iter()
                .any(|s| s["player"] == guest_id && s["ready"] == true)
        })
    })
    .await;
    assert!(!msg["view"]["game"].to_string().contains("\"v\""));

    socket.close().await;
    for _ in 0..50 {
        if observer_count(&state, &code).await == 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the observer lease was not released on disconnect");
}

#[tokio::test]
async fn test_removal_closes_an_open_watch_socket() {
    let state = get_state().await;
    let host = client(&state);
    let code = create_room(&host).await;
    let member = ws_client(&state);
    member
        .post(&format!("/api/rooms/{code}/join"))
        .await
        .assert_status_ok();
    let mut socket = member
        .get_websocket(&format!("/r/{code}/watch/ws"))
        .await
        .into_websocket()
        .await;
    assert_eq!(next(&mut socket).await["type"], "view");
    host.post(&format!("/api/rooms/{code}/remove"))
        .json(&json!({ "player": me(&member).await }))
        .await
        .assert_status_ok();
    let msg = next_matching(&mut socket, |m| m["type"] == "removed").await;
    assert_eq!(msg["code"], "removed");
}

/// Wait for the server to close a socket, skipping any views on the way.
async fn closed(socket: &mut axum_test::TestWebSocket, within: std::time::Duration) {
    use axum_test::WsMessage;
    tokio::time::timeout(within, async {
        loop {
            if let WsMessage::Close(_) = socket.receive_message().await {
                return;
            }
        }
    })
    .await
    .expect("the socket stayed open");
}

#[tokio::test]
async fn test_observer_socket_closes_when_its_lease_is_gone() {
    let state = get_state().await;
    let host = client(&state);
    let code = create_room(&host).await;
    let watcher = ws_client(&state);
    me(&watcher).await;
    let mut socket = watcher
        .get_websocket(&format!("/r/{code}/watch/ws"))
        .await
        .into_websocket()
        .await;
    assert_eq!(next(&mut socket).await["type"], "view");
    // the lease expired and was cleaned up, so the slot may belong to someone else
    sqlx::query("DELETE FROM room_observers")
        .execute(&state.db)
        .await
        .unwrap();
    // the next heartbeat (every 10s) notices and closes the socket
    closed(&mut socket, std::time::Duration::from_secs(15)).await;
}

#[tokio::test]
async fn test_observer_leases_refresh_only_while_live() {
    let state = get_state().await;
    let host = client(&state);
    let code = create_room(&host).await;
    let room = komino::models::room_by_code(&state.db, &code)
        .await
        .unwrap();
    let lease = komino::models::claim_observer(&state.db, room.id)
        .await
        .unwrap();
    assert!(komino::models::refresh_observer(&state.db, &lease)
        .await
        .unwrap());
    sqlx::query("UPDATE room_observers SET until = now() - interval '1 second'")
        .execute(&state.db)
        .await
        .unwrap();
    assert!(!komino::models::refresh_observer(&state.db, &lease)
        .await
        .unwrap());
    assert!(!komino::models::refresh_observer(&state.db, "nope")
        .await
        .unwrap());
}

#[tokio::test]
async fn test_views_carry_the_server_clock() {
    let state = get_state().await;
    let host = client(&state);
    let code = create_room(&host).await;
    let before = chrono::Utc::now().timestamp_millis();
    let view: Value = host.get(&format!("/api/rooms/{code}")).await.json();
    let at = view["server_now"].as_i64().unwrap();
    assert!(at >= before && at <= chrono::Utc::now().timestamp_millis());
}

#[tokio::test]
async fn test_at_most_four_observers() {
    let state = get_state().await;
    let host = client(&state);
    let code = create_room(&host).await;
    let watcher = ws_client(&state);
    me(&watcher).await;
    let mut sockets = Vec::new();
    for _ in 0..4 {
        let mut socket = watcher
            .get_websocket(&format!("/r/{code}/watch/ws"))
            .await
            .into_websocket()
            .await;
        assert_eq!(next(&mut socket).await["type"], "view");
        sockets.push(socket);
    }
    assert_eq!(observer_count(&state, &code).await, 4);
    let mut fifth = watcher
        .get_websocket(&format!("/r/{code}/watch/ws"))
        .await
        .into_websocket()
        .await;
    let msg = next(&mut fifth).await;
    assert_eq!(msg["type"], "full");
    assert_eq!(observer_count(&state, &code).await, 4);

    // an expired lease frees its slot even if its socket never said goodbye
    sqlx::query("UPDATE room_observers SET until = now() - interval '1 second' WHERE id IN (SELECT id FROM room_observers LIMIT 1)")
        .execute(&state.db)
        .await
        .unwrap();
    let mut late = watcher
        .get_websocket(&format!("/r/{code}/watch/ws"))
        .await
        .into_websocket()
        .await;
    assert_eq!(next(&mut late).await["type"], "view");
}

#[tokio::test]
async fn test_member_socket_rejects_non_json_and_closes_on_removal() {
    let state = get_state().await;
    let host = client(&state);
    let code = create_room(&host).await;
    let guest = ws_client(&state);
    guest
        .post(&format!("/api/rooms/{code}/join"))
        .await
        .assert_status_ok();
    let mut socket = guest
        .get_websocket(&format!("/r/{code}/ws"))
        .await
        .into_websocket()
        .await;
    let first = next(&mut socket).await;
    let guest_id = me(&guest).await;
    let present = first["view"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["id"] == guest_id && m["present"] == true);
    assert!(present);

    socket.send_text("not json").await;
    let result = next_matching(&mut socket, |m| m["type"] == "result").await;
    assert_eq!(result["ok"], false);
    assert_eq!(result["code"], "invalid");
    assert_eq!(result["ref"], Value::Null);

    host.post(&format!("/api/rooms/{code}/remove"))
        .json(&json!({ "player": guest_id }))
        .await
        .assert_status_ok();
    let msg = next_matching(&mut socket, |m| m["type"] == "removed").await;
    assert_eq!(msg["code"], "removed");
}

#[tokio::test]
async fn test_presence_ends_when_the_last_socket_closes() {
    let state = get_state().await;
    let host = ws_client(&state);
    let code = create_room(&host).await;
    let host_id = me(&host).await;
    let mut socket = host
        .get_websocket(&format!("/r/{code}/ws"))
        .await
        .into_websocket()
        .await;
    next(&mut socket).await;
    socket.close().await;
    for _ in 0..50 {
        let view: Value = client(&state)
            .get(&format!("/api/rooms/{code}/watch"))
            .await
            .json();
        let away = view["members"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == host_id && m["present"] == false);
        if away {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the host stayed present after closing their only socket");
}

#[tokio::test]
async fn test_idle_rooms_and_dead_observer_leases_are_cleaned() {
    let state = get_state().await;
    let host = client(&state);
    let idle = create_room(&host).await;
    let fresh = create_room(&host).await;
    sqlx::query("UPDATE rooms SET last_active = now() - interval '31 days' WHERE code = $1")
        .bind(&idle)
        .execute(&state.db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO room_observers (id, room_id, until)
         SELECT 'dead', id, now() - interval '1 minute' FROM rooms WHERE code = $1",
    )
    .bind(&fresh)
    .execute(&state.db)
    .await
    .unwrap();
    let deleted = komino::models::delete_stale_rooms(&state.db).await.unwrap();
    assert_eq!(deleted, 1);
    host.get(&format!("/api/rooms/{idle}"))
        .await
        .assert_status(StatusCode::NOT_FOUND);
    host.get(&format!("/api/rooms/{fresh}"))
        .await
        .assert_status_ok();
    let leases: i64 = sqlx::query_scalar("SELECT count(*) FROM room_observers")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(leases, 0);
}

// ---------------------------------------------------------------------------
// Sealed reveals (SEAL-*)
// ---------------------------------------------------------------------------

/// A two player game in play, host (seat 0) to move, with a live peek the
/// host made of the guest's slot 1, whose value was 9. Returns the peek id.
async fn game_with_a_peek(state: &State, host: &TestServer, code: &str) -> String {
    act(host, code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    let host_id = me(host).await;
    let id = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6".to_string();
    let peek = id.clone();
    set_game(state, code, move |g| {
        g.status = Status::Playing;
        g.stage = Stage::Start;
        g.ready_deadline = None;
        g.turn = 0;
        // the host joined first, so holds seat 0
        assert_eq!(g.seats[0].player, host_id);
        g.reveals = vec![Reveal {
            id: peek,
            player: host_id,
            seat: 1,
            slot: 1,
            value: 9,
            until: Some(chrono::Utc::now().timestamp_millis() + 60_000),
        }];
    })
    .await;
    id
}

// ---------------------------------------------------------------------------
// Settings (SET-*)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_rooms_default_their_settings() {
    let state = get_state().await;
    let host = client(&state);
    let view: Value = host.post("/api/rooms").await.json();
    assert_eq!(
        view["room"]["settings"],
        json!({ "hand_size": 4, "away_grace_secs": 30, "turn_limit_secs": null, "reveal_secs": null })
    );
}

#[tokio::test]
async fn test_room_settings_shape_the_game() {
    let state = get_state().await;
    let host = client(&state);
    let guest = client(&state);
    let settings =
        json!({ "hand_size": 9, "away_grace_secs": 60, "turn_limit_secs": 90, "reveal_secs": 10 });
    let resp = host.post("/api/rooms").json(&settings).await;
    resp.assert_status_ok();
    let code = resp.json::<Value>()["room"]["code"]
        .as_str()
        .unwrap()
        .to_string();
    guest
        .post(&format!("/api/rooms/{code}/join"))
        .await
        .assert_status_ok();
    // every later read of the room carries the same settings
    let view: Value = guest.get(&format!("/api/rooms/{code}")).await.json();
    assert_eq!(view["room"]["settings"], settings);

    let game = act(&host, &code, json!({ "type": "start" }))
        .await
        .json::<Value>()["game"]
        .clone();
    assert_eq!(game["hand_size"], 9);
    for seat in game["seats"].as_array().unwrap() {
        assert_eq!(seat["slots"].as_array().unwrap().len(), 9);
    }
    // 2 x 9 + 1 leaves 41 of one deck to draw
    assert_eq!(game["deck_count"], 41);
    let key = ClientKey::new();
    let opened = key.open(
        &state,
        &code,
        reveal(&host, &code, &key, "opening", None).await,
    );
    let slots: Vec<u64> = opened["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["slot"].as_u64().unwrap())
        .collect();
    assert_eq!(slots, vec![5, 6, 7, 8]);

    for player in [&host, &guest] {
        act(player, &code, json!({ "type": "ready" }))
            .await
            .assert_status_ok();
    }
    let view: Value = host.get(&format!("/api/rooms/{code}")).await.json();
    let deadline = view["game"]["turn_deadline"].as_i64().unwrap();
    let now = view["server_now"].as_i64().unwrap();
    assert!(
        (now + 85_000..=now + 90_000).contains(&deadline),
        "{deadline} vs {now}"
    );
}

#[tokio::test]
async fn test_out_of_range_settings_create_no_room() {
    let state = get_state().await;
    let host = client(&state);
    for bad in [
        json!({ "hand_size": 11 }),
        json!({ "hand_size": 3 }),
        json!({ "away_grace_secs": 5 }),
        json!({ "turn_limit_secs": 0 }),
        json!({ "reveal_secs": 61 }),
        json!({ "hand_size": "six" }),
    ] {
        let resp = host.post("/api/rooms").json(&bad).await;
        resp.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(resp.json::<Value>()["code"], "invalid", "{bad}");
    }
    let rooms: i64 = sqlx::query_scalar("SELECT count(*) FROM rooms")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(rooms, 0);
}

#[tokio::test]
async fn test_hide_ends_a_peek_for_everyone() {
    let state = get_state().await;
    let (host, guest, code) = room_of_two(&state).await;
    game_with_a_peek(&state, &host, &code).await;
    set_game(&state, &code, |g| g.reveals[0].until = None).await;
    let view: Value = guest.get(&format!("/api/rooms/{code}")).await.json();
    assert_eq!(
        view["game"]["peeked"],
        json!([{ "seat": 1, "slot": 1, "until": null }])
    );
    // the guest holds no peek, so hiding changes nothing for the host's
    act(&guest, &code, json!({ "type": "hide" }))
        .await
        .assert_status_ok();
    let view: Value = guest.get(&format!("/api/rooms/{code}")).await.json();
    assert_eq!(view["game"]["peeked"].as_array().unwrap().len(), 1);
    let view: Value = act(&host, &code, json!({ "type": "hide" })).await.json();
    assert_eq!(view["game"]["reveals"], json!([]));
    let view: Value = guest.get(&format!("/api/rooms/{code}")).await.json();
    assert_eq!(view["game"]["peeked"], json!([]));
}

// ---------------------------------------------------------------------------
// Within a turn (RULE-10, RULE-22)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_a_special_discard_waits_and_komino_lands_at_the_turn_end() {
    let state = get_state().await;
    let (host, guest, code) = room_of_two(&state).await;
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    set_game(&state, &code, |g| {
        g.status = Status::Playing;
        g.stage = Stage::Drawn { card: 9 };
        g.ready_deadline = None;
        g.turn = 0;
        for seat in g.seats.iter_mut() {
            seat.turns = 1;
        }
        g.seats[0].slots = vec![Some(1); 4];
        g.seats[1].slots = vec![Some(9), Some(2), Some(2), Some(2)];
    })
    .await;
    let url = format!("/api/rooms/{code}");
    let seq = |v: &Value| v["game"]["turn_seq"].as_u64().unwrap();
    let view: Value = host.get(&url).await.json();
    assert_eq!(view["game"]["can_call"], true);

    // the host calls komino with the drawn card still in hand
    let view: Value = act(
        &host,
        &code,
        json!({ "type": "komino", "turn_seq": seq(&view) }),
    )
    .await
    .json();
    assert_eq!(view["game"]["calling"], true);
    assert_eq!(view["game"]["status"], "playing");
    let view: Value = act(
        &host,
        &code,
        json!({ "type": "discard", "turn_seq": seq(&view) }),
    )
    .await
    .json();
    assert_eq!(
        view["game"]["stage"],
        json!({ "kind": "earned", "mv": "peek_other" })
    );

    // the guest sees neither the call nor a finished turn, and matches the 9
    let guest_view: Value = guest.get(&url).await.json();
    assert_eq!(guest_view["game"]["calling"], false);
    assert_eq!(guest_view["game"]["turn"], 0);
    act(
        &guest,
        &code,
        json!({ "type": "match", "seq": guest_view["game"]["discard_seq"], "seat": 1, "slot": 0 }),
    )
    .await
    .assert_status_ok();

    let view: Value = host.get(&url).await.json();
    let view: Value = act(
        &host,
        &code,
        json!({ "type": "use_special", "turn_seq": seq(&view) }),
    )
    .await
    .json();
    assert_eq!(
        view["game"]["stage"],
        json!({ "kind": "special", "mv": "peek_other" })
    );
    let view: Value = act(
        &host,
        &code,
        json!({ "type": "peek", "seat": 1, "slot": 1, "turn_seq": seq(&view) }),
    )
    .await
    .json();
    assert_eq!(view["game"]["status"], "final");
    assert_eq!(view["game"]["caller"], 0);
    assert_eq!(view["game"]["turn"], 1);
    let kinds: Vec<&str> = view["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"komino"));
}

#[tokio::test]
async fn test_server_key_is_published() {
    let state = get_state().await;
    let server = client(&state);
    let body: Value = server.get("/api/key").await.json();
    assert_eq!(body["kid"], state.server_key.kid);
    assert_eq!(body["public_key"], state.server_key.public_key);
    assert!(komino::sealed::parse_public(body["public_key"].as_str().unwrap()).is_some());
}

#[tokio::test]
async fn test_peeks_reveal_sealed_and_only_once() {
    let state = get_state().await;
    let (host, guest, code) = room_of_two(&state).await;
    let id = game_with_a_peek(&state, &host, &code).await;
    // the view names the peek but carries no value
    let view: Value = host.get(&format!("/api/rooms/{code}")).await.json();
    assert_eq!(view["game"]["reveals"][0]["id"], id);
    assert!(!view["game"].to_string().contains("\"v\""));

    let key = ClientKey::new();
    let resp = reveal(&host, &code, &key, "peek", Some(&id)).await;
    assert_eq!(resp.header("cache-control"), "no-store");
    let raw = resp.text();
    assert!(!raw.contains("\"v\""), "the response is not sealed: {raw}");
    let cards = key.open(&state, &code, resp);
    assert_eq!(
        cards,
        json!({ "cards": [{ "seat": 1, "slot": 1, "v": 9 }] })
    );

    // spent
    let again = reveal(&host, &code, &key, "peek", Some(&id)).await;
    again.assert_status(StatusCode::CONFLICT);
    assert_eq!(again.json::<Value>()["code"], "already_revealed");
    // and never anyone else's
    let resp = reveal(&guest, &code, &ClientKey::new(), "peek", Some(&id)).await;
    resp.assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_a_failed_peek_reveal_does_not_spend_it() {
    let state = get_state().await;
    let (host, _guest, code) = room_of_two(&state).await;
    let id = game_with_a_peek(&state, &host, &code).await;
    let key = ClientKey::new();
    // the host's key is already past its window
    sqlx::query(
        "INSERT INTO client_keys (key_hash, player_id, first_seen)
         VALUES ($1, $2, now() - interval '10 minutes')",
    )
    .bind(key.hash())
    .bind(me(&host).await)
    .execute(&state.db)
    .await
    .unwrap();
    let resp = reveal(&host, &code, &key, "peek", Some(&id)).await;
    resp.assert_status(StatusCode::CONFLICT);
    assert_eq!(resp.json::<Value>()["code"], "key_expired");
    // a fresh key still gets the peek
    let fresh = ClientKey::new();
    let cards = fresh.open(
        &state,
        &code,
        reveal(&host, &code, &fresh, "peek", Some(&id)).await,
    );
    assert_eq!(cards["cards"][0]["v"], 9);
}

#[tokio::test]
async fn test_client_keys_are_bound_to_one_player_for_five_minutes() {
    let state = get_state().await;
    let (host, guest, code) = room_of_two(&state).await;
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    let key = ClientKey::new();
    reveal(&host, &code, &key, "opening", None)
        .await
        .assert_status_ok();
    // the same key works again for the same player inside the window
    reveal(&host, &code, &key, "opening", None)
        .await
        .assert_status_ok();
    // another player can't reuse it
    let resp = reveal(&guest, &code, &key, "opening", None).await;
    resp.assert_status(StatusCode::FORBIDDEN);
    // after five minutes it is refused
    sqlx::query("UPDATE client_keys SET first_seen = now() - interval '301 seconds'")
        .execute(&state.db)
        .await
        .unwrap();
    let resp = reveal(&host, &code, &key, "opening", None).await;
    resp.assert_status(StatusCode::CONFLICT);
    assert_eq!(resp.json::<Value>()["code"], "key_expired");
    // stale bindings are cleaned up after a day
    sqlx::query("UPDATE client_keys SET first_seen = now() - interval '2 days'")
        .execute(&state.db)
        .await
        .unwrap();
    komino::models::delete_stale_rooms(&state.db).await.unwrap();
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM client_keys")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn test_reveals_check_the_cookie_and_the_game() {
    let state = get_state().await;
    let (host, guest, code) = room_of_two(&state).await;
    let key = ClientKey::new();
    // no game yet
    reveal(&host, &code, &key, "opening", None)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    // strangers and observers are not members
    let stranger = client(&state);
    stranger
        .get(&format!("/api/rooms/{code}/watch"))
        .await
        .assert_status_ok();
    reveal(&stranger, &code, &ClientKey::new(), "opening", None)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    // nothing has been drawn, and after ready the opening peek is over
    reveal(&guest, &code, &ClientKey::new(), "drawn", None)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    act(&guest, &code, json!({ "type": "ready" }))
        .await
        .assert_status_ok();
    reveal(&guest, &code, &ClientKey::new(), "opening", None)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    reveal(&host, &code, &key, "opening", None)
        .await
        .assert_status_ok();
}

/// A reveal waits for an action that holds the game row, then checks the
/// state that action committed.
#[tokio::test]
async fn test_reveals_serialize_with_actions() {
    let state = get_state().await;
    let (host, _guest, code) = room_of_two(&state).await;
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    let host_id = me(&host).await;

    // an in-flight action: holds room and game, marks the host ready
    let mut tx = state.db.begin().await.unwrap();
    sqlx::query("SELECT id FROM rooms WHERE code = $1 FOR UPDATE")
        .bind(&code)
        .execute(&mut *tx)
        .await
        .unwrap();
    let game: Value = sqlx::query_scalar(
        "SELECT g.state FROM games g JOIN rooms r ON r.id = g.room_id
         WHERE r.code = $1 AND g.status <> 'scored' FOR UPDATE",
    )
    .bind(&code)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let mut game: Game = serde_json::from_value(game).unwrap();
    let seat = game.seat_of(&host_id).unwrap();
    game.seats[seat].ready = true;

    let key = ClientKey::new();
    let pending = tokio::spawn({
        let host = host;
        let code = code.clone();
        async move { reveal(&host, &code, &key, "opening", None).await }
    });
    // the reveal is blocked on the row lock, not answered from the old state
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!pending.is_finished());

    sqlx::query(
        "UPDATE games g SET state = $1 FROM rooms r
         WHERE r.id = g.room_id AND r.code = $2 AND g.status <> 'scored'",
    )
    .bind(serde_json::to_value(&game).unwrap())
    .bind(&code)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    pending.await.unwrap().assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_reveal_requests_are_validated() {
    let state = get_state().await;
    let (host, _guest, code) = room_of_two(&state).await;
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    let key = ClientKey::new();
    for body in [
        json!({ "client_key": "not a key", "what": "opening" }),
        json!({ "client_key": key.public_b64, "what": "everything" }),
        json!({ "client_key": key.public_b64, "what": "peek" }),
    ] {
        let resp = host
            .post(&format!("/api/rooms/{code}/reveal"))
            .json(&body)
            .await;
        resp.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(resp.json::<Value>()["code"], "invalid");
    }
    reveal(&host, "ZZZZZZ", &key, "opening", None)
        .await
        .assert_status(StatusCode::NOT_FOUND);
    // a sealed reveal opens only for its room
    let resp = reveal(&host, &code, &key, "opening", None).await;
    let body: Value = resp.json();
    let sealed = komino::sealed::Sealed {
        kid: body["kid"].as_str().unwrap().into(),
        salt: body["salt"].as_str().unwrap().into(),
        iv: body["iv"].as_str().unwrap().into(),
        ct: body["ct"].as_str().unwrap().into(),
    };
    assert!(komino::sealed::open(
        &key.secret,
        &state.server_key.public_key,
        b"ZZZZZZ",
        &sealed
    )
    .is_err());
}
