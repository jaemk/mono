use axum::{
    extract::{Query, Request, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{models, storage, State as TransferState};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn err(status: StatusCode, msg: &str) -> (StatusCode, Json<serde_json::Value>) {
    (status, Json(serde_json::json!({ "error": msg })))
}

type ApiError = (StatusCode, Json<serde_json::Value>);
type ApiResult<T> = Result<T, ApiError>;

fn map_err(e: impl std::fmt::Display) -> ApiError {
    err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())
}

// ---------------------------------------------------------------------------
// GET /transfer/api/upload/defaults
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct UploadDefaults {
    pub upload_limit_bytes: usize,
    pub upload_lifespan_secs_default: i64,
}

pub async fn api_upload_defaults(State(state): State<TransferState>) -> impl IntoResponse {
    Json(UploadDefaults {
        upload_limit_bytes: state.config.upload_limit_bytes,
        upload_lifespan_secs_default: state.config.upload_lifespan_secs_default,
    })
}

// ---------------------------------------------------------------------------
// POST /transfer/api/upload/init
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct UploadInitRequest {
    /// Hex-encoded client-generated AES-GCM nonce (12 bytes).
    pub nonce: String,
    /// Hex-encoded SHA-256 of the encrypted filename.
    pub file_name_hash: String,
    /// Hex-encoded SHA-256 of the ciphertext.
    pub content_hash: String,
    pub size: i64,
    /// Hex-encoded raw bytes of the access password (hashed server-side).
    pub access_password: String,
    /// Hex-encoded raw bytes of the deletion password (optional).
    pub deletion_password: Option<String>,
    pub download_limit: Option<i32>,
    /// Lifespan in seconds; defaults to config value.
    pub lifespan: Option<i64>,
}

#[derive(Serialize)]
pub struct UploadInitResponse {
    pub key: String,
}

pub async fn api_upload_init(
    State(state): State<TransferState>,
    Json(body): Json<UploadInitRequest>,
) -> ApiResult<Json<UploadInitResponse>> {
    let nonce =
        hex::decode(&body.nonce).map_err(|_| err(StatusCode::BAD_REQUEST, "invalid nonce hex"))?;
    let file_name_hash = hex::decode(&body.file_name_hash)
        .map_err(|_| err(StatusCode::BAD_REQUEST, "invalid file_name_hash hex"))?;
    let content_hash = hex::decode(&body.content_hash)
        .map_err(|_| err(StatusCode::BAD_REQUEST, "invalid content_hash hex"))?;
    let access_pw_bytes = hex::decode(&body.access_password)
        .map_err(|_| err(StatusCode::BAD_REQUEST, "invalid access_password hex"))?;

    if body.size <= 0 {
        return Err(err(StatusCode::BAD_REQUEST, "size must be positive"));
    }
    if body.size as usize > state.config.upload_limit_bytes {
        return Err(err(StatusCode::PAYLOAD_TOO_LARGE, "file too large"));
    }

    let (access_salt, access_hash) =
        common::crypto::hash_password(&access_pw_bytes).map_err(map_err)?;
    let access_auth_id = models::insert_auth(&state.db, access_salt, access_hash)
        .await
        .map_err(map_err)?;

    let deletion_auth_id = if let Some(del_pw) = &body.deletion_password {
        let del_bytes = hex::decode(del_pw)
            .map_err(|_| err(StatusCode::BAD_REQUEST, "invalid deletion_password hex"))?;
        let (del_salt, del_hash) = common::crypto::hash_password(&del_bytes).map_err(map_err)?;
        Some(
            models::insert_auth(&state.db, del_salt, del_hash)
                .await
                .map_err(map_err)?,
        )
    } else {
        None
    };

    // expire_date in init_upload holds the *final* upload lifespan, not the
    // init-session timeout. The upload handler checks timeout via date_created.
    let lifespan = body
        .lifespan
        .unwrap_or(state.config.upload_lifespan_secs_default);
    let expire_date = Utc::now() + chrono::Duration::seconds(lifespan);

    let uuid_ = Uuid::new_v4();
    models::insert_init_upload(
        &state.db,
        uuid_,
        file_name_hash,
        content_hash,
        body.size,
        nonce,
        access_auth_id,
        deletion_auth_id,
        body.download_limit,
        expire_date,
    )
    .await
    .map_err(map_err)?;

    Ok(Json(UploadInitResponse {
        key: uuid_.to_string(),
    }))
}

// ---------------------------------------------------------------------------
// POST /transfer/api/upload?key=<uuid>
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct UploadKey {
    pub key: Uuid,
}

pub async fn api_upload_file(
    State(state): State<TransferState>,
    Query(params): Query<UploadKey>,
    request: Request,
) -> ApiResult<Json<serde_json::Value>> {
    let init = models::get_init_upload(&state.db, params.key)
        .await
        .map_err(map_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "upload key not found"))?;

    // Check init-session timeout via date_created (expire_date holds final lifespan).
    let timeout_cutoff = Utc::now() - chrono::Duration::seconds(state.config.upload_timeout_secs);
    if init.date_created < timeout_cutoff {
        return Err(err(StatusCode::NOT_FOUND, "upload key expired"));
    }

    if let Some(len) = request
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
    {
        if len > state.config.upload_limit_bytes {
            return Err(err(StatusCode::PAYLOAD_TOO_LARGE, "file too large"));
        }
    }

    let storage_key = init.uuid_.to_string();
    let body = request.into_body();

    let bytes_uploaded = storage::multipart_upload(
        &state.s3,
        &state.config.s3_bucket,
        &storage_key,
        body,
        state.config.upload_limit_bytes,
    )
    .await
    .map_err(|e| {
        tracing::error!("S3 upload error: {e}");
        err(StatusCode::INTERNAL_SERVER_ERROR, "upload failed")
    })?;

    models::delete_init_upload(&state.db, init.id)
        .await
        .map_err(map_err)?;

    models::insert_upload(
        &state.db,
        init.uuid_,
        init.file_name_hash,
        init.content_hash,
        init.size_,
        storage_key,
        init.nonce,
        init.access_password,
        init.deletion_password,
        init.download_limit,
        init.expire_date, // preserved from init_upload
    )
    .await
    .map_err(map_err)?;

    models::increment_status(&state.db, bytes_uploaded as i64)
        .await
        .map_err(map_err)?;

    Ok(Json(
        serde_json::json!({ "ok": "ok", "bytes": bytes_uploaded }),
    ))
}

