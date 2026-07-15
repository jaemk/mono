use axum::http::StatusCode;
use axum_test::TestServer;
use mapour::{service, Config, State};
use serde_json::{json, Value};

fn set_workspace_root() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    std::env::set_current_dir(workspace_root).ok();
}

/// Initialise the mapour service and return the shared state.  Individual
/// "browsers" are created per identity with [`client`] so each keeps its own
/// cookie jar against the same underlying state.
async fn get_state() -> State {
    set_workspace_root();
    let state = service::init(Config::load())
        .await
        .expect("failed to initialize mapour state");
    mapour::test_utils::clean_mapour_db(&state.db, &state.s3, &state.config).await;
    state
}

/// A fresh cookie-holding client over the shared state.
fn client(state: &State) -> TestServer {
    let router = service::router(state.clone()).with_state(state.clone());
    TestServer::builder().save_cookies().build(router)
}

/// Returns `true` (and prints a message) when S3 credentials are absent.
fn skip_if_no_s3() -> bool {
    let has_creds = std::env::var("AWS_ACCESS_KEY_ID")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    if !has_creds {
        eprintln!("Skipping test: AWS_ACCESS_KEY_ID not set (S3 integration required)");
    }
    !has_creds
}

async fn register(server: &TestServer, email: &str, name: &str) {
    let resp = server
        .post("/api/register")
        .json(&json!({ "email": email, "name": name, "password": "password123" }))
        .await;
    resp.assert_status_ok();
}

/// Create a standard org and return its public id.
async fn create_org(server: &TestServer, name: &str, domain: Option<&str>) -> String {
    let resp = server
        .post("/api/orgs")
        .json(&json!({ "name": name, "allowed_email_domain": domain }))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    body["org"]["public_id"].as_str().unwrap().to_string()
}

async fn get_categories(server: &TestServer, pid: &str) -> Vec<Value> {
    let resp = server.get(&format!("/api/orgs/{pid}/categories")).await;
    resp.assert_status_ok();
    resp.json::<Value>()["categories"]
        .as_array()
        .unwrap()
        .clone()
}

async fn category_id(server: &TestServer, pid: &str, key: &str) -> i64 {
    get_categories(server, pid)
        .await
        .iter()
        .find(|c| c["key"] == key)
        .unwrap()["id"]
        .as_i64()
        .unwrap()
}

async fn create_pin(server: &TestServer, pid: &str, category_id: i64, lat: f64, lng: f64) -> Value {
    let resp = server
        .post(&format!("/api/orgs/{pid}/pins"))
        .json(&json!({ "category_id": category_id, "lat": lat, "lng": lng, "description": "a place" }))
        .await;
    resp.assert_status_ok();
    resp.json()
}

// ---------------------------------------------------------------------------
// basics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_status() {
    let state = get_state().await;
    let server = client(&state);
    let resp = server.get("/status").await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>()["ok"], "ok");
}

#[tokio::test]
async fn test_index_serves_app() {
    let state = get_state().await;
    let server = client(&state);
    let resp = server.get("/").await;
    resp.assert_status_ok();
    assert!(resp.text().contains("mapour"));
}

// ---------------------------------------------------------------------------
// auth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_register_login_me_logout() {
    let state = get_state().await;
    let server = client(&state);

    register(&server, "ada@example.com", "Ada").await;
    let me: Value = server.get("/api/me").await.json();
    assert_eq!(me["user"]["email"], "ada@example.com");
    assert_eq!(me["user"]["name"], "Ada");

    server.post("/api/logout").await.assert_status_ok();
    let me: Value = server.get("/api/me").await.json();
    assert!(me["user"].is_null());

    // wrong password
    let resp = server
        .post("/api/login")
        .json(&json!({ "email": "ada@example.com", "password": "wrong-password" }))
        .await;
    resp.assert_status(StatusCode::UNAUTHORIZED);

    // correct password
    let resp = server
        .post("/api/login")
        .json(&json!({ "email": "ada@example.com", "password": "password123" }))
        .await;
    resp.assert_status_ok();
    let me: Value = server.get("/api/me").await.json();
    assert_eq!(me["user"]["email"], "ada@example.com");
}

