use axum::http::{header, StatusCode};
use axum_test::TestServer;
use mono::{app, CONFIG};
use std::sync::Arc;

/// Build the full app using ephemeral config loaded from the current
/// environment (set by bin/test-db.sh during `make test`).
async fn get_server() -> TestServer {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent() // crates/
        .unwrap()
        .parent() // workspace root
        .unwrap();
    std::env::set_current_dir(workspace_root).unwrap();

    let spot_config = spot::Config::load();
    let spot_pool = common::db::init_pool(&spot_config.db_url)
        .await
        .expect("failed to initialize spot db pool");
    let spot_state = Arc::new(spot::SpotResources {
        pool: spot_pool,
        config: spot_config,
    });

    let paste_state = paste::service::init(paste::Config::load())
        .await
        .expect("failed to initialize paste state");

    TestServer::new(app(spot_state, paste_state))
}

#[tokio::test]
async fn test_status() {
    let server = get_server().await;
    let response = server.get("/status").await;
    response.assert_status_ok();
    response.assert_json(&serde_json::json!({
        "version": CONFIG.version,
        "ok": "ok"
    }));
}

#[tokio::test]
async fn test_robots_txt() {
    let server = get_server().await;
    let response = server.get("/robots.txt").await;
    response.assert_status_ok();
}

#[tokio::test]
async fn test_not_found() {
    let server = get_server().await;
    let response = server.get("/some-random-path").await;
    response.assert_status(StatusCode::NOT_FOUND);
    response.assert_json(&serde_json::json!({
        "code": 404,
        "message": "NOT_FOUND"
    }));
}

#[tokio::test]
async fn test_homepage_host() {
    let server = get_server().await;
    let response = server
        .get("/")
        .add_header(header::HOST, "kominick.com")
        .await;
    response.assert_status_ok();
    response.assert_header(header::CONTENT_TYPE, "text/html");
}

#[tokio::test]
async fn test_ugh_host_index_text() {
    let server = get_server().await;
    let response = server
        .get("/")
        .add_header(header::HOST, "ugh.kominick.com")
        .await;
    response.assert_status_ok();
    assert!(response.text().contains("business days left"));
}

#[tokio::test]
async fn test_ip_host() {
    let server = get_server().await;
    let response = server
        .get("/")
        .add_header(header::HOST, "ip.kominick.com")
        .add_header("fly-client-ip", "1.2.3.4")
        .await;
    response.assert_status_ok();
    assert_eq!(response.text(), "1.2.3.4\n");
}

#[tokio::test]
async fn test_git_redirect() {
    let server = get_server().await;
    let response = server
        .get("/some-repo")
        .add_header(header::HOST, "git.jaemk.me")
        .await;
    response.assert_status(StatusCode::TEMPORARY_REDIRECT);
    response.assert_header(header::LOCATION, "https://github.com/jaemk/some-repo");
}

#[tokio::test]
async fn test_favicon_ugh() {
    let server = get_server().await;
    let response = server
        .get("/favicon.ico")
        .add_header(header::HOST, "ugh.kominick.com")
        .await;
    response.assert_status_ok();
}

#[tokio::test]
async fn test_favicon_default() {
    let server = get_server().await;
    let response = server
        .get("/favicon.ico")
        .add_header(header::HOST, "kominick.com")
        .await;
    response.assert_status_ok();
}

#[tokio::test]
async fn test_static_css() {
    let server = get_server().await;
    let response = server.get("/static/css/site.css").await;
    response.assert_status_ok();
    response.assert_header(header::CONTENT_TYPE, "text/css");
}

#[tokio::test]
async fn test_default_host_behavior() {
    let server = get_server().await;
    let response = server
        .get("/")
        .add_header(header::HOST, "unknown.com")
        .await;
    response.assert_status_ok();
    response.assert_header(header::CONTENT_TYPE, "text/html");
}

#[tokio::test]
async fn test_outside_host() {
    let server = get_server().await;
    let response = server
        .get("/")
        .add_header(header::HOST, "outside.kominick.com")
        .await;
    // This might fail if network is not available, but let's see.
    // In the real environment it might fail with "Something went wrong..."
    response.assert_status_ok();
}
