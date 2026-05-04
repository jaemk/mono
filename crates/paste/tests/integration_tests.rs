use axum::http::StatusCode;
use axum_test::TestServer;
use paste::{service, Config, State};

fn set_workspace_root() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    std::env::set_current_dir(workspace_root).ok();
}

/// Initialise the paste service returning both the TestServer and the shared
/// State (needed for DB/S3 cleanup between tests).
async fn get_server() -> (TestServer, State) {
    set_workspace_root();
    let state = service::init(Config::load())
        .await
        .expect("failed to initialize paste state");
    let router = service::router(state.clone()).with_state(state.clone());
    (TestServer::new(router), state)
}

/// Wipe paste DB rows and their S3 objects.  Called at the **start** of every
/// DB-touching test so that prior-test debris is removed even after a panic.
async fn setup(state: &State) {
    paste::test_utils::clean_paste_db(&state.db, &state.s3, &state.config).await;
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
// ---------------------------------------------------------------------------
// Non-DB / non-S3 tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_status() {
    let (server, _state) = get_server().await;
    let response = server.get("/status").await;
    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert!(body.get("hash").is_some());
}

#[tokio::test]
async fn test_home_returns_html() {
    let (server, _state) = get_server().await;
    let response = server.get("/").await;
    response.assert_status_ok();
    assert!(response.text().contains("html"));
}

#[tokio::test]
async fn test_new_paste_too_large_returns_413() {
    // This test never reaches S3 (size check happens first) — no guard needed.
    let (server, _state) = get_server().await;
    let big_body = "x".repeat(2 * 1_000_001);
    let response = server.post("/new").text(big_body).await;
    response.assert_status(StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        response.json::<serde_json::Value>()["error"],
        "Upload too large"
    );
}

#[tokio::test]
async fn test_fetch_paste_json_not_found() {
    let (server, _state) = get_server().await;
    let response = server.get("/json/definitely-does-not-exist-xyz999").await;
    response.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(
        response.json::<serde_json::Value>()["error"],
        "Paste not found"
    );
}

#[tokio::test]
async fn test_fetch_paste_raw_not_found() {
    let (server, _state) = get_server().await;
    let response = server.get("/raw/definitely-does-not-exist-xyz998").await;
    response.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(
        response.json::<serde_json::Value>()["error"],
        "Paste not found"
    );
}

#[tokio::test]
async fn test_view_paste_html_not_found_falls_back_to_home() {
    let (server, _state) = get_server().await;
    server
        .get("/definitely-does-not-exist-xyz997")
        .await
        .assert_status_ok();
}

// ---------------------------------------------------------------------------
// S3-backed paste tests (require AWS credentials)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_new_paste_returns_key() {
    if skip_if_no_s3() {
        return;
    }
    let (server, state) = get_server().await;
    setup(&state).await;
    let response = server.post("/new").text("hello, world").await;
    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert_eq!(body["message"], "success");
    assert!(!body["key"].as_str().unwrap_or("").is_empty());
    setup(&state).await;
}

#[tokio::test]
async fn test_new_paste_with_content_type_query() {
    if skip_if_no_s3() {
        return;
    }
    let (server, state) = get_server().await;
    setup(&state).await;
    let response = server
        .post("/new")
        .add_query_params([("type", "rust")])
        .text("fn main() {}")
        .await;
    response.assert_status_ok();
    let key = response.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let fetch = server.get(&format!("/json/{}", key)).await;
    fetch.assert_status_ok();
    assert_eq!(
        fetch.json::<serde_json::Value>()["paste"]["content_type"],
        "rust"
    );
    setup(&state).await;
}

#[tokio::test]
async fn test_fetch_paste_json() {
    if skip_if_no_s3() {
        return;
    }
    let (server, state) = get_server().await;
    setup(&state).await;
    let create = server.post("/new").text("test content").await;
    create.assert_status_ok();
    let key = create.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let fetch = server.get(&format!("/json/{}", key)).await;
    fetch.assert_status_ok();
    let body: serde_json::Value = fetch.json();
    assert_eq!(body["paste"]["content"], "test content");
    assert_eq!(body["paste"]["key"], key);
    setup(&state).await;
}

