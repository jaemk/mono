use axum::http::StatusCode;
use axum_test::TestServer;
use komino::game::{Game, Stage, Status};
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
    // the guest does see their own opening peek
    let guest_seat = seats.iter().find(|s| s["player"] != host_id).unwrap();
    assert!(guest_seat["slots"][2].get("v").is_some());
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
    assert_eq!(view["game"]["stage"]["card"], 3);
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