#[tokio::test]
async fn test_register_validation() {
    let state = get_state().await;
    let server = client(&state);

    let resp = server
        .post("/api/register")
        .json(&json!({ "email": "not-an-email", "name": "X", "password": "password123" }))
        .await;
    resp.assert_status(StatusCode::BAD_REQUEST);

    let resp = server
        .post("/api/register")
        .json(&json!({ "email": "x@example.com", "name": "X", "password": "short" }))
        .await;
    resp.assert_status(StatusCode::BAD_REQUEST);

    register(&server, "dupe@example.com", "First").await;
    let resp = server
        .post("/api/register")
        .json(&json!({ "email": "dupe@example.com", "name": "Second", "password": "password123" }))
        .await;
    resp.assert_status(StatusCode::CONFLICT);
}

// ---------------------------------------------------------------------------
// orgs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_create_org_requires_login() {
    let state = get_state().await;
    let server = client(&state);
    let resp = server
        .post("/api/orgs")
        .json(&json!({ "name": "No Auth Inc" }))
        .await;
    resp.assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_create_org_seeds_categories_and_creator_is_admin() {
    let state = get_state().await;
    let server = client(&state);
    register(&server, "founder@example.com", "Founder").await;
    let pid = create_org(&server, "Acme", None).await;

    let org: Value = server.get(&format!("/api/orgs/{pid}")).await.json();
    assert_eq!(org["member"]["role"], "admin");
    assert_eq!(org["org"]["name"], "Acme");

    let cats = get_categories(&server, &pid).await;
    let keys: Vec<&str> = cats.iter().map(|c| c["key"].as_str().unwrap()).collect();
    assert_eq!(
        keys,
        vec![
            "live",
            "work",
            "fav_outside",
            "fav_restaurant",
            "fav_activity"
        ]
    );
    // "where I live" allows a single pin per member
    let live = cats.iter().find(|c| c["key"] == "live").unwrap();
    assert_eq!(live["max_per_member"], 1);

    let me: Value = server.get("/api/me").await.json();
    assert_eq!(me["orgs"].as_array().unwrap().len(), 1);
    assert_eq!(me["orgs"][0]["role"], "admin");
}

#[tokio::test]
async fn test_org_visibility_for_non_members() {
    let state = get_state().await;
    let admin = client(&state);
    register(&admin, "owner@example.com", "Owner").await;
    let pid = create_org(&admin, "Private Co", None).await;

    // logged-out stranger sees only the join preview
    let stranger = client(&state);
    let org: Value = stranger.get(&format!("/api/orgs/{pid}")).await.json();
    assert!(org["member"].is_null());
    assert_eq!(org["can_join"], false);

    // members-only endpoints are forbidden
    stranger
        .get(&format!("/api/orgs/{pid}/pins"))
        .await
        .assert_status(StatusCode::FORBIDDEN);

    // unknown org is a 404
    stranger
        .get("/api/orgs/does-not-exist")
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_join_by_email_domain() {
    let state = get_state().await;
    let admin = client(&state);
    register(&admin, "boss@acme.com", "Boss").await;
    let pid = create_org(&admin, "Acme", Some("acme.com")).await;

    // matching domain joins directly
    let emp = client(&state);
    register(&emp, "dev@acme.com", "Dev").await;
    let resp = emp.post(&format!("/api/orgs/{pid}/join")).await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>()["joined"], true);

    // non-matching domain becomes a join request
    let outsider = client(&state);
    register(&outsider, "someone@other.com", "Someone").await;
    let resp = outsider.post(&format!("/api/orgs/{pid}/join")).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["joined"], false);
    assert_eq!(body["requested"], true);
}

#[tokio::test]
async fn test_join_request_approval_flow() {
    let state = get_state().await;
    let admin = client(&state);
    register(&admin, "owner@example.com", "Owner").await;
    let pid = create_org(&admin, "Club", None).await;

    let applicant = client(&state);
    register(&applicant, "hopeful@example.com", "Hopeful").await;
    applicant
        .post(&format!("/api/orgs/{pid}/join"))
        .await
        .assert_status_ok();

    // pending request visible to admin
    let requests: Value = admin.get(&format!("/api/orgs/{pid}/requests")).await.json();
    let reqs = requests["requests"].as_array().unwrap();
    assert_eq!(reqs.len(), 1);
    let req_id = reqs[0]["id"].as_i64().unwrap();

    // non-admin cannot see or decide requests
    applicant
        .get(&format!("/api/orgs/{pid}/requests"))
        .await
        .assert_status(StatusCode::FORBIDDEN);

    admin
        .post(&format!("/api/orgs/{pid}/requests/{req_id}/approve"))
        .await
        .assert_status_ok();

    let org: Value = applicant.get(&format!("/api/orgs/{pid}")).await.json();
    assert_eq!(org["member"]["role"], "member");
}