#[tokio::test]
async fn test_fetch_paste_raw() {
    if skip_if_no_s3() {
        return;
    }
    let (server, state) = get_server().await;
    setup(&state).await;
    let create = server.post("/new").text("raw content here").await;
    create.assert_status_ok();
    let key = create.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let fetch = server.get(&format!("/raw/{}", key)).await;
    fetch.assert_status_ok();
    assert_eq!(fetch.text(), "raw content here");
    setup(&state).await;
}

#[tokio::test]
async fn test_view_paste_html() {
    if skip_if_no_s3() {
        return;
    }
    let (server, state) = get_server().await;
    setup(&state).await;
    let create = server.post("/new").text("html test content").await;
    create.assert_status_ok();
    let key = create.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let view = server.get(&format!("/{}", key)).await;
    view.assert_status_ok();
    assert!(view.text().contains("html test content"));
    setup(&state).await;
}

#[tokio::test]
async fn test_encrypted_paste_fetch_json_with_correct_key() {
    if skip_if_no_s3() {
        return;
    }
    let (server, state) = get_server().await;
    setup(&state).await;
    let create = server
        .post("/new")
        .add_header("x-paste-encryption-key", "my-secret-password")
        .text("secret content")
        .await;
    create.assert_status_ok();
    let key = create.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let fetch = server
        .get(&format!("/json/{}", key))
        .add_header("x-paste-encryption-key", "my-secret-password")
        .await;
    fetch.assert_status_ok();
    assert_eq!(
        fetch.json::<serde_json::Value>()["paste"]["content"],
        "secret content"
    );
    setup(&state).await;
}

#[tokio::test]
async fn test_encrypted_paste_json_without_key_returns_404() {
    if skip_if_no_s3() {
        return;
    }
    let (server, state) = get_server().await;
    setup(&state).await;
    let create = server
        .post("/new")
        .add_header("x-paste-encryption-key", "my-secret-password")
        .text("hidden content")
        .await;
    create.assert_status_ok();
    let key = create.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .to_string();
    server
        .get(&format!("/json/{}", key))
        .await
        .assert_status(StatusCode::NOT_FOUND);
    setup(&state).await;
}

#[tokio::test]
async fn test_encrypted_paste_raw_without_key_returns_400() {
    if skip_if_no_s3() {
        return;
    }
    let (server, state) = get_server().await;
    setup(&state).await;
    let create = server
        .post("/new")
        .add_header("x-paste-encryption-key", "my-secret-password")
        .text("hidden raw content")
        .await;
    create.assert_status_ok();
    let key = create.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let fetch = server.get(&format!("/raw/{}", key)).await;
    fetch.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        fetch.json::<serde_json::Value>()["error"],
        "decryption_key_required"
    );
    setup(&state).await;
}

#[tokio::test]
async fn test_encrypted_paste_raw_with_wrong_key_returns_404() {
    if skip_if_no_s3() {
        return;
    }
    let (server, state) = get_server().await;
    setup(&state).await;
    let create = server
        .post("/new")
        .add_header("x-paste-encryption-key", "correct-password")
        .text("hidden raw content 2")
        .await;
    create.assert_status_ok();
    let key = create.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let fetch = server
        .get(&format!("/raw/{}", key))
        .add_header("x-paste-encryption-key", "wrong-password")
        .await;
    fetch.assert_status(StatusCode::NOT_FOUND);
    setup(&state).await;
}

