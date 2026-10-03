pub mod config;
pub mod dev;
pub mod flip;
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

/// Compile the workspace templates from an absolute path so tests do not
/// depend on the process working directory (the shared `TERA` glob is
/// relative to the workspace root).
#[cfg(test)]
pub(crate) fn workspace_tera() -> Tera {
    let glob = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent() // crates/
        .unwrap()
        .parent() // workspace root
        .unwrap()
        .join("templates/**/*.html");
    Tera::new(glob.to_str().unwrap()).expect("unable to compile tera templates")
}

#[derive(Clone)]
pub struct AppState {
    pub spot_state: spot::SpotState,
    pub paste_state: paste::State,
    // None when mapour is disabled (MAPOUR_ENABLED != true); the mapour
    // router is only mounted when this is Some
    pub mapour_state: Option<mapour::State>,
    // None when komino is disabled (KOMINO_ENABLED != true)
    pub komino_state: Option<komino::State>,
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

impl FromRef<AppState> for mapour::State {
    fn from_ref(state: &AppState) -> Self {
        state
            .mapour_state
            .clone()
            .expect("mapour handlers are not mounted when mapour is disabled")
    }
}

impl FromRef<AppState> for komino::State {
    fn from_ref(state: &AppState) -> Self {
        state
            .komino_state
            .clone()
            .expect("komino handlers are not mounted when komino is disabled")
    }
}

pub fn app(
    spot_state: spot::SpotState,
    paste_state: paste::State,
    mapour_state: Option<mapour::State>,
    komino_state: Option<komino::State>,
) -> Router {
    let state = AppState {
        spot_state,
        paste_state,
        mapour_state,
        komino_state,
    };
    let mut router = Router::new()
        .nest("/spot", spot::service::router(state.clone()))
        .nest("/paste", paste::service::router(state.clone()))
        .merge(flip::router());
    if state.mapour_state.is_some() {
        router = router.nest("/mapour", mapour::service::router(state.clone()));
    }
    if state.komino_state.is_some() {
        router = router
            .nest("/komino", komino::service::router(state.clone()))
            .route(
                "/komino/",
                get(|| async { axum::response::Redirect::permanent("/komino") }),
            );
    }
    router
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
