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

/// The next json message on a socket, failing if none arrives in 5s. The
/// server's latency pings (RT-18) are skipped; the client answers them.
async fn next(socket: &mut axum_test::TestWebSocket) -> Value {
    use axum_test::WsMessage;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match socket.receive_message().await {
                WsMessage::Text(text) => return serde_json::from_str(&text).unwrap(),
                WsMessage::Ping(_) | WsMessage::Pong(_) => {}
                other => panic!("unexpected socket message {other:?}"),
            }
        }
    })
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

/// The running game's state.
async fn game_state(state: &State, code: &str) -> Game {
    let row: Value = sqlx::query_scalar(
        "SELECT g.state FROM games g JOIN rooms r ON r.id = g.room_id
         WHERE r.code = $1 ORDER BY g.id DESC LIMIT 1",
    )
    .bind(code)
    .fetch_one(&state.db)
    .await
    .unwrap();
    serde_json::from_value(row).unwrap()
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
    assert_eq!(cards["cards"][0]["slot"], 0);
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
    // events come newest first, with ids that only grow
    let ids: Vec<i64> = view["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_i64().unwrap())
        .collect();
    assert!(ids.windows(2).all(|w| w[0] > w[1]));
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

/// A match that reacted faster wins even when it arrives second (RT-17): an
/// untimed http match counts from when the discard landed, long ago, while
/// the socket's counts from when its view carried the discard.
#[tokio::test]
async fn test_a_faster_reaction_beats_an_earlier_arrival() {
    let state = get_state().await;
    let host = ws_client(&state);
    let guest = client(&state);
    let code = create_room(&host).await;
    guest
        .post(&format!("/api/rooms/{code}/join"))
        .await
        .assert_status_ok();
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
        g.discard_at = Some(komino::models::now_ms() - 10_000);
        // the window the first claim joins: long enough that the socket's
        // claim always lands in it, however slow the machine
        g.claim_deadline = Some(komino::models::now_ms() + 2_000);
        g.seats[0].slots = vec![Some(5), Some(1), Some(1), Some(1)];
        g.seats[1].slots = vec![Some(5), Some(2), Some(2), Some(2)];
    })
    .await;
    let mut socket = host
        .get_websocket(&format!("/r/{code}/ws"))
        .await
        .into_websocket()
        .await;
    let view = next(&mut socket).await;
    assert_eq!(view["view"]["game"]["discard_seq"], 3);

    let m = |seat: usize| json!({ "type": "match", "seq": 3, "seat": seat, "slot": 0 });
    let socket_match = async {
        // the http match claims first
        for _ in 0..100 {
            if !game_state(&state, &code).await.claims.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let mut msg = m(0);
        msg["ref"] = json!(7);
        msg["reaction_ms"] = json!(400);
        socket.send_json(&msg).await;
        next_matching(&mut socket, |m| m["type"] == "result").await
    };
    let (http, result) = tokio::join!(act(&guest, &code, m(1)), socket_match);
    http.assert_status(StatusCode::CONFLICT);
    assert_eq!(http.json::<Value>()["code"], "too_late");
    assert_eq!(result["ref"], 7);
    assert_eq!(result["ok"], true);

    let g = game_state(&state, &code).await;
    assert_eq!(g.seats[0].slots[0], None);
    assert_eq!(g.seats[1].slots[0], Some(5));
    assert!(g.claims.is_empty());
}

/// A claim whose waiter is gone still settles on the next timer pass.
#[tokio::test]
async fn test_tick_settles_an_abandoned_claim() {
    let state = get_state().await;
    let (host, _guest, code) = room_of_two(&state).await;
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    let host_id = me(&host).await;
    set_game(&state, &code, |g| {
        g.status = Status::Playing;
        g.ready_deadline = None;
        g.discard = vec![5];
        g.discard_seq = 3;
        g.matchable = true;
        let seat = g.seat_of(&host_id).unwrap();
        g.seats[seat].slots[0] = Some(5);
        g.claims = vec![komino::game::Claim {
            id: "abandoned".into(),
            player: host_id.clone(),
            seq: 3,
            seat,
            slot: 0,
            give_slot: None,
            reaction_ms: 100,
            at: 0,
        }];
        g.claim_deadline = Some(0);
    })
    .await;
    komino::models::tick_all(&state.db).await.unwrap();
    let g = game_state(&state, &code).await;
    assert_eq!(g.claim_result("abandoned"), Some(Ok(())));
    assert_eq!(g.discard_seq, 4);
    let view: Value = host.get(&format!("/api/rooms/{code}")).await.json();
    assert_eq!(view["events"][0]["kind"], "match");
    assert_eq!(view["events"][0]["payload"]["ok"], true);
}

/// A card a matcher never chose to give is given for them on the timer
/// pass after its deadline (RULE-19).
#[tokio::test]
async fn test_tick_gives_an_owed_card_once_due() {
    let state = get_state().await;
    let (host, _guest, code) = room_of_two(&state).await;
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    let host_id = me(&host).await;
    set_game(&state, &code, |g| {
        g.status = Status::Playing;
        g.ready_deadline = None;
        let mine = g.seat_of(&host_id).unwrap();
        g.seats[mine].slots = vec![None, Some(3), Some(9), Some(4)];
        g.seats[1 - mine].slots[2] = None;
        g.owed = vec![komino::game::Owed {
            player: host_id.clone(),
            seat: 1 - mine,
            slot: 2,
            deadline: 0,
        }];
    })
    .await;
    komino::models::tick_all(&state.db).await.unwrap();
    let g = game_state(&state, &code).await;
    let mine = g.seat_of(&host_id).unwrap();
    let theirs = 1 - mine;
    assert!(g.owed.is_empty());
    // the highest card goes, not the lowest slot
    assert_eq!(g.seats[mine].slots[2], None);
    assert_eq!(g.seats[theirs].slots[2], Some(9));
    let view: Value = host.get(&format!("/api/rooms/{code}")).await.json();
    assert_eq!(view["events"][0]["kind"], "give");
}

/// The game row's version, which every save bumps.
async fn game_version(state: &State, code: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT g.version FROM games g JOIN rooms r ON r.id = g.room_id
         WHERE r.code = $1 ORDER BY g.id DESC LIMIT 1",
    )
    .bind(code)
    .fetch_one(&state.db)
    .await
    .unwrap()
}

/// The sweep reads every unfinished game but locks and saves only those with
/// something due (RT-14).
#[tokio::test]
async fn test_tick_saves_only_games_with_something_due() {
    let state = get_state().await;
    let (host, _guest, code) = room_of_two(&state).await;
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    // a peek phase whose deadline is far off: nothing to do
    let before = game_version(&state, &code).await;
    komino::models::tick_all(&state.db).await.unwrap();
    assert_eq!(game_version(&state, &code).await, before);

    set_game(&state, &code, |g| g.ready_deadline = Some(0)).await;
    komino::models::tick_all(&state.db).await.unwrap();
    assert_eq!(game_version(&state, &code).await, before + 1);
    assert_eq!(game_state(&state, &code).await.status, Status::Playing);
}