#[tokio::test]
async fn test_join_request_deny_flow() {
    let state = get_state().await;
    let admin = client(&state);
    register(&admin, "owner@example.com", "Owner").await;
    let pid = create_org(&admin, "Club", None).await;

    let applicant = client(&state);
    register(&applicant, "nope@example.com", "Nope").await;
    applicant
        .post(&format!("/api/orgs/{pid}/join"))
        .await
        .assert_status_ok();

    let requests: Value = admin.get(&format!("/api/orgs/{pid}/requests")).await.json();
    let req_id = requests["requests"][0]["id"].as_i64().unwrap();
    admin
        .post(&format!("/api/orgs/{pid}/requests/{req_id}/deny"))
        .await
        .assert_status_ok();

    let org: Value = applicant.get(&format!("/api/orgs/{pid}")).await.json();
    assert!(org["member"].is_null());
}

// ---------------------------------------------------------------------------
// invites
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_invite_flow() {
    let state = get_state().await;
    let admin = client(&state);
    register(&admin, "owner@example.com", "Owner").await;
    let pid = create_org(&admin, "Invitees", None).await;

    let resp = admin
        .post(&format!("/api/orgs/{pid}/invites"))
        .json(&json!({ "email": "friend@example.com" }))
        .await;
    resp.assert_status_ok();
    let link = resp.json::<Value>()["link"].as_str().unwrap().to_string();
    let token = link.split("invite=").nth(1).unwrap().to_string();

    // the invite is listed as pending
    let invites: Value = admin.get(&format!("/api/orgs/{pid}/invites")).await.json();
    assert_eq!(invites["invites"].as_array().unwrap().len(), 1);

    // a different email cannot use the token
    let wrong = client(&state);
    register(&wrong, "other@example.com", "Other").await;
    wrong
        .post("/api/invites/accept")
        .json(&json!({ "token": token }))
        .await
        .assert_status(StatusCode::FORBIDDEN);

    // the invited email can
    let friend = client(&state);
    register(&friend, "friend@example.com", "Friend").await;
    let resp = friend
        .post("/api/invites/accept")
        .json(&json!({ "token": token }))
        .await;
    resp.assert_status_ok();
    let org: Value = friend.get(&format!("/api/orgs/{pid}")).await.json();
    assert_eq!(org["member"]["role"], "member");

    // token is single-use
    friend
        .post("/api/invites/accept")
        .json(&json!({ "token": token }))
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    // and no longer listed
    let invites: Value = admin.get(&format!("/api/orgs/{pid}/invites")).await.json();
    assert_eq!(invites["invites"].as_array().unwrap().len(), 0);
}

// ---------------------------------------------------------------------------
// roles / members
// ---------------------------------------------------------------------------

