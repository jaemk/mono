use axum::{
    extract::{Query, Request, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use axum_extra::extract::CookieJar;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{auth, models, storage, State as TransferState};

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

fn is_valid_email(email: &str) -> bool {
    let parts: Vec<&str> = email.splitn(2, '@').collect();
    if parts.len() != 2 { return false; }
    let domain = parts[1];
    domain.contains('.') && domain.len() > 2
}

// ---------------------------------------------------------------------------
// GET /transfer/api/upload/defaults
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct UploadDefaults {
    pub upload_limit_bytes:           usize,
    pub upload_lifespan_secs_default: i64,
    pub google_enabled:               bool,
    pub google_client_id:             Option<String>,
    pub email_enabled:                bool,
}

pub async fn api_upload_defaults(State(state): State<TransferState>) -> impl IntoResponse {
    Json(UploadDefaults {
        upload_limit_bytes:           state.config.upload_limit_bytes,
        upload_lifespan_secs_default: state.config.upload_lifespan_secs_default,
        google_enabled:               state.config.google_enabled(),
        google_client_id:             state.config.google_client_id.clone(),
        email_enabled:                state.config.smtp_enabled(),
    })
}

// ---------------------------------------------------------------------------
// POST /transfer/api/upload/init
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct UploadInitRequest {
    pub nonce:             String,
    pub file_name_hash:    String,
    pub content_hash:      String,
    pub size:              i64,
    pub access_password:   String,
    pub deletion_password: Option<String>,
    pub download_limit:    Option<i32>,
    pub lifespan:          Option<i64>,
}

#[derive(Serialize)]
pub struct UploadInitResponse {
    pub key: String,
}

pub async fn api_upload_init(
    State(state): State<TransferState>,
    jar: CookieJar,
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

    let lifespan = body
        .lifespan
        .unwrap_or(state.config.upload_lifespan_secs_default);
    let expire_date = Utc::now() + chrono::Duration::seconds(lifespan);
    let user_id = auth::get_current_user(&jar, &state.db).await.map(|u| u.id);

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
        user_id,
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
        init.expire_date,
        init.user_id,
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
    pub key:               Uuid,
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

    let auth_row = models::get_auth(&state.db, del_auth_id)
        .await
        .map_err(map_err)?
        .ok_or_else(|| err(StatusCode::INTERNAL_SERVER_ERROR, "auth record missing"))?;

    let del_bytes = hex::decode(&body.deletion_password)
        .map_err(|_| err(StatusCode::BAD_REQUEST, "invalid deletion_password hex"))?;

    if !common::crypto::verify_password(&del_bytes, &auth_row.salt, &auth_row.hash) {
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
    pub key:             Uuid,
    pub access_password: String,
}

#[derive(Serialize)]
pub struct DownloadInitResponse {
    pub nonce:        String,
    pub size:         i64,
    pub download_key: String,
    pub confirm_key:  String,
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

    let auth_row = models::get_auth(&state.db, upload.access_password)
        .await
        .map_err(map_err)?
        .ok_or_else(|| err(StatusCode::INTERNAL_SERVER_ERROR, "auth record missing"))?;

    let access_bytes = hex::decode(&body.access_password)
        .map_err(|_| err(StatusCode::BAD_REQUEST, "invalid access_password hex"))?;

    if !common::crypto::verify_password(&access_bytes, &auth_row.salt, &auth_row.hash) {
        return Err(err(StatusCode::UNAUTHORIZED, "invalid access password"));
    }

    let content_tok = models::insert_init_download(&state.db, Uuid::new_v4(), "content", upload.id)
        .await
        .map_err(map_err)?;
    let confirm_tok = models::insert_init_download(&state.db, Uuid::new_v4(), "confirm", upload.id)
        .await
        .map_err(map_err)?;

    Ok(Json(DownloadInitResponse {
        nonce:        hex::encode(&upload.nonce),
        size:         upload.size_,
        download_key: content_tok.uuid_.to_string(),
        confirm_key:  confirm_tok.uuid_.to_string(),
    }))
}

// ---------------------------------------------------------------------------
// POST /transfer/api/download
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct DownloadRequest {
    pub key:             Uuid,
    pub access_password: String,
}

pub async fn api_download(
    State(state): State<TransferState>,
    Json(body): Json<DownloadRequest>,
) -> ApiResult<impl IntoResponse> {
    let init_dl = models::get_init_download(&state.db, body.key, "content")
        .await
        .map_err(map_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "download key not found or already used"))?;

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

    let auth_row = models::get_auth(&state.db, upload.access_password)
        .await
        .map_err(map_err)?
        .ok_or_else(|| err(StatusCode::INTERNAL_SERVER_ERROR, "auth record missing"))?;

    let access_bytes = hex::decode(&body.access_password)
        .map_err(|_| err(StatusCode::BAD_REQUEST, "invalid access_password hex"))?;

    if !common::crypto::verify_password(&access_bytes, &auth_row.salt, &auth_row.hash) {
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
    pub key:  Uuid,
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
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "confirm key not found or already used"))?;

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

// ---------------------------------------------------------------------------
// POST /transfer/api/auth/register
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RegisterRequest {
    pub email:    String,
    pub password: String,
}

#[derive(Serialize)]
pub struct RegisterResponse {
    pub pending_id: String,
}

pub async fn api_auth_register(
    State(state): State<TransferState>,
    Json(body): Json<RegisterRequest>,
) -> ApiResult<Json<RegisterResponse>> {
    if !state.config.smtp_enabled() {
        return Err(err(StatusCode::SERVICE_UNAVAILABLE, "email registration not configured"));
    }
    if !is_valid_email(&body.email) {
        return Err(err(StatusCode::BAD_REQUEST, "invalid email address"));
    }
    if body.password.len() < 8 {
        return Err(err(StatusCode::BAD_REQUEST, "password must be at least 8 characters"));
    }
    if models::get_user_by_email(&state.db, &body.email).await.map_err(map_err)?.is_some() {
        return Err(err(StatusCode::CONFLICT, "email already registered"));
    }

    models::delete_pending_by_email(&state.db, &body.email).await.map_err(map_err)?;

    let (salt, hash) = common::crypto::hash_password(body.password.as_bytes()).map_err(map_err)?;
    let auth_id = models::insert_auth(&state.db, salt, hash).await.map_err(map_err)?;

    let (code, code_hash) = auth::generate_verification_code();
    let uuid_ = Uuid::new_v4();
    let expire_date = Utc::now() + chrono::Duration::seconds(state.config.registration_code_secs);
    models::insert_pending_registration(&state.db, uuid_, &body.email, auth_id, code_hash, expire_date)
        .await
        .map_err(map_err)?;

    auth::send_registration_code(&state.config, &body.email, &code)
        .await
        .map_err(|e| {
            tracing::error!("Failed to send registration code: {e}");
            err(StatusCode::INTERNAL_SERVER_ERROR, "failed to send verification code")
        })?;

    Ok(Json(RegisterResponse { pending_id: uuid_.to_string() }))
}

// ---------------------------------------------------------------------------
// POST /transfer/api/auth/verify-code
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct VerifyCodeRequest {
    pub pending_id: String,
    pub code:       String,
}

#[derive(Serialize)]
pub struct AuthUserResponse {
    pub email: String,
}

pub async fn api_auth_verify_code(
    State(state): State<TransferState>,
    jar: CookieJar,
    Json(body): Json<VerifyCodeRequest>,
) -> ApiResult<(CookieJar, Json<AuthUserResponse>)> {
    let pending_id = Uuid::parse_str(&body.pending_id)
        .map_err(|_| err(StatusCode::BAD_REQUEST, "invalid pending_id"))?;

    let pending = models::get_pending_registration(&state.db, pending_id)
        .await
        .map_err(map_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "code expired, please register again"))?;

    if Utc::now() > pending.expire_date {
        return Err(err(StatusCode::GONE, "code expired, please register again"));
    }

    let new_attempts = models::increment_pending_attempts(&state.db, pending.id)
        .await
        .map_err(map_err)?;

    if new_attempts > auth::MAX_CODE_ATTEMPTS {
        models::delete_pending_registration(&state.db, pending.id).await.map_err(map_err)?;
        return Err(err(StatusCode::TOO_MANY_REQUESTS, "too many attempts, please register again"));
    }

    let code_hash = common::crypto::sha256(body.code.as_bytes());
    if !auth::verify_code_hash(&code_hash, &pending.code_hash) {
        return Err(err(StatusCode::UNAUTHORIZED, "invalid code"));
    }

    let user = models::insert_user(&state.db, &pending.email, Some(pending.auth_id), None)
        .await
        .map_err(map_err)?;

    // Clean up pending record without cascade-deleting the auth row (user now owns it)
    sqlx::query("delete from pending_registration where id = $1")
        .bind(pending.id)
        .execute(&state.db)
        .await
        .map_err(map_err)?;

    let session_token = Uuid::new_v4();
    let expire_date = Utc::now() + chrono::Duration::seconds(auth::SESSION_DURATION_SECS);
    models::insert_session(&state.db, session_token, user.id, expire_date)
        .await
        .map_err(map_err)?;

    let cookie = auth::make_session_cookie(session_token);
    Ok((jar.add(cookie), Json(AuthUserResponse { email: user.email })))
}

// ---------------------------------------------------------------------------
// POST /transfer/api/auth/resend-code
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ResendCodeRequest {
    pub pending_id: String,
}

pub async fn api_auth_resend_code(
    State(state): State<TransferState>,
    Json(body): Json<ResendCodeRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    if !state.config.smtp_enabled() {
        return Err(err(StatusCode::SERVICE_UNAVAILABLE, "email registration not configured"));
    }
    let pending_id = Uuid::parse_str(&body.pending_id)
        .map_err(|_| err(StatusCode::BAD_REQUEST, "invalid pending_id"))?;

    let pending = models::get_pending_registration(&state.db, pending_id)
        .await
        .map_err(map_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "code expired, please register again"))?;

    if Utc::now() > pending.expire_date {
        return Err(err(StatusCode::GONE, "code expired, please register again"));
    }
    if Utc::now() < pending.resend_after {
        return Err(err(StatusCode::TOO_MANY_REQUESTS, "please wait before resending"));
    }

    let (code, code_hash) = auth::generate_verification_code();
    let resend_after = Utc::now() + chrono::Duration::seconds(auth::RESEND_COOLDOWN_SECS);
    models::update_pending_code(&state.db, pending.id, code_hash, resend_after)
        .await
        .map_err(map_err)?;

    auth::send_registration_code(&state.config, &pending.email, &code)
        .await
        .map_err(|e| {
            tracing::error!("Failed to resend code: {e}");
            err(StatusCode::INTERNAL_SERVER_ERROR, "failed to send verification code")
        })?;

    Ok(Json(serde_json::json!({ "ok": "ok" })))
}