/// After a ping, a room's sockets on one machine share one snapshot load;
/// the next ping starts a fresh one (RT-21).
#[tokio::test]
async fn test_sockets_share_one_snapshot_per_change() {
    let state = get_state().await;
    let (host, _guest, code) = room_of_two(&state).await;
    let room = komino::models::room_by_code(&state.db, &code)
        .await
        .unwrap();
    let _rx = state.hub.subscribe(room.id);
    // let the listener relay the join's notify first, so it can't land
    // between the two loads below
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    state.hub.ping(room.id);
    let a = state.hub.snapshot(&state.db, room.id).await.unwrap();
    let b = state.hub.snapshot(&state.db, room.id).await.unwrap();
    assert!(std::sync::Arc::ptr_eq(&a, &b));

    // a change after the load is not in it, until the next ping
    host.post("/api/me")
        .json(&json!({ "name": "renamed" }))
        .await
        .assert_status_ok();
    let stale = a.render(None, "");
    assert!(stale["members"]
        .as_array()
        .unwrap()
        .iter()
        .all(|m| m["name"] != "renamed"));
    state.hub.ping(room.id);
    let c = state.hub.snapshot(&state.db, room.id).await.unwrap();
    assert!(!std::sync::Arc::ptr_eq(&a, &c));
    let fresh = c.render(None, "");
    assert!(fresh["members"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["name"] == "renamed"));

    // membership checks come from the same load
    let host_id = me(&host).await;
    assert!(c.require_member(&host_id).is_ok());
    assert_eq!(c.require_member("nobody").unwrap_err().code, "forbidden");
    assert!(c.require_not_removed("nobody").is_ok());
}

/// Member sockets ping to measure their round trip (RT-18).
#[tokio::test]
async fn test_member_sockets_ping() {
    use axum_test::WsMessage;
    let state = get_state().await;
    let host = ws_client(&state);
    let code = create_room(&host).await;
    let mut socket = host
        .get_websocket(&format!("/r/{code}/ws"))
        .await
        .into_websocket()
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let WsMessage::Ping(payload) = socket.receive_message().await {
                assert_eq!(payload.len(), 8);
                return;
            }
        }
    })
    .await
    .expect("no ping");
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

#[tokio::test]
async fn test_joining_a_room_its_host_left_empty_takes_the_host_role() {
    let state = get_state().await;
    let host = client(&state);
    let code = create_room(&host).await;
    host.post(&format!("/api/rooms/{code}/leave"))
        .await
        .assert_status_ok();
    let guest = client(&state);
    guest
        .post(&format!("/api/rooms/{code}/join"))
        .await
        .assert_status_ok();
    let view: Value = guest.get(&format!("/api/rooms/{code}")).await.json();
    assert_eq!(view["room"]["host"], me(&guest).await);

    // the old host comes back as a plain member
    host.post(&format!("/api/rooms/{code}/join"))
        .await
        .assert_status_ok();
    let view: Value = host.get(&format!("/api/rooms/{code}")).await.json();
    assert_eq!(view["room"]["host"], me(&guest).await);
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
    let first = next(&mut socket).await;
    assert_eq!(first["type"], "view");
    assert_eq!(first["view"]["room"]["code"], code);

    socket
        .send_json(&json!({ "type": "start", "ref": 1 }))
        .await;
    let mut saw_result = false;
    let mut saw_game = false;
    for _ in 0..6 {
        let msg = next(&mut socket).await;
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
        let msg = next(&mut socket).await;
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
        let msg = next(&mut socket).await;
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
    for path in ["/r/ABCDEF", "/r/ABCDEF/watch", "/account"] {
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
        json!({
            "hand_size": 4, "away_grace_secs": 30, "turn_limit_secs": null, "reveal_secs": 15,
            "show_misses": true, "target_score": null, "caller_penalty": 0, "exact_reset": false,
            "memory_marks": false
        })
    );
    assert_eq!(view["room"]["history"], json!([]));
}

/// The host adds bots, which sit in the next game and play it from the
/// sweep; a removed bot just leaves, and bots go with their room (BOT-*).
#[tokio::test]
async fn test_bots_join_play_and_leave() {
    let state = get_state().await;
    let (host, guest, code) = room_of_two(&state).await;
    let resp = guest.post(&format!("/api/rooms/{code}/bots")).await;
    resp.assert_status(StatusCode::FORBIDDEN);
    let view: Value = host.post(&format!("/api/rooms/{code}/bots")).await.json();
    let bot = view["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["bot"] == true)
        .unwrap()
        .clone();
    assert_eq!(bot["name"], "bot ava");
    assert_eq!(bot["present"], true);
    // no body plays at normal; a level must be one of the three (BOT-5)
    assert_eq!(bot["bot_level"], "normal");
    host.post(&format!("/api/rooms/{code}/bots"))
        .json(&json!({ "level": "expert" }))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    let view: Value = host
        .post(&format!("/api/rooms/{code}/bots"))
        .json(&json!({ "level": "hard" }))
        .await
        .json();
    let hard = view["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "bot ben")
        .unwrap();
    assert_eq!(hard["bot_level"], "hard");
    // people carry no level
    assert_eq!(view["members"][0]["bot_level"], Value::Null);
    let names: Vec<&str> = view["members"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["bot"] == true)
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["bot ava", "bot ben"]);
    let bot_id = bot["id"].as_str().unwrap().to_string();

    let view: Value = act(&host, &code, json!({ "type": "start" })).await.json();
    let seats = view["game"]["seats"].as_array().unwrap();
    assert_eq!(seats.len(), 4);
    assert_eq!(
        seats.iter().filter(|s| s["bot"] == true).count(),
        2,
        "both bots are seated"
    );
    // the sweep readies the bots
    komino::models::tick_all(&state.db).await.unwrap();
    let g = game_state(&state, &code).await;
    let bot_seat = g.seat_of(&bot_id).unwrap();
    assert!(g.seats[bot_seat].ready);
    let levels: Vec<_> = g.minds.iter().map(|m| m.level).collect();
    assert_eq!(
        levels,
        [komino::bot::Level::Normal, komino::bot::Level::Hard]
    );
    // and plays the bot's turn once its clock has run
    set_game(&state, &code, |g| {
        g.status = Status::Playing;
        g.ready_deadline = None;
        g.turn = bot_seat;
        g.stage = Stage::Start;
        g.bot_clock = Some((g.turn_seq, 0));
    })
    .await;
    komino::models::tick_all(&state.db).await.unwrap();
    let view: Value = host.get(&format!("/api/rooms/{code}")).await.json();
    let first = &view["events"][0];
    assert_eq!(first["player"], bot_id.as_str());
    assert!(["draw", "take"].contains(&first["kind"].as_str().unwrap()));

    // removing a bot forfeits it and leaves nothing to unban
    host.post(&format!("/api/rooms/{code}/remove"))
        .json(&json!({ "player": bot_id }))
        .await
        .assert_status_ok();
    let view: Value = host.get(&format!("/api/rooms/{code}")).await.json();
    let gone = view["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == bot_id.as_str())
        .unwrap();
    assert_eq!(
        (gone["left"].clone(), gone["removed"].clone()),
        (json!(true), json!(false))
    );
    assert_eq!(view["game"]["seats"][bot_seat]["forfeited"], true);

    // a leaving host hands the room to a person, never a bot
    host.post(&format!("/api/rooms/{code}/leave"))
        .await
        .assert_status_ok();
    let view: Value = guest.get(&format!("/api/rooms/{code}")).await.json();
    assert_eq!(view["room"]["host"], me(&guest).await.as_str());

    // a deleted room takes its bots with it
    sqlx::query("UPDATE rooms SET last_active = now() - interval '31 days' WHERE code = $1")
        .bind(&code)
        .execute(&state.db)
        .await
        .unwrap();
    komino::models::delete_stale_rooms(&state.db).await.unwrap();
    let bots: i64 = sqlx::query_scalar("SELECT count(*) FROM players WHERE bot")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(bots, 0);
}

#[tokio::test]
async fn test_a_room_holds_at_most_seven_bots() {
    let state = get_state().await;
    let host = client(&state);
    let code = create_room(&host).await;
    for _ in 0..7 {
        host.post(&format!("/api/rooms/{code}/bots"))
            .await
            .assert_status_ok();
    }
    let resp = host.post(&format!("/api/rooms/{code}/bots")).await;
    resp.assert_status(StatusCode::BAD_REQUEST);
    // with the host, a full table: the bots never leave it alone
    let view: Value = act(&host, &code, json!({ "type": "start" })).await.json();
    assert_eq!(view["game"]["seats"].as_array().unwrap().len(), 8);
}

/// Score the room's running game now, with these hands by seat.
async fn score_with(state: &State, code: &str, hands: Vec<[i8; 4]>) {
    set_game(state, code, |g| {
        g.status = Status::Scoring;
        g.ready_deadline = None;
        g.score_at = Some(0);
        for (seat, hand) in g.seats.iter_mut().zip(hands) {
            seat.slots = hand.iter().map(|&v| Some(v)).collect();
        }
    })
    .await;
    komino::models::tick_all(&state.db).await.unwrap();
}

/// Running totals carry from game to game until one reaches the target,
/// and a player joining mid match starts level with the highest (SET-14).
#[tokio::test]
async fn test_a_match_carries_totals_until_the_target() {
    let state = get_state().await;
    let host = client(&state);
    let guest = client(&state);
    let late = client(&state);
    let resp = host
        .post("/api/rooms")
        .json(&json!({ "target_score": 30 }))
        .await;
    resp.assert_status_ok();
    let code = resp.json::<Value>()["room"]["code"]
        .as_str()
        .unwrap()
        .to_string();
    guest
        .post(&format!("/api/rooms/{code}/join"))
        .await
        .assert_status_ok();
    let (h, g) = (me(&host).await, me(&guest).await);
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    assert_eq!(game_state(&state, &code).await.match_no, 1);
    score_with(&state, &code, vec![[5; 4], [1; 4]]).await;
    let view: Value = host.get(&format!("/api/rooms/{code}")).await.json();
    assert_eq!(
        view["room"]["history"],
        json!([{ "m": 1, "over": false, "totals": { &h: 20, &g: 4 }, "scores": { &h: 20, &g: 4 } }])
    );

    late.post(&format!("/api/rooms/{code}/join"))
        .await
        .assert_status_ok();
    let l = me(&late).await;
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    let game = game_state(&state, &code).await;
    assert_eq!(game.match_no, 1);
    let carry: Vec<(String, i32)> = game
        .seats
        .iter()
        .map(|s| (s.player.clone(), s.carry))
        .collect();
    assert_eq!(carry, [(h.clone(), 20), (g.clone(), 4), (l.clone(), 20)]);
    // the host passes 30, so the lowest total takes the match
    score_with(&state, &code, vec![[3; 4], [0; 4], [1; 4]]).await;
    let view: Value = host.get(&format!("/api/rooms/{code}")).await.json();
    let history = view["room"]["history"].as_array().unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[1]["over"], true);
    assert_eq!(history[1]["totals"], json!({ &h: 32, &g: 4, &l: 24 }));
    assert_eq!(view["events"][0]["payload"]["match_winners"], json!([&g]));
    assert_eq!(view["game"]["match_over"], true);

    // the next game starts a new match from zero
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    let game = game_state(&state, &code).await;
    assert_eq!(game.match_no, 2);
    assert!(game.seats.iter().all(|s| s.carry == 0));
    score_with(&state, &code, vec![[2; 4], [2; 4], [2; 4]]).await;
    let view: Value = host.get(&format!("/api/rooms/{code}")).await.json();
    // the view shows only the newest match
    let history = view["room"]["history"].as_array().unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0]["m"], 2);
}