#[tokio::test]
async fn test_encrypted_paste_html_without_key_shows_placeholder() {
    if skip_if_no_s3() {
        return;
    }
    let (server, state) = get_server().await;
    setup(&state).await;
    let create = server
        .post("/new")
        .add_header("x-paste-encryption-key", "my-html-password")
        .text("secret html content")
        .await;
    create.assert_status_ok();
    let key = create.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let view = server.get(&format!("/{}", key)).await;
    view.assert_status_ok();
    let text = view.text();
    assert!(
        text.contains("encrypted"),
        "should indicate encrypted state"
    );
    assert!(
        !text.contains("secret html content"),
        "should not expose content"
    );
    setup(&state).await;
}

#[tokio::test]
async fn test_view_paste_html_post_with_enc_key_in_body() {
    if skip_if_no_s3() {
        return;
    }
    let (server, state) = get_server().await;
    setup(&state).await;
    let create = server
        .post("/new")
        .add_header("x-paste-encryption-key", "body-key-password")
        .text("body key content")
        .await;
    create.assert_status_ok();
    let key = create.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let view = server
        .post(&format!("/{}", key))
        .json(&serde_json::json!({ "encryption_key": "body-key-password" }))
        .await;
    view.assert_status_ok();
    assert!(view.text().contains("body key content"));
    setup(&state).await;
}

#[tokio::test]
async fn test_paste_zero_ttl_expired_on_fetch() {
    if skip_if_no_s3() {
        return;
    }
    let (server, state) = get_server().await;
    setup(&state).await;
    let create = server
        .post("/new")
        .add_query_params([("ttl_seconds", "0")])
        .text("ephemeral content")
        .await;
    create.assert_status_ok();
    let key = create.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .to_string();
    server
        .get(&format!("/json/{}", key))
        .await
        .assert_status(StatusCode::NOT_FOUND);
    setup(&state).await;
}

#[tokio::test]
async fn test_paste_future_ttl_is_accessible() {
    if skip_if_no_s3() {
        return;
    }
    let (server, state) = get_server().await;
    setup(&state).await;
    let create = server
        .post("/new")
        .add_query_params([("ttl_seconds", "3600")])
        .text("not yet expired")
        .await;
    create.assert_status_ok();
    let key = create.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let fetch = server.get(&format!("/json/{}", key)).await;
    fetch.assert_status_ok();
    assert_eq!(
        fetch.json::<serde_json::Value>()["paste"]["content"],
        "not yet expired"
    );
    setup(&state).await;
}

#[tokio::test]
async fn test_fetch_paste_json_response_shape() {
    if skip_if_no_s3() {
        return;
    }
    let (server, state) = get_server().await;
    setup(&state).await;
    let create = server
        .post("/new")
        .add_query_params([("type", "python")])
        .text("print(\"hi\")")
        .await;
    create.assert_status_ok();
    let key = create.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let fetch = server.get(&format!("/json/{}", key)).await;
    fetch.assert_status_ok();
    let body = fetch.json::<serde_json::Value>();
    assert_eq!(body["paste"]["content"], "print(\"hi\")");
    assert_eq!(body["paste"]["content_type"], "python");
    assert_eq!(body["paste"]["key"], key);
    setup(&state).await;
}

// ---------------------------------------------------------------------------
// Direct model / DB tests (no HTTP layer)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_paste_exists_returns_false_on_empty_db() {
    if skip_if_no_s3() {
        return;
    }
    let (_server, state) = get_server().await;
    setup(&state).await;
    let exists = paste::models::Paste::exists(&state.db, "no-such-key")
        .await
        .expect("exists query should not fail");
    assert!(!exists, "fresh DB should have no pastes");
}

#[tokio::test]
async fn test_paste_exists_returns_true_after_insert() {
    if skip_if_no_s3() {
        return;
    }
    let (server, state) = get_server().await;
    setup(&state).await;
    let create = server.post("/new").text("exists check").await;
    create.assert_status_ok();
    let key = create.json::<serde_json::Value>()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let exists = paste::models::Paste::exists(&state.db, &key)
        .await
        .expect("exists query should not fail");
    assert!(exists, "paste should exist after creation");
    setup(&state).await;
}
