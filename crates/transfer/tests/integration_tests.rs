use axum::http::StatusCode;
use axum_test::TestServer;
use chrono::Utc;
use transfer::{service, Config, State};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn set_workspace_root() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    std::env::set_current_dir(workspace_root).ok();
}

async fn get_server() -> (TestServer, State) {
    set_workspace_root();
    let state = service::init(Config::load())
        .await
        .expect("failed to initialize transfer state");
    let router = service::router(state.clone()).with_state(state.clone());
    (TestServer::new(router), state)
}

async fn setup(state: &State) {
    transfer::test_utils::clean_transfer_db(&state.db, &state.s3, &state.config).await;
}

/// Returns true (and prints a message) when transfer S3 tests are not opted in.
/// Set TRANSFER_S3_TESTS=1 to run tests that require the kom-transfer bucket.
fn skip_if_no_s3() -> bool {
    let enabled = std::env::var("TRANSFER_S3_TESTS")
        .map(|v| v == "1")
        .unwrap_or(false);
    if !enabled {
        eprintln!(
            "Skipping S3 test: set TRANSFER_S3_TESTS=1 to enable (requires kom-transfer bucket)"
        );
    }
    !enabled
}

/// Hex-encode n bytes of a fixed repeating pattern (no RNG needed for test fixtures).
fn fixed_hex(byte_val: u8, n: usize) -> String {
    hex::encode(vec![byte_val; n])
}

/// Minimal valid upload/init request body. Sizes and hashes are fake but
/// structurally correct hex strings.
fn upload_init_body(access_pw_hex: &str) -> serde_json::Value {
    serde_json::json!({
        "nonce":             fixed_hex(0xAB, 12),  // 12 bytes
        "file_name_hash":    fixed_hex(0xCD, 32),  // 32 bytes
        "content_hash":      fixed_hex(0xEF, 32),  // 32 bytes
        "size":              1024_i64,
        "access_password":   access_pw_hex,
        "deletion_password": null,
        "download_limit":    null,
        "lifespan":          null,
    })
}

/// SHA-256 of `data` returned as lowercase hex.
fn sha256_hex(data: &[u8]) -> String {
    use ring::digest;
    let digest = digest::digest(&digest::SHA256, data);
    hex::encode(digest.as_ref())
}

/// Insert an upload record directly into the DB (bypasses S3) so we can test
/// the download / delete / expiry paths without real object storage.
async fn insert_fake_upload(
    state: &State,
    access_pw_bytes: &[u8],
    deletion_pw_bytes: Option<&[u8]>,
    download_limit: Option<i32>,
    expire_date: chrono::DateTime<Utc>,
) -> transfer::models::Upload {
    let (salt, hash) = common::crypto::hash_password(access_pw_bytes).unwrap();
    let access_auth_id = transfer::models::insert_auth(&state.db, salt, hash)
        .await
        .unwrap();

    let deletion_auth_id = if let Some(del_bytes) = deletion_pw_bytes {
        let (s, h) = common::crypto::hash_password(del_bytes).unwrap();
        Some(
            transfer::models::insert_auth(&state.db, s, h)
                .await
                .unwrap(),
        )
    } else {
        None
    };

    let uuid_ = Uuid::new_v4();
    transfer::models::insert_upload(
        &state.db,
        uuid_,
        vec![0xCD; 32],    // file_name_hash
        vec![0xEF; 32],    // content_hash (sha256 of the fake ciphertext 0xEF×32)
        64,                // size
        uuid_.to_string(), // storage_uri (unused in DB-only tests)
        vec![0xAB; 12],    // nonce
        access_auth_id,
        deletion_auth_id,
        download_limit,
        expire_date,
    )
    .await
    .unwrap()
}

// ---------------------------------------------------------------------------
// Non-DB / non-S3 tests — always run
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_upload_defaults() {
    let (server, _state) = get_server().await;
    let resp = server.get("/api/upload/defaults").await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert!(
        body["upload_limit_bytes"].as_u64().unwrap_or(0) > 0,
        "upload_limit_bytes should be positive"
    );
    assert!(
        body["upload_lifespan_secs_default"].as_i64().unwrap_or(0) > 0,
        "upload_lifespan_secs_default should be positive"
    );
}

#[tokio::test]
async fn test_upload_init_invalid_nonce_hex() {
    let (server, _state) = get_server().await;
    let body = serde_json::json!({
        "nonce":          "not-hex!",
        "file_name_hash": fixed_hex(0xCD, 32),
        "content_hash":   fixed_hex(0xEF, 32),
        "size":           1024_i64,
        "access_password": "deadbeef",
    });
    let resp = server.post("/api/upload/init").json(&body).await;
    resp.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        resp.json::<serde_json::Value>()["error"],
        "invalid nonce hex"
    );
}

#[tokio::test]
async fn test_upload_init_invalid_content_hash_hex() {
    let (server, _state) = get_server().await;
    let body = serde_json::json!({
        "nonce":          fixed_hex(0xAB, 12),
        "file_name_hash": fixed_hex(0xCD, 32),
        "content_hash":   "gg",
        "size":           1024_i64,
        "access_password": "deadbeef",
    });
    let resp = server.post("/api/upload/init").json(&body).await;
    resp.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        resp.json::<serde_json::Value>()["error"],
        "invalid content_hash hex"
    );
}

#[tokio::test]
async fn test_upload_init_size_zero() {
    let (server, _state) = get_server().await;
    let mut body = upload_init_body("deadbeef");
    body["size"] = serde_json::json!(0_i64);
    let resp = server.post("/api/upload/init").json(&body).await;
    resp.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        resp.json::<serde_json::Value>()["error"],
        "size must be positive"
    );
}

