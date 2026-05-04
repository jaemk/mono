use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tera::Context;
use tracing::{error, info};

use crate::models::{self, CONTENT_TYPES};
use crate::State as AppState;

#[derive(Debug, Deserialize)]
pub struct NewPasteQueryParams {
    #[serde(rename = "type")]
    pub type_: Option<String>,
    pub ttl_seconds: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct ViewParams {
    pub encryption_key: Option<String>,
}

#[derive(Serialize)]
struct PasteContent {
    pub key: String,
    pub content: String,
    pub content_type: String,
}

pub async fn new_paste(
    State(state): State<AppState>,
    Query(params): Query<NewPasteQueryParams>,
    headers: HeaderMap,
    body: String,
) -> std::result::Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let paste_type = params.type_.unwrap_or_else(|| "auto".to_string());
    let paste_ttl_seconds = params.ttl_seconds;
    let encryption_key = headers
        .get("x-paste-encryption-key")
        .and_then(|h| h.to_str().ok());

    if body.len() > state.config.max_paste_bytes {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({ "error": "Upload too large" })),
        ));
    }

    let new_paste = models::NewPaste {
        content: body,
        content_type: paste_type,
    };

    let paste = new_paste
        .insert(
            &state.db,
            &state.s3,
            &state.config,
            paste_ttl_seconds,
            encryption_key,
        )
        .await
        .map_err(|e| {
            error!("Error inserting paste: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Internal server error" })),
            )
        })?;

    Ok(Json(json!({"message": "success", "key": &paste.key})))
}

pub async fn view_paste_json(
    State(state): State<AppState>,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> std::result::Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let enc_key = headers
        .get("x-paste-encryption-key")
        .and_then(|h| h.to_str().ok());
    let paste = models::Paste::touch_and_get(&state.db, &state.s3, &state.config, &key, enc_key)
        .await
        .map_err(|e| {
            info!("Paste not found or error: {:?}, key: {}", e, key);
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "Paste not found" })),
            )
        })?;

    let content = PasteContent {
        key: paste.key,
        content: paste.content,
        content_type: paste.content_type,
    };
    Ok(Json(json!({ "paste": content })))
}

pub async fn view_paste_raw(
    State(state): State<AppState>,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> std::result::Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let enc_key = headers
        .get("x-paste-encryption-key")
        .and_then(|h| h.to_str().ok());
    match models::Paste::touch_and_get(&state.db, &state.s3, &state.config, &key, enc_key).await {
        Ok(paste) => Ok(paste.content.into_response()),
        Err(e) => {
            if e.to_string().contains("decryption failure") {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": "decryption_key_required",
                        "message": "x-paste-encryption-key header is required"
                    })),
                ));
            }
            Err((
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "Paste not found" })),
            ))
        }
    }
}

pub async fn view_paste(
    State(state): State<AppState>,
    Path(key): Path<String>,
    headers: HeaderMap,
    body: Option<Json<ViewParams>>,
) -> impl IntoResponse {
    let mut enc_key = headers
        .get("x-paste-encryption-key")
        .and_then(|h| h.to_str().ok())
        .map(String::from);

    if enc_key.is_none() {
        if let Some(Json(params)) = body {
            enc_key = params.encryption_key;
        }
    }

    let mut context = Context::new();
    match models::Paste::touch_and_get(
        &state.db,
        &state.s3,
        &state.config,
        &key,
        enc_key.as_deref(),
    )
    .await
    {
        Ok(paste) => {
            context.insert("paste_key", &paste.key);
            context.insert("content", &paste.content);
            context.insert("content_type", &paste.content_type);
            context.insert("content_types", &&CONTENT_TYPES[..]);
        }
        Err(e) => {
            if e.to_string().contains("decryption failure") {
                context.insert("paste_key", &key);
                context.insert("content", &"< encrypted >");
                context.insert("content_type", &"");
                context.insert("content_types", &&CONTENT_TYPES[..]);
                context.insert("encrypted", &true);
            } else {
                // Return home if not found
                return home(State(state)).await.into_response();
            }
        }
    }

    match state.tera.render("core/edit.html", &context) {
        Ok(content) => Html(content).into_response(),
        Err(e) => {
            error!("Tera render error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
        }
    }
}

pub async fn home(State(state): State<AppState>) -> impl IntoResponse {
    let mut context = Context::new();
    context.insert("content_types", &&CONTENT_TYPES[..]);
    match state.tera.render("core/edit.html", &context) {
        Ok(content) => Html(content).into_response(),
        Err(e) => {
            error!("Tera render error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
        }
    }
}

pub async fn status() -> impl IntoResponse {
    Json(json!({
        "hash": include_str!("../../../commit_hash.txt").trim(),
    }))
}
