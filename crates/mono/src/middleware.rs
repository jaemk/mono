use axum::{body::Body, http::Request, middleware::Next, response::Response};
use axum_extra::extract::Host;

pub async fn trace_middleware(Host(host): Host, request: Request<Body>, next: Next) -> Response {
    let path = request.uri().path().to_string();
    let remote = request
        .headers()
        .get("fly-client-ip")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();

    tracing::debug!(
        path = %path,
        host = %host,
        remote = %remote,
        method = %request.method(),
        "handling request",
    );

    let response = next.run(request).await;

    let status = response.status();
    if status.is_server_error() {
        tracing::error!(
            path = %path,
            host = %host,
            remote = %remote,
            status = %status,
        );
    } else if status.is_client_error() {
        tracing::warn!(
            path = %path,
            host = %host,
            remote = %remote,
            status = %status,
        );
    } else {
        tracing::info!(
            path = %path,
            host = %host,
            remote = %remote,
            status = %status,
        );
    }
    response
}
