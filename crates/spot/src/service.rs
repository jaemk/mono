use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect},
    routing::get,
    Router,
};
use axum_extra::extract::cookie::{Cookie, CookieJar};
use chrono::{DateTime, Duration, TimeZone, Utc};
use sqlx::PgPool;

use crate::{models, spotify, SpotState};
use common::crypto;
use common::utils;
use common::Result;
use std::collections::HashMap;

macro_rules! user_or_redirect {
    ($state:expr, $jar:expr, $path:expr) => {{
        let user = get_auth_user(&$state.pool, &$jar, &$state.config).await;
        if user.is_none() {
            return (
                $jar,
                Redirect::temporary(&format!(
                    "{}/spot/login?redirect={}",
                    $state.config.redirect_host(),
                    $path
                )),
            )
                .into_response();
        }
        user.unwrap()
    }};
}

pub fn router<S>(state: S) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    SpotState: axum::extract::FromRef<S>,
{
    Router::new()
        .route("/", get(index))
        .route("/favicon.ico", get(favicon))
        .route("/status", get(status))
        .route("/login", get(login))
        .route("/auth", get(auth_callback))
        .route("/current", get(current_user))
        .route("/top", get(user_top))
        .route("/recent", get(recent))
        .route("/summary", get(summary))
        // api versions
        .route("/api/token", get(auth_token))
        .route("/api/status", get(status))
        .route("/api/login", get(login))
        .route("/api/auth", get(auth_callback))
        .route("/api/current", get(current_user))
        .route("/api/top", get(user_top))
        .route("/api/recent", get(recent))
        .route("/api/summary", get(summary))
        .with_state(state)
}

async fn index(State(state): State<SpotState>, jar: CookieJar) -> impl IntoResponse {
    let _ = user_or_redirect!(state, jar, "/spot");
    let content = std::fs::read_to_string("static/spot/index.html").unwrap_or_default();
    (StatusCode::OK, [("content-type", "text/html")], content).into_response()
}

async fn favicon() -> impl IntoResponse {
    let content = std::fs::read("static/spot/favicon.ico").unwrap_or_default();
    (StatusCode::OK, [("content-type", "image/x-icon")], content).into_response()
}

async fn status(State(state): State<SpotState>) -> impl IntoResponse {
    let version = state.config.version.clone();
    axum::Json(serde_json::json!({ "ok": "ok", "version": version }))
}

#[derive(serde::Deserialize)]
struct MaybeRedirect {
    redirect: Option<String>,
}

async fn login(
    State(state): State<SpotState>,
    Query(maybe_redirect): Query<MaybeRedirect>,
) -> impl IntoResponse {
    let token = match new_one_time_login_token(&state.pool, maybe_redirect.redirect.clone()).await {
        Ok(t) => t,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    tracing::info!(
        "redirecting to spotify-auth with state token {}, post-redirect-redirect {:?}",
        token,
        maybe_redirect.redirect,
    );
    Redirect::temporary(&format!("https://accounts.spotify.com/authorize?client_id={id}&response_type=code&redirect_uri={redirect}&scope={scope}&state={state_token}",
                id = state.config.spotify_client_id,
                redirect = state.config.spotify_redirect_url(),
                scope = "user-read-private user-read-email user-read-recently-played user-read-currently-playing user-read-playback-state user-modify-playback-state streaming",
                state_token = token)).into_response()
}

#[derive(Debug, serde::Deserialize)]
struct SpotifyAuthCallback {
    code: String,
    state: String,
}

async fn auth_callback(
    State(state): State<SpotState>,
    jar: CookieJar,
    Query(spotify_auth): Query<SpotifyAuthCallback>,
) -> impl IntoResponse {
    tracing::info!("got login redirect");
    if !is_valid_one_time_login_token(&state.pool, &spotify_auth).await {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({
                "error": format!("invalid one-time login token {}", spotify_auth.state)
            })),
        )
            .into_response();
    }
    let token_bytes = match base64::decode(&spotify_auth.state) {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("decode error {}", e)).into_response(),
    };
    let token_str = match String::from_utf8(token_bytes) {
        Ok(s) => s,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("token utf8 error {}", e)).into_response()
        }
    };
    let login_token: OneTimeLoginToken = match serde_json::from_str(&token_str) {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("deserialize token error {}", e),
            )
                .into_response()
        }
    };

    let spotify_access =
        match spotify::new_spotify_access_token(&spotify_auth.code, &state.config).await {
            Ok(a) => a,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("spotify access error {}", e),
                )
                    .into_response()
            }
        };
    let name_email = match spotify::get_new_user_name_email(&spotify_access).await {
        Ok(ne) => ne,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("error getting name {}", e),
            )
                .into_response()
        }
    };
    let new_auth_token = get_new_auth_token(&name_email.email);

    let user = match upsert_user(
        &state.pool,
        &state.config,
        &spotify_access,
        &name_email,
        &new_auth_token,
    )
    .await
    {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("user upsert error {}", e),
            )
                .into_response()
        }
    };
    let is_new = user.created == user.modified;
    tracing::info!(user_email = %user.email, user_id = %user.id, is_new = %is_new, "completing user login");
    if is_new {
        tracing::info!(user_email = %user.email, "inserting recently played for new user");
        if let Err(e) = _recently_played_user(&state.pool, &state.config, &user).await {
            tracing::error!(user_email = %user.email, error = %e, "error loading recently played for new user");
        }
    }

    let cookie = Cookie::build(("auth_token", new_auth_token))
        .domain(state.config.domain())
        .secure(true)
        .http_only(true)
        .max_age(time::Duration::days(30))
        .path("/")
        .build();

    let jar = jar.add(cookie);

    if let Some(redirect) = login_token.redirect {
        if !redirect.contains("login") {
            tracing::info!(redirect = ?redirect, "found login redirect");
            return (
                jar,
                Redirect::temporary(&format!("{}{}", state.config.redirect_host(), redirect)),
            )
                .into_response();
        }
    }
    (
        jar,
        axum::Json(serde_json::json!({
            "ok": "ok",
            "user.id": user.id,
            "user.display_name": &user.name,
            "user.email": &user.email,
        })),
    )
        .into_response()
}

