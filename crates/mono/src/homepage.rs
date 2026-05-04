use crate::TERA;
use axum::{
    http::{header, StatusCode},
    response::IntoResponse,
};
use tera::Context;

pub async fn index() -> impl IntoResponse {
    match TERA.render("home.html", &Context::new()) {
        Ok(s) => ([(header::CONTENT_TYPE, "text/html")], s).into_response(),
        Err(e) => {
            tracing::error!("tera render error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "content error").into_response()
        }
    }
}