/// A miss carries the targeted card's value to every viewer only in a room
/// that shows misses (SET-13).
#[tokio::test]
async fn test_a_miss_shows_its_card_when_the_room_says_so() {
    let state = get_state().await;
    for show_misses in [true, false] {
        let host = client(&state);
        let guest = client(&state);
        let resp = host
            .post("/api/rooms")
            .json(&json!({ "show_misses": show_misses }))
            .await;
        resp.assert_status_ok();
        let code = resp.json::<Value>()["room"]["code"]
            .as_str()
            .unwrap()
            .to_string();
        guest
            .post(&format!("/api/rooms/{code}/join"))
            .await
            .assert_status_ok();
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
            g.seats[0].slots = vec![Some(7), Some(1), Some(1), Some(1)];
            g.seats[1].slots = vec![Some(9), Some(2), Some(2), Some(2)];
        })
        .await;
        act(
            &guest,
            &code,
            json!({ "type": "match", "seq": 3, "seat": 1, "slot": 0 }),
        )
        .await
        .assert_status_ok();
        let view: Value = host.get(&format!("/api/rooms/{code}")).await.json();
        let e = &view["events"][0];
        assert_eq!(e["kind"], "match");
        assert_eq!(e["payload"]["ok"], false);
        let shown = if show_misses { json!(9) } else { Value::Null };
        assert_eq!(e["payload"]["value"], shown, "show_misses {show_misses}");
        // the missed card stays face down in the hand
        assert!(view["game"]["seats"][1]["slots"][0].get("v").is_none());
    }
}

