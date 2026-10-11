use axum::http::{header, StatusCode};
use axum::Router;
use axum_test::TestServer;

/// Serve the tick router nested the way mono mounts it. The asset paths are
/// relative to the workspace root.
fn get_server() -> TestServer {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent() // crates/
        .unwrap()
        .parent() // workspace root
        .unwrap();
    std::env::set_current_dir(workspace_root).unwrap();
    TestServer::new(Router::new().nest("/tick", tick::router()))
}

#[tokio::test]
async fn test_index_serves_the_page() {
    let server = get_server();
    let response = server.get("/tick").await;
    response.assert_status_ok();
    assert!(response
        .header(header::CONTENT_TYPE)
        .to_str()
        .unwrap()
        .starts_with("text/html"));
    let body = response.text();
    assert!(body.contains("<title>tick</title>"));
    for script in ["detector.js", "app.js", "app.css"] {
        assert!(
            body.contains(&format!("/tick/static/{script}")),
            "page links {script}"
        );
    }
}

#[tokio::test]
async fn test_static_assets_serve() {
    let server = get_server();
    for (path, kind) in [
        ("/tick/static/app.js", "javascript"),
        ("/tick/static/detector.js", "javascript"),
        ("/tick/static/worklet.js", "javascript"),
        ("/tick/static/app.css", "text/css"),
    ] {
        let response = server.get(path).await;
        response.assert_status_ok();
        let ct = response.header(header::CONTENT_TYPE);
        assert!(ct.to_str().unwrap().contains(kind), "{path}: {ct:?}");
    }
}

#[tokio::test]
async fn test_unknown_paths_are_not_found() {
    let server = get_server();
    for path in ["/tick/api", "/tick/static/missing.js", "/tick/x"] {
        let response = server.get(path).expect_failure().await;
        response.assert_status(StatusCode::NOT_FOUND);
    }
}
