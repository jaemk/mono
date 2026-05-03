pub mod config;
pub mod dev;
pub mod handlers;
pub mod homepage;
pub mod middleware;
pub mod ugh;

use axum::{middleware as axum_middleware, routing::get, Router};
use tera::Tera;
use tower_http::services::ServeDir;

lazy_static::lazy_static! {
    pub static ref CONFIG: config::Config = config::Config::load();
    pub static ref TERA: Tera = Tera::new("templates/**/*.html").expect("unable to compile tera templates");
}

pub fn app() -> Router {
    Router::new()
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
}
