use common::utils::env_or;
use mono::{app, CONFIG};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    if env_or("SKIP_DOT_ENV", "false") == "false" {
        dotenv::dotenv().ok();
    }
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

    let spot_config = spot::Config::load();
    let spot_pool = common::db::init_pool(&spot_config.db_url)
        .await
        .expect("failed to initialize spot db pool");
    let spot_state = Arc::new(spot::SpotResources {
        pool: spot_pool,
        config: spot_config,
    });

    tokio::spawn(spot::service::background_currently_playing_poll(
        spot_state.clone(),
    ));

    let paste_state = paste::service::init(paste::Config::load())
        .await
        .expect("failed to initialize paste state");

    let app = app(spot_state, paste_state);
    let addr = CONFIG.get_host_port();
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind to {}: {}", addr, e));

    tracing::info!("listening on {}", addr);
    axum::serve(listener, app).await.unwrap();
}
