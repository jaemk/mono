use mono::{app, CONFIG};

#[tokio::main]
async fn main() {
    CONFIG.initialize();

    let filter = tracing_subscriber::filter::EnvFilter::new(&CONFIG.log_level);
    if CONFIG.log_json {
        tracing_subscriber::fmt()
            .json()
            .with_current_span(false)
            .with_env_filter(filter)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }

    let app = app();
    let addr = CONFIG.get_host_port();
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind to {}: {}", addr, e));

    tracing::info!("listening on {}", addr);
    axum::serve(listener, app).await.unwrap();
}