#[derive(sqlx::FromRow, Debug, serde::Serialize, serde::Deserialize)]
pub struct CurrentUser {
    pub user_id: i64,
    pub user_name: String,
    pub play_id: i64,
    pub played_at: chrono::DateTime<chrono::Utc>,
    pub played_at_minute: chrono::DateTime<chrono::Utc>,
    pub track_name: String,
    pub track_artist_names: Vec<String>,
    pub last_known_listen: Option<chrono::DateTime<chrono::Utc>>,
    pub is_listening: Option<bool>,
}

#[derive(serde::Serialize)]
struct CurrentUserResponse {
    user: CurrentUser,
}

async fn current_user(State(state): State<SpotState>, jar: CookieJar) -> impl IntoResponse {
    let user = user_or_redirect!(state, jar, "/spot/current");
    let current = sqlx::query_as::<_, CurrentUser>(
        "select
            distinct on(u.id) u.id as user_id,
            u.name as user_name,
            p.id as play_id,
            p.played_at,
            p.played_at_minute,
            p.name as track_name,
            p.artist_names as track_artist_names,
            p.last_known_listen,
            extract(epoch from(now() - p.last_known_listen)) < 60 as is_listening
        from spot.users u inner join spot.plays p on u.id = p.user_id
        where u.id = $1
        order by u.id, p.played_at desc, p.id",
    )
    .bind(user.id)
    .fetch_one(&state.pool)
    .await;

    match current {
        Ok(c) => axum::Json(CurrentUserResponse { user: c }).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("error fetching current user {:?}", e),
        )
            .into_response(),
    }
}

#[derive(serde::Serialize)]
struct AuthTokenResponse {
    token: String,
}

async fn auth_token(State(state): State<SpotState>, jar: CookieJar) -> impl IntoResponse {
    let user = user_or_redirect!(state, jar, "/spot/api/token");
    match spotify::get_user_access_token(&state.pool, &user, &state.config).await {
        Ok(token) => axum::Json(AuthTokenResponse { token }).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("error fetching access token {:?}", e),
        )
            .into_response(),
    }
}

#[derive(sqlx::FromRow, Debug, serde::Serialize, serde::Deserialize)]
pub struct UserTop {
    pub artist_names: Vec<String>,
    pub count: Option<i64>,
}

#[derive(serde::Serialize)]
struct TopResponse {
    top: Vec<UserTop>,
}

async fn user_top(State(state): State<SpotState>, jar: CookieJar) -> impl IntoResponse {
    let user = user_or_redirect!(state, jar, "/spot/top");
    let top = sqlx::query_as::<_, UserTop>(
        "with src as (
            select artist_names, count(*)
            from spot.plays
            where user_id = $1
            group by artist_names
        )
        select artist_names, count
        from src
        order by count desc
        limit 10",
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await;

    match top {
        Ok(t) => axum::Json(TopResponse { top: t }).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("error fetching user top {:?}", e),
        )
            .into_response(),
    }
}

#[derive(serde::Serialize)]
struct RecentResponse {
    count: usize,
    recent: Vec<models::Play>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct RecentParams {
    days: Option<u64>,
}
impl RecentParams {
    fn range_days(&self) -> i64 {
        self.days.unwrap_or(7) as i64
    }
    fn range_start(&self) -> DateTime<Utc> {
        Utc::now()
            .checked_sub_signed(Duration::days(self.range_days()))
            .or_else(|| Some("2021-03-01".parse::<DateTime<Utc>>().unwrap()))
            .unwrap()
    }
}

async fn recent(
    State(state): State<SpotState>,
    jar: CookieJar,
    Query(params): Query<RecentParams>,
) -> impl IntoResponse {
    let user = user_or_redirect!(state, jar, "/spot/recent");
    let range_start = params.range_start();
    tracing::info!(
        user_id = %user.id,
        range_days = %params.range_days(),
        range_start = %params.range_start().to_string(),
        "fetching recent plays for user"
    );
    let recent = sqlx::query_as::<_, models::Play>(
        "select *
        from spot.plays
        where user_id = $1
            and played_at > $2
        order by played_at desc",
    )
    .bind(user.id)
    .bind(range_start)
    .fetch_all(&state.pool)
    .await;

    match recent {
        Ok(r) => axum::Json(RecentResponse {
            count: r.len(),
            recent: r,
        })
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("error getting plays for user {} {}", user.id, e),
        )
            .into_response(),
    }
}