// ---------------------------------------------------------------------------
// POST /transfer/api/upload/delete
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct UploadDeleteRequest {
    pub key: Uuid,
    pub deletion_password: String,
}

pub async fn api_upload_delete(
    State(state): State<TransferState>,
    Json(body): Json<UploadDeleteRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let upload = models::get_upload(&state.db, body.key)
        .await
        .map_err(map_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "not found"))?;

    let del_auth_id = upload
        .deletion_password
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "no deletion password set"))?;

    let auth = models::get_auth(&state.db, del_auth_id)
        .await
        .map_err(map_err)?
        .ok_or_else(|| err(StatusCode::INTERNAL_SERVER_ERROR, "auth record missing"))?;

    let del_bytes = hex::decode(&body.deletion_password)
        .map_err(|_| err(StatusCode::BAD_REQUEST, "invalid deletion_password hex"))?;

    if !common::crypto::verify_password(&del_bytes, &auth.salt, &auth.hash) {
        return Err(err(StatusCode::UNAUTHORIZED, "invalid deletion password"));
    }

    models::soft_delete_upload(&state.db, upload.id)
        .await
        .map_err(map_err)?;
    let _ = storage::delete_object(&state.s3, &state.config.s3_bucket, &upload.storage_uri).await;

    Ok(Json(serde_json::json!({ "ok": "ok" })))
}

// ---------------------------------------------------------------------------
// POST /transfer/api/download/init
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct DownloadInitRequest {
    pub key: Uuid,
    pub access_password: String,
}

#[derive(Serialize)]
pub struct DownloadInitResponse {
    pub nonce: String,
    pub size: i64,
    pub download_key: String,
    pub confirm_key: String,
}

pub async fn api_download_init(
    State(state): State<TransferState>,
    Json(body): Json<DownloadInitRequest>,
) -> ApiResult<Json<DownloadInitResponse>> {
    let upload = models::get_upload(&state.db, body.key)
        .await
        .map_err(map_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "not found"))?;

    if Utc::now() > upload.expire_date {
        return Err(err(StatusCode::NOT_FOUND, "upload expired"));
    }

    if let Some(limit) = upload.download_limit {
        let count = models::count_downloads(&state.db, upload.id)
            .await
            .map_err(map_err)?;
        if count >= limit as i64 {
            return Err(err(StatusCode::GONE, "download limit reached"));
        }
    }

    let auth = models::get_auth(&state.db, upload.access_password)
        .await
        .map_err(map_err)?
        .ok_or_else(|| err(StatusCode::INTERNAL_SERVER_ERROR, "auth record missing"))?;

    let access_bytes = hex::decode(&body.access_password)
        .map_err(|_| err(StatusCode::BAD_REQUEST, "invalid access_password hex"))?;

    if !common::crypto::verify_password(&access_bytes, &auth.salt, &auth.hash) {
        return Err(err(StatusCode::UNAUTHORIZED, "invalid access password"));
    }

    let content_tok = models::insert_init_download(&state.db, Uuid::new_v4(), "content", upload.id)
        .await
        .map_err(map_err)?;
    let confirm_tok = models::insert_init_download(&state.db, Uuid::new_v4(), "confirm", upload.id)
        .await
        .map_err(map_err)?;

    Ok(Json(DownloadInitResponse {
        nonce: hex::encode(&upload.nonce),
        size: upload.size_,
        download_key: content_tok.uuid_.to_string(),
        confirm_key: confirm_tok.uuid_.to_string(),
    }))
}

