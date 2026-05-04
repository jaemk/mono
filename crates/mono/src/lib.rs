pub mod config;
pub mod dev;
pub mod handlers;
pub mod homepage;
pub mod middleware;
pub mod outside;
pub mod ugh;

use axum::{extract::FromRef, middleware as axum_middleware, routing::get, Router};
use tera::Tera;
use tower_http::services::ServeDir;

pub use common::{Error, Result};
pub use config::CONFIG;

lazy_static::lazy_static! {
    pub static ref TERA: Tera = Tera::new("templates/**/*.html").expect("unable to compile tera templates");
}

#[derive(Clone)]
pub struct AppState {
    pub spot_state: spot::SpotState,
    pub paste_state: paste::State,
}

impl FromRef<AppState> for spot::SpotState {
    fn from_ref(state: &AppState) -> Self {
        state.spot_state.clone()
    }
}

impl FromRef<AppState> for paste::State {
    fn from_ref(state: &AppState) -> Self {
        state.paste_state.clone()
    }
}

pub fn app(spot_state: spot::SpotState, paste_state: paste::State) -> Router {
    let state = AppState {
        spot_state,
        paste_state,
    };
    Router::new()
        .nest("/spot", spot::service::router(state.clone()))
        .nest("/paste", paste::service::router(state.clone()))
        .route("/", get(handlers::root_handler))
        .route("/status", get(handlers::status_handler))
        .route("/favicon.ico", get(handlers::favicon_handler))
        .route(
            "/robots.txt",
            get(|| async { handlers::serve_file("static/robots.txt").await }),
        )
        .nest_service("/static", ServeDir::new("static"))
        .route("/{*path}", get(handlers::wildcard_handler))
        .fallback(handlers::fallback_handler)
        .layer(axum_middleware::from_fn(middleware::trace_middleware))
        .with_state(state)
}