#[derive(serde::Serialize)]
struct SummaryResponse {
    summary: Vec<models::PlaySummary>,
}

async fn summary(
    State(state): State<SpotState>,
    jar: CookieJar,
    Query(params): Query<RecentParams>,
) -> impl IntoResponse {
    let user = user_or_redirect!(state, jar, "/spot/summary");
    let range_start = params.range_start();
    tracing::info!(
        user_id = %user.id,
        range_days = %params.range_days(),
        range_start = %params.range_start().to_string(),
        "fetching play summary for user"
    );
    let summary = sqlx::query_as::<_, models::PlaySummary>(
        "select played_at::date as date, count(*)
            from spot.plays
        where user_id = $1
            and played_at > $2
        group by played_at::date
        order by played_at::date desc",
    )
    .bind(user.id)
    .bind(range_start)
    .fetch_all(&state.pool)
    .await;

    match summary {
        Ok(s) => axum::Json(SummaryResponse { summary: s }).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("error getting play summary for user {} {}", user.id, e),
        )
            .into_response(),
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct OneTimeLoginToken {
    token: String,
    redirect: Option<String>,
}

async fn new_one_time_login_token(pool: &PgPool, redirect: Option<String>) -> Result<String> {
    let mut buffer = uuid::Uuid::encode_buffer();
    let s = uuid::Uuid::new_v4()
        .simple()
        .encode_lower(&mut buffer)
        .to_string();
    let s = serde_json::to_string(&OneTimeLoginToken { token: s, redirect })
        .map_err(|e| crate::StringError(format!("token json error {}", e)))?;
    let token = base64::encode_config(&s, base64::URL_SAFE);
    insert_one_time_token(pool, &token).await?;
    Ok(token)
}

pub(crate) async fn insert_one_time_token(pool: &PgPool, token: &str) -> Result<()> {
    let expires = chrono::Utc::now()
        .checked_add_signed(chrono::Duration::seconds(120))
        .ok_or_else(|| crate::StringError("error computing token expiry".to_string()))?;
    sqlx::query("insert into spot.one_time_tokens (token, expires) values ($1, $2)")
        .bind(token)
        .bind(expires)
        .execute(pool)
        .await
        .map_err(|e| crate::StringError(format!("failed to insert one-time token: {:?}", e)))?;
    Ok(())
}

async fn is_valid_one_time_login_token(pool: &PgPool, auth: &SpotifyAuthCallback) -> bool {
    consume_one_time_token(pool, &auth.state)
        .await
        .unwrap_or(false)
}

pub(crate) async fn consume_one_time_token(pool: &PgPool, token: &str) -> Result<bool> {
    let row = sqlx::query(
        "delete from spot.one_time_tokens
         where token = $1 and expires > now()
         returning token",
    )
    .bind(token)
    .fetch_optional(pool)
    .await
    .map_err(|e| crate::StringError(format!("failed to consume one-time token: {:?}", e)))?;
    Ok(row.is_some())
}

fn get_new_auth_token(email: &str) -> String {
    let mut buffer = uuid::Uuid::encode_buffer();
    let s = uuid::Uuid::new_v4()
        .simple()
        .encode_lower(&mut buffer)
        .to_string();
    let s = format!("{}:{}", email, s);
    let b = crypto::sha256(s.as_bytes());
    hex::encode(&b)
}

async fn upsert_user(
    pool: &PgPool,
    config: &crate::config::Config,
    access: &spotify::SpotifyAccess,
    name_email: &spotify::SpotifyNameEmail,
    new_auth_token: &str,
) -> Result<models::User> {
    let scopes = access
        .scope
        .split_whitespace()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let access_expires =
        spotify::spotify_expiry_seconds_to_epoch_expiration(access.expires_in - 60)?;

    let enc_key = config.enc_key.as_key_ref();
    let access_token = crypto::encrypt(access.access_token.as_bytes(), enc_key)?;
    let refresh_token = crypto::encrypt(
        access
            .refresh_token
            .as_ref()
            .ok_or_else(|| crate::StringError("missing refresh token".to_string()))?
            .as_bytes(),
        enc_key,
    )?;
    let auth_token = crypto::hmac_sign(new_auth_token, config.enc_key.as_key_ref().key);
    let mut tr = pool
        .begin()
        .await
        .map_err(|e| format!("error starting user transaction {:?}", e))?;
    let user = sqlx::query_as::<_, models::User>(
        "insert into
        spot.users (
            email, name, scopes,
            access_token,
            refresh_token,
            access_expires,
            auth_token
        )
        values ($1, $2, $3, $4, $5, $6, $7)
        on conflict (email) do update set name = excluded.name, scopes = excluded.scopes,
        access_token = excluded.access_token,
        refresh_token = excluded.refresh_token,
        access_expires = excluded.access_expires, auth_token = excluded.auth_token,
        modified = now(), revoked = false
        returning *",
    )
    .bind(&name_email.email)
    .bind(&name_email.display_name)
    .bind(scopes.as_slice())
    .bind(access_token.encode())
    .bind(refresh_token.encode())
    .bind(access_expires)
    .bind(&auth_token)
    .fetch_one(&mut tr)
    .await
    .map_err(|e| format!("error upserting user {:?}", e))?;
    let expires = Utc::now()
        .checked_add_signed(Duration::seconds(config.auth_expiration_seconds as i64))
        .ok_or("error creating expiration timestamp")?;
    sqlx::query(
        "insert into
        spot.auth_tokens (
            hash, user_id, expires
        )
        values ($1, $2, $3)",
    )
    .bind(&auth_token)
    .bind(user.id)
    .bind(expires)
    .execute(&mut tr)
    .await
    .map_err(|e| format!("failed to insert user auth token {:?}", e))?;
    tr.commit()
        .await
        .map_err(|e| format!("error committing user insert {:?}", e))?;

    Ok(user)
}

async fn get_auth_user(
    pool: &PgPool,
    jar: &CookieJar,
    config: &crate::config::Config,
) -> Option<models::User> {
    match jar.get("auth_token") {
        None => {
            tracing::info!("no auth token cookie found");
            None
        }
        Some(cookie) => {
            let token = cookie.value();
            let hash = crypto::hmac_sign(token, config.enc_key.as_key_ref().key);
            let u = sqlx::query_as::<_, models::User>(
                "select u.*
                from spot.users u
                    inner join spot.auth_tokens at
                    on u.id = at.user_id
                where hash = $1 and expires > now()",
            )
            .bind(&hash)
            .fetch_one(pool)
            .await
            .ok();
            tracing::debug!(user = ?u, "current user");
            if let Some(ref u) = u {
                sqlx::query("delete from spot.auth_tokens where user_id = $1 and expires <= now()")
                    .bind(u.id)
                    .execute(pool)
                    .await
                    .map_err(|e| {
                        format!(
                            "error deleting expired auth tokens for user {}, continuing: {:?}",
                            u.id, e
                        )
                    })
                    .ok();
            }
            u
        }
    }
}

async fn _recently_played_user(
    pool: &PgPool,
    config: &crate::config::Config,
    user: &models::User,
) -> Result<()> {
    let mut new_plays = vec![];
    let recent = spotify::get_history(pool, user, config).await?;
    for item in recent["items"]
        .as_array()
        .ok_or_else(|| format!("items: unexpected shape {:?}", recent))?
    {
        let played_at = item["played_at"]
            .as_str()
            .ok_or_else(|| format!("played_at: unexpected shape {:?}", item))?
            .parse::<chrono::DateTime<chrono::Utc>>()
            .map_err(|e| format!("invalid datetime {:?}", e))?;
        let duration_ms = chrono::Duration::milliseconds(
            item["track"]["duration_ms"]
                .as_i64()
                .ok_or_else(|| format!("duration: unexpected shape {:?}", item))?,
        );
        let played_at = played_at - duration_ms;
        let played_at_minute = utils::truncate_to_minute(played_at)?;
        let spotify_id = item["track"]["id"]
            .as_str()
            .ok_or_else(|| format!("spotify_id: unexpected shape {:?}", item))?;
        let name = item["track"]["name"]
            .as_str()
            .ok_or_else(|| format!("track name: unexpected shape {:?}", item))?;
        let album_name = item["track"]["album"]["name"]
            .as_str()
            .ok_or_else(|| format!("currently playing album name: unexpected shape {:?}", item))?;
        let album_id = item["track"]["album"]["id"]
            .as_str()
            .ok_or_else(|| format!("currently playing album name: unexpected shape {:?}", item))?;
        let album_images = &item["track"]["album"]["images"];
        if album_images.is_null() {
            return Err(crate::StringError(format!(
                "currently playing album images: unexpected shape {:?}",
                item
            ))
            .into());
        }
        let mut artist_names = vec![];
        let mut artist_ids = vec![];
        for artist in item["track"]["artists"]
            .as_array()
            .ok_or_else(|| format!("track artists: unexpected shape {:?}", item))?
        {
            artist_names.push(
                artist["name"]
                    .as_str()
                    .ok_or_else(|| format!("artist name: unexpected shape {:?}", artist))?
                    .to_string(),
            );
            artist_ids.push(
                artist["id"]
                    .as_str()
                    .ok_or_else(|| format!("artist id: unexpected shape {:?}", artist))?
                    .to_string(),
            );
        }

        #[derive(sqlx::FromRow)]
        struct Around {
            before: Option<String>,
            after: Option<String>,
        }
        let around_time = sqlx::query_as::<_, Around>(
            "select
                (select spotify_id from spot.plays
                    where user_id = $1 and played_at <= $2
                    order by played_at desc limit 1) as before,
                (select spotify_id from spot.plays
                    where user_id = $1 and played_at >= $2
                    order by played_at asc limit 1) as after",
        )
        .bind(user.id)
        .bind(played_at_minute)
        .fetch_one(pool)
        .await
        .map_err(|e| format!("failed to query before and after plays {:?}", e))?;
        let some_spotify_id = Some(spotify_id.to_string());
        let probably_exists =
            around_time.before == some_spotify_id || around_time.after == some_spotify_id;
        if !probably_exists {
            sqlx::query(
                "insert into spot.tracks
                (spotify_id, name, artist_names, artist_ids, album_name, album_id, album_images)
                values
                ($1, $2, $3, $4, $5, $6, $7)
                on conflict (spotify_id) do update set
                name = excluded.name, artist_names = excluded.artist_names,
                artist_ids = excluded.artist_ids,
                album_name = excluded.album_name, album_id = excluded.album_id,
                album_images = excluded.album_images,
                modified = now()",
            )
            .bind(spotify_id)
            .bind(name)
            .bind(artist_names.as_slice())
            .bind(artist_ids.as_slice())
            .bind(album_name)
            .bind(album_id)
            .bind(album_images)
            .execute(pool)
            .await
            .map_err(|e| format!("failed to upsert track {:?}", e))?;
            let new_play = sqlx::query_as::<_, models::NewPlay>(
                "insert into spot.plays
                (user_id, spotify_id, played_at, played_at_minute, name, artist_names)
                values
                ($1, $2, $3, $4, $5, $6)
                on conflict (user_id, spotify_id, played_at_minute) do update set modified = now()
                returning id, created, modified",
            )
            .bind(user.id)
            .bind(spotify_id)
            .bind(played_at)
            .bind(played_at_minute)
            .bind(name)
            .bind(artist_names.as_slice())
            .fetch_one(pool)
            .await
            .map_err(|e| format!("failed to insert play {:?}", e))?;
            if new_play.created == new_play.modified {
                new_plays.push(new_play.id);
            }
        }
    }
    if !new_plays.is_empty() {
        tracing::info!(new_plays = ?new_plays, "inserted new plays");
    }
    Ok(())
}

async fn revoke_user(pool: &PgPool, user: &models::User) -> Result<()> {
    tracing::info!(user_id = %user.id, "revoking user");
    sqlx::query(
        "update spot.users
            set revoked = true
            where id = $1",
    )
    .bind(user.id)
    .execute(pool)
    .await
    .map_err(|e| crate::StringError(format!("failed updating user revoked status {:?}", e)))?;
    Ok(())
}

async fn _currently_playing_user(
    pool: &PgPool,
    config: &crate::config::Config,
    user: &models::User,
) -> Result<()> {
    let (revoke, current) = match spotify::get_currently_playing(pool, user, config).await {
        Ok(c) => (false, c),
        Err(e) => {
            if e.downcast_ref::<crate::UserAccessRevokedError>().is_some() {
                (true, None)
            } else {
                return Err(e);
            }
        }
    };
    if revoke {
        revoke_user(pool, user).await?;
        return Ok(());
    }

    if let Some(current) = current {
        if current["item"].is_null() {
            tracing::debug!(user_id = %user.id, "currently playing for user is not a track");
            return Ok(());
        }
        let spotify_id = &current["item"]["id"];
        if spotify_id.is_null() {
            return Ok(());
        }

        let is_playing = current["is_playing"].as_bool().ok_or_else(|| {
            crate::StringError(format!(
                "currently playing is_playing: unexpected shape {:?}",
                current
            ))
        })?;
        if !is_playing {
            return Ok(());
        }

        let start_millis = current["timestamp"].as_i64().ok_or_else(|| {
            crate::StringError(format!(
                "currently playing timestamp: unexpected shape {:?}",
                current
            ))
        })?;
        let played_at = chrono::Utc.timestamp_millis_opt(start_millis).unwrap();
        let played_at_minute = utils::truncate_to_minute(played_at)?;
        let spotify_id = spotify_id.as_str().ok_or_else(|| {
            crate::StringError(format!(
                "currently playing spotify_id: unexpected shape {:?}",
                current
            ))
        })?;
        let name = current["item"]["name"].as_str().ok_or_else(|| {
            crate::StringError(format!(
                "currently playing name: unexpected shape {:?}",
                current
            ))
        })?;
        let album_name = current["item"]["album"]["name"].as_str().ok_or_else(|| {
            crate::StringError(format!(
                "currently playing album name: unexpected shape {:?}",
                current
            ))
        })?;
        let album_id = current["item"]["album"]["id"].as_str().ok_or_else(|| {
            crate::StringError(format!(
                "currently playing album name: unexpected shape {:?}",
                current
            ))
        })?;
        let album_images = &current["item"]["album"]["images"];
        if album_images.is_null() {
            return Err(crate::StringError(format!(
                "currently playing album images: unexpected shape {:?}",
                current
            ))
            .into());
        }
        let mut artist_names = vec![];
        let mut artist_ids = vec![];
        for artist in current["item"]["artists"].as_array().ok_or_else(|| {
            crate::StringError(format!(
                "currently playing artists: unexpected shape {:?}",
                current
            ))
        })? {
            artist_names.push(
                artist["name"]
                    .as_str()
                    .ok_or_else(|| {
                        crate::StringError(format!(
                            "currently playing artist name: unexpected shape {:?}",
                            artist
                        ))
                    })?
                    .to_string(),
            );
            artist_ids.push(
                artist["id"]
                    .as_str()
                    .ok_or_else(|| {
                        crate::StringError(format!(
                            "currently playing artist id: unexpected shape {:?}",
                            artist
                        ))
                    })?
                    .to_string(),
            );
        }
        sqlx::query(
            "update spot.users
                set last_known_listen = now()
                where id = $1",
        )
        .bind(user.id)
        .execute(pool)
        .await
        .map_err(|e| {
            crate::StringError(format!("failed updating user last known listen {:?}", e))
        })?;
        let latest = sqlx::query_as::<_, models::Play>(
            "select * from spot.plays where user_id = $1
            order by played_at desc
            limit 1",
        )
        .bind(user.id)
        .fetch_optional(pool)
        .await
        .map_err(|e| crate::StringError(format!("failed fetching optional latest play {:?}", e)))?;

        let current_is_latest = latest
            .as_ref()
            .map(|play| play.spotify_id == spotify_id)
            .unwrap_or(false);
        if current_is_latest {
            let latest = latest.unwrap();
            tracing::debug!(user_email = %user.email, track_name = %name, "currently listening (no change)");
            sqlx::query(
                "update spot.plays
                    set modified = now(),
                        last_known_listen = now()
                    where id = $1",
            )
            .bind(latest.id)
            .execute(pool)
            .await
            .map_err(|e| {
                crate::StringError(format!("failed updating play last known listen {:?}", e))
            })?;
        } else {
            sqlx::query(
                "insert into spot.tracks
                (spotify_id, name, artist_names, artist_ids, album_name, album_id, album_images)
                values
                ($1, $2, $3, $4, $5, $6, $7)
                on conflict (spotify_id) do update set
                name = excluded.name, artist_names = excluded.artist_names,
                artist_ids = excluded.artist_ids,
                album_name = excluded.album_name, album_id = excluded.album_id,
                album_images = excluded.album_images,
                modified = now()",
            )
            .bind(spotify_id)
            .bind(name)
            .bind(artist_names.as_slice())
            .bind(artist_ids.as_slice())
            .bind(album_name)
            .bind(album_id)
            .bind(album_images)
            .execute(pool)
            .await
            .map_err(|e| crate::StringError(format!("failed to upsert track {:?}", e)))?;
            let new_play = sqlx::query_as::<_, models::NewPlay>(
                "insert into spot.plays
                (user_id, spotify_id, played_at, played_at_minute, name, artist_names, last_known_listen)
                values
                ($1, $2, $3, $4, $5, $6, now())
                on conflict (user_id, spotify_id, played_at_minute)
                do update set modified = now(), last_known_listen = excluded.last_known_listen
                returning id, created, modified"
            )
            .bind(user.id)
            .bind(spotify_id)
            .bind(played_at)
            .bind(played_at_minute)
            .bind(name)
            .bind(artist_names.as_slice())
            .fetch_one(pool)
            .await
            .map_err(|e| crate::StringError(format!("failed to insert play for user {:?} {:?}", user.id, e)))?;
            if new_play.created == new_play.modified {
                tracing::info!(user_email = %user.email, track_name = %name, "new current song");
            }
        }
    };
    Ok(())
}

async fn _background_currently_playing_poll_inner(
    pool: &PgPool,
    config: &crate::config::Config,
) -> Result<()> {
    let now = Utc::now();
    let two_minutes_ago = now
        .checked_sub_signed(Duration::seconds(120))
        .ok_or_else(|| crate::StringError("error subtracting 2mins from now".to_string()))?;
    let ten_seconds_ago = now
        .checked_sub_signed(Duration::seconds(10))
        .ok_or_else(|| crate::StringError("error subtracting 10s from now".to_string()))?;
    let thirty_seconds_ago = now
        .checked_sub_signed(Duration::seconds(30))
        .ok_or_else(|| crate::StringError("error subtracting 30s from now".to_string()))?;

    let active_users = sqlx::query_as::<_, models::User>(
        "select * from spot.users
        where
            revoked is false
            and poll_enabled is true
            and (
                (last_known_listen >= $1 and last_poll < $2)
                or last_known_listen is null
                or last_poll is null
            )",
    )
    .bind(two_minutes_ago)
    .bind(ten_seconds_ago)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("error getting active users for poll {:?}", e))?;

    let inactive_users = sqlx::query_as::<_, models::User>(
        "select * from spot.users
        where last_known_listen < $1
            and last_poll < $2
            and revoked is false
            and poll_enabled is true",
    )
    .bind(two_minutes_ago)
    .bind(thirty_seconds_ago)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("error getting inactive users for poll {:?}", e))?;

    let active_users_count = active_users.len();
    let inactive_users_count = inactive_users.len();

    let mut users = HashMap::with_capacity(active_users.len() + inactive_users.len());
    for u in active_users.into_iter().chain(inactive_users) {
        users.insert(u.id, u);
    }

    tracing::info!(
        users_count = %users.len(),
        active_users = %active_users_count,
        inactive_users = %inactive_users_count,
        "polling users"
    );
    for user in users.values() {
        let mut conn = match pool.acquire().await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = ?e, "error acquiring connection for user poll");
                continue;
            }
        };

        // set lock timeout
        if let Err(e) = sqlx::query(&format!(
            "set local lock_timeout = '{}s'",
            config.poll_lock_timeout_seconds
        ))
        .execute(&mut conn)
        .await
        {
            tracing::error!(user_id = %user.id, error = ?e, "error setting lock timeout for user poll");
            continue;
        }

        use sqlx::Row;
        let locked: bool = match sqlx::query("select pg_try_advisory_lock($1)")
            .bind(user.id)
            .fetch_one(&mut conn)
            .await
        {
            Ok(row) => row.get(0),
            Err(e) => {
                tracing::error!(user_id = %user.id, error = ?e, "error acquiring advisory lock for user poll");
                continue;
            }
        };

        if !locked {
            tracing::debug!(user_id = %user.id, "could not acquire advisory lock, skipping");
            continue;
        }

        if let Err(e) = _currently_playing_user(pool, config, user).await {
            tracing::error!(
                user_email = %user.email,
                error = %e,
                "error polling currently playing for user"
            );
        }

        if let Err(e) = sqlx::query(
            "update spot.users
                set last_poll = now(), modified = now()
                where id = $1",
        )
        .bind(user.id)
        .execute(pool)
        .await
        {
            tracing::error!(user_id = %user.id, error = ?e, "error setting last_poll for user")
        }

        let _ = sqlx::query("select pg_advisory_unlock($1)")
            .bind(user.id)
            .execute(&mut conn)
            .await;
    }
    Ok(())
}

