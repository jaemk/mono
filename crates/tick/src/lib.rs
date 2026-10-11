//! tick: a mechanical watch timegrapher. Capture, beat detection, and the
//! rate and beat error fit all run in the browser (`assets/static`), so the
//! server only serves the page and its assets (TICK-1, TICK-2).

use axum::Router;
use tower_http::services::{ServeDir, ServeFile};

pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route_service("/", ServeFile::new("crates/tick/assets/index.html"))
        .nest_service("/static", ServeDir::new("crates/tick/assets/static"))
}
