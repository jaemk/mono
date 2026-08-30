use axum::http::{header, StatusCode};
use axum_extra::extract::cookie::Cookie;
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

    let mapour_state = mapour::service::init(mapour::Config::load())
        .await
        .expect("failed to initialize mapour state");

    TestServer::new(app(spot_state, paste_state, Some(mapour_state)))
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
async fn test_flip_spa_serves_on_both_routes() {
    let server = get_server().await;
    for path in ["/flip", "/flip/", "/flip/odds", "/flip/odds/"] {
        let response = server.get(path).await;
        response.assert_status_ok();
        response.assert_header(header::CONTENT_TYPE, "text/html");
        let body = response.text();
        assert!(body.contains("view-flip"), "{path} should serve the spa");
        assert!(body.contains("view-odds"), "{path} should serve the spa");
    }
}

#[tokio::test]
async fn test_flip_odds_default_is_fair() {
    let server = get_server().await;
    let response = server.get("/flip/api/odds").await;
    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert_eq!(body["heads_pct"], 50);
    assert_eq!(body["step"], 10);
}

#[tokio::test]
async fn test_flip_returns_a_side() {
    let server = get_server().await;
    let response = server.post("/flip/api/flip").await;
    response.assert_status_ok();
    let result = response.json::<serde_json::Value>()["result"]
        .as_str()
        .expect("result should be a string")
        .to_string();
    assert!(result == "heads" || result == "tails", "got {result}");
}

/// Saving the odds hands back a persistent, unreadable cookie scoped to the
/// flip pages.
#[tokio::test]
async fn test_flip_odds_sets_a_persistent_cookie() {
    let server = get_server().await;
    let response = server
        .post("/flip/api/odds")
        .json(&serde_json::json!({"heads_pct": 70}))
        .await;
    response.assert_status_ok();
    assert_eq!(response.json::<serde_json::Value>()["heads_pct"], 70);

    let cookie = response.cookie("flip_odds");
    assert_eq!(cookie.value(), "70");
    assert_eq!(cookie.http_only(), Some(true));
    assert_eq!(cookie.path(), Some("/flip"));
    assert!(cookie.max_age().is_some(), "should outlive the session");
}

/// A browser holding the cookie gets its odds back and has its flips steered;
/// the same server flips fair for a browser without one.
#[tokio::test]
async fn test_flip_odds_follow_the_browser() {
    let mut server = get_server().await;
    server.save_cookies();

    server
        .post("/flip/api/odds")
        .json(&serde_json::json!({"heads_pct": 100}))
        .await
        .assert_status_ok();

    let response = server.get("/flip/api/odds").await;
    assert_eq!(response.json::<serde_json::Value>()["heads_pct"], 100);
    for _ in 0..20 {
        let response = server.post("/flip/api/flip").await;
        assert_eq!(response.json::<serde_json::Value>()["result"], "heads");
    }

    server
        .post("/flip/api/odds")
        .json(&serde_json::json!({"heads_pct": 0}))
        .await
        .assert_status_ok();
    for _ in 0..20 {
        let response = server.post("/flip/api/flip").await;
        assert_eq!(response.json::<serde_json::Value>()["result"], "tails");
    }

    // nothing was stored server side, so a fresh browser is still fair
    let fresh = get_server().await;
    assert_eq!(
        fresh
            .get("/flip/api/odds")
            .await
            .json::<serde_json::Value>()["heads_pct"],
        50
    );
}

/// The odds come out of the request's own cookie, so two browsers hitting the
/// same server don't see each other's.
#[tokio::test]
async fn test_flip_odds_are_per_browser() {
    let server = get_server().await;

    let rigged = server
        .get("/flip/api/odds")
        .add_cookie(Cookie::new("flip_odds", "100"))
        .await;
    assert_eq!(rigged.json::<serde_json::Value>()["heads_pct"], 100);

    let untouched = server.get("/flip/api/odds").await;
    assert_eq!(untouched.json::<serde_json::Value>()["heads_pct"], 50);
}

/// A hand edited cookie can't push the draw off the steps or out of range.
#[tokio::test]
async fn test_flip_tampered_cookie_falls_back_to_fair() {
    let server = get_server().await;
    for bad in ["abc", "55", "-10", "110"] {
        let response = server
            .get("/flip/api/odds")
            .add_cookie(Cookie::new("flip_odds", bad))
            .await;
        response.assert_status_ok();
        assert_eq!(response.json::<serde_json::Value>()["heads_pct"], 50);

        let response = server
            .post("/flip/api/flip")
            .add_cookie(Cookie::new("flip_odds", bad))
            .await;
        response.assert_status_ok();
    }
}

#[tokio::test]
async fn test_flip_odds_rejects_off_step_values() {
    let server = get_server().await;
    for bad in [55, 101, -10] {
        let response = server
            .post("/flip/api/odds")
            .json(&serde_json::json!({"heads_pct": bad}))
            .await;
        response.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(response.json::<serde_json::Value>()["code"], 400);
        // a rejected update must not touch the browser's cookie
        assert!(response.maybe_cookie("flip_odds").is_none());
    }
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
async fn test_mapour_host_redirect() {
    let server = get_server().await;
    let response = server.get("/").add_header(header::HOST, "mapour.org").await;
    response.assert_status(StatusCode::TEMPORARY_REDIRECT);
    response.assert_header(header::LOCATION, "/mapour");
}

#[tokio::test]
async fn test_mapour_app_serves() {
    let server = get_server().await;
    let response = server.get("/mapour").await;
    response.assert_status_ok();
    assert!(response.text().contains("mapour"));
}

/// With mapour disabled (state None) the router is not mounted and the rest
/// of the app still works.
#[tokio::test]
async fn test_mapour_disabled() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
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
    let server = TestServer::new(app(spot_state, paste_state, None));

    server
        .get("/mapour")
        .await
        .assert_status(StatusCode::NOT_FOUND);
    server.get("/status").await.assert_status_ok();
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