pub async fn background_currently_playing_poll(state: SpotState) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(
            state.config.poll_interval_seconds,
        ))
        .await;
        if let Err(e) = _background_currently_playing_poll_inner(&state.pool, &state.config).await {
            tracing::error!(error = ?e, "error while running background currently playing poll");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::db::init_pool;

    async fn test_pool() -> PgPool {
        let config = crate::Config::load();
        init_pool(&config.db_url)
            .await
            .expect("failed to connect to test db")
    }

    // ---------------------------------------------------------------------------
    // Helpers
    // ---------------------------------------------------------------------------

    /// Wipe all spot schema tables so each test starts from a clean slate.
    async fn clean_spot_db(pool: &PgPool) {
        common::test_utils::truncate_tables(
            pool,
            &[
                "spot.plays",
                "spot.tracks",
                "spot.auth_tokens",
                "spot.one_time_tokens",
                "spot.users",
            ],
        )
        .await;
    }

    // ---------------------------------------------------------------------------
    // RecentParams – pure logic tests (no DB needed)
    // ---------------------------------------------------------------------------

    #[test]
    fn recent_params_default_range_days_is_7() {
        let params = RecentParams { days: None };
        assert_eq!(params.range_days(), 7);
    }

    #[test]
    fn recent_params_custom_range_days() {
        let params = RecentParams { days: Some(30) };
        assert_eq!(params.range_days(), 30);
    }

    #[test]
    fn recent_params_range_start_is_in_the_past() {
        let params = RecentParams { days: Some(7) };
        let start = params.range_start();
        assert!(start < Utc::now(), "range_start should be in the past");
    }

    #[test]
    fn recent_params_range_start_is_approximately_correct() {
        let params = RecentParams { days: Some(7) };
        let start = params.range_start();
        let expected = Utc::now().checked_sub_signed(Duration::days(7)).unwrap();
        let diff_secs = (Utc::now() - start).num_seconds();
        assert!(
            (604_798..=604_802).contains(&diff_secs),
            "range_start should be approximately 7 days ago, diff={diff_secs}s"
        );
        let _ = expected;
    }

    // ---------------------------------------------------------------------------
    // get_new_auth_token – pure logic tests (no DB needed)
    // ---------------------------------------------------------------------------

    #[test]
    fn get_new_auth_token_returns_hex_string() {
        let token = get_new_auth_token("test@example.com");
        assert!(!token.is_empty());
        assert!(
            token.chars().all(|c| c.is_ascii_hexdigit()),
            "auth token should be a hex string, got: {token}"
        );
    }

    #[test]
    fn get_new_auth_token_is_not_deterministic() {
        let t1 = get_new_auth_token("user@example.com");
        let t2 = get_new_auth_token("user@example.com");
        assert_ne!(t1, t2, "two calls should produce different tokens");
    }

    #[test]
    fn get_new_auth_token_differs_per_email() {
        let t1 = get_new_auth_token("alice@example.com");
        let t2 = get_new_auth_token("bob@example.com");
        assert_ne!(t1, t2);
    }

    #[test]
    fn get_new_auth_token_length_is_64() {
        let token = get_new_auth_token("len@example.com");
        assert_eq!(token.len(), 64, "hex-encoded SHA-256 must be 64 chars");
    }

    // ---------------------------------------------------------------------------
    // OneTimeLoginToken – serialization tests (no DB needed)
    // ---------------------------------------------------------------------------

    #[test]
    fn one_time_login_token_serializes_without_redirect() {
        let t = OneTimeLoginToken {
            token: "abc123".to_string(),
            redirect: None,
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: OneTimeLoginToken = serde_json::from_str(&json).unwrap();
        assert_eq!(back.token, "abc123");
        assert!(back.redirect.is_none());
    }

    #[test]
    fn one_time_login_token_serializes_with_redirect() {
        let t = OneTimeLoginToken {
            token: "xyz789".to_string(),
            redirect: Some("/spot/top".to_string()),
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: OneTimeLoginToken = serde_json::from_str(&json).unwrap();
        assert_eq!(back.token, "xyz789");
        assert_eq!(back.redirect.as_deref(), Some("/spot/top"));
    }

    // ---------------------------------------------------------------------------
    // DB tests – one_time_token lifecycle
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn test_insert_and_consume_token() {
        let pool = test_pool().await;
        clean_spot_db(&pool).await;

        let token = "test-insert-consume-token";
        insert_one_time_token(&pool, token)
            .await
            .expect("insert should succeed");

        let consumed = consume_one_time_token(&pool, token)
            .await
            .expect("consume should not error");
        assert!(consumed, "first consume should return true");
    }

    #[tokio::test]
    async fn test_token_can_only_be_consumed_once() {
        let pool = test_pool().await;
        clean_spot_db(&pool).await;

        let token = "test-consume-once-token";
        insert_one_time_token(&pool, token)
            .await
            .expect("insert should succeed");

        let first = consume_one_time_token(&pool, token)
            .await
            .expect("first consume should not error");
        assert!(first, "first consume should return true");

        let second = consume_one_time_token(&pool, token)
            .await
            .expect("second consume should not error");
        assert!(
            !second,
            "second consume should return false (already consumed)"
        );
    }

    #[tokio::test]
    async fn test_consume_nonexistent_token() {
        let pool = test_pool().await;
        clean_spot_db(&pool).await;

        let consumed = consume_one_time_token(&pool, "never-inserted-token-xyz")
            .await
            .expect("consume should not error");
        assert!(
            !consumed,
            "consuming a nonexistent token should return false"
        );
    }

    #[tokio::test]
    async fn test_consume_expired_token() {
        let pool = test_pool().await;
        clean_spot_db(&pool).await;

        let token = "test-expired-token";
        let past = chrono::Utc::now()
            .checked_sub_signed(chrono::Duration::seconds(10))
            .unwrap();
        sqlx::query("insert into spot.one_time_tokens (token, expires) values ($1, $2)")
            .bind(token)
            .bind(past)
            .execute(&pool)
            .await
            .expect("inserting expired token should succeed");

        let consumed = consume_one_time_token(&pool, token)
            .await
            .expect("consume should not error");
        assert!(!consumed, "an expired token should not be consumable");
    }

    #[tokio::test]
    async fn test_new_one_time_login_token_round_trip() {
        let pool = test_pool().await;
        clean_spot_db(&pool).await;

        let tok = new_one_time_login_token(&pool, None)
            .await
            .expect("generating token should succeed");

        let auth = SpotifyAuthCallback {
            code: "dummy-code".to_string(),
            state: tok.clone(),
        };

        assert!(
            is_valid_one_time_login_token(&pool, &auth).await,
            "token should be valid immediately after creation"
        );

        assert!(
            !is_valid_one_time_login_token(&pool, &auth).await,
            "token should not be valid after it has been consumed"
        );
    }

    #[tokio::test]
    async fn test_new_one_time_login_token_with_redirect() {
        let pool = test_pool().await;
        clean_spot_db(&pool).await;

        let tok = new_one_time_login_token(&pool, Some("/spot/top".to_string()))
            .await
            .expect("generating token with redirect should succeed");

        let bytes =
            base64::decode_config(&tok, base64::URL_SAFE).expect("token should be valid base64");
        let json: OneTimeLoginToken =
            serde_json::from_slice(&bytes).expect("token payload should deserialise");
        assert_eq!(json.redirect.as_deref(), Some("/spot/top"));

        let auth = SpotifyAuthCallback {
            code: "dummy-code".to_string(),
            state: tok,
        };
        assert!(
            is_valid_one_time_login_token(&pool, &auth).await,
            "redirect token should be valid"
        );
    }

    // ---------------------------------------------------------------------------
    // DB tests – token count after truncate
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn test_clean_spot_db_wipes_tokens() {
        let pool = test_pool().await;
        clean_spot_db(&pool).await;

        insert_one_time_token(&pool, "tok-a").await.unwrap();
        insert_one_time_token(&pool, "tok-b").await.unwrap();

        let count: i64 = sqlx::query_scalar("select count(*) from spot.one_time_tokens")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 2);

        clean_spot_db(&pool).await;

        let after: i64 = sqlx::query_scalar("select count(*) from spot.one_time_tokens")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(after, 0, "clean_spot_db should truncate all tokens");
    }
}