// ---------------------------------------------------------------------------
// POST /transfer/api/auth/login
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct LoginRequest {
    pub email:    String,
    pub password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub email:         String,
    pub google_linked: bool,
    pub has_password:  bool,
}

pub async fn api_auth_login(
    State(state): State<TransferState>,
    jar: CookieJar,
    Json(body): Json<LoginRequest>,
) -> ApiResult<(CookieJar, Json<LoginResponse>)> {
    let user = models::get_user_by_email(&state.db, &body.email)
        .await
        .map_err(map_err)?
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "invalid email or password"))?;

    let auth_id = user.auth_id
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "use Google sign-in for this account"))?;

    let auth_row = models::get_auth(&state.db, auth_id)
        .await
        .map_err(map_err)?
        .ok_or_else(|| err(StatusCode::INTERNAL_SERVER_ERROR, "auth record missing"))?;

    if !common::crypto::verify_password(body.password.as_bytes(), &auth_row.salt, &auth_row.hash) {
        return Err(err(StatusCode::UNAUTHORIZED, "invalid email or password"));
    }

    let session_token = Uuid::new_v4();
    let expire_date = Utc::now() + chrono::Duration::seconds(auth::SESSION_DURATION_SECS);
    models::insert_session(&state.db, session_token, user.id, expire_date)
        .await
        .map_err(map_err)?;

    let cookie = auth::make_session_cookie(session_token);
    Ok((
        jar.add(cookie),
        Json(LoginResponse {
            email:         user.email,
            google_linked: user.google_sub.is_some(),
            has_password:  true,
        }),
    ))
}

