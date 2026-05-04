use crate::{handlers::Status, CONFIG};
use axum::{http::StatusCode, response::IntoResponse};

pub async fn index() -> impl IntoResponse {
    (
        StatusCode::OK,
        axum::Json(Status {
            version: &CONFIG.version,
            ok: "ok",
        }),
    )
        .into_response()
}