// ---------------------------------------------------------------------------
// POST /transfer/api/download — stream encrypted bytes to client
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct DownloadRequest {
    pub key: Uuid,
    pub access_password: String,
}

pub async fn api_download(
    State(state): State<TransferState>,
    Json(body): Json<DownloadRequest>,
) -> ApiResult<impl IntoResponse> {
    let init_dl = models::get_init_download(&state.db, body.key, "content")
        .await
        .map_err(map_err)?
        .ok_or_else(|| {
            err(
                StatusCode::NOT_FOUND,
                "download key not found or already used",
            )
        })?;

    let timeout_cutoff = Utc::now() - chrono::Duration::seconds(state.config.download_timeout_secs);
    if init_dl.date_created < timeout_cutoff {
        return Err(err(StatusCode::NOT_FOUND, "download key expired"));
    }

    let upload = sqlx::query_as::<_, models::Upload>(
        "select * from upload where id = $1 and deleted = false",
    )
    .bind(init_dl.upload_id)
    .fetch_optional(&state.db)
    .await
    .map_err(map_err)?
    .ok_or_else(|| err(StatusCode::NOT_FOUND, "upload not found"))?;

    let auth = models::get_auth(&state.db, upload.access_password)
        .await
        .map_err(map_err)?
        .ok_or_else(|| err(StatusCode::INTERNAL_SERVER_ERROR, "auth record missing"))?;

    let access_bytes = hex::decode(&body.access_password)
        .map_err(|_| err(StatusCode::BAD_REQUEST, "invalid access_password hex"))?;

    if !common::crypto::verify_password(&access_bytes, &auth.salt, &auth.hash) {
        return Err(err(StatusCode::UNAUTHORIZED, "invalid access password"));
    }

    models::delete_init_download(&state.db, init_dl.id)
        .await
        .map_err(map_err)?;
    models::record_download(&state.db, upload.id)
        .await
        .map_err(map_err)?;

    let stream_body =
        storage::get_object_stream(&state.s3, &state.config.s3_bucket, &upload.storage_uri)
            .await
            .map_err(|e| {
                tracing::error!("S3 download error: {e}");
                err(StatusCode::INTERNAL_SERVER_ERROR, "download failed")
            })?;

    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
        stream_body,
    ))
}

// ---------------------------------------------------------------------------
// POST /transfer/api/download/confirm
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct DownloadConfirmRequest {
    pub key: Uuid,
    /// Hex-encoded SHA-256 of the decrypted file content (client-computed).
    pub hash: String,
}

#[derive(Serialize)]
pub struct DownloadConfirmResponse {
    pub file_name_hash: String,
}

pub async fn api_download_confirm(
    State(state): State<TransferState>,
    Json(body): Json<DownloadConfirmRequest>,
) -> ApiResult<Json<DownloadConfirmResponse>> {
    let init_dl = models::get_init_download(&state.db, body.key, "confirm")
        .await
        .map_err(map_err)?
        .ok_or_else(|| {
            err(
                StatusCode::NOT_FOUND,
                "confirm key not found or already used",
            )
        })?;

    let timeout_cutoff = Utc::now() - chrono::Duration::seconds(state.config.download_timeout_secs);
    if init_dl.date_created < timeout_cutoff {
        return Err(err(StatusCode::NOT_FOUND, "confirm key expired"));
    }

    let upload = sqlx::query_as::<_, models::Upload>(
        "select * from upload where id = $1 and deleted = false",
    )
    .bind(init_dl.upload_id)
    .fetch_optional(&state.db)
    .await
    .map_err(map_err)?
    .ok_or_else(|| err(StatusCode::NOT_FOUND, "upload not found"))?;

    let provided_hash =
        hex::decode(&body.hash).map_err(|_| err(StatusCode::BAD_REQUEST, "invalid hash hex"))?;

    if provided_hash != upload.content_hash {
        return Err(err(StatusCode::UNPROCESSABLE_ENTITY, "hash mismatch"));
    }

    models::delete_init_download(&state.db, init_dl.id)
        .await
        .map_err(map_err)?;

    Ok(Json(DownloadConfirmResponse {
        file_name_hash: hex::encode(&upload.file_name_hash),
    }))
}