// ---------------------------------------------------------------------------
// POST /transfer/api/auth/logout
// ---------------------------------------------------------------------------

pub async fn api_auth_logout(
    State(state): State<TransferState>,
    jar: CookieJar,
) -> ApiResult<(CookieJar, Json<serde_json::Value>)> {
    if let Some(cookie) = jar.get(auth::SESSION_COOKIE) {
        if let Ok(uuid) = Uuid::parse_str(cookie.value()) {
            let _ = models::delete_session(&state.db, uuid).await;
        }
    }
    Ok((jar.add(auth::clear_session_cookie()), Json(serde_json::json!({ "ok": "ok" }))))
}

// ---------------------------------------------------------------------------
// GET /transfer/api/auth/me
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct MeResponse {
    pub email:         String,
    pub google_linked: bool,
    pub has_password:  bool,
}

pub async fn api_auth_me(
    State(state): State<TransferState>,
    jar: CookieJar,
) -> ApiResult<Json<MeResponse>> {
    let user = auth::get_current_user(&jar, &state.db)
        .await
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "not authenticated"))?;

    Ok(Json(MeResponse {
        email:         user.email,
        google_linked: user.google_sub.is_some(),
        has_password:  user.auth_id.is_some(),
    }))
}

// ---------------------------------------------------------------------------
// POST /transfer/api/auth/google
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct GoogleAuthRequest {
    pub id_token: String,
}

