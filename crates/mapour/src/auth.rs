//! Password hashing and cookie-token authentication.
//!
//! Passwords are stretched with PBKDF2-HMAC-SHA512 (via
//! [`common::crypto::derive_encryption_key`]) using a fresh random salt, and
//! the hex-encoded salt/hash pair is stored on the user row.
//!
//! Session tokens follow the spot approach: a random token is handed to the
//! browser as a cookie and only its HMAC (keyed by the configured signing
//! key) is stored server side.  Anonymous visitors get the same treatment via
//! a separate long-lived cookie backed by the `anons` table.

use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};

use crate::models::{Anon, User};
use crate::State;

pub const AUTH_COOKIE: &str = "mapour_auth";
pub const ANON_COOKIE: &str = "mapour_anon";

// ---------------------------------------------------------------------------
// Passwords
// ---------------------------------------------------------------------------

/// Hash `password` with a fresh salt, returning hex `(salt, hash)`.
pub fn new_pw_hash(password: &str) -> anyhow::Result<(String, String)> {
    let salt = common::crypto::new_salt().map_err(|e| anyhow::anyhow!("salt error: {e}"))?;
    let hash = common::crypto::derive_encryption_key(password.as_bytes(), &salt);
    Ok((hex::encode(salt), hex::encode(hash)))
}

/// Check `password` against a stored hex `(salt, hash)` pair.
///
/// Uses the double-HMAC pattern for the comparison: both sides are HMAC'd
/// under the same random key first, so the final equality check leaks no
/// usable timing information about the stored hash.
pub fn verify_pw(password: &str, salt_hex: &str, hash_hex: &str) -> bool {
    let Ok(salt) = hex::decode(salt_hex) else {
        return false;
    };
    let derived = common::crypto::derive_encryption_key(password.as_bytes(), &salt);
    let Ok(cmp_key) = common::crypto::rand_bytes(32) else {
        return false;
    };
    common::crypto::hmac_sign(&hex::encode(derived), &cmp_key)
        == common::crypto::hmac_sign(hash_hex, &cmp_key)
}

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

/// Generate a fresh random token (hex, 64 chars).
pub fn new_token() -> anyhow::Result<String> {
    let bytes = common::crypto::rand_bytes(32).map_err(|e| anyhow::anyhow!("rng error: {e}"))?;
    Ok(hex::encode(bytes))
}

/// The value stored server side for any token handed out to a client.
pub fn token_hash(token: &str, signing_key: &str) -> String {
    common::crypto::hmac_sign(token, signing_key.as_bytes())
}

fn build_cookie(name: &'static str, value: String, max_age_days: i64) -> Cookie<'static> {
    Cookie::build((name, value))
        .secure(true)
        .http_only(true)
        .same_site(SameSite::Lax)
        .max_age(time::Duration::days(max_age_days))
        .path("/")
        .build()
}

// ---------------------------------------------------------------------------
// User sessions
// ---------------------------------------------------------------------------

/// Create a session token for `user_id` and return the cookie carrying it.
pub async fn create_session(state: &State, user_id: i64) -> anyhow::Result<Cookie<'static>> {
    let token = new_token()?;
    let hash = token_hash(&token, &state.config.signing_key);
    sqlx::query(
        "insert into auth_tokens (hash, user_id, expires)
         values ($1, $2, now() + make_interval(days => $3::int))",
    )
    .bind(&hash)
    .bind(user_id)
    .bind(state.config.auth_token_days as i32)
    .fetch_optional(&state.db)
    .await?;
    Ok(build_cookie(
        AUTH_COOKIE,
        token,
        state.config.auth_token_days,
    ))
}

/// Resolve the current logged-in user from the auth cookie, if any.
pub async fn current_user(state: &State, jar: &CookieJar) -> Option<User> {
    let token = jar.get(AUTH_COOKIE)?.value().to_string();
    let hash = token_hash(&token, &state.config.signing_key);
    sqlx::query_as::<_, User>(
        "select u.* from users u
         inner join auth_tokens t on t.user_id = u.id
         where t.hash = $1 and t.expires > now()",
    )
    .bind(&hash)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| tracing::error!("error resolving current user: {e}"))
    .ok()
    .flatten()
}

/// Delete the session referenced by the auth cookie and return a removal cookie.
pub async fn end_session(state: &State, jar: &CookieJar) -> Cookie<'static> {
    if let Some(cookie) = jar.get(AUTH_COOKIE) {
        let hash = token_hash(cookie.value(), &state.config.signing_key);
        let _ = sqlx::query("delete from auth_tokens where hash = $1")
            .bind(&hash)
            .execute(&state.db)
            .await;
    }
    let mut removal = Cookie::build((AUTH_COOKIE, "")).path("/").build();
    removal.make_removal();
    removal
}

// ---------------------------------------------------------------------------
// Anonymous identities
// ---------------------------------------------------------------------------

/// Resolve the anonymous identity from the anon cookie, if present and known.
pub async fn current_anon(state: &State, jar: &CookieJar) -> Option<Anon> {
    let token = jar.get(ANON_COOKIE)?.value().to_string();
    let hash = token_hash(&token, &state.config.signing_key);
    sqlx::query_as::<_, Anon>("select * from anons where token_hash = $1")
        .bind(&hash)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| tracing::error!("error resolving anon: {e}"))
        .ok()
        .flatten()
}

/// Resolve or create an anonymous identity.  Returns the anon row and, when a
/// new identity was minted, the cookie that must be set on the response.
pub async fn get_or_create_anon(
    state: &State,
    jar: &CookieJar,
) -> anyhow::Result<(Anon, Option<Cookie<'static>>)> {
    if let Some(anon) = current_anon(state, jar).await {
        return Ok((anon, None));
    }
    let token = new_token()?;
    let hash = token_hash(&token, &state.config.signing_key);
    let anon = sqlx::query_as::<_, Anon>("insert into anons (token_hash) values ($1) returning *")
        .bind(&hash)
        .fetch_one(&state.db)
        .await?;
    // anon identities live long: they are how reloads / return visits keep
    // working before an account exists
    Ok((anon, Some(build_cookie(ANON_COOKIE, token, 365))))
}