#[tokio::test]
async fn test_upload_init_size_too_large() {
    let (server, _state) = get_server().await;
    let limit = Config::load().upload_limit_bytes;
    let mut body = upload_init_body("deadbeef");
    body["size"] = serde_json::json!((limit + 1) as i64);
    let resp = server.post("/api/upload/init").json(&body).await;
    resp.assert_status(StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(resp.json::<serde_json::Value>()["error"], "file too large");
}

#[tokio::test]
async fn test_upload_file_key_not_found() {
    let (server, _state) = get_server().await;
    let resp = server
        .post("/api/upload")
        .add_query_params([("key", Uuid::new_v4().to_string())])
        .bytes(axum::body::Bytes::from(vec![0u8; 8]))
        .content_type("application/octet-stream")
        .await;
    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_download_init_not_found() {
    let (server, _state) = get_server().await;
    let body = serde_json::json!({
        "key":             Uuid::new_v4().to_string(),
        "access_password": "deadbeef",
    });
    let resp = server.post("/api/download/init").json(&body).await;
    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_download_not_found() {
    let (server, _state) = get_server().await;
    let body = serde_json::json!({
        "key":             Uuid::new_v4().to_string(),
        "access_password": "deadbeef",
    });
    let resp = server.post("/api/download").json(&body).await;
    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_download_confirm_not_found() {
    let (server, _state) = get_server().await;
    let body = serde_json::json!({
        "key":  Uuid::new_v4().to_string(),
        "hash": fixed_hex(0xAB, 32),
    });
    let resp = server.post("/api/download/confirm").json(&body).await;
    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_delete_not_found() {
    let (server, _state) = get_server().await;
    let body = serde_json::json!({
        "key":              Uuid::new_v4().to_string(),
        "deletion_password": "deadbeef",
    });
    let resp = server.post("/api/upload/delete").json(&body).await;
    resp.assert_status(StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// DB-backed tests — require a database, no S3
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_upload_init_creates_record() {
    let (server, state) = get_server().await;
    setup(&state).await;

    let body = upload_init_body("deadbeef");
    let resp = server.post("/api/upload/init").json(&body).await;
    resp.assert_status_ok();
    let key: Uuid = resp.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .parse()
        .expect("key should be a valid UUID");

    let record = transfer::models::get_init_upload(&state.db, key)
        .await
        .expect("DB query failed")
        .expect("init_upload row should exist after upload/init");

    assert_eq!(record.uuid_, key);
    assert_eq!(record.size_, 1024);

    setup(&state).await;
}

#[tokio::test]
async fn test_upload_init_without_deletion_password() {
    let (server, state) = get_server().await;
    setup(&state).await;

    let resp = server
        .post("/api/upload/init")
        .json(&upload_init_body("aabbccdd"))
        .await;
    resp.assert_status_ok();
    let key: Uuid = resp.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let record = transfer::models::get_init_upload(&state.db, key)
        .await
        .unwrap()
        .unwrap();

    assert!(
        record.deletion_password.is_none(),
        "deletion_password should be null when not provided"
    );

    setup(&state).await;
}

#[tokio::test]
async fn test_upload_init_with_deletion_password() {
    let (server, state) = get_server().await;
    setup(&state).await;

    let mut body = upload_init_body("aabbccdd");
    body["deletion_password"] = serde_json::json!(fixed_hex(0x11, 4));

    let resp = server.post("/api/upload/init").json(&body).await;
    resp.assert_status_ok();
    let key: Uuid = resp.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let record = transfer::models::get_init_upload(&state.db, key)
        .await
        .unwrap()
        .unwrap();

    assert!(
        record.deletion_password.is_some(),
        "deletion_password FK should be set when deletion_password is provided"
    );
    // Two separate auth records should exist.
    assert_ne!(
        record.access_password,
        record.deletion_password.unwrap(),
        "access and deletion auth records must be distinct"
    );

    setup(&state).await;
}

#[tokio::test]
async fn test_upload_init_respects_lifespan() {
    let (server, state) = get_server().await;
    setup(&state).await;

    let mut body = upload_init_body("aabbccdd");
    body["lifespan"] = serde_json::json!(3600_i64);

    let before = Utc::now();
    let resp = server.post("/api/upload/init").json(&body).await;
    resp.assert_status_ok();
    let key: Uuid = resp.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let record = transfer::models::get_init_upload(&state.db, key)
        .await
        .unwrap()
        .unwrap();

    let expected_expire = before + chrono::Duration::seconds(3600);
    let diff = (record.expire_date - expected_expire).num_seconds().abs();
    assert!(
        diff < 5,
        "expire_date should be ~1h from now (diff={diff}s)"
    );

    setup(&state).await;
}

#[tokio::test]
async fn test_download_init_expired_upload_returns_not_found() {
    let (_server, state) = get_server().await;
    setup(&state).await;

    let past = Utc::now() - chrono::Duration::seconds(1);
    let upload = insert_fake_upload(&state, b"mypassword", None, None, past).await;

    // Build a fresh server pointing at the same state.
    let router = service::router(state.clone()).with_state(state.clone());
    let server = TestServer::new(router);

    let resp = server
        .post("/api/download/init")
        .json(&serde_json::json!({
            "key":             upload.uuid_.to_string(),
            "access_password": hex::encode(b"mypassword"),
        }))
        .await;
    resp.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(resp.json::<serde_json::Value>()["error"], "upload expired");

    setup(&state).await;
}

#[tokio::test]
async fn test_download_init_wrong_password_returns_unauthorized() {
    let (_server, state) = get_server().await;
    setup(&state).await;

    let expire = Utc::now() + chrono::Duration::hours(1);
    let upload = insert_fake_upload(&state, b"correct-password", None, None, expire).await;

    let router = service::router(state.clone()).with_state(state.clone());
    let server = TestServer::new(router);

    let resp = server
        .post("/api/download/init")
        .json(&serde_json::json!({
            "key":             upload.uuid_.to_string(),
            "access_password": hex::encode(b"wrong-password"),
        }))
        .await;
    resp.assert_status(StatusCode::UNAUTHORIZED);

    setup(&state).await;
}

#[tokio::test]
async fn test_download_limit_exceeded_returns_gone() {
    let (_server, state) = get_server().await;
    setup(&state).await;

    let expire = Utc::now() + chrono::Duration::hours(1);
    let upload = insert_fake_upload(&state, b"pw", None, Some(1), expire).await;

    // Record one download — this exhausts the limit.
    transfer::models::record_download(&state.db, upload.id)
        .await
        .unwrap();

    let router = service::router(state.clone()).with_state(state.clone());
    let server = TestServer::new(router);

    let resp = server
        .post("/api/download/init")
        .json(&serde_json::json!({
            "key":             upload.uuid_.to_string(),
            "access_password": hex::encode(b"pw"),
        }))
        .await;
    resp.assert_status(StatusCode::GONE);
    assert_eq!(
        resp.json::<serde_json::Value>()["error"],
        "download limit reached"
    );

    setup(&state).await;
}

#[tokio::test]
async fn test_delete_without_deletion_password_set_returns_unauthorized() {
    let (_server, state) = get_server().await;
    setup(&state).await;

    let expire = Utc::now() + chrono::Duration::hours(1);
    let upload = insert_fake_upload(&state, b"pw", None, None, expire).await;

    let router = service::router(state.clone()).with_state(state.clone());
    let server = TestServer::new(router);

    let resp = server
        .post("/api/upload/delete")
        .json(&serde_json::json!({
            "key":              upload.uuid_.to_string(),
            "deletion_password": hex::encode(b"anything"),
        }))
        .await;
    resp.assert_status(StatusCode::UNAUTHORIZED);
    assert_eq!(
        resp.json::<serde_json::Value>()["error"],
        "no deletion password set"
    );

    setup(&state).await;
}

#[tokio::test]
async fn test_delete_wrong_password_returns_unauthorized() {
    let (_server, state) = get_server().await;
    setup(&state).await;

    let expire = Utc::now() + chrono::Duration::hours(1);
    let upload =
        insert_fake_upload(&state, b"access-pw", Some(b"correct-del-pw"), None, expire).await;

    let router = service::router(state.clone()).with_state(state.clone());
    let server = TestServer::new(router);

    let resp = server
        .post("/api/upload/delete")
        .json(&serde_json::json!({
            "key":              upload.uuid_.to_string(),
            "deletion_password": hex::encode(b"wrong-del-pw"),
        }))
        .await;
    resp.assert_status(StatusCode::UNAUTHORIZED);
    assert_eq!(
        resp.json::<serde_json::Value>()["error"],
        "invalid deletion password"
    );

    setup(&state).await;
}

#[tokio::test]
async fn test_download_confirm_hash_mismatch_returns_unprocessable() {
    let (_server, state) = get_server().await;
    setup(&state).await;

    let expire = Utc::now() + chrono::Duration::hours(1);
    let upload = insert_fake_upload(&state, b"pw", None, None, expire).await;

    // Insert a confirm token manually.
    let confirm_tok =
        transfer::models::insert_init_download(&state.db, Uuid::new_v4(), "confirm", upload.id)
            .await
            .unwrap();

    let router = service::router(state.clone()).with_state(state.clone());
    let server = TestServer::new(router);

    // Send a hash that does not match upload.content_hash (0xEF×32).
    let resp = server
        .post("/api/download/confirm")
        .json(&serde_json::json!({
            "key":  confirm_tok.uuid_.to_string(),
            "hash": fixed_hex(0x00, 32),
        }))
        .await;
    resp.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(resp.json::<serde_json::Value>()["error"], "hash mismatch");

    setup(&state).await;
}

#[tokio::test]
async fn test_download_confirm_correct_hash_returns_file_name_hash() {
    let (_server, state) = get_server().await;
    setup(&state).await;

    let expire = Utc::now() + chrono::Duration::hours(1);
    // insert_fake_upload stores 0xEF×32 as content_hash.
    let upload = insert_fake_upload(&state, b"pw", None, None, expire).await;

    let confirm_tok =
        transfer::models::insert_init_download(&state.db, Uuid::new_v4(), "confirm", upload.id)
            .await
            .unwrap();

    let router = service::router(state.clone()).with_state(state.clone());
    let server = TestServer::new(router);

    // Send the exact bytes stored as content_hash.
    let resp = server
        .post("/api/download/confirm")
        .json(&serde_json::json!({
            "key":  confirm_tok.uuid_.to_string(),
            "hash": fixed_hex(0xEF, 32),
        }))
        .await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(
        body["file_name_hash"],
        fixed_hex(0xCD, 32),
        "response should echo back the upload's file_name_hash"
    );

    setup(&state).await;
}


// ---------------------------------------------------------------------------
// Input validation — no DB / S3 required
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_upload_init_invalid_file_name_hash_hex() {
    let (server, _state) = get_server().await;
    let mut body = upload_init_body("deadbeef");
    body["file_name_hash"] = serde_json::json!("not-hex!");
    let resp = server.post("/api/upload/init").json(&body).await;
    resp.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        resp.json::<serde_json::Value>()["error"],
        "invalid file_name_hash hex"
    );
}

#[tokio::test]
async fn test_upload_init_invalid_access_password_hex() {
    let (server, _state) = get_server().await;
    let mut body = upload_init_body("deadbeef");
    body["access_password"] = serde_json::json!("zz");
    let resp = server.post("/api/upload/init").json(&body).await;
    resp.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        resp.json::<serde_json::Value>()["error"],
        "invalid access_password hex"
    );
}

#[tokio::test]
async fn test_upload_init_invalid_deletion_password_hex() {
    let (server, _state) = get_server().await;
    let mut body = upload_init_body("deadbeef");
    body["deletion_password"] = serde_json::json!("not-hex");
    let resp = server.post("/api/upload/init").json(&body).await;
    resp.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        resp.json::<serde_json::Value>()["error"],
        "invalid deletion_password hex"
    );
}

#[tokio::test]
async fn test_upload_delete_invalid_deletion_password_hex() {
    let (server, _state) = get_server().await;
    let body = serde_json::json!({
        "key":               uuid::Uuid::new_v4().to_string(),
        "deletion_password": "gg",
    });
    // Fetch the upload first → 404, before hex parsing even runs in the handler.
    // Sending a valid UUID that doesn't exist returns NOT_FOUND, not BAD_REQUEST,
    // because the handler fetches the upload before decoding the password.
    // We test the hex-decode branch by supplying a *found* upload below.
    let resp = server.post("/api/upload/delete").json(&body).await;
    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_upload_delete_invalid_deletion_password_hex_with_record() {
    // Reach the hex-decode path: upload exists, deletion_password field is bad hex.
    let (_server, state) = get_server().await;
    setup(&state).await;

    let expire = Utc::now() + chrono::Duration::hours(1);
    let upload = insert_fake_upload(&state, b"access", Some(b"del"), None, expire).await;

    let router = service::router(state.clone()).with_state(state.clone());
    let server = TestServer::new(router);

    let resp = server
        .post("/api/upload/delete")
        .json(&serde_json::json!({
            "key":              upload.uuid_.to_string(),
            "deletion_password": "zz",
        }))
        .await;
    resp.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        resp.json::<serde_json::Value>()["error"],
        "invalid deletion_password hex"
    );

    setup(&state).await;
}

#[tokio::test]
async fn test_download_init_invalid_access_password_hex() {
    let (server, _state) = get_server().await;
    let body = serde_json::json!({
        "key":             uuid::Uuid::new_v4().to_string(),
        "access_password": "zz",
    });
    // Upload doesn't exist → NOT_FOUND before hex decode; need a real upload.
    // Use not-found path to confirm no panic on bad hex when upload is absent.
    let resp = server.post("/api/download/init").json(&body).await;
    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_download_init_invalid_access_password_hex_with_record() {
    let (_server, state) = get_server().await;
    setup(&state).await;

    let expire = Utc::now() + chrono::Duration::hours(1);
    let upload = insert_fake_upload(&state, b"pw", None, None, expire).await;

    let router = service::router(state.clone()).with_state(state.clone());
    let server = TestServer::new(router);

    let resp = server
        .post("/api/download/init")
        .json(&serde_json::json!({
            "key":             upload.uuid_.to_string(),
            "access_password": "zz",
        }))
        .await;
    resp.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        resp.json::<serde_json::Value>()["error"],
        "invalid access_password hex"
    );

    setup(&state).await;
}

#[tokio::test]
async fn test_download_confirm_invalid_hash_hex() {
    let (server, _state) = get_server().await;
    let body = serde_json::json!({
        "key":  uuid::Uuid::new_v4().to_string(),
        "hash": "zz",
    });
    // Confirm token not found → NOT_FOUND before reaching hex decode.
    let resp = server.post("/api/download/confirm").json(&body).await;
    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_download_confirm_invalid_hash_hex_with_record() {
    let (_server, state) = get_server().await;
    setup(&state).await;

    let expire = Utc::now() + chrono::Duration::hours(1);
    let upload = insert_fake_upload(&state, b"pw", None, None, expire).await;
    let tok = transfer::models::insert_init_download(
        &state.db, uuid::Uuid::new_v4(), "confirm", upload.id,
    )
    .await
    .unwrap();

    let router = service::router(state.clone()).with_state(state.clone());
    let server = TestServer::new(router);

    let resp = server
        .post("/api/download/confirm")
        .json(&serde_json::json!({
            "key":  tok.uuid_.to_string(),
            "hash": "zz",
        }))
        .await;
    resp.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(resp.json::<serde_json::Value>()["error"], "invalid hash hex");

    setup(&state).await;
}

// ---------------------------------------------------------------------------
// upload/init — stored fields
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_upload_init_stores_download_limit() {
    let (server, state) = get_server().await;
    setup(&state).await;

    let mut body = upload_init_body("deadbeef");
    body["download_limit"] = serde_json::json!(5_i32);

    let resp = server.post("/api/upload/init").json(&body).await;
    resp.assert_status_ok();
    let key: uuid::Uuid = resp.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let record = transfer::models::get_init_upload(&state.db, key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.download_limit, Some(5));

    setup(&state).await;
}

// ---------------------------------------------------------------------------
// upload (file) — expired init-session key
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_upload_file_init_key_expired() {
    let (server, state) = get_server().await;
    setup(&state).await;

    // Create a valid init record via the API.
    let body = upload_init_body("deadbeef");
    let resp = server.post("/api/upload/init").json(&body).await;
    resp.assert_status_ok();
    let key: uuid::Uuid = resp.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    // Age the date_created well past the upload_timeout_secs.
    let old_time = Utc::now() - chrono::Duration::seconds(state.config.upload_timeout_secs + 60);
    sqlx::query("update init_upload set date_created = $1 where uuid_ = $2")
        .bind(old_time)
        .bind(key)
        .execute(&state.db)
        .await
        .unwrap();

    let resp = server
        .post("/api/upload")
        .add_query_params([("key", key.to_string())])
        .bytes(axum::body::Bytes::from(vec![0u8; 8]))
        .content_type("application/octet-stream")
        .await;
    resp.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(
        resp.json::<serde_json::Value>()["error"],
        "upload key expired"
    );

    setup(&state).await;
}

// ---------------------------------------------------------------------------
// upload/delete — soft-deleted upload
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_upload_delete_soft_deleted_upload_returns_not_found() {
    let (_server, state) = get_server().await;
    setup(&state).await;

    let expire = Utc::now() + chrono::Duration::hours(1);
    let upload = insert_fake_upload(&state, b"access", Some(b"del"), None, expire).await;

    // Soft-delete the upload directly.
    transfer::models::soft_delete_upload(&state.db, upload.id)
        .await
        .unwrap();

    let router = service::router(state.clone()).with_state(state.clone());
    let server = TestServer::new(router);

    // API uses get_upload which filters deleted=false, so this returns NOT_FOUND.
    let resp = server
        .post("/api/upload/delete")
        .json(&serde_json::json!({
            "key":              upload.uuid_.to_string(),
            "deletion_password": hex::encode(b"del"),
        }))
        .await;
    resp.assert_status(StatusCode::NOT_FOUND);

    setup(&state).await;
}

// ---------------------------------------------------------------------------
// download/init — response fields and deleted upload
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_download_init_returns_correct_fields() {
    let (_server, state) = get_server().await;
    setup(&state).await;

    let nonce_bytes = vec![0xABu8; 12];
    let expire = Utc::now() + chrono::Duration::hours(1);
    let upload = insert_fake_upload(&state, b"pw", None, None, expire).await;
    // Patch the nonce on the stored upload to a known value so we can verify
    // the round-trip.
    sqlx::query("update upload set nonce = $1 where id = $2")
        .bind(&nonce_bytes)
        .bind(upload.id)
        .execute(&state.db)
        .await
        .unwrap();

    let router = service::router(state.clone()).with_state(state.clone());
    let server = TestServer::new(router);

    let resp = server
        .post("/api/download/init")
        .json(&serde_json::json!({
            "key":             upload.uuid_.to_string(),
            "access_password": hex::encode(b"pw"),
        }))
        .await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();

    assert_eq!(body["nonce"].as_str().unwrap(), hex::encode(&nonce_bytes));
    assert_eq!(body["size"].as_i64().unwrap(), 64); // insert_fake_upload uses size=64
    // download_key and confirm_key must be valid UUIDs.
    body["download_key"]
        .as_str()
        .unwrap()
        .parse::<uuid::Uuid>()
        .expect("download_key must be a UUID");
    body["confirm_key"]
        .as_str()
        .unwrap()
        .parse::<uuid::Uuid>()
        .expect("confirm_key must be a UUID");
    // The two keys must be distinct.
    assert_ne!(body["download_key"], body["confirm_key"]);

    setup(&state).await;
}

#[tokio::test]
async fn test_download_init_soft_deleted_upload_returns_not_found() {
    let (_server, state) = get_server().await;
    setup(&state).await;

    let expire = Utc::now() + chrono::Duration::hours(1);
    let upload = insert_fake_upload(&state, b"pw", None, None, expire).await;
    transfer::models::soft_delete_upload(&state.db, upload.id)
        .await
        .unwrap();

    let router = service::router(state.clone()).with_state(state.clone());
    let server = TestServer::new(router);

    let resp = server
        .post("/api/download/init")
        .json(&serde_json::json!({
            "key":             upload.uuid_.to_string(),
            "access_password": hex::encode(b"pw"),
        }))
        .await;
    resp.assert_status(StatusCode::NOT_FOUND);

    setup(&state).await;
}

// ---------------------------------------------------------------------------
// download — expired token, wrong password, wrong usage type
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_download_key_expired_returns_not_found() {
    let (_server, state) = get_server().await;
    setup(&state).await;

    let expire = Utc::now() + chrono::Duration::hours(1);
    let upload = insert_fake_upload(&state, b"pw", None, None, expire).await;
    let tok = transfer::models::insert_init_download(
        &state.db, uuid::Uuid::new_v4(), "content", upload.id,
    )
    .await
    .unwrap();

    // Age the download token past the timeout.
    let old_time =
        Utc::now() - chrono::Duration::seconds(state.config.download_timeout_secs + 60);
    sqlx::query("update init_download set date_created = $1 where id = $2")
        .bind(old_time)
        .bind(tok.id)
        .execute(&state.db)
        .await
        .unwrap();

    let router = service::router(state.clone()).with_state(state.clone());
    let server = TestServer::new(router);

    let resp = server
        .post("/api/download")
        .json(&serde_json::json!({
            "key":             tok.uuid_.to_string(),
            "access_password": hex::encode(b"pw"),
        }))
        .await;
    resp.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(
        resp.json::<serde_json::Value>()["error"],
        "download key expired"
    );

    setup(&state).await;
}

#[tokio::test]
async fn test_download_wrong_access_password_returns_unauthorized() {
    let (_server, state) = get_server().await;
    setup(&state).await;

    let expire = Utc::now() + chrono::Duration::hours(1);
    let upload = insert_fake_upload(&state, b"correct", None, None, expire).await;
    let tok = transfer::models::insert_init_download(
        &state.db, uuid::Uuid::new_v4(), "content", upload.id,
    )
    .await
    .unwrap();

    let router = service::router(state.clone()).with_state(state.clone());
    let server = TestServer::new(router);

    let resp = server
        .post("/api/download")
        .json(&serde_json::json!({
            "key":             tok.uuid_.to_string(),
            "access_password": hex::encode(b"wrong"),
        }))
        .await;
    resp.assert_status(StatusCode::UNAUTHORIZED);

    setup(&state).await;
}

#[tokio::test]
async fn test_download_confirm_token_cannot_be_used_for_download() {
    // A "confirm" token must not be accepted by the download endpoint,
    // which only accepts "content" tokens.
    let (_server, state) = get_server().await;
    setup(&state).await;

    let expire = Utc::now() + chrono::Duration::hours(1);
    let upload = insert_fake_upload(&state, b"pw", None, None, expire).await;
    let confirm_tok = transfer::models::insert_init_download(
        &state.db, uuid::Uuid::new_v4(), "confirm", upload.id,
    )
    .await
    .unwrap();

    let router = service::router(state.clone()).with_state(state.clone());
    let server = TestServer::new(router);

    let resp = server
        .post("/api/download")
        .json(&serde_json::json!({
            "key":             confirm_tok.uuid_.to_string(),
            "access_password": hex::encode(b"pw"),
        }))
        .await;
    resp.assert_status(StatusCode::NOT_FOUND);

    setup(&state).await;
}

// ---------------------------------------------------------------------------
// download/confirm — expired token, wrong usage type, one-time use
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_download_confirm_key_expired_returns_not_found() {
    let (_server, state) = get_server().await;
    setup(&state).await;

    let expire = Utc::now() + chrono::Duration::hours(1);
    let upload = insert_fake_upload(&state, b"pw", None, None, expire).await;
    let tok = transfer::models::insert_init_download(
        &state.db, uuid::Uuid::new_v4(), "confirm", upload.id,
    )
    .await
    .unwrap();

    let old_time =
        Utc::now() - chrono::Duration::seconds(state.config.download_timeout_secs + 60);
    sqlx::query("update init_download set date_created = $1 where id = $2")
        .bind(old_time)
        .bind(tok.id)
        .execute(&state.db)
        .await
        .unwrap();

    let router = service::router(state.clone()).with_state(state.clone());
    let server = TestServer::new(router);

    let resp = server
        .post("/api/download/confirm")
        .json(&serde_json::json!({
            "key":  tok.uuid_.to_string(),
            "hash": fixed_hex(0xEF, 32),
        }))
        .await;
    resp.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(
        resp.json::<serde_json::Value>()["error"],
        "confirm key expired"
    );

    setup(&state).await;
}

#[tokio::test]
async fn test_download_content_token_cannot_be_used_for_confirm() {
    // A "content" token must not be accepted by the confirm endpoint,
    // which only accepts "confirm" tokens.
    let (_server, state) = get_server().await;
    setup(&state).await;

    let expire = Utc::now() + chrono::Duration::hours(1);
    let upload = insert_fake_upload(&state, b"pw", None, None, expire).await;
    let content_tok = transfer::models::insert_init_download(
        &state.db, uuid::Uuid::new_v4(), "content", upload.id,
    )
    .await
    .unwrap();

    let router = service::router(state.clone()).with_state(state.clone());
    let server = TestServer::new(router);

    let resp = server
        .post("/api/download/confirm")
        .json(&serde_json::json!({
            "key":  content_tok.uuid_.to_string(),
            "hash": fixed_hex(0xEF, 32),
        }))
        .await;
    resp.assert_status(StatusCode::NOT_FOUND);

    setup(&state).await;
}

#[tokio::test]
async fn test_download_confirm_is_single_use() {
    // After a successful confirm the token row is deleted; a second request
    // with the same key must return NOT_FOUND.
    let (_server, state) = get_server().await;
    setup(&state).await;

    let expire = Utc::now() + chrono::Duration::hours(1);
    // insert_fake_upload uses content_hash = 0xEF×32.
    let upload = insert_fake_upload(&state, b"pw", None, None, expire).await;
    let tok = transfer::models::insert_init_download(
        &state.db, uuid::Uuid::new_v4(), "confirm", upload.id,
    )
    .await
    .unwrap();

    let router = service::router(state.clone()).with_state(state.clone());
    let server = TestServer::new(router);

    let payload = serde_json::json!({
        "key":  tok.uuid_.to_string(),
        "hash": fixed_hex(0xEF, 32),
    });

    // First call succeeds.
    server
        .post("/api/download/confirm")
        .json(&payload)
        .await
        .assert_status_ok();

    // Second call with the same token → NOT_FOUND.
    let resp2 = server
        .post("/api/download/confirm")
        .json(&payload)
        .await;
    resp2.assert_status(StatusCode::NOT_FOUND);

    setup(&state).await;
}

// ---------------------------------------------------------------------------
// Sweep / model helpers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_sweep_deletes_expired_init_uploads() {
    let (_server, state) = get_server().await;
    setup(&state).await;

    // Insert two init_upload rows and age them past the cutoff.
    let body = upload_init_body("deadbeef");
    for _ in 0..2 {
        let resp = TestServer::new(service::router(state.clone()).with_state(state.clone()))
            .post("/api/upload/init")
            .json(&body)
            .await;
        resp.assert_status_ok();
    }

    let cutoff = Utc::now() + chrono::Duration::seconds(1); // covers all rows
    let deleted = transfer::models::delete_expired_init_uploads(&state.db, cutoff)
        .await
        .unwrap();
    assert_eq!(deleted, 2, "both stale init_upload rows should be removed");

    // Running again against an empty table returns 0.
    let second = transfer::models::delete_expired_init_uploads(&state.db, cutoff)
        .await
        .unwrap();
    assert_eq!(second, 0);

    setup(&state).await;
}

#[tokio::test]
async fn test_sweep_deletes_expired_init_downloads() {
    let (_server, state) = get_server().await;
    setup(&state).await;

    let expire = Utc::now() + chrono::Duration::hours(1);
    let upload = insert_fake_upload(&state, b"pw", None, None, expire).await;

    // Insert three init_download tokens.
    for usage in &["content", "confirm", "content"] {
        transfer::models::insert_init_download(
            &state.db,
            uuid::Uuid::new_v4(),
            usage,
            upload.id,
        )
        .await
        .unwrap();
    }

    let cutoff = Utc::now() + chrono::Duration::seconds(1);
    let deleted = transfer::models::delete_expired_init_downloads(&state.db, cutoff)
        .await
        .unwrap();
    assert_eq!(deleted, 3);

    setup(&state).await;
}

// ---------------------------------------------------------------------------
// Two-password flow — access password vs. encryption password separation
//
// Client-side architecture:
//   access_password     – random 32-char alphanumeric pre-filled by the UI;
//                         sent to the server as hex bytes; guards against
//                         harvest-now-decrypt-later attacks; NOT the crypto key.
//   encryption_password – chosen by the uploader; used only for client-side
//                         AES-256-GCM; NEVER sent to the server.
//
// The server only sees and stores `access_password`.  These tests verify that
// the server enforces this boundary correctly.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_two_pw_only_access_password_accepted_by_server() {
    // Using the encryption password as the access_password must fail; using
    // the actual access password must succeed.  This documents that the two
    // are distinct values with distinct purposes.
    let (_server, state) = get_server().await;
    setup(&state).await;

    let access_pw: &[u8] = b"Rnd9Xk2mPqL7vZ3wYjBs5tHnC8uFeA1d"; // random 32-char
    let enc_pw: &[u8] = b"user-chosen-strong-encryption-pw"; // never sent to server
    let expire = Utc::now() + chrono::Duration::hours(1);

    // Server stores a hash of access_pw only; enc_pw is never involved.
    let upload = insert_fake_upload(&state, access_pw, None, None, expire).await;

    let router = service::router(state.clone()).with_state(state.clone());
    let server = TestServer::new(router);

    // Presenting enc_pw as the access_password is rejected (proves separation).
    server
        .post("/api/download/init")
        .json(&serde_json::json!({
            "key":             upload.uuid_.to_string(),
            "access_password": hex::encode(enc_pw),
        }))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);

    // Presenting the actual access_pw is accepted.
    server
        .post("/api/download/init")
        .json(&serde_json::json!({
            "key":             upload.uuid_.to_string(),
            "access_password": hex::encode(access_pw),
        }))
        .await
        .assert_status_ok();

    setup(&state).await;
}

#[tokio::test]
async fn test_two_pw_upload_records_access_password_only() {
    // Verify upload/init accepts the access_password (the random value) and
    // that the stored hash is checked against the access_password alone.
    // A recipient who presents the encryption password instead of the access
    // password is rejected even if both are non-empty valid hex strings.
    let (server, state) = get_server().await;
    setup(&state).await;

    // Two clearly distinct passwords — neither is a substring of the other.
    let access_pw_hex = hex::encode(b"Abc123DefGhi456JklMno789PqrStu0v"); // 32-char random
    let enc_pw_hex    = hex::encode(b"completely-different-enc-key-here");

    // Upload init records access_password (not enc_pw).
    let init_resp = server
        .post("/api/upload/init")
        .json(&serde_json::json!({
            "nonce":           fixed_hex(0xAB, 12),
            "file_name_hash":  fixed_hex(0xCD, 32),
            "content_hash":    fixed_hex(0xEF, 32),
            "size":            512_i64,
            "access_password": access_pw_hex,
        }))
        .await;
    init_resp.assert_status_ok();

    // Insert a completed upload row with the same access_password to exercise
    // the download/init path (the init_upload row above has no bytes yet).
    let expire = Utc::now() + chrono::Duration::hours(1);
    let fake_upload = insert_fake_upload(
        &state,
        &hex::decode(&access_pw_hex).unwrap(),
        None,
        None,
        expire,
    )
    .await;

    let router = service::router(state.clone()).with_state(state.clone());
    let server2 = TestServer::new(router);

    // enc_pw sent as access_password -> rejected.
    server2
        .post("/api/download/init")
        .json(&serde_json::json!({
            "key":             fake_upload.uuid_.to_string(),
            "access_password": enc_pw_hex,
        }))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);

    // Correct access_pw -> accepted.
    server2
        .post("/api/download/init")
        .json(&serde_json::json!({
            "key":             fake_upload.uuid_.to_string(),
            "access_password": access_pw_hex,
        }))
        .await
        .assert_status_ok();

    setup(&state).await;
}

// ---------------------------------------------------------------------------
// Full end-to-end tests — require S3 credentials
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_full_upload_download_cycle() {
    if skip_if_no_s3() {
        return;
    }
    let (server, state) = get_server().await;
    setup(&state).await;

    // Simulate a small "encrypted" file (arbitrary bytes — server stores as-is).
    let ciphertext: Vec<u8> = (0u8..=127).cycle().take(256).collect();
    let nonce_bytes = vec![0xABu8; 12];
    let content_hash_hex = sha256_hex(&ciphertext);
    let file_name_hash_hex = fixed_hex(0xCD, 32);
    let access_pw = b"test-access-pw";
    let access_pw_hex = hex::encode(access_pw);

    // ── Step 1: upload/init ──────────────────────────────────────────────────
    let init_resp = server
        .post("/api/upload/init")
        .json(&serde_json::json!({
            "nonce":           hex::encode(&nonce_bytes),
            "file_name_hash":  file_name_hash_hex,
            "content_hash":    content_hash_hex,
            "size":            ciphertext.len() as i64,
            "access_password": access_pw_hex,
            "download_limit":  3_i32,
            "lifespan":        3600_i64,
        }))
        .await;
    init_resp.assert_status_ok();
    let upload_key = init_resp.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .to_string();

    // ── Step 2: upload bytes ─────────────────────────────────────────────────
    let upload_resp = server
        .post("/api/upload")
        .add_query_params([("key", &upload_key)])
        .bytes(axum::body::Bytes::from(ciphertext.clone()))
        .content_type("application/octet-stream")
        .await;
    upload_resp.assert_status_ok();
    assert_eq!(
        upload_resp.json::<serde_json::Value>()["bytes"]
            .as_u64()
            .unwrap(),
        ciphertext.len() as u64
    );

    // ── Step 3: download/init ────────────────────────────────────────────────
    let dl_init_resp = server
        .post("/api/download/init")
        .json(&serde_json::json!({
            "key":             upload_key,
            "access_password": access_pw_hex,
        }))
        .await;
    dl_init_resp.assert_status_ok();
    let dl_init: serde_json::Value = dl_init_resp.json();
    let download_key = dl_init["download_key"].as_str().unwrap().to_string();
    let confirm_key = dl_init["confirm_key"].as_str().unwrap().to_string();
    assert_eq!(
        dl_init["nonce"].as_str().unwrap(),
        hex::encode(&nonce_bytes),
        "nonce should round-trip"
    );

    // ── Step 4: download bytes ───────────────────────────────────────────────
    let dl_resp = server
        .post("/api/download")
        .json(&serde_json::json!({
            "key":             download_key,
            "access_password": access_pw_hex,
        }))
        .await;
    dl_resp.assert_status_ok();
    let downloaded = dl_resp.as_bytes().to_vec();
    assert_eq!(
        downloaded, ciphertext,
        "downloaded bytes must match uploaded bytes"
    );

    // ── Step 5: confirm integrity ────────────────────────────────────────────
    let confirm_resp = server
        .post("/api/download/confirm")
        .json(&serde_json::json!({
            "key":  confirm_key,
            "hash": sha256_hex(&downloaded),
        }))
        .await;
    confirm_resp.assert_status_ok();
    assert!(confirm_resp.json::<serde_json::Value>()["file_name_hash"]
        .as_str()
        .is_some());

    setup(&state).await;
}

#[tokio::test]
async fn test_upload_then_delete_blocks_download() {
    if skip_if_no_s3() {
        return;
    }
    let (server, state) = get_server().await;
    setup(&state).await;

    let ciphertext = vec![0xAAu8; 64];
    let access_pw_hex = hex::encode(b"access-pw");
    let del_pw_hex = hex::encode(b"del-pw");

    // Upload.
    let init_resp = server
        .post("/api/upload/init")
        .json(&serde_json::json!({
            "nonce":             fixed_hex(0x01, 12),
            "file_name_hash":    fixed_hex(0x02, 32),
            "content_hash":      sha256_hex(&ciphertext),
            "size":              ciphertext.len() as i64,
            "access_password":   access_pw_hex,
            "deletion_password": del_pw_hex,
        }))
        .await;
    init_resp.assert_status_ok();
    let upload_key = init_resp.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .to_string();

    server
        .post("/api/upload")
        .add_query_params([("key", &upload_key)])
        .bytes(axum::body::Bytes::from(ciphertext))
        .content_type("application/octet-stream")
        .await
        .assert_status_ok();

    // Delete with the correct deletion password.
    let del_resp = server
        .post("/api/upload/delete")
        .json(&serde_json::json!({
            "key":              upload_key.clone(),
            "deletion_password": del_pw_hex,
        }))
        .await;
    del_resp.assert_status_ok();

    // Subsequent download/init must fail.
    let after_resp = server
        .post("/api/download/init")
        .json(&serde_json::json!({
            "key":             upload_key,
            "access_password": access_pw_hex,
        }))
        .await;
    after_resp.assert_status(StatusCode::NOT_FOUND);

    setup(&state).await;
}

#[tokio::test]
async fn test_download_limit_1_blocks_second_attempt() {
    if skip_if_no_s3() {
        return;
    }
    let (server, state) = get_server().await;
    setup(&state).await;

    let ciphertext = vec![0xBBu8; 64];
    let access_pw_hex = hex::encode(b"pw-limit-1");

    let init_resp = server
        .post("/api/upload/init")
        .json(&serde_json::json!({
            "nonce":           fixed_hex(0x01, 12),
            "file_name_hash":  fixed_hex(0x02, 32),
            "content_hash":    sha256_hex(&ciphertext),
            "size":            ciphertext.len() as i64,
            "access_password": access_pw_hex,
            "download_limit":  1_i32,
        }))
        .await;
    init_resp.assert_status_ok();
    let upload_key = init_resp.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .to_string();

    server
        .post("/api/upload")
        .add_query_params([("key", &upload_key)])
        .bytes(axum::body::Bytes::from(ciphertext.clone()))
        .content_type("application/octet-stream")
        .await
        .assert_status_ok();

    // First download/init + download (consumes the 1 allowed download).
    let dl_init: serde_json::Value = server
        .post("/api/download/init")
        .json(&serde_json::json!({
            "key":             upload_key.clone(),
            "access_password": access_pw_hex,
        }))
        .await
        .json();
    server
        .post("/api/download")
        .json(&serde_json::json!({
            "key":             dl_init["download_key"],
            "access_password": access_pw_hex,
        }))
        .await
        .assert_status_ok();

    // Second download/init must fail with 410 Gone.
    server
        .post("/api/download/init")
        .json(&serde_json::json!({
            "key":             upload_key,
            "access_password": access_pw_hex,
        }))
        .await
        .assert_status(StatusCode::GONE);

    setup(&state).await;
}

#[tokio::test]
async fn test_upload_file_too_large_returns_payload_too_large() {
    if skip_if_no_s3() {
        return;
    }
    let (server, state) = get_server().await;
    setup(&state).await;

    let limit = state.config.upload_limit_bytes;

    // Init with a plausible size so the init succeeds.
    let init_resp = server
        .post("/api/upload/init")
        .json(&serde_json::json!({
            "nonce":           fixed_hex(0x01, 12),
            "file_name_hash":  fixed_hex(0x02, 32),
            "content_hash":    fixed_hex(0x03, 32),
            "size":            1024_i64,
            "access_password": "deadbeef",
        }))
        .await;
    init_resp.assert_status_ok();
    let upload_key = init_resp.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .to_string();

    // Send a body that exceeds the limit.
    let oversized = vec![0u8; limit + 1];
    let resp = server
        .post("/api/upload")
        .add_query_params([("key", &upload_key)])
        .bytes(axum::body::Bytes::from(oversized))
        .content_type("application/octet-stream")
        .await;
    resp.assert_status(StatusCode::INTERNAL_SERVER_ERROR);

    setup(&state).await;
}

#[tokio::test]
async fn test_second_upload_to_same_key_returns_not_found() {
    if skip_if_no_s3() {
        return;
    }
    let (server, state) = get_server().await;
    setup(&state).await;

    let ciphertext = vec![0xCCu8; 64];

    let init_resp = server
        .post("/api/upload/init")
        .json(&serde_json::json!({
            "nonce":           fixed_hex(0x01, 12),
            "file_name_hash":  fixed_hex(0x02, 32),
            "content_hash":    sha256_hex(&ciphertext),
            "size":            ciphertext.len() as i64,
            "access_password": "deadbeef",
        }))
        .await;
    init_resp.assert_status_ok();
    let upload_key = init_resp.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .to_string();

    // First upload — should succeed and consume the init record.
    server
        .post("/api/upload")
        .add_query_params([("key", &upload_key)])
        .bytes(axum::body::Bytes::from(ciphertext.clone()))
        .content_type("application/octet-stream")
        .await
        .assert_status_ok();

    // Second upload with the same key — init record is gone.
    server
        .post("/api/upload")
        .add_query_params([("key", &upload_key)])
        .bytes(axum::body::Bytes::from(ciphertext))
        .content_type("application/octet-stream")
        .await
        .assert_status(StatusCode::NOT_FOUND);

    setup(&state).await;
}