pub async fn api_auth_google(
    State(state): State<TransferState>,
    jar: CookieJar,
    Json(body): Json<GoogleAuthRequest>,
) -> ApiResult<(CookieJar, Json<AuthUserResponse>)> {
    let client_id = state
        .config
        .google_client_id
        .as_deref()
        .ok_or_else(|| err(StatusCode::SERVICE_UNAVAILABLE, "Google sign-in not configured"))?;

    let claims = auth::verify_google_token(&state.http, &body.id_token, client_id)
        .await
        .map_err(|e| {
            tracing::warn!("Google token verification failed: {e}");
            err(StatusCode::UNAUTHORIZED, "invalid Google token")
        })?;

    let user = match models::get_user_by_email(&state.db, &claims.email).await.map_err(map_err)? {
        Some(existing) => {
            if existing.google_sub.is_none() {
                models::set_user_google_sub(&state.db, existing.id, &claims.sub)
                    .await
                    .map_err(map_err)?;
            }
            existing
        }
        None => {
            models::insert_user(&state.db, &claims.email, None, Some(&claims.sub))
                .await
                .map_err(map_err)?
        }
    };

    let session_token = Uuid::new_v4();
    let expire_date = Utc::now() + chrono::Duration::seconds(auth::SESSION_DURATION_SECS);
    models::insert_session(&state.db, session_token, user.id, expire_date)
        .await
        .map_err(map_err)?;

    let cookie = auth::make_session_cookie(session_token);
    Ok((jar.add(cookie), Json(AuthUserResponse { email: user.email })))
}

// ---------------------------------------------------------------------------
// GET /transfer/api/my/transfers
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct MyTransferItem {
    pub key:            String,
    pub size:           i64,
    pub expire_date:    String,
    pub download_limit: Option<i32>,
    pub deleted:        bool,
    pub expired:        bool,
    pub date_created:   String,
    pub download_count: i64,
}

pub async fn api_my_transfers(
    State(state): State<TransferState>,
    jar: CookieJar,
) -> ApiResult<Json<Vec<MyTransferItem>>> {
    let user = auth::get_current_user(&jar, &state.db)
        .await
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "not authenticated"))?;

    let rows = models::list_uploads_by_user(&state.db, user.id)
        .await
        .map_err(map_err)?;

    let now = Utc::now();
    let items: Vec<MyTransferItem> = rows
        .into_iter()
        .map(|r| MyTransferItem {
            key:            r.uuid_.to_string(),
            size:           r.size_,
            expire_date:    r.expire_date.to_rfc3339(),
            download_limit: r.download_limit,
            deleted:        r.deleted,
            expired:        !r.deleted && now > r.expire_date,
            date_created:   r.date_created.to_rfc3339(),
            download_count: r.download_count,
        })
        .collect();

    Ok(Json(items))
}

// ---------------------------------------------------------------------------
// POST /transfer/api/my/delete
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct MyDeleteRequest {
    pub key: Uuid,
}

pub async fn api_my_delete(
    State(state): State<TransferState>,
    jar: CookieJar,
    Json(body): Json<MyDeleteRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = auth::get_current_user(&jar, &state.db)
        .await
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "not authenticated"))?;

    let upload = models::get_upload_by_uuid_and_owner(&state.db, body.key, user.id)
        .await
        .map_err(map_err)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "not found"))?;

    models::soft_delete_upload(&state.db, upload.id)
        .await
        .map_err(map_err)?;
    let _ = storage::delete_object(&state.s3, &state.config.s3_bucket, &upload.storage_uri).await;

    Ok(Json(serde_json::json!({ "ok": "ok" })))
}

// ---------------------------------------------------------------------------
// POST /transfer/api/settings/add-password
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AddPasswordRequest {
    pub password: String,
}

pub async fn api_settings_add_password(
    State(state): State<TransferState>,
    jar: CookieJar,
    Json(body): Json<AddPasswordRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = auth::get_current_user(&jar, &state.db)
        .await
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "not authenticated"))?;

    if user.auth_id.is_some() {
        return Err(err(StatusCode::CONFLICT, "password already set"));
    }
    if body.password.len() < 8 {
        return Err(err(StatusCode::BAD_REQUEST, "password must be at least 8 characters"));
    }

    let (salt, hash) = common::crypto::hash_password(body.password.as_bytes()).map_err(map_err)?;
    let auth_id = models::insert_auth(&state.db, salt, hash).await.map_err(map_err)?;
    models::set_user_auth_id(&state.db, user.id, auth_id).await.map_err(map_err)?;

    Ok(Json(serde_json::json!({ "ok": "ok" })))
}
