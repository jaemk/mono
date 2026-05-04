use sqlx::PgPool;

use crate::config::Config;
use crate::models;
use common::crypto;
use common::utils;
use common::Result;

#[derive(serde::Deserialize, Debug)]
pub struct SpotifyAccess {
    pub access_token: String,
    pub token_type: String,
    pub scope: String,
    pub expires_in: u64,
    pub refresh_token: Option<String>,
}

#[derive(serde::Serialize)]
struct SpotifyAccessParams {
    grant_type: String,
    code: String,
    redirect_uri: String,
}

impl SpotifyAccessParams {
    fn from_code(code: &str, config: &Config) -> Self {
        SpotifyAccessParams {
            grant_type: "authorization_code".to_string(),
            code: code.to_string(),
            redirect_uri: config.spotify_redirect_url(),
        }
    }
}

pub async fn new_spotify_access_token(code: &str, config: &Config) -> Result<SpotifyAccess> {
    let auth = base64::encode(
        format!("{}:{}", config.spotify_client_id, config.spotify_secret_id).as_bytes(),
    );
    let client = reqwest::Client::new();
    let resp = client
        .post("https://accounts.spotify.com/api/token")
        .form(&SpotifyAccessParams::from_code(code, config))
        .header("authorization", format!("Basic {}", auth))
        .send()
        .await
        .map_err(|e| format!("account request error {:?}", e))?;
    let access: SpotifyAccess = resp
        .json()
        .await
        .map_err(|e| crate::StringError(format!("json parse error {}", e)))?;
    Ok(access)
}

#[derive(serde::Serialize)]
struct RefreshParams {
    grant_type: String,
    refresh_token: String,
}

impl RefreshParams {
    fn from_token(token: &str) -> Self {
        RefreshParams {
            grant_type: "refresh_token".to_string(),
            refresh_token: token.to_string(),
        }
    }
}

pub async fn refresh_access_token(refresh_token: &str, config: &Config) -> Result<SpotifyAccess> {
    let auth = base64::encode(
        format!("{}:{}", config.spotify_client_id, config.spotify_secret_id).as_bytes(),
    );
    let client = reqwest::Client::new();
    let resp = client
        .post("https://accounts.spotify.com/api/token")
        .form(&RefreshParams::from_token(refresh_token))
        .header("authorization", format!("Basic {}", auth))
        .send()
        .await
        .map_err(|e| format!("account refresh request error {:?}", e))?;
    let access: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("account refresh json parse to value error {:?}", e))?;
    let copy = access.clone();

    if let Some(err) = access["error"].as_str() {
        if err == "invalid_grant" {
            return Err(Box::new(crate::UserAccessRevokedError));
        }
    }

    let access: SpotifyAccess = serde_json::from_value(access)
        .map_err(|e| format!("account refresh json parse error {:?}: {:?}", e, copy))?;
    Ok(access)
}

#[derive(serde::Deserialize)]
pub struct SpotifyNameEmail {
    pub display_name: String,
    pub email: String,
}

pub async fn get_new_user_name_email(access: &SpotifyAccess) -> Result<SpotifyNameEmail> {
    let client = reqwest::Client::new();
    let resp = client
        .get("https://api.spotify.com/v1/me")
        .header("authorization", format!("Bearer {}", access.access_token))
        .send()
        .await
        .map_err(|e| crate::StringError(format!("get user error {}", e)))?;
    Ok(resp
        .json()
        .await
        .map_err(|e| crate::StringError(format!("json error {}", e)))?)
}

pub fn spotify_expiry_seconds_to_epoch_expiration(expires_in: u64) -> Result<i64> {
    let now = std::time::SystemTime::now();
    Ok(now
        .checked_add(std::time::Duration::from_secs(expires_in - 60))
        .ok_or_else(|| format!("can't add {:?} to time {:?}", expires_in - 60, now))?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("invalid duration {:?}", e))?
        .as_secs() as i64)
}

pub async fn get_currently_playing(
    pool: &PgPool,
    user: &models::User,
    config: &Config,
) -> Result<Option<serde_json::Value>> {
    let access_token = get_user_access_token(pool, user, config).await?;
    let client = reqwest::Client::new();
    let resp = client
        .get("https://api.spotify.com/v1/me/player/currently-playing")
        .header("authorization", format!("Bearer {}", access_token))
        .send()
        .await
        .map_err(|e| format!("get currently playing error {:?}", e))?;
    if resp.status() == reqwest::StatusCode::NO_CONTENT {
        return Ok(None);
    }
    let resp: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("get currently playing json error {:?}", e))?;
    Ok(Some(resp))
}

pub async fn get_user_access_token(
    pool: &PgPool,
    user: &models::User,
    config: &Config,
) -> Result<String> {
    let enc_key = config.enc_key.as_key_ref();
    if user.access_expires > utils::now_seconds()? {
        let enc = crypto::Encrypted::decode(&user.access_token)?;
        let bytes = crypto::decrypt(&enc, &[enc_key])?;
        return Ok(
            String::from_utf8(bytes).map_err(|e| crate::StringError(format!("utf8 error: {e}")))?
        );
    }

    tracing::info!(user_id = %user.id, "refreshing access token for user");
    let enc = crypto::Encrypted::decode(&user.refresh_token)?;
    let bytes = crypto::decrypt(&enc, &[enc_key])?;
    let refresh_token =
        String::from_utf8(bytes).map_err(|e| crate::StringError(format!("utf8 error: {e}")))?;

    let access = refresh_access_token(&refresh_token, config).await?;
    let enc_access = crypto::encrypt(access.access_token.as_bytes(), enc_key)?;
    let access_expires = spotify_expiry_seconds_to_epoch_expiration(access.expires_in - 60)?;
    sqlx::query_as::<_, models::User>(
        "update spot.users set access_token = $1, access_expires = $2, modified = now() where id = $3 returning *",
    )
    .bind(enc_access.encode())
    .bind(access_expires)
    .bind(user.id)
    .fetch_one(pool)
    .await
    .map_err(|e| crate::StringError(format!("db error {}", e)))?;

    Ok(access.access_token)
}

pub async fn get_history(
    pool: &PgPool,
    user: &models::User,
    config: &Config,
) -> Result<serde_json::Value> {
    let access_token = get_user_access_token(pool, user, config).await?;
    let client = reqwest::Client::new();
    let resp = client
        .get("https://api.spotify.com/v1/me/player/recently-played?limit=50")
        .header("authorization", format!("Bearer {}", access_token))
        .send()
        .await
        .map_err(|e| format!("get history error {:?}", e))?;
    let resp: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("get history json error {:?}", e))?;
    Ok(resp)
}
