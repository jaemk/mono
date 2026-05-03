use crate::{dev, homepage, outside, ugh, CONFIG};
use axum::{
    body::Body,
    extract::Path,
    http::{HeaderMap, Request, StatusCode},
    response::IntoResponse,
};
use axum_extra::extract::Host;
use tower::ServiceExt;
use tower_http::services::ServeFile;

pub fn is_host(host: &str, targets: &[&str]) -> bool {
    let host = host.to_lowercase();
    targets.iter().any(|&t| host == t)
        || host == CONFIG.get_localhost_port()
        || host == CONFIG.get_127_port()
}

#[derive(serde::Serialize)]
pub struct Status<'a> {
    pub version: &'a str,
    pub ok: &'a str,
}

pub async fn status_handler() -> impl IntoResponse {
    axum::Json(Status {
        version: &CONFIG.version,
        ok: "ok",
    })
}

pub async fn ip_index(headers: HeaderMap) -> impl IntoResponse {
    let ip = headers
        .get("fly-client-ip")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");
    format!("{ip}\n")
}

pub async fn root_handler(Host(host): Host, headers: HeaderMap) -> impl IntoResponse {
    if is_host(&host, &["ugh.kominick.com"]) {
        return ugh::index().await.into_response();
    }
    if is_host(&host, &["ip.kominick.com"]) {
        return ip_index(headers).await.into_response();
    }
    if is_host(&host, &["outside.kominick.com"]) {
        return outside::index().await.into_response();
    }
    if is_host(&host, &["git.jaemk.me"]) {
        return axum::response::Redirect::temporary("https://github.com/jaemk/").into_response();
    }

    // homepage hosts
    if is_host(
        &host,
        &["kominick.com", "james.kominick.com", "kominick.org"],
    ) {
        return homepage::index().await.into_response();
    }

    if is_host(&host, &["kominick.dev", "jaemk.me"]) {
        return dev::index().await.into_response();
    }

    // Default to homepage
    homepage::index().await.into_response()
}

pub async fn wildcard_handler(Host(host): Host, Path(path): Path<String>) -> impl IntoResponse {
    if is_host(&host, &["git.jaemk.me"]) {
        return axum::response::Redirect::temporary(&format!("https://github.com/jaemk/{path}"))
            .into_response();
    }
    fallback_handler().await.into_response()
}

pub async fn favicon_handler(Host(host): Host) -> impl IntoResponse {
    if is_host(&host, &["ugh.kominick.com"]) {
        return serve_file("static/think.jpg").await.into_response();
    }
    serve_file("static/assets/favicon.ico")
        .await
        .into_response()
}

pub async fn serve_file(path: &'static str) -> impl IntoResponse {
    match ServeFile::new(path)
        .oneshot(Request::new(Body::empty()))
        .await
    {
        Ok(res) => res.into_response(),
        Err(e) => {
            tracing::error!("error serving {}: {:?}", path, e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn fallback_handler() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        axum::Json(serde_json::json!({
            "code": 404,
            "message": "NOT_FOUND"
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_host_logic() {
        assert!(is_host("kominick.com", &["kominick.com"]));
        assert!(is_host("KOMINICK.COM", &["kominick.com"]));
        assert!(is_host("localhost:3003", &["something.else"]));
        assert!(is_host("127.0.0.1:3003", &["something.else"]));
        assert!(!is_host("google.com", &["kominick.com"]));
    }
}
