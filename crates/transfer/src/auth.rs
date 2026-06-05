use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use chrono::Utc;
use uuid::Uuid;

use crate::{models, Config};

pub const SESSION_COOKIE:        &str = "tx_session";
pub const SESSION_DURATION_SECS: i64 = 30 * 24 * 60 * 60;
pub const MAX_CODE_ATTEMPTS:     i32 = 5;
pub const RESEND_COOLDOWN_SECS:  i64 = 60;

#[derive(Debug)]
pub struct GoogleClaims {
    pub email: String,
    pub sub:   String,
}

/// Extract the current user from request cookies, extending the session on success (rolling window).
pub async fn get_current_user(
    cookies: &CookieJar,
    pool: &common::db::DbPool,
) -> Option<models::TransferUser> {
    let token_str = cookies.get(SESSION_COOKIE)?.value().to_string();
    let uuid = Uuid::parse_str(&token_str).ok()?;
    let (session, user) = models::get_session_with_user(pool, uuid).await.ok()??;
    if Utc::now() > session.expire_date {
        return None;
    }
    let new_expiry = Utc::now() + chrono::Duration::seconds(SESSION_DURATION_SECS);
    let _ = models::renew_session(pool, session.id, new_expiry).await;
    Some(user)
}

pub fn make_session_cookie(token: Uuid) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, token.to_string()))
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Lax)
        .path("/transfer")
        .max_age(time::Duration::seconds(SESSION_DURATION_SECS))
        .build()
}

pub fn clear_session_cookie() -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, ""))
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Lax)
        .path("/transfer")
        .max_age(time::Duration::ZERO)
        .build()
}

/// Generate a 6-digit numeric code and return (code_string, sha256_hash).
pub fn generate_verification_code() -> (String, Vec<u8>) {
    let bytes = common::crypto::rand_bytes(4).expect("rand_bytes failed");
    let n = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) % 1_000_000;
    let code = format!("{:06}", n);
    let hash = common::crypto::sha256(code.as_bytes());
    (code, hash)
}

pub async fn send_registration_code(
    config: &Config,
    to_email: &str,
    code: &str,
) -> anyhow::Result<()> {
    let smtp_cfg = config
        .smtp_config
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("SMTP not configured"))?;
    let subject = "Your transfer verification code";
    let body = format!(
        "Your verification code is: {}\n\nThis code expires in 10 minutes.\n\nIf you did not request this, please ignore this email.",
        code
    );
    common::smtp::send_email(smtp_cfg, to_email, subject, &body)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Verify a Google ID token via Google's tokeninfo endpoint.
/// Requires a live HTTPS call to Google per login.
pub async fn verify_google_token(
    http: &reqwest::Client,
    id_token: &str,
    expected_client_id: &str,
) -> anyhow::Result<GoogleClaims> {
    let url = format!(
        "https://oauth2.googleapis.com/tokeninfo?id_token={}",
        id_token
    );
    let resp: serde_json::Value = http
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let aud = resp["aud"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("tokeninfo: missing aud"))?;
    if aud != expected_client_id {
        return Err(anyhow::anyhow!("tokeninfo: invalid audience"));
    }

    let email_verified = resp["email_verified"]
        .as_str()
        .map(|s| s == "true")
        .unwrap_or(false);
    if !email_verified {
        return Err(anyhow::anyhow!("tokeninfo: email not verified by Google"));
    }

    Ok(GoogleClaims {
        email: resp["email"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("tokeninfo: missing email"))?
            .to_string(),
        sub: resp["sub"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("tokeninfo: missing sub"))?
            .to_string(),
    })
}

pub fn verify_code_hash(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.ct_eq(b).into()
}