#[tokio::test]
async fn test_room_settings_shape_the_game() {
    let state = get_state().await;
    let host = client(&state);
    let guest = client(&state);
    let settings = json!({
        "hand_size": 9, "away_grace_secs": 60, "turn_limit_secs": 90, "reveal_secs": 10,
        "show_misses": false, "target_score": 100, "caller_penalty": 10, "exact_reset": true,
        "memory_marks": true
    });
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
    // the near row is slots 1-4
    assert_eq!(slots, vec![0, 1, 2, 3]);

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
        json!({ "show_misses": null }),
        json!({ "target_score": 10 }),
        json!({ "caller_penalty": 60 }),
        json!({ "memory_marks": "yes" }),
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

// ---------------------------------------------------------------------------
// Accounts
// ---------------------------------------------------------------------------

async fn signup(server: &TestServer, username: &str, password: &str) -> axum_test::TestResponse {
    server
        .post("/api/signup")
        .json(&json!({ "username": username, "password": password }))
        .await
}

async fn login(server: &TestServer, username: &str, password: &str) -> axum_test::TestResponse {
    server
        .post("/api/login")
        .json(&json!({ "username": username, "password": password }))
        .await
}

/// A member's name as `viewer` sees the room.
async fn member_name(viewer: &TestServer, code: &str, id: &str) -> String {
    let view: Value = viewer.get(&format!("/api/rooms/{code}")).await.json();
    view["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == id)
        .unwrap()["name"]
        .as_str()
        .unwrap()
        .to_string()
}

/// A browser with no cookies but the one given.
fn bare(state: &State) -> TestServer {
    TestServer::new(service::router(state.clone()).with_state(state.clone()))
}

#[tokio::test]
async fn test_signup_keeps_the_player_and_its_rooms() {
    let state = get_state().await;
    let (host, guest, code) = room_of_two(&state).await;
    let id = me(&host).await;
    let resp = signup(&host, "  Ada_L ", "correct horse").await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["id"], id.as_str());
    assert_eq!(body["username"], "ada_l");
    // the username is the default name
    assert_eq!(body["name"], "ada_l");
    let session = resp.cookie("komino_session");
    assert_eq!(session.value().len(), 64);
    assert_eq!(session.http_only(), Some(true));
    assert_eq!(session.secure(), Some(true));
    assert_eq!(session.path(), Some("/komino"));
    // the anonymous cookie is dropped
    assert_eq!(resp.cookie("komino_player").value(), "");
    // still the same player, in the same room, renamed for everyone
    assert_eq!(me(&host).await, id);
    host.get(&format!("/api/rooms/{code}"))
        .await
        .assert_status_ok();
    assert_eq!(member_name(&guest, &code, &id).await, "ada_l");
    // only a hash of the token is stored
    let stored: String = sqlx::query_scalar("SELECT token_hash FROM sessions")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_ne!(stored, session.value());
    assert_eq!(stored, komino::accounts::token_hash(session.value()));
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM players WHERE id = $1")
        .bind(&id)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert!(hash.starts_with("pbkdf2-sha512$"));
    assert!(!hash.contains("correct horse"));
}

#[tokio::test]
async fn test_signup_validates() {
    let state = get_state().await;
    let server = client(&state);
    let resp = server
        .post("/api/signup")
        .json(&json!({ "username": "grace", "password": "hunter22", "name": "  Grace H " }))
        .await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>()["name"], "Grace H");
    // signed in, there is no second account
    signup(&server, "grace2", "hunter22")
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    let other = client(&state);
    for (u, p) in [
        ("gr", "hunter22"),
        ("grace hopper", "hunter22"),
        ("gracie", "short"),
        ("gracie", &"x".repeat(257)),
    ] {
        let resp = signup(&other, u, p).await;
        resp.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(resp.json::<Value>()["code"], "invalid");
    }
    other
        .post("/api/signup")
        .json(&json!({ "username": "gracie", "password": "hunter22", "name": "a\u{7}b" }))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    // usernames are case-insensitive
    let resp = signup(&other, "GRACE", "hunter22").await;
    resp.assert_status(StatusCode::CONFLICT);
    assert_eq!(resp.json::<Value>()["code"], "taken");
    // nothing failed changed the player
    let body: Value = other.get("/api/me").await.json();
    assert_eq!(body["username"], Value::Null);
}

#[tokio::test]
async fn test_the_player_cookie_cannot_reach_an_account() {
    let state = get_state().await;
    let (host, _guest, code) = room_of_two(&state).await;
    let id = me(&host).await;
    // the signed player cookie, as it was before signup
    let signed = komino::handlers::sign_player(&id, &state.config.signing_key);
    signup(&host, "ada", "correct horse")
        .await
        .assert_status_ok();
    let resp = bare(&state)
        .get("/api/me")
        .add_cookie(axum_extra::extract::cookie::Cookie::new(
            "komino_player",
            signed.clone(),
        ))
        .await;
    assert_ne!(resp.json::<Value>()["id"], id.as_str());
    // nor can it open the account's sockets
    let sock = ws_client(&state);
    sock.get_websocket(&format!("/r/{code}/ws"))
        .add_cookie(axum_extra::extract::cookie::Cookie::new(
            "komino_player",
            signed,
        ))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    // the session can
    let sock = ws_client(&state);
    login(&sock, "ada", "correct horse")
        .await
        .assert_status_ok();
    let mut socket = sock
        .get_websocket(&format!("/r/{code}/ws"))
        .await
        .into_websocket()
        .await;
    let msg = next(&mut socket).await;
    assert_eq!(msg["type"], "view");
    assert_eq!(msg["view"]["me"], id.as_str());
}

#[tokio::test]
async fn test_login_reaches_the_account_from_another_browser() {
    let state = get_state().await;
    let (host, _guest, code) = room_of_two(&state).await;
    let id = me(&host).await;
    signup(&host, "ada", "correct horse")
        .await
        .assert_status_ok();
    let laptop = client(&state);
    let anon = me(&laptop).await;
    assert_ne!(anon, id);
    let resp = login(&laptop, " ADA ", "correct horse").await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>()["id"], id.as_str());
    assert_eq!(me(&laptop).await, id);
    laptop
        .get(&format!("/api/rooms/{code}"))
        .await
        .assert_status_ok();
    // signing out returns this browser to its own anonymous player
    laptop.post("/api/logout").await.assert_status_ok();
    assert_eq!(me(&laptop).await, anon);
    // the first browser is still signed in
    assert_eq!(me(&host).await, id);
}