/// Set up an org with an admin and one plain member; returns (pid, member_id).
async fn org_with_member(admin: &TestServer, member: &TestServer) -> (String, i64) {
    register(admin, "owner@corp.com", "Owner").await;
    register(member, "member@corp.com", "Member").await;
    let pid = create_org(admin, "Corp", Some("corp.com")).await;
    member
        .post(&format!("/api/orgs/{pid}/join"))
        .await
        .assert_status_ok();
    let members: Value = admin.get(&format!("/api/orgs/{pid}/members")).await.json();
    let member_id = members["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "member")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    (pid, member_id)
}

#[tokio::test]
async fn test_promote_and_demote_member() {
    let state = get_state().await;
    let admin = client(&state);
    let member = client(&state);
    let (pid, member_id) = org_with_member(&admin, &member).await;

    // members cannot change roles
    member
        .post(&format!("/api/orgs/{pid}/members/{member_id}/role"))
        .json(&json!({ "role": "admin" }))
        .await
        .assert_status(StatusCode::FORBIDDEN);

    // admins can promote
    admin
        .post(&format!("/api/orgs/{pid}/members/{member_id}/role"))
        .json(&json!({ "role": "admin" }))
        .await
        .assert_status_ok();
    let org: Value = member.get(&format!("/api/orgs/{pid}")).await.json();
    assert_eq!(org["member"]["role"], "admin");

    // and demote, since another admin remains
    admin
        .post(&format!("/api/orgs/{pid}/members/{member_id}/role"))
        .json(&json!({ "role": "member" }))
        .await
        .assert_status_ok();
}

#[tokio::test]
async fn test_last_admin_cannot_be_demoted_or_removed() {
    let state = get_state().await;
    let admin = client(&state);
    let member = client(&state);
    let (pid, _member_id) = org_with_member(&admin, &member).await;

    let members: Value = admin.get(&format!("/api/orgs/{pid}/members")).await.json();
    let admin_id = members["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "admin")
        .unwrap()["id"]
        .as_i64()
        .unwrap();

    admin
        .post(&format!("/api/orgs/{pid}/members/{admin_id}/role"))
        .json(&json!({ "role": "member" }))
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    // removal (self-leave included) is also blocked for the last admin
    admin
        .delete(&format!("/api/orgs/{pid}/members/{admin_id}"))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_admin_removes_member_and_their_pins() {
    let state = get_state().await;
    let admin = client(&state);
    let member = client(&state);
    let (pid, member_id) = org_with_member(&admin, &member).await;

    let work = category_id(&member, &pid, "work").await;
    create_pin(&member, &pid, work, 40.7, -74.0).await;

    admin
        .delete(&format!("/api/orgs/{pid}/members/{member_id}"))
        .await
        .assert_status_ok();

    // the removed member no longer has access
    member
        .get(&format!("/api/orgs/{pid}/pins"))
        .await
        .assert_status(StatusCode::FORBIDDEN);

    // and their pins are gone
    let pins: Value = admin.get(&format!("/api/orgs/{pid}/pins")).await.json();
    assert_eq!(pins["pins"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn test_member_can_leave() {
    let state = get_state().await;
    let admin = client(&state);
    let member = client(&state);
    let (pid, member_id) = org_with_member(&admin, &member).await;

    member
        .delete(&format!("/api/orgs/{pid}/members/{member_id}"))
        .await
        .assert_status_ok();
    let org: Value = member.get(&format!("/api/orgs/{pid}")).await.json();
    assert!(org["member"].is_null());
}

#[tokio::test]
async fn test_member_cannot_remove_others() {
    let state = get_state().await;
    let admin = client(&state);
    let member = client(&state);
    let (pid, _member_id) = org_with_member(&admin, &member).await;

    let members: Value = admin.get(&format!("/api/orgs/{pid}/members")).await.json();
    let admin_id = members["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "admin")
        .unwrap()["id"]
        .as_i64()
        .unwrap();

    member
        .delete(&format!("/api/orgs/{pid}/members/{admin_id}"))
        .await
        .assert_status(StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// categories
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_admin_configures_category_color() {
    let state = get_state().await;
    let admin = client(&state);
    let member = client(&state);
    let (pid, _member_id) = org_with_member(&admin, &member).await;
    let live = category_id(&admin, &pid, "live").await;

    // members cannot recolor
    member
        .post(&format!("/api/orgs/{pid}/categories/{live}"))
        .json(&json!({ "color": "#123456" }))
        .await
        .assert_status(StatusCode::FORBIDDEN);

    // bad color rejected
    admin
        .post(&format!("/api/orgs/{pid}/categories/{live}"))
        .json(&json!({ "color": "red" }))
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    let resp = admin
        .post(&format!("/api/orgs/{pid}/categories/{live}"))
        .json(&json!({ "color": "#123456" }))
        .await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>()["category"]["color"], "#123456");
}

// ---------------------------------------------------------------------------
// pins
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_pin_crud_and_live_limit() {
    let state = get_state().await;
    let server = client(&state);
    register(&server, "pinner@example.com", "Pinner").await;
    let pid = create_org(&server, "Pins", None).await;
    let live = category_id(&server, &pid, "live").await;
    let work = category_id(&server, &pid, "work").await;

    // one live pin allowed
    create_pin(&server, &pid, live, 52.52, 13.405).await;
    let resp = server
        .post(&format!("/api/orgs/{pid}/pins"))
        .json(&json!({ "category_id": live, "lat": 48.85, "lng": 2.35 }))
        .await;
    resp.assert_status(StatusCode::BAD_REQUEST);

    // many work pins allowed
    create_pin(&server, &pid, work, 40.71, -74.0).await;
    create_pin(&server, &pid, work, 37.77, -122.42).await;

    let pins: Value = server.get(&format!("/api/orgs/{pid}/pins")).await.json();
    let pins = pins["pins"].as_array().unwrap();
    assert_eq!(pins.len(), 3);
    assert!(pins.iter().all(|p| p["is_yours"] == true));

    // update the description
    let pin_id = pins[0]["id"].as_i64().unwrap();
    server
        .post(&format!("/api/orgs/{pid}/pins/{pin_id}"))
        .json(&json!({ "description": "home sweet home" }))
        .await
        .assert_status_ok();
    let pins: Value = server.get(&format!("/api/orgs/{pid}/pins")).await.json();
    let updated = pins["pins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"].as_i64() == Some(pin_id))
        .unwrap()
        .clone();
    assert_eq!(updated["description"], "home sweet home");

    // deleting frees the live slot
    server
        .delete(&format!("/api/orgs/{pid}/pins/{pin_id}"))
        .await
        .assert_status_ok();
    create_pin(&server, &pid, live, 48.85, 2.35).await;
}

#[tokio::test]
async fn test_pin_validation() {
    let state = get_state().await;
    let server = client(&state);
    register(&server, "val@example.com", "Val").await;
    let pid = create_org(&server, "Validation", None).await;
    let work = category_id(&server, &pid, "work").await;

    // out-of-range coordinates
    let resp = server
        .post(&format!("/api/orgs/{pid}/pins"))
        .json(&json!({ "category_id": work, "lat": 91.0, "lng": 0.0 }))
        .await;
    resp.assert_status(StatusCode::BAD_REQUEST);

    // category from another org
    let other = client(&state);
    register(&other, "other@example.com", "Other").await;
    let other_pid = create_org(&other, "Other Org", None).await;
    let other_cat = category_id(&other, &other_pid, "work").await;
    let resp = server
        .post(&format!("/api/orgs/{pid}/pins"))
        .json(&json!({ "category_id": other_cat, "lat": 10.0, "lng": 10.0 }))
        .await;
    resp.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_pin_permissions() {
    let state = get_state().await;
    let admin = client(&state);
    let member = client(&state);
    let (pid, _member_id) = org_with_member(&admin, &member).await;
    let work = category_id(&member, &pid, "work").await;

    let pin: Value = create_pin(&member, &pid, work, 40.7, -74.0).await;
    let pin_id = pin["pin"]["id"].as_i64().unwrap();

    // a third member cannot edit or delete someone else's pin
    let third = client(&state);
    register(&third, "third@corp.com", "Third").await;
    third
        .post(&format!("/api/orgs/{pid}/join"))
        .await
        .assert_status_ok();
    third
        .post(&format!("/api/orgs/{pid}/pins/{pin_id}"))
        .json(&json!({ "description": "hijack" }))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    third
        .delete(&format!("/api/orgs/{pid}/pins/{pin_id}"))
        .await
        .assert_status(StatusCode::FORBIDDEN);

    // but an admin can delete anyone's pin
    admin
        .delete(&format!("/api/orgs/{pid}/pins/{pin_id}"))
        .await
        .assert_status_ok();
}

// ---------------------------------------------------------------------------
// anonymous maps
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_anonymous_map_flow() {
    let state = get_state().await;

    // creator needs no account
    let creator = client(&state);
    let resp = creator
        .post("/api/orgs")
        .json(&json!({
            "name": "Offsite",
            "anonymous": true,
            "display_name": "Organizer",
            "ttl_days": 7,
        }))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    let pid = body["org"]["public_id"].as_str().unwrap().to_string();
    assert_eq!(body["org"]["kind"], "anon");
    assert!(!body["org"]["expires_at"].is_null());

    let me: Value = creator.get("/api/me").await.json();
    assert_eq!(me["anonymous"], true);
    assert_eq!(me["orgs"][0]["role"], "admin");

    // another anonymous visitor joins via the link and drops a pin
    let visitor = client(&state);
    let resp = visitor
        .post(&format!("/api/orgs/{pid}/join"))
        .json(&json!({ "display_name": "Drop-in" }))
        .await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>()["joined"], true);

    let work = category_id(&visitor, &pid, "work").await;
    create_pin(&visitor, &pid, work, 51.5, -0.12).await;

    let pins: Value = creator.get(&format!("/api/orgs/{pid}/pins")).await.json();
    let pins = pins["pins"].as_array().unwrap();
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0]["member_name"], "Drop-in");

    // a logged-in user can join the anon map too
    let user = client(&state);
    register(&user, "acct@example.com", "Account Haver").await;
    user.post(&format!("/api/orgs/{pid}/join"))
        .await
        .assert_status_ok();
    let org: Value = user.get(&format!("/api/orgs/{pid}")).await.json();
    assert_eq!(org["member"]["role"], "member");
}

#[tokio::test]
async fn test_anonymous_members_cannot_be_admins() {
    let state = get_state().await;
    let creator = client(&state);
    register(&creator, "owner@example.com", "Owner").await;
    let resp = creator
        .post("/api/orgs")
        .json(&json!({ "name": "Anon Map", "anonymous": true }))
        .await;
    resp.assert_status_ok();
    let pid = resp.json::<Value>()["org"]["public_id"]
        .as_str()
        .unwrap()
        .to_string();

    let visitor = client(&state);
    visitor
        .post(&format!("/api/orgs/{pid}/join"))
        .json(&json!({ "display_name": "Ghost" }))
        .await
        .assert_status_ok();

    let members: Value = creator
        .get(&format!("/api/orgs/{pid}/members"))
        .await
        .json();
    let anon_id = members["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["anonymous"] == true)
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    creator
        .post(&format!("/api/orgs/{pid}/members/{anon_id}/role"))
        .json(&json!({ "role": "admin" }))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_claim_anonymous_memberships() {
    let state = get_state().await;

    // an anonymous visitor creates a map...
    let browser = client(&state);
    let resp = browser
        .post("/api/orgs")
        .json(&json!({ "name": "Mine", "anonymous": true, "display_name": "Me" }))
        .await;
    resp.assert_status_ok();
    let pid = resp.json::<Value>()["org"]["public_id"]
        .as_str()
        .unwrap()
        .to_string();

    // ...then creates an account in the same browser and claims the map
    register(&browser, "claimer@example.com", "Claimer").await;
    let resp = browser.post("/api/claim").await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>()["claimed"], 1);

    let me: Value = browser.get("/api/me").await.json();
    assert_eq!(me["anonymous"], false);
    assert_eq!(me["orgs"][0]["public_id"], pid.as_str());
    assert_eq!(me["orgs"][0]["role"], "admin");

    // the membership row now belongs to the user with their account name
    let org: Value = browser.get(&format!("/api/orgs/{pid}")).await.json();
    assert_eq!(org["member"]["display_name"], "Claimer");
}

#[tokio::test]
async fn test_expired_orgs_hidden_and_swept() {
    let state = get_state().await;
    let server = client(&state);
    register(&server, "ttl@example.com", "Ttl").await;
    let resp = server
        .post("/api/orgs")
        .json(&json!({ "name": "Short Lived", "anonymous": true, "ttl_days": 1 }))
        .await;
    resp.assert_status_ok();
    let pid = resp.json::<Value>()["org"]["public_id"]
        .as_str()
        .unwrap()
        .to_string();

    // force-expire it
    sqlx::query("update orgs set expires_at = now() - interval '1 hour' where public_id = $1")
        .bind(&pid)
        .execute(&state.db)
        .await
        .unwrap();

    // hidden from the api even before the sweeper runs
    server
        .get(&format!("/api/orgs/{pid}"))
        .await
        .assert_status(StatusCode::NOT_FOUND);
    let me: Value = server.get("/api/me").await.json();
    assert_eq!(me["orgs"].as_array().unwrap().len(), 0);

    // the sweeper hard-deletes it
    let swept = service::sweep_expired_orgs(&state).await.unwrap();
    assert_eq!(swept, 1);
    let count: i64 = sqlx::query_scalar("select count(*) from orgs where public_id = $1")
        .bind(&pid)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

// ---------------------------------------------------------------------------
// photos (S3-backed, skipped without credentials)
// ---------------------------------------------------------------------------

const PNG_BYTES: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

async fn upload_photo(server: &TestServer, pid: &str, pin_id: i64) -> Value {
    let resp = server
        .post(&format!("/api/orgs/{pid}/pins/{pin_id}/photo"))
        .content_type("image/png")
        .bytes(PNG_BYTES.to_vec().into())
        .await;
    resp.assert_status_ok();
    resp.json()
}

#[tokio::test]
async fn test_photo_upload_rejects_bad_content_type() {
    let state = get_state().await;
    let server = client(&state);
    register(&server, "pic@example.com", "Pic").await;
    let pid = create_org(&server, "Pics", None).await;
    let work = category_id(&server, &pid, "work").await;
    let pin: Value = create_pin(&server, &pid, work, 1.0, 1.0).await;
    let pin_id = pin["pin"]["id"].as_i64().unwrap();

    let resp = server
        .post(&format!("/api/orgs/{pid}/pins/{pin_id}/photo"))
        .content_type("text/plain")
        .bytes(b"not a photo".to_vec().into())
        .await;
    resp.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_photo_approval_flow() {
    if skip_if_no_s3() {
        return;
    }
    let state = get_state().await;
    let admin = client(&state);
    let member = client(&state);
    let (pid, _member_id) = org_with_member(&admin, &member).await;
    let work = category_id(&member, &pid, "work").await;
    let pin: Value = create_pin(&member, &pid, work, 40.0, -70.0).await;
    let pin_id = pin["pin"]["id"].as_i64().unwrap();

    // member upload starts unapproved
    let photo = upload_photo(&member, &pid, pin_id).await;
    let photo_id = photo["photo"]["id"].as_i64().unwrap();
    assert_eq!(photo["photo"]["approved"], false);

    // a third member sees the pin without the photo and cannot fetch it
    let third = client(&state);
    register(&third, "third@corp.com", "Third").await;
    third
        .post(&format!("/api/orgs/{pid}/join"))
        .await
        .assert_status_ok();
    let pins: Value = third.get(&format!("/api/orgs/{pid}/pins")).await.json();
    assert!(pins["pins"][0]["photo"].is_null());
    third
        .get(&format!("/api/orgs/{pid}/photos/{photo_id}"))
        .await
        .assert_status(StatusCode::NOT_FOUND);

    // the submitter can fetch their own pending photo
    member
        .get(&format!("/api/orgs/{pid}/photos/{photo_id}"))
        .await
        .assert_status_ok();

    // pending queue is admin-only, then approval opens it up
    member
        .get(&format!("/api/orgs/{pid}/photos/pending"))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    let pending: Value = admin
        .get(&format!("/api/orgs/{pid}/photos/pending"))
        .await
        .json();
    assert_eq!(pending["photos"].as_array().unwrap().len(), 1);
    admin
        .post(&format!("/api/orgs/{pid}/photos/{photo_id}/approve"))
        .await
        .assert_status_ok();

    let resp = third
        .get(&format!("/api/orgs/{pid}/photos/{photo_id}"))
        .await;
    resp.assert_status_ok();
    assert_eq!(resp.header("content-type"), "image/png");
    let pins: Value = third.get(&format!("/api/orgs/{pid}/pins")).await.json();
    assert_eq!(pins["pins"][0]["photo"]["approved"], true);
}

#[tokio::test]
async fn test_admin_photo_uploads_auto_approve() {
    if skip_if_no_s3() {
        return;
    }
    let state = get_state().await;
    let admin = client(&state);
    register(&admin, "solo@example.com", "Solo").await;
    let pid = create_org(&admin, "Solo Org", None).await;
    let work = category_id(&admin, &pid, "work").await;
    let pin: Value = create_pin(&admin, &pid, work, 35.68, 139.69).await;
    let pin_id = pin["pin"]["id"].as_i64().unwrap();

    let photo = upload_photo(&admin, &pid, pin_id).await;
    assert_eq!(photo["photo"]["approved"], true);
}

#[tokio::test]
async fn test_member_deletes_own_photo() {
    if skip_if_no_s3() {
        return;
    }
    let state = get_state().await;
    let admin = client(&state);
    let member = client(&state);
    let (pid, _member_id) = org_with_member(&admin, &member).await;
    let work = category_id(&member, &pid, "work").await;
    let pin: Value = create_pin(&member, &pid, work, 40.0, -70.0).await;
    let pin_id = pin["pin"]["id"].as_i64().unwrap();
    let photo = upload_photo(&member, &pid, pin_id).await;
    let photo_id = photo["photo"]["id"].as_i64().unwrap();

    member
        .delete(&format!("/api/orgs/{pid}/photos/{photo_id}"))
        .await
        .assert_status_ok();
    member
        .get(&format!("/api/orgs/{pid}/photos/{photo_id}"))
        .await
        .assert_status(StatusCode::NOT_FOUND);
}