#[tokio::test]
async fn test_wrong_logins_fail_alike_and_lock_the_account() {
    let state = get_state().await;
    let owner = client(&state);
    signup(&owner, "ada", "correct horse")
        .await
        .assert_status_ok();
    let owner_id = me(&owner).await;
    let other = client(&state);
    for (u, p) in [("ada", "wrong horse"), ("nobody", "correct horse")] {
        let resp = login(&other, u, p).await;
        resp.assert_status(StatusCode::UNAUTHORIZED);
        assert_eq!(
            resp.json::<Value>()["message"],
            "wrong username or password"
        );
        assert!(resp.maybe_cookie("komino_session").is_none());
    }
    // a success resets the count
    login(&other, "ada", "correct horse")
        .await
        .assert_status_ok();
    let other = client(&state);
    for _ in 0..komino::accounts::MAX_FAILED_LOGINS {
        login(&other, "ada", "wrong horse")
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }
    // locked, even with the right password
    let resp = login(&other, "ada", "correct horse").await;
    resp.assert_status(StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(resp.json::<Value>()["code"], "locked");
    // until the lock runs out
    sqlx::query("UPDATE players SET locked_until = now() - interval '1 second'")
        .execute(&state.db)
        .await
        .unwrap();
    login(&other, "ada", "correct horse")
        .await
        .assert_status_ok();
    // sessions already open were untouched by the lock
    assert_eq!(me(&owner).await, owner_id);
}

#[tokio::test]
async fn test_logout_ends_the_session_for_good() {
    let state = get_state().await;
    let server = client(&state);
    let resp = signup(&server, "ada", "correct horse").await;
    let token = resp.cookie("komino_session").value().to_string();
    let id = resp.json::<Value>()["id"].as_str().unwrap().to_string();
    let replay = || {
        bare(&state)
            .get("/api/me")
            .add_cookie(axum_extra::extract::cookie::Cookie::new(
                "komino_session",
                token.clone(),
            ))
    };
    assert_eq!(replay().await.json::<Value>()["id"], id.as_str());
    let resp = server.post("/api/logout").await;
    resp.assert_status_ok();
    assert_eq!(resp.cookie("komino_session").value(), "");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
    // a copy of the old token reaches nobody, and is told to drop it
    let resp = replay().await;
    assert_ne!(resp.json::<Value>()["id"], id.as_str());
    assert_eq!(resp.cookie("komino_session").value(), "");
    // signing out without a session is fine
    bare(&state).post("/api/logout").await.assert_status_ok();
}

#[tokio::test]
async fn test_sessions_expire_and_extend() {
    let state = get_state().await;
    let server = client(&state);
    signup(&server, "ada", "correct horse")
        .await
        .assert_status_ok();
    let id = me(&server).await;
    // a fresh session is not sent again
    assert!(server
        .get("/api/me")
        .await
        .maybe_cookie("komino_session")
        .is_none());
    // a day old, it is extended and its cookie sent again
    sqlx::query("UPDATE sessions SET expires = now() + interval '28 days'")
        .execute(&state.db)
        .await
        .unwrap();
    let resp = server.get("/api/me").await;
    assert_eq!(resp.json::<Value>()["id"], id.as_str());
    assert!(resp.maybe_cookie("komino_session").is_some());
    let left: f64 = sqlx::query_scalar(
        "SELECT extract(epoch FROM expires - now())::float8 / 86400 FROM sessions",
    )
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert!(left > 29.9, "{left}");
    // an expired session reaches nobody and is swept
    sqlx::query("UPDATE sessions SET expires = now() - interval '1 second'")
        .execute(&state.db)
        .await
        .unwrap();
    assert_ne!(me(&server).await, id);
    komino::models::delete_stale_rooms(&state.db).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn test_an_account_default_name_falls_back_to_its_username() {
    let state = get_state().await;
    let server = client(&state);
    signup(&server, "ada", "correct horse")
        .await
        .assert_status_ok();
    let resp = server
        .post("/api/me")
        .json(&json!({ "name": "Ada L" }))
        .await;
    assert_eq!(resp.json::<Value>()["name"], "Ada L");
    let resp = server.post("/api/me").json(&json!({ "name": "  " })).await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>()["name"], "ada");
    assert_eq!(resp.json::<Value>()["username"], "ada");
}

#[tokio::test]
async fn test_a_room_name_overrides_the_default_in_that_room_only() {
    let state = get_state().await;
    let (host, guest, code) = room_of_two(&state).await;
    let other = create_room(&host).await;
    let id = me(&host).await;
    host.post("/api/me")
        .json(&json!({ "name": "ada" }))
        .await
        .assert_status_ok();
    let resp = host
        .post(&format!("/api/rooms/{code}/name"))
        .json(&json!({ "name": "  the countess " }))
        .await;
    resp.assert_status_ok();
    assert!(resp.json::<Value>()["members"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["id"] == id.as_str() && m["name"] == "the countess"));
    assert_eq!(member_name(&guest, &code, &id).await, "the countess");
    assert_eq!(member_name(&host, &other, &id).await, "ada");
    // a new default leaves the room name alone
    host.post("/api/me")
        .json(&json!({ "name": "lovelace" }))
        .await
        .assert_status_ok();
    assert_eq!(member_name(&guest, &code, &id).await, "the countess");
    assert_eq!(member_name(&host, &other, &id).await, "lovelace");
    // a blank room name shows the default again
    host.post(&format!("/api/rooms/{code}/name"))
        .json(&json!({ "name": "" }))
        .await
        .assert_status_ok();
    assert_eq!(member_name(&guest, &code, &id).await, "lovelace");
    // validated like any name, and only for members
    host.post(&format!("/api/rooms/{code}/name"))
        .json(&json!({ "name": "x".repeat(25) }))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    let stranger = client(&state);
    stranger
        .post(&format!("/api/rooms/{code}/name"))
        .json(&json!({ "name": "spy" }))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    host.post(&format!("/api/rooms/{code}/leave"))
        .await
        .assert_status_ok();
    host.post(&format!("/api/rooms/{code}/name"))
        .json(&json!({ "name": "back" }))
        .await
        .assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_stats_show_the_room_name() {
    let state = get_state().await;
    let (host, guest, code) = room_of_two(&state).await;
    let id = me(&host).await;
    host.post(&format!("/api/rooms/{code}/name"))
        .json(&json!({ "name": "the countess" }))
        .await
        .assert_status_ok();
    sqlx::query(
        "INSERT INTO room_stats (room_id, player_id, wins)
         SELECT r.id, $2, 1 FROM rooms r WHERE r.code = $1",
    )
    .bind(&code)
    .bind(&id)
    .execute(&state.db)
    .await
    .unwrap();
    let view: Value = guest.get(&format!("/api/rooms/{code}")).await.json();
    assert_eq!(view["stats"][0]["player"], id.as_str());
    assert_eq!(view["stats"][0]["name"], "the countess");
}

async fn change_password(server: &TestServer, current: &str, new: &str) -> axum_test::TestResponse {
    server
        .post("/api/account/password")
        .json(&json!({ "current": current, "new": new }))
        .await
}

async fn session_count(state: &State) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(&state.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn test_changing_the_password_ends_every_other_session() {
    let state = get_state().await;
    let server = client(&state);
    let resp = signup(&server, "ada", "correct horse").await;
    let old = resp.cookie("komino_session").value().to_string();
    let id = me(&server).await;
    let laptop = client(&state);
    login(&laptop, "ada", "correct horse")
        .await
        .assert_status_ok();
    // only an account has a password to change
    let resp = change_password(&client(&state), "correct horse", "battery staple").await;
    resp.assert_status(StatusCode::UNAUTHORIZED);
    assert_eq!(resp.json::<Value>()["message"], "sign in first");
    let resp = change_password(&server, "wrong horse", "battery staple").await;
    resp.assert_status(StatusCode::UNAUTHORIZED);
    assert_eq!(resp.json::<Value>()["message"], "wrong password");
    change_password(&server, "correct horse", "short")
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    // nothing failed ended a session
    assert_eq!(me(&laptop).await, id);

    let resp = change_password(&server, "correct horse", "battery staple").await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>()["id"], id.as_str());
    let new = resp.cookie("komino_session").value().to_string();
    assert_eq!(new.len(), 64);
    assert_ne!(new, old);
    // this browser stays signed in on its new session; the other is out
    assert_eq!(me(&server).await, id);
    assert_ne!(me(&laptop).await, id);
    assert_eq!(session_count(&state).await, 1);
    let replay = bare(&state)
        .get("/api/me")
        .add_cookie(axum_extra::extract::cookie::Cookie::new(
            "komino_session",
            old,
        ))
        .await;
    assert_ne!(replay.json::<Value>()["id"], id.as_str());
    login(&laptop, "ada", "correct horse")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    login(&laptop, "ada", "battery staple")
        .await
        .assert_status_ok();
}

#[tokio::test]
async fn test_wrong_current_passwords_count_toward_the_lock() {
    let state = get_state().await;
    let server = client(&state);
    signup(&server, "ada", "correct horse")
        .await
        .assert_status_ok();
    for _ in 0..komino::accounts::MAX_FAILED_LOGINS {
        change_password(&server, "wrong horse", "battery staple")
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }
    let resp = change_password(&server, "correct horse", "battery staple").await;
    resp.assert_status(StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(resp.json::<Value>()["code"], "locked");
    server
        .post("/api/account/delete")
        .json(&json!({ "password": "correct horse" }))
        .await
        .assert_status(StatusCode::TOO_MANY_REQUESTS);
    // the session itself still works
    let body: Value = server.get("/api/me").await.json();
    assert_eq!(body["username"], "ada");
}

async fn host_of(state: &State, code: &str) -> Option<String> {
    sqlx::query_scalar("SELECT host_player_id FROM rooms WHERE code = $1")
        .bind(code)
        .fetch_optional(&state.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn test_deleting_an_account_leaves_every_room_and_frees_the_username() {
    let state = get_state().await;
    let (host, guest, code) = room_of_two(&state).await;
    signup(&host, "ada", "correct horse")
        .await
        .assert_status_ok();
    let id = me(&host).await;
    let guest_id = me(&guest).await;
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    // a room only the account was ever in
    let alone = create_room(&host).await;
    // a room everyone else has left
    let emptied = create_room(&host).await;
    let visitor = client(&state);
    visitor
        .post(&format!("/api/rooms/{emptied}/join"))
        .await
        .assert_status_ok();
    visitor
        .post(&format!("/api/rooms/{emptied}/leave"))
        .await
        .assert_status_ok();
    let visitor_id = me(&visitor).await;

    // only an account can be deleted, and only with its password
    client(&state)
        .post("/api/account/delete")
        .json(&json!({ "password": "correct horse" }))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    let resp = host
        .post("/api/account/delete")
        .json(&json!({ "password": "wrong horse" }))
        .await;
    resp.assert_status(StatusCode::UNAUTHORIZED);
    assert_eq!(resp.json::<Value>()["message"], "wrong password");
    assert_eq!(me(&host).await, id);

    let resp = host
        .post("/api/account/delete")
        .json(&json!({ "password": "correct horse" }))
        .await;
    resp.assert_status_ok();
    assert_eq!(resp.cookie("komino_session").value(), "");
    assert_eq!(session_count(&state).await, 0);
    // this browser is a stranger now
    assert_ne!(me(&host).await, id);
    // the running game was forfeited and the host role passed on
    let view: Value = guest.get(&format!("/api/rooms/{code}")).await.json();
    assert_eq!(view["room"]["host"], guest_id.as_str());
    assert_eq!(view["game"]["status"], "scored");
    assert!(!view["members"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["id"] == id.as_str()));
    assert!(!view["stats"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["player"] == id.as_str()));
    // a room with no one else is gone, and one whose members all left goes
    // to one of them until the next join
    assert_eq!(host_of(&state, &alone).await, None);
    assert_eq!(host_of(&state, &emptied).await, Some(visitor_id));
    let players: i64 = sqlx::query_scalar("SELECT count(*) FROM players WHERE id = $1")
        .bind(&id)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(players, 0);
    // the username is free again
    login(&client(&state), "ada", "correct horse")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    signup(&client(&state), "ada", "battery staple")
        .await
        .assert_status_ok();
}

// ---------------------------------------------------------------------------
// Lifetime stats
// ---------------------------------------------------------------------------

async fn lifetime(server: &TestServer) -> Value {
    let resp = server.get("/api/account/stats").await;
    resp.assert_status_ok();
    resp.json()
}

/// Partner names and game counts, as listed.
fn partners(stats: &Value) -> Vec<(String, i64)> {
    stats["partners"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["name"].as_str().unwrap().to_string(),
                p["games"].as_i64().unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn test_lifetime_stats_add_up_across_rooms_and_outlive_them() {
    let state = get_state().await;
    let (host, guest, code) = room_of_two(&state).await;
    signup(&host, "ada", "correct horse")
        .await
        .assert_status_ok();
    guest
        .post("/api/me")
        .json(&json!({ "name": "bob" }))
        .await
        .assert_status_ok();
    let fresh = lifetime(&host).await;
    assert_eq!(fresh["games_played"], 0);
    assert_eq!(
        fresh["match_time"],
        json!({ "timed": 0, "average_ms": null, "median_ms": null })
    );
    assert_eq!(fresh["partners"], json!([]));

    // ada matches bob's 5, which landed 2s ago, then draws and swaps
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    set_game(&state, &code, |g| {
        g.status = Status::Playing;
        g.stage = Stage::Start;
        g.ready_deadline = None;
        g.turn = 0;
        g.discard = vec![5];
        g.discard_seq = 3;
        g.matchable = true;
        g.discard_at = Some(komino::models::now_ms() - 2_000);
        g.seats[0].slots = vec![Some(1), Some(2), Some(3), Some(4)];
        g.seats[1].slots = vec![Some(5), Some(6), Some(7), Some(8)];
    })
    .await;
    act(
        &host,
        &code,
        json!({ "type": "match", "seq": 3, "seat": 1, "slot": 0, "give_slot": 1 }),
    )
    .await
    .assert_status_ok();
    let seq = game_state(&state, &code).await.turn_seq;
    act(&host, &code, json!({ "type": "draw", "turn_seq": seq }))
        .await
        .assert_status_ok();
    let seq = game_state(&state, &code).await.turn_seq;
    act(
        &host,
        &code,
        json!({ "type": "swap", "slot": 0, "turn_seq": seq }),
    )
    .await
    .assert_status_ok();
    score_with(&state, &code, vec![[1; 4], [9; 4]]).await;
    // the room keeps the new counters too
    let view: Value = host.get(&format!("/api/rooms/{code}")).await.json();
    let host_id = me(&host).await;
    let row = view["stats"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["player"] == host_id.as_str())
        .unwrap()
        .clone();
    assert_eq!(row["draws"], 1);
    assert_eq!(row["swaps"], 1);
    assert_eq!(row["others_matched"], 1);

    // a second room, with a third player and a bot
    let other = create_room(&host).await;
    let carol = client(&state);
    carol
        .post("/api/me")
        .json(&json!({ "name": "carol" }))
        .await
        .assert_status_ok();
    for c in [&guest, &carol] {
        c.post(&format!("/api/rooms/{other}/join"))
            .await
            .assert_status_ok();
    }
    host.post(&format!("/api/rooms/{other}/bots"))
        .await
        .assert_status_ok();
    act(&host, &other, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    score_with(&state, &other, vec![[1; 4], [9; 4], [9; 4], [9; 4]]).await;

    // the first room goes; the totals stay
    sqlx::query("DELETE FROM rooms WHERE code = $1")
        .bind(&code)
        .execute(&state.db)
        .await
        .unwrap();
    let ada = lifetime(&host).await;
    assert_eq!(ada["games_played"], 2);
    assert_eq!(ada["wins"], 2);
    assert_eq!(ada["draws"], 1);
    assert_eq!(ada["swaps"], 1);
    assert_eq!(ada["matches"], 1);
    assert_eq!(ada["others_matched"], 1);
    assert_eq!(ada["bot_games"], 1);
    assert_eq!(ada["match_time"]["timed"], 1);
    let average = ada["match_time"]["average_ms"].as_i64().unwrap();
    assert!((2_000..4_000).contains(&average), "{average}");
    // one sample: the median is the middle of its band
    assert_eq!(
        ada["match_time"]["median_ms"].as_i64().unwrap(),
        average / 50 * 50 + 25
    );
    // most played with first; no bots
    assert_eq!(
        partners(&ada),
        vec![("bob".to_string(), 2), ("carol".to_string(), 1)]
    );
    let bob = lifetime(&guest).await;
    assert_eq!(bob["games_played"], 2);
    assert_eq!(bob["wins"], 0);
    assert_eq!(bob["partners"][0]["username"], "ada");
    assert_eq!(bob["partners"][0]["games"], 2);
    let bot_rows: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM player_stats s JOIN players p ON p.id = s.player_id
                 WHERE p.bot)
              + (SELECT count(*) FROM player_partners pp JOIN players p
                     ON p.id IN (pp.player_id, pp.other_id) WHERE p.bot)",
    )
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(bot_rows, 0);

    // a deleted account leaves the others' partner lists
    host.post("/api/account/delete")
        .json(&json!({ "password": "correct horse" }))
        .await
        .assert_status_ok();
    assert_eq!(
        partners(&lifetime(&guest).await),
        vec![("carol".to_string(), 1)]
    );
    let left: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM player_stats WHERE player_id = $1)
              + (SELECT count(*) FROM player_match_times WHERE player_id = $1)",
    )
    .bind(&host_id)
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(left, 0);
}

/// The lifetime totals migration, run on rooms seeded here.
const BACKFILL_SQL: &str = include_str!(
    "../../../migrations/komino/migrations/20261012000001_player_stats_backfill/up.sql"
);

#[tokio::test]
async fn test_the_backfill_sums_each_players_room_counters() {
    let state = get_state().await;
    sqlx::raw_sql(
        "INSERT INTO players (id, name, bot) VALUES
             ('ann', 'ann', false), ('ben', 'ben', false), ('cat', 'cat', false),
             ('bot', 'bot', true);
         INSERT INTO rooms (id, code, host_player_id) VALUES (1, 'aaaaaa', 'ann'),
             (2, 'bbbbbb', 'ann');
         INSERT INTO room_stats (room_id, player_id, games_played, wins, komino_calls,
                                 komino_wins, matches, failed_matches, special_moves,
                                 cards_interacted, forfeits) VALUES
             (1, 'ann', 3, 2, 1, 1, 4, 1, 2, 9, 0),
             (2, 'ann', 2, 1, 1, 0, 1, 2, 3, 5, 1),
             (1, 'ben', 3, 1, 0, 0, 2, 0, 1, 6, 1),
             (1, 'bot', 3, 0, 0, 0, 5, 5, 5, 5, 0),
             (1, 'cat', 1, 1, 1, 1, 1, 1, 1, 1, 1);
         -- cat already has lifetime totals, which the backfill keeps
         INSERT INTO player_stats (player_id, games_played, draws) VALUES ('cat', 7, 4);",
    )
    .execute(&state.db)
    .await
    .unwrap();
    sqlx::raw_sql(BACKFILL_SQL)
        .execute(&state.db)
        .await
        .unwrap();
    let rows: Vec<Value> =
        sqlx::query_scalar("SELECT to_jsonb(s) FROM player_stats s ORDER BY player_id")
            .fetch_all(&state.db)
            .await
            .unwrap();
    let pick = |r: &Value| {
        let cols = [
            "games_played",
            "wins",
            "komino_calls",
            "komino_wins",
            "matches",
            "failed_matches",
            "special_moves",
            "cards_interacted",
            "forfeits",
            "draws",
            "match_ms",
        ];
        (
            r["player_id"].as_str().unwrap().to_string(),
            cols.map(|c| r[c].as_i64().unwrap()),
        )
    };
    assert_eq!(
        rows.iter().map(pick).collect::<Vec<_>>(),
        vec![
            ("ann".to_string(), [5, 3, 2, 1, 5, 3, 5, 14, 1, 0, 0]),
            ("ben".to_string(), [3, 1, 0, 0, 2, 0, 1, 6, 1, 0, 0]),
            ("cat".to_string(), [7, 0, 0, 0, 0, 0, 0, 0, 0, 4, 0]),
        ]
    );
}

async fn account_info(server: &TestServer) -> Value {
    let resp = server.get("/api/account").await;
    resp.assert_status_ok();
    resp.json()
}

async fn recover(
    server: &TestServer,
    username: &str,
    code: &str,
    password: &str,
) -> axum_test::TestResponse {
    server
        .post("/api/account/recover")
        .json(&json!({ "username": username, "code": code, "password": password }))
        .await
}

#[tokio::test]
async fn test_a_recovery_code_sets_a_new_password_once() {
    let state = get_state().await;
    let server = client(&state);
    let resp = signup(&server, "ada", "correct horse").await;
    resp.assert_status_ok();
    assert_eq!(resp.header("cache-control"), "no-store");
    let code = resp.json::<Value>()["recovery_code"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(code.len(), komino::accounts::RECOVERY_CHARS + 4);
    let id = me(&server).await;
    assert_eq!(account_info(&server).await["recovery"], true);
    // only its hash is stored
    let stored: String = sqlx::query_scalar("SELECT recovery_hash FROM players WHERE id = $1")
        .bind(&id)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(stored, komino::accounts::recovery_hash(&code));
    // the account gets locked out by someone else
    for _ in 0..komino::accounts::MAX_FAILED_LOGINS {
        login(&client(&state), "ada", "wrong horse")
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }

    let phone = client(&state);
    for (user, c) in [
        ("ada", "00000-00000-00000-00000-00000"),
        ("nobody", code.as_str()),
    ] {
        let resp = recover(&phone, user, c, "battery staple").await;
        resp.assert_status(StatusCode::UNAUTHORIZED);
        assert_eq!(
            resp.json::<Value>()["message"],
            "wrong username or recovery code"
        );
    }
    recover(&phone, "ada", &code, "short")
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    // typed back loosely, it still works, and lifts the lock
    let typed = code.to_uppercase().replace('-', " ");
    let resp = recover(&phone, " ADA ", &typed, "battery staple").await;
    resp.assert_status_ok();
    assert_eq!(resp.header("cache-control"), "no-store");
    let body: Value = resp.json();
    assert_eq!(body["id"], id.as_str());
    let next = body["recovery_code"].as_str().unwrap().to_string();
    assert_ne!(next, code);
    assert_eq!(me(&phone).await, id);
    // every other session ended
    assert_ne!(me(&server).await, id);
    assert_eq!(session_count(&state).await, 1);
    login(&client(&state), "ada", "correct horse")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    login(&client(&state), "ada", "battery staple")
        .await
        .assert_status_ok();
    // a used code is spent; the new one works
    recover(&client(&state), "ada", &code, "another one")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    recover(&client(&state), "ada", &next, "another one")
        .await
        .assert_status_ok();
}

#[tokio::test]
async fn test_a_new_recovery_code_replaces_the_old() {
    let state = get_state().await;
    let server = client(&state);
    let resp = signup(&server, "ada", "correct horse").await;
    let old = resp.json::<Value>()["recovery_code"]
        .as_str()
        .unwrap()
        .to_string();
    let anon = client(&state);
    assert_eq!(account_info(&anon).await["recovery"], false);
    let ask = |s: &TestServer, pw: &str| {
        s.post("/api/account/recovery")
            .json(&json!({ "password": pw }))
    };
    ask(&anon, "correct horse")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    let resp = ask(&server, "wrong horse").await;
    resp.assert_status(StatusCode::UNAUTHORIZED);
    assert_eq!(resp.json::<Value>()["message"], "wrong password");
    let resp = ask(&server, "correct horse").await;
    resp.assert_status_ok();
    assert_eq!(resp.header("cache-control"), "no-store");
    let new = resp.json::<Value>()["recovery_code"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(new, old);
    // asking for one signs nobody out
    assert_eq!(account_info(&server).await["username"], "ada");
    assert_eq!(session_count(&state).await, 1);
    recover(&client(&state), "ada", &old, "battery staple")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    recover(&client(&state), "ada", &new, "battery staple")
        .await
        .assert_status_ok();
}

#[tokio::test]
async fn test_signing_out_other_devices_keeps_this_one() {
    let state = get_state().await;
    let server = client(&state);
    signup(&server, "ada", "correct horse")
        .await
        .assert_status_ok();
    let id = me(&server).await;
    let laptop = client(&state);
    let phone = client(&state);
    for c in [&laptop, &phone] {
        login(c, "ada", "correct horse").await.assert_status_ok();
    }
    assert_eq!(account_info(&server).await["sessions"], 3);
    client(&state)
        .post("/api/account/sign-out-others")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    let resp = server.post("/api/account/sign-out-others").await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>()["ended"], 2);
    assert_eq!(me(&server).await, id);
    assert_ne!(me(&laptop).await, id);
    assert_ne!(me(&phone).await, id);
    assert_eq!(account_info(&server).await["sessions"], 1);
}

#[tokio::test]
async fn test_changing_the_username() {
    let state = get_state().await;
    let (host, guest, code) = room_of_two(&state).await;
    signup(&host, "ada", "correct horse")
        .await
        .assert_status_ok();
    let id = me(&host).await;
    let other = client(&state);
    let resp = other
        .post("/api/signup")
        .json(&json!({ "username": "grace", "password": "hunter22", "name": "Grace H" }))
        .await;
    resp.assert_status_ok();
    let rename = |s: &TestServer, username: &str, pw: &str| {
        s.post("/api/account/username")
            .json(&json!({ "username": username, "password": pw }))
    };
    rename(&client(&state), "ada2", "correct horse")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    rename(&host, "ada2", "wrong horse")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    rename(&host, "a!", "correct horse")
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    let resp = rename(&host, "Grace", "correct horse").await;
    resp.assert_status(StatusCode::CONFLICT);
    assert_eq!(resp.json::<Value>()["code"], "taken");
    // the same name is no change
    rename(&host, "ADA", "correct horse")
        .await
        .assert_status_ok();

    // a default name that was the username follows it, into the room too
    let resp = rename(&host, " Countess ", "correct horse").await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["id"], id.as_str());
    assert_eq!(body["username"], "countess");
    assert_eq!(body["name"], "countess");
    assert_eq!(member_name(&guest, &code, &id).await, "countess");
    login(&client(&state), "ada", "correct horse")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    login(&client(&state), "countess", "correct horse")
        .await
        .assert_status_ok();
    // the old one is free again
    signup(&client(&state), "ada", "battery staple")
        .await
        .assert_status_ok();
    // a chosen default name stays
    let resp = rename(&other, "grace_h", "hunter22").await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>()["name"], "Grace H");
}

#[tokio::test]
async fn test_the_account_page_lists_the_rooms_you_are_in() {
    let state = get_state().await;
    let (host, guest, code) = room_of_two(&state).await;
    act(&host, &code, json!({ "type": "start" }))
        .await
        .assert_status_ok();
    let quiet = create_room(&guest).await;
    let left = create_room(&guest).await;
    host.post(&format!("/api/rooms/{quiet}/join"))
        .await
        .assert_status_ok();
    host.post(&format!("/api/rooms/{left}/join"))
        .await
        .assert_status_ok();
    host.post(&format!("/api/rooms/{left}/leave"))
        .await
        .assert_status_ok();
    sqlx::query("UPDATE rooms SET last_active = now() - interval '1 day' WHERE code = $1")
        .bind(&quiet)
        .execute(&state.db)
        .await
        .unwrap();
    // works the same for an anonymous player
    let info = account_info(&host).await;
    assert_eq!(info["username"], Value::Null);
    assert_eq!(info["sessions"], 0);
    assert_eq!(info["recovery"], false);
    let rooms: Vec<(String, bool, i64, bool)> = info["rooms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["code"].as_str().unwrap().to_string(),
                r["host"].as_bool().unwrap(),
                r["members"].as_i64().unwrap(),
                r["playing"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(rooms, vec![(code, true, 2, true), (quiet, false, 2, false)]);
    assert_eq!(account_info(&client(&state)).await["rooms"], json!([]));
}

#[tokio::test]
async fn test_stale_password_hashes_are_rehashed_at_sign_in() {
    let state = get_state().await;
    let server = client(&state);
    signup(&server, "ada", "correct horse")
        .await
        .assert_status_ok();
    let stored = || async {
        sqlx::query_scalar::<_, String>("SELECT password_hash FROM players WHERE username = 'ada'")
            .fetch_one(&state.db)
            .await
            .unwrap()
    };
    let old = common::crypto::hash_password_with(b"correct horse", 1_000).unwrap();
    sqlx::query("UPDATE players SET password_hash = $1 WHERE username = 'ada'")
        .bind(&old)
        .execute(&state.db)
        .await
        .unwrap();
    // a wrong password changes nothing
    login(&client(&state), "ada", "wrong horse")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    assert_eq!(stored().await, old);
    login(&client(&state), "ada", "correct horse")
        .await
        .assert_status_ok();
    let new = stored().await;
    assert!(new.starts_with("pbkdf2-sha512$210000$"), "{new}");
    assert!(!common::crypto::password_needs_rehash(&new));
    login(&client(&state), "ada", "correct horse")
        .await
        .assert_status_ok();
    assert_eq!(stored().await, new);
}

#[tokio::test]
async fn test_password_checks_are_limited_per_address() {
    let state = get_state().await;
    let server = client(&state);
    let from = |addr: &'static str, path: &str, body: Value| {
        server
            .post(path)
            .add_header(komino::handlers::CLIENT_IP_HEADER, addr)
            .json(&body)
    };
    // sign ups refused before any hashing still use the bucket up, quickly
    // enough that none of it refills meanwhile
    let bad = json!({ "username": "a!", "password": "correct horse" });
    for _ in 0..komino::accounts::AUTH_BURST as usize {
        from("1.2.3.4", "/api/signup", bad.clone())
            .await
            .assert_status(StatusCode::BAD_REQUEST);
    }
    // every password route draws from the same bucket
    let guess = json!({ "username": "nobody", "password": "guess" });
    let resp = from("1.2.3.4", "/api/login", guess.clone()).await;
    resp.assert_status(StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(resp.json::<Value>()["code"], "slow_down");
    from(
        "1.2.3.4",
        "/api/account/recover",
        json!({
            "username": "nobody", "code": "x", "password": "correct horse",
        }),
    )
    .await
    .assert_status(StatusCode::TOO_MANY_REQUESTS);
    // another address is not held back
    from("5.6.7.8", "/api/login", guess)
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_quiet_anonymous_players_are_forgotten() {
    let state = get_state().await;
    let (host, guest, _code) = room_of_two(&state).await;
    let in_room = me(&guest).await;
    let account = client(&state);
    signup(&account, "ada", "correct horse")
        .await
        .assert_status_ok();
    let account_id = me(&account).await;
    let host_id = me(&host).await;
    sqlx::raw_sql(
        "INSERT INTO players (id, name, last_seen) VALUES
             ('played_long_ago', 'a', now() - interval '91 days'),
             ('played_lately', 'b', now() - interval '30 days'),
             ('never_played', 'c', now() - interval '8 days'),
             ('just_visited', 'd', now() - interval '1 day');
         INSERT INTO player_stats (player_id, games_played) VALUES
             ('played_long_ago', 1), ('played_lately', 1);
         INSERT INTO player_partners (player_id, other_id, games) VALUES
             ('played_long_ago', 'played_lately', 1), ('played_lately', 'played_long_ago', 1);",
    )
    .execute(&state.db)
    .await
    .unwrap();
    sqlx::query("UPDATE players SET last_seen = now() - interval '200 days' WHERE id = ANY($1)")
        .bind(vec![in_room.clone(), account_id.clone(), host_id.clone()])
        .execute(&state.db)
        .await
        .unwrap();
    let forgotten = komino::models::forget_anonymous_players(&state.db)
        .await
        .unwrap();
    assert_eq!(forgotten, 2);
    let mut left: Vec<String> = sqlx::query_scalar("SELECT id FROM players")
        .fetch_all(&state.db)
        .await
        .unwrap();
    left.sort();
    let mut want = vec![
        "played_lately".to_string(),
        "just_visited".to_string(),
        in_room,
        account_id,
        host_id,
    ];
    want.sort();
    assert_eq!(left, want);
    // their partner rows went with them
    let partners: i64 = sqlx::query_scalar("SELECT count(*) FROM player_partners")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(partners, 0);
}
