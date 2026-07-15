use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Redirect},
    Json,
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{error, info};

use crate::auth;
use crate::models::{
    self, Category, Invite, JoinRequest, Member, Org, Photo, Pin, User, ORG_KIND_ANON,
    ORG_KIND_STANDARD, ROLE_ADMIN, ROLE_MEMBER,
};
use crate::storage;
use crate::State as AppState;

type ApiError = (StatusCode, Json<Value>);
type ApiResult<T> = std::result::Result<T, ApiError>;

fn err(status: StatusCode, msg: &str) -> ApiError {
    (status, Json(json!({ "error": msg })))
}

fn internal<E: std::fmt::Display>(e: E) -> ApiError {
    error!("internal error: {e}");
    err(StatusCode::INTERNAL_SERVER_ERROR, "internal server error")
}

fn not_found() -> ApiError {
    err(StatusCode::NOT_FOUND, "not found")
}

fn forbidden() -> ApiError {
    err(StatusCode::FORBIDDEN, "forbidden")
}

fn unauthorized() -> ApiError {
    err(StatusCode::UNAUTHORIZED, "login required")
}

pub async fn status(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({ "ok": "ok", "version": state.config.version }))
}

// ---------------------------------------------------------------------------
// Actor resolution
// ---------------------------------------------------------------------------

/// Who is making this request: an authenticated user, an anonymous identity
/// (tracked by cookie), neither, or both.
pub struct Actor {
    pub user: Option<User>,
    pub anon: Option<models::Anon>,
}

impl Actor {
    pub async fn resolve(state: &AppState, jar: &CookieJar) -> Self {
        Self {
            user: auth::current_user(state, jar).await,
            anon: auth::current_anon(state, jar).await,
        }
    }

    fn user_id(&self) -> Option<i64> {
        self.user.as_ref().map(|u| u.id)
    }

    fn anon_id(&self) -> Option<i64> {
        self.anon.as_ref().map(|a| a.id)
    }

    /// Membership of this actor in `org_id`, preferring the user identity.
    async fn member(&self, state: &AppState, org_id: i64) -> ApiResult<Option<Member>> {
        models::member_for(&state.db, org_id, self.user_id(), self.anon_id())
            .await
            .map_err(internal)
    }
}

/// Look up an org by public id and require the actor to be a member.
async fn require_member(
    state: &AppState,
    jar: &CookieJar,
    public_id: &str,
) -> ApiResult<(Org, Member)> {
    let org = models::org_by_public_id(&state.db, public_id)
        .await
        .map_err(internal)?
        .ok_or_else(not_found)?;
    let actor = Actor::resolve(state, jar).await;
    let member = actor.member(state, org.id).await?.ok_or_else(forbidden)?;
    Ok((org, member))
}

async fn require_admin(
    state: &AppState,
    jar: &CookieJar,
    public_id: &str,
) -> ApiResult<(Org, Member)> {
    let (org, member) = require_member(state, jar, public_id).await?;
    if !member.is_admin() {
        return Err(forbidden());
    }
    Ok((org, member))
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

fn valid_email(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

#[derive(Deserialize)]
pub struct RegisterBody {
    pub email: String,
    pub name: String,
    pub password: String,
}

pub async fn register(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(body): Json<RegisterBody>,
) -> ApiResult<impl IntoResponse> {
    let email = normalize_email(&body.email);
    let name = body.name.trim().to_string();
    if !valid_email(&email) {
        return Err(err(StatusCode::BAD_REQUEST, "invalid email"));
    }
    if name.is_empty() || name.len() > 100 {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "name must be 1-100 characters",
        ));
    }
    if body.password.len() < 8 {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "password must be at least 8 characters",
        ));
    }

    let (salt, hash) = auth::new_pw_hash(&body.password).map_err(internal)?;
    // email verification is disabled until an smtp relay exists — accounts are
    // marked verified immediately, but the verification record is still
    // created so the flow can be turned on without schema changes
    let verified = !state.config.require_email_verification;
    let user = sqlx::query_as::<_, User>(
        "insert into users (email, name, pw_salt, pw_hash, email_verified)
         values ($1, $2, $3, $4, $5)
         on conflict (email) do nothing
         returning *",
    )
    .bind(&email)
    .bind(&name)
    .bind(&salt)
    .bind(&hash)
    .bind(verified)
    .fetch_optional(&state.db)
    .await
    .map_err(internal)?
    .ok_or_else(|| {
        err(
            StatusCode::CONFLICT,
            "an account with this email already exists",
        )
    })?;

    let verify_token = auth::new_token().map_err(internal)?;
    sqlx::query(
        "insert into email_verifications (user_id, token_hash, expires)
         values ($1, $2, now() + interval '2 days')",
    )
    .bind(user.id)
    .bind(auth::token_hash(&verify_token, &state.config.signing_key))
    .execute(&state.db)
    .await
    .map_err(internal)?;
    // once smtp exists this link should be emailed instead of logged
    info!(
        user_email = %user.email,
        "verification link: {}/mapour/api/verify-email?token={}",
        state.config.real_hostname,
        verify_token
    );

    let cookie = auth::create_session(&state, user.id)
        .await
        .map_err(internal)?;
    Ok((
        jar.add(cookie),
        Json(json!({
            "user": user.to_api(),
            "verification_required": state.config.require_email_verification,
        })),
    ))
}

#[derive(Deserialize)]
pub struct LoginBody {
    pub email: String,
    pub password: String,
}

pub async fn login(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(body): Json<LoginBody>,
) -> ApiResult<impl IntoResponse> {
    let email = normalize_email(&body.email);
    let user = sqlx::query_as::<_, User>("select * from users where email = $1")
        .bind(&email)
        .fetch_optional(&state.db)
        .await
        .map_err(internal)?;
    let Some(user) = user else {
        return Err(err(StatusCode::UNAUTHORIZED, "invalid email or password"));
    };
    if !auth::verify_pw(&body.password, &user.pw_salt, &user.pw_hash) {
        return Err(err(StatusCode::UNAUTHORIZED, "invalid email or password"));
    }
    if state.config.require_email_verification && !user.email_verified {
        return Err(err(
            StatusCode::FORBIDDEN,
            "email not verified — check your inbox for the verification link",
        ));
    }
    let cookie = auth::create_session(&state, user.id)
        .await
        .map_err(internal)?;
    Ok((jar.add(cookie), Json(json!({ "user": user.to_api() }))))
}

pub async fn logout(State(state): State<AppState>, jar: CookieJar) -> impl IntoResponse {
    let removal = auth::end_session(&state, &jar).await;
    (jar.add(removal), Json(json!({ "ok": "ok" })))
}

#[derive(Deserialize)]
pub struct VerifyEmailParams {
    pub token: String,
}

pub async fn verify_email(
    State(state): State<AppState>,
    Query(params): Query<VerifyEmailParams>,
) -> ApiResult<impl IntoResponse> {
    let hash = auth::token_hash(&params.token, &state.config.signing_key);
    let user_id: Option<i64> = sqlx::query_scalar(
        "delete from email_verifications
         where token_hash = $1 and expires > now()
         returning user_id",
    )
    .bind(&hash)
    .fetch_optional(&state.db)
    .await
    .map_err(internal)?;
    let Some(user_id) = user_id else {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "invalid or expired verification link",
        ));
    };
    sqlx::query("update users set email_verified = true, modified = now() where id = $1")
        .bind(user_id)
        .execute(&state.db)
        .await
        .map_err(internal)?;
    Ok(Redirect::temporary("/mapour?verified=1"))
}

/// Orgs the actor belongs to, for both user and anon identities.
async fn actor_orgs(state: &AppState, actor: &Actor) -> ApiResult<Vec<Value>> {
    #[derive(sqlx::FromRow)]
    struct OrgRow {
        public_id: String,
        name: String,
        kind: String,
        role: String,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    }
    let rows = sqlx::query_as::<_, OrgRow>(
        "select o.public_id, o.name, o.kind, m.role, o.expires_at
         from orgs o inner join members m on m.org_id = o.id
         where (o.expires_at is null or o.expires_at > now())
           and ((m.user_id is not null and m.user_id = $1)
             or (m.anon_id is not null and m.anon_id = $2))
         order by o.created",
    )
    .bind(actor.user.as_ref().map(|u| u.id))
    .bind(actor.anon.as_ref().map(|a| a.id))
    .fetch_all(&state.db)
    .await
    .map_err(internal)?;
    Ok(rows
        .into_iter()
        .map(|r| {
            json!({
                "public_id": r.public_id,
                "name": r.name,
                "kind": r.kind,
                "role": r.role,
                "expires_at": r.expires_at,
            })
        })
        .collect())
}

pub async fn me(State(state): State<AppState>, jar: CookieJar) -> ApiResult<impl IntoResponse> {
    let actor = Actor::resolve(&state, &jar).await;
    let orgs = actor_orgs(&state, &actor).await?;
    Ok(Json(json!({
        "user": actor.user.as_ref().map(|u| u.to_api()),
        "anonymous": actor.user.is_none() && actor.anon.is_some(),
        "orgs": orgs,
    })))
}

/// Associate anonymous memberships with the logged-in user's account.
pub async fn claim(State(state): State<AppState>, jar: CookieJar) -> ApiResult<impl IntoResponse> {
    let actor = Actor::resolve(&state, &jar).await;
    let user = actor.user.clone().ok_or_else(unauthorized)?;
    let Some(anon) = actor.anon.clone() else {
        return Ok(Json(json!({ "claimed": 0 })));
    };

    let anon_members = sqlx::query_as::<_, Member>("select * from members where anon_id = $1")
        .bind(anon.id)
        .fetch_all(&state.db)
        .await
        .map_err(internal)?;

    let mut claimed = 0;
    for m in anon_members {
        let existing = models::member_for(&state.db, m.org_id, Some(user.id), None)
            .await
            .map_err(internal)?;
        if let Some(existing) = existing {
            // already a member through the account: move the anon pins over
            sqlx::query("update pins set member_id = $1 where member_id = $2")
                .bind(existing.id)
                .bind(m.id)
                .execute(&state.db)
                .await
                .map_err(internal)?;
            sqlx::query("delete from members where id = $1")
                .bind(m.id)
                .execute(&state.db)
                .await
                .map_err(internal)?;
        } else {
            sqlx::query(
                "update members set user_id = $1, anon_id = null, display_name = $2 where id = $3",
            )
            .bind(user.id)
            .bind(&user.name)
            .bind(m.id)
            .execute(&state.db)
            .await
            .map_err(internal)?;
        }
        claimed += 1;
    }
    Ok(Json(json!({ "claimed": claimed })))
}

// ---------------------------------------------------------------------------
// Orgs
// ---------------------------------------------------------------------------

fn new_public_id() -> anyhow::Result<String> {
    let bytes = common::crypto::rand_bytes(6).map_err(|e| anyhow::anyhow!("rng error: {e}"))?;
    Ok(hex::encode(bytes))
}

fn normalize_domain(domain: &str) -> Option<String> {
    let d = domain.trim().trim_start_matches('@').to_lowercase();
    if d.is_empty() {
        return None;
    }
    Some(d)
}

fn org_link(state: &AppState, org: &Org) -> String {
    format!(
        "{}/mapour?org={}",
        state.config.real_hostname, org.public_id
    )
}

#[derive(Deserialize)]
pub struct CreateOrgBody {
    pub name: String,
    pub allowed_email_domain: Option<String>,
    /// anonymous maps are joinable by anyone with the link, no login needed
    #[serde(default)]
    pub anonymous: bool,
    /// optional ttl for anonymous maps: delete everything after this many days
    pub ttl_days: Option<i64>,
    /// display name used for the creating member on anonymous maps
    pub display_name: Option<String>,
}

pub async fn create_org(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(body): Json<CreateOrgBody>,
) -> ApiResult<impl IntoResponse> {
    let name = body.name.trim().to_string();
    if name.is_empty() || name.len() > 100 {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "name must be 1-100 characters",
        ));
    }

    let actor = Actor::resolve(&state, &jar).await;
    let mut jar = jar;

    let kind = if body.anonymous {
        ORG_KIND_ANON
    } else {
        ORG_KIND_STANDARD
    };
    let (user_id, anon_id, display_name) = if body.anonymous {
        match &actor.user {
            Some(u) => (Some(u.id), None, u.name.clone()),
            None => {
                let (anon, cookie) = auth::get_or_create_anon(&state, &jar)
                    .await
                    .map_err(internal)?;
                if let Some(c) = cookie {
                    jar = jar.add(c);
                }
                let display_name = body
                    .display_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                    .unwrap_or("Anonymous")
                    .to_string();
                (None, Some(anon.id), display_name)
            }
        }
    } else {
        let user = actor.user.clone().ok_or_else(unauthorized)?;
        (Some(user.id), None, user.name.clone())
    };

    let expires_at = if body.anonymous {
        body.ttl_days
            .map(|d| chrono::Utc::now() + chrono::Duration::days(d.clamp(1, 365)))
    } else {
        None
    };
    let allowed_email_domain = if body.anonymous {
        None
    } else {
        body.allowed_email_domain
            .as_deref()
            .and_then(normalize_domain)
    };

    let public_id = new_public_id().map_err(internal)?;
    let org = sqlx::query_as::<_, Org>(
        "insert into orgs (public_id, name, kind, allowed_email_domain, expires_at)
         values ($1, $2, $3, $4, $5)
         returning *",
    )
    .bind(&public_id)
    .bind(&name)
    .bind(kind)
    .bind(&allowed_email_domain)
    .bind(expires_at)
    .fetch_one(&state.db)
    .await
    .map_err(internal)?;

    models::seed_categories(&state.db, org.id)
        .await
        .map_err(internal)?;

    sqlx::query(
        "insert into members (org_id, user_id, anon_id, role, display_name)
         values ($1, $2, $3, 'admin', $4)",
    )
    .bind(org.id)
    .bind(user_id)
    .bind(anon_id)
    .bind(&display_name)
    .execute(&state.db)
    .await
    .map_err(internal)?;

    let link = org_link(&state, &org);
    Ok((jar, Json(json!({ "org": org, "link": link }))))
}

pub async fn get_org(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(public_id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let org = models::org_by_public_id(&state.db, &public_id)
        .await
        .map_err(internal)?
        .ok_or_else(not_found)?;
    let actor = Actor::resolve(&state, &jar).await;
    let member = actor.member(&state, org.id).await?;

    match member {
        Some(member) => {
            let categories = sqlx::query_as::<_, Category>(
                "select * from categories where org_id = $1 order by sort",
            )
            .bind(org.id)
            .fetch_all(&state.db)
            .await
            .map_err(internal)?;
            let link = org_link(&state, &org);
            Ok(Json(json!({
                "org": org,
                "member": member,
                "categories": categories,
                "link": link,
            })))
        }
        None => {
            // non-members only see enough to decide whether/how to join
            let can_join_by_domain = match (&actor.user, &org.allowed_email_domain) {
                (Some(user), Some(domain)) => {
                    (user.email_verified || !state.config.require_email_verification)
                        && user.email.ends_with(&format!("@{domain}"))
                }
                _ => false,
            };
            let pending_request = match &actor.user {
                Some(user) => {
                    sqlx::query_scalar::<_, i64>(
                        "select count(*) from join_requests
                     where org_id = $1 and user_id = $2 and status = 'pending'",
                    )
                    .bind(org.id)
                    .bind(user.id)
                    .fetch_one(&state.db)
                    .await
                    .map_err(internal)?
                        > 0
                }
                None => false,
            };
            Ok(Json(json!({
                "org": { "public_id": org.public_id, "name": org.name, "kind": org.kind },
                "member": null,
                "can_join": org.kind == ORG_KIND_ANON || can_join_by_domain,
                "pending_request": pending_request,
            })))
        }
    }
}

#[derive(Deserialize)]
pub struct JoinBody {
    pub display_name: Option<String>,
}

pub async fn join_org(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(public_id): Path<String>,
    body: Option<Json<JoinBody>>,
) -> ApiResult<impl IntoResponse> {
    let org = models::org_by_public_id(&state.db, &public_id)
        .await
        .map_err(internal)?
        .ok_or_else(not_found)?;
    let actor = Actor::resolve(&state, &jar).await;
    let mut jar = jar;

    if actor.member(&state, org.id).await?.is_some() {
        return Ok((jar, Json(json!({ "joined": true }))));
    }

    let display_name_override = body
        .and_then(|Json(b)| b.display_name)
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty() && n.len() <= 100);

    if org.kind == ORG_KIND_ANON {
        // anyone with the link can join an anonymous map
        let (user_id, anon_id, display_name) = match &actor.user {
            Some(u) => (
                Some(u.id),
                None,
                display_name_override.unwrap_or_else(|| u.name.clone()),
            ),
            None => {
                let (anon, cookie) = auth::get_or_create_anon(&state, &jar)
                    .await
                    .map_err(internal)?;
                if let Some(c) = cookie {
                    jar = jar.add(c);
                }
                (
                    None,
                    Some(anon.id),
                    display_name_override.unwrap_or_else(|| "Anonymous".to_string()),
                )
            }
        };
        sqlx::query(
            "insert into members (org_id, user_id, anon_id, role, display_name)
             values ($1, $2, $3, 'member', $4)
             on conflict do nothing",
        )
        .bind(org.id)
        .bind(user_id)
        .bind(anon_id)
        .bind(&display_name)
        .execute(&state.db)
        .await
        .map_err(internal)?;
        return Ok((jar, Json(json!({ "joined": true }))));
    }

    // standard orgs require an account
    let user = actor.user.clone().ok_or_else(unauthorized)?;
    let domain_ok = org
        .allowed_email_domain
        .as_ref()
        .map(|d| user.email.ends_with(&format!("@{d}")))
        .unwrap_or(false);
    if domain_ok {
        sqlx::query(
            "insert into members (org_id, user_id, role, display_name)
             values ($1, $2, 'member', $3)
             on conflict do nothing",
        )
        .bind(org.id)
        .bind(user.id)
        .bind(display_name_override.unwrap_or_else(|| user.name.clone()))
        .execute(&state.db)
        .await
        .map_err(internal)?;
        return Ok((jar, Json(json!({ "joined": true }))));
    }

    // otherwise leave a join request for the org admins
    sqlx::query(
        "insert into join_requests (org_id, user_id, status)
         values ($1, $2, 'pending')
         on conflict (org_id, user_id)
         do update set status = 'pending', modified = now()
         where join_requests.status <> 'pending'",
    )
    .bind(org.id)
    .bind(user.id)
    .execute(&state.db)
    .await
    .map_err(internal)?;
    Ok((jar, Json(json!({ "joined": false, "requested": true }))))
}

// ---------------------------------------------------------------------------
// Members
// ---------------------------------------------------------------------------

pub async fn list_members(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(public_id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let (org, me) = require_member(&state, &jar, &public_id).await?;

    #[derive(sqlx::FromRow)]
    struct MemberRow {
        id: i64,
        role: String,
        display_name: String,
        email: Option<String>,
        anonymous: bool,
    }
    let rows = sqlx::query_as::<_, MemberRow>(
        "select m.id, m.role, m.display_name, u.email, (m.anon_id is not null) as anonymous
         from members m left join users u on u.id = m.user_id
         where m.org_id = $1
         order by m.created",
    )
    .bind(org.id)
    .fetch_all(&state.db)
    .await
    .map_err(internal)?;

    let members: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "role": r.role,
                "display_name": r.display_name,
                // emails are only shown to admins
                "email": if me.is_admin() { r.email } else { None },
                "anonymous": r.anonymous,
                "is_you": r.id == me.id,
            })
        })
        .collect();
    Ok(Json(json!({ "members": members })))
}

#[derive(Deserialize)]
pub struct SetRoleBody {
    pub role: String,
}

pub async fn set_member_role(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((public_id, member_id)): Path<(String, i64)>,
    Json(body): Json<SetRoleBody>,
) -> ApiResult<impl IntoResponse> {
    let (org, _me) = require_admin(&state, &jar, &public_id).await?;
    if body.role != ROLE_ADMIN && body.role != ROLE_MEMBER {
        return Err(err(StatusCode::BAD_REQUEST, "role must be admin or member"));
    }
    let target = sqlx::query_as::<_, Member>("select * from members where id = $1 and org_id = $2")
        .bind(member_id)
        .bind(org.id)
        .fetch_optional(&state.db)
        .await
        .map_err(internal)?
        .ok_or_else(not_found)?;

    if target.anon_id.is_some() && body.role == ROLE_ADMIN {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "anonymous members cannot be admins",
        ));
    }
    if target.is_admin()
        && body.role == ROLE_MEMBER
        && models::admin_count(&state.db, org.id)
            .await
            .map_err(internal)?
            <= 1
    {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "there must always be at least one admin",
        ));
    }

    sqlx::query("update members set role = $1 where id = $2")
        .bind(&body.role)
        .bind(target.id)
        .execute(&state.db)
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "ok": "ok" })))
}

pub async fn remove_member(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((public_id, member_id)): Path<(String, i64)>,
) -> ApiResult<impl IntoResponse> {
    let (org, me) = require_member(&state, &jar, &public_id).await?;
    // members may remove themselves (leave), admins may remove anyone
    if member_id != me.id && !me.is_admin() {
        return Err(forbidden());
    }
    let target = sqlx::query_as::<_, Member>("select * from members where id = $1 and org_id = $2")
        .bind(member_id)
        .bind(org.id)
        .fetch_optional(&state.db)
        .await
        .map_err(internal)?
        .ok_or_else(not_found)?;

    if target.is_admin()
        && models::admin_count(&state.db, org.id)
            .await
            .map_err(internal)?
            <= 1
    {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "there must always be at least one admin",
        ));
    }

    // clean up the member's photo objects before the rows cascade away
    let keys: Vec<String> = sqlx::query_scalar(
        "select p.s3_key from photos p
         inner join pins on pins.id = p.pin_id
         where pins.member_id = $1",
    )
    .bind(target.id)
    .fetch_all(&state.db)
    .await
    .map_err(internal)?;
    storage::delete_objects_best_effort(&state.s3, &state.config.s3_bucket, &keys).await;

    sqlx::query("delete from members where id = $1")
        .bind(target.id)
        .execute(&state.db)
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "ok": "ok" })))
}

// ---------------------------------------------------------------------------
// Invites
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateInviteBody {
    pub email: String,
}

pub async fn create_invite(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(public_id): Path<String>,
    Json(body): Json<CreateInviteBody>,
) -> ApiResult<impl IntoResponse> {
    let (org, me) = require_admin(&state, &jar, &public_id).await?;
    let email = normalize_email(&body.email);
    if !valid_email(&email) {
        return Err(err(StatusCode::BAD_REQUEST, "invalid email"));
    }
    let token = auth::new_token().map_err(internal)?;
    sqlx::query(
        "insert into invites (org_id, email, token_hash, created_by, expires)
         values ($1, $2, $3, $4, now() + interval '14 days')",
    )
    .bind(org.id)
    .bind(&email)
    .bind(auth::token_hash(&token, &state.config.signing_key))
    .bind(me.id)
    .execute(&state.db)
    .await
    .map_err(internal)?;

    // no smtp relay yet: hand the link back to the admin to share manually.
    // once email sending exists this should be delivered to `email` directly.
    let link = format!("{}/mapour?invite={}", state.config.real_hostname, token);
    Ok(Json(json!({ "email": email, "link": link })))
}

pub async fn list_invites(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(public_id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let (org, _me) = require_admin(&state, &jar, &public_id).await?;
    #[derive(sqlx::FromRow, serde::Serialize)]
    struct InviteRow {
        id: i64,
        email: String,
        expires: chrono::DateTime<chrono::Utc>,
        created: chrono::DateTime<chrono::Utc>,
    }
    let invites = sqlx::query_as::<_, InviteRow>(
        "select id, email, expires, created from invites
         where org_id = $1 and accepted_by is null and expires > now()
         order by created desc",
    )
    .bind(org.id)
    .fetch_all(&state.db)
    .await
    .map_err(internal)?;
    Ok(Json(json!({ "invites": invites })))
}

pub async fn delete_invite(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((public_id, invite_id)): Path<(String, i64)>,
) -> ApiResult<impl IntoResponse> {
    let (org, _me) = require_admin(&state, &jar, &public_id).await?;
    sqlx::query("delete from invites where id = $1 and org_id = $2")
        .bind(invite_id)
        .bind(org.id)
        .execute(&state.db)
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "ok": "ok" })))
}

#[derive(Deserialize)]
pub struct AcceptInviteBody {
    pub token: String,
}

pub async fn accept_invite(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(body): Json<AcceptInviteBody>,
) -> ApiResult<impl IntoResponse> {
    let actor = Actor::resolve(&state, &jar).await;
    let user = actor.user.clone().ok_or_else(unauthorized)?;

    let hash = auth::token_hash(&body.token, &state.config.signing_key);
    let invite = sqlx::query_as::<_, Invite>(
        "select * from invites
         where token_hash = $1 and accepted_by is null and expires > now()",
    )
    .bind(&hash)
    .fetch_optional(&state.db)
    .await
    .map_err(internal)?
    .ok_or_else(|| err(StatusCode::BAD_REQUEST, "invalid or expired invite"))?;

    if invite.email != user.email {
        return Err(err(
            StatusCode::FORBIDDEN,
            "this invite was issued for a different email address",
        ));
    }
    let org = sqlx::query_as::<_, Org>("select * from orgs where id = $1")
        .bind(invite.org_id)
        .fetch_one(&state.db)
        .await
        .map_err(internal)?;

    sqlx::query(
        "insert into members (org_id, user_id, role, display_name)
         values ($1, $2, 'member', $3)
         on conflict do nothing",
    )
    .bind(org.id)
    .bind(user.id)
    .bind(&user.name)
    .execute(&state.db)
    .await
    .map_err(internal)?;
    sqlx::query("update invites set accepted_by = $1 where id = $2")
        .bind(user.id)
        .bind(invite.id)
        .execute(&state.db)
        .await
        .map_err(internal)?;
    Ok(Json(
        json!({ "joined": true, "org": { "public_id": org.public_id, "name": org.name } }),
    ))
}

// ---------------------------------------------------------------------------
// Join requests
// ---------------------------------------------------------------------------

pub async fn list_join_requests(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(public_id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let (org, _me) = require_admin(&state, &jar, &public_id).await?;
    #[derive(sqlx::FromRow, serde::Serialize)]
    struct RequestRow {
        id: i64,
        name: String,
        email: String,
        created: chrono::DateTime<chrono::Utc>,
    }
    let requests = sqlx::query_as::<_, RequestRow>(
        "select r.id, u.name, u.email, r.created
         from join_requests r inner join users u on u.id = r.user_id
         where r.org_id = $1 and r.status = 'pending'
         order by r.created",
    )
    .bind(org.id)
    .fetch_all(&state.db)
    .await
    .map_err(internal)?;
    Ok(Json(json!({ "requests": requests })))
}

pub async fn decide_join_request(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((public_id, request_id, decision)): Path<(String, i64, String)>,
) -> ApiResult<impl IntoResponse> {
    let (org, _me) = require_admin(&state, &jar, &public_id).await?;
    let approve = match decision.as_str() {
        "approve" => true,
        "deny" => false,
        _ => return Err(not_found()),
    };
    let request = sqlx::query_as::<_, JoinRequest>(
        "update join_requests set status = $1, modified = now()
         where id = $2 and org_id = $3 and status = 'pending'
         returning *",
    )
    .bind(if approve { "approved" } else { "denied" })
    .bind(request_id)
    .bind(org.id)
    .fetch_optional(&state.db)
    .await
    .map_err(internal)?
    .ok_or_else(not_found)?;

    if approve {
        let user = sqlx::query_as::<_, User>("select * from users where id = $1")
            .bind(request.user_id)
            .fetch_one(&state.db)
            .await
            .map_err(internal)?;
        sqlx::query(
            "insert into members (org_id, user_id, role, display_name)
             values ($1, $2, 'member', $3)
             on conflict do nothing",
        )
        .bind(org.id)
        .bind(user.id)
        .bind(&user.name)
        .execute(&state.db)
        .await
        .map_err(internal)?;
    }
    Ok(Json(json!({ "ok": "ok" })))
}

// ---------------------------------------------------------------------------
// Categories
// ---------------------------------------------------------------------------

pub async fn list_categories(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(public_id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let (org, _me) = require_member(&state, &jar, &public_id).await?;
    let categories =
        sqlx::query_as::<_, Category>("select * from categories where org_id = $1 order by sort")
            .bind(org.id)
            .fetch_all(&state.db)
            .await
            .map_err(internal)?;
    Ok(Json(json!({ "categories": categories })))
}

#[derive(Deserialize)]
pub struct UpdateCategoryBody {
    pub color: Option<String>,
    pub name: Option<String>,
}

fn valid_color(color: &str) -> bool {
    color.len() == 7 && color.starts_with('#') && color[1..].chars().all(|c| c.is_ascii_hexdigit())
}

pub async fn update_category(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((public_id, category_id)): Path<(String, i64)>,
    Json(body): Json<UpdateCategoryBody>,
) -> ApiResult<impl IntoResponse> {
    let (org, _me) = require_admin(&state, &jar, &public_id).await?;
    if let Some(color) = &body.color {
        if !valid_color(color) {
            return Err(err(StatusCode::BAD_REQUEST, "color must be like #33aa77"));
        }
    }
    if let Some(name) = &body.name {
        if name.trim().is_empty() || name.len() > 100 {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "name must be 1-100 characters",
            ));
        }
    }
    let category = sqlx::query_as::<_, Category>(
        "update categories
         set color = coalesce($1, color), name = coalesce($2, name)
         where id = $3 and org_id = $4
         returning *",
    )
    .bind(&body.color)
    .bind(body.name.as_deref().map(str::trim))
    .bind(category_id)
    .bind(org.id)
    .fetch_optional(&state.db)
    .await
    .map_err(internal)?
    .ok_or_else(not_found)?;
    Ok(Json(json!({ "category": category })))
}

// ---------------------------------------------------------------------------
// Pins
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct PinRow {
    id: i64,
    member_id: i64,
    category_id: i64,
    lat: f64,
    lng: f64,
    description: Option<String>,
    category_key: String,
    color: String,
    display_name: String,
    photo_id: Option<i64>,
    photo_approved: Option<bool>,
}

pub async fn list_pins(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(public_id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let (org, me) = require_member(&state, &jar, &public_id).await?;
    let rows = sqlx::query_as::<_, PinRow>(
        "select p.id, p.member_id, p.category_id, p.lat, p.lng, p.description,
                c.key as category_key, c.color, m.display_name,
                ph.id as photo_id, ph.approved as photo_approved
         from pins p
         inner join categories c on c.id = p.category_id
         inner join members m on m.id = p.member_id
         left join photos ph on ph.pin_id = p.id
         where p.org_id = $1
         order by p.created",
    )
    .bind(org.id)
    .fetch_all(&state.db)
    .await
    .map_err(internal)?;

    let pins: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let is_yours = r.member_id == me.id;
            // pending photos are only surfaced to their submitter and admins
            let photo = match (r.photo_id, r.photo_approved) {
                (Some(id), Some(approved)) if approved || is_yours || me.is_admin() => {
                    Some(json!({ "id": id, "approved": approved }))
                }
                _ => None,
            };
            json!({
                "id": r.id,
                "lat": r.lat,
                "lng": r.lng,
                "description": r.description,
                "category_id": r.category_id,
                "category_key": r.category_key,
                "color": r.color,
                "member_id": r.member_id,
                "member_name": r.display_name,
                "is_yours": is_yours,
                "photo": photo,
            })
        })
        .collect();
    Ok(Json(json!({ "pins": pins })))
}

#[derive(Deserialize)]
pub struct CreatePinBody {
    pub category_id: i64,
    pub lat: f64,
    pub lng: f64,
    pub description: Option<String>,
}

fn valid_coords(lat: f64, lng: f64) -> bool {
    lat.is_finite()
        && lng.is_finite()
        && (-90.0..=90.0).contains(&lat)
        && (-180.0..=180.0).contains(&lng)
}

fn clean_description(desc: Option<String>) -> ApiResult<Option<String>> {
    match desc {
        None => Ok(None),
        Some(d) => {
            let d = d.trim().to_string();
            if d.len() > 2000 {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    "description must be under 2000 characters",
                ));
            }
            Ok(if d.is_empty() { None } else { Some(d) })
        }
    }
}

pub async fn create_pin(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(public_id): Path<String>,
    Json(body): Json<CreatePinBody>,
) -> ApiResult<impl IntoResponse> {
    let (org, me) = require_member(&state, &jar, &public_id).await?;
    if !valid_coords(body.lat, body.lng) {
        return Err(err(StatusCode::BAD_REQUEST, "invalid coordinates"));
    }
    let description = clean_description(body.description)?;
    let category =
        sqlx::query_as::<_, Category>("select * from categories where id = $1 and org_id = $2")
            .bind(body.category_id)
            .bind(org.id)
            .fetch_optional(&state.db)
            .await
            .map_err(internal)?
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, "unknown category"))?;

    if let Some(max) = category.max_per_member {
        let count: i64 = sqlx::query_scalar(
            "select count(*) from pins where member_id = $1 and category_id = $2",
        )
        .bind(me.id)
        .bind(category.id)
        .fetch_one(&state.db)
        .await
        .map_err(internal)?;
        if count >= max as i64 {
            return Err(err(
                StatusCode::BAD_REQUEST,
                &format!("you can only add {max} '{}' pin(s)", category.name),
            ));
        }
    }

    let pin = sqlx::query_as::<_, Pin>(
        "insert into pins (org_id, member_id, category_id, lat, lng, description)
         values ($1, $2, $3, $4, $5, $6)
         returning *",
    )
    .bind(org.id)
    .bind(me.id)
    .bind(category.id)
    .bind(body.lat)
    .bind(body.lng)
    .bind(&description)
    .fetch_one(&state.db)
    .await
    .map_err(internal)?;
    Ok(Json(json!({ "pin": { "id": pin.id } })))
}

/// Fetch a pin in the org, requiring the actor to own it or be an admin.
async fn require_editable_pin(
    state: &AppState,
    org: &Org,
    me: &Member,
    pin_id: i64,
) -> ApiResult<Pin> {
    let pin = sqlx::query_as::<_, Pin>("select * from pins where id = $1 and org_id = $2")
        .bind(pin_id)
        .bind(org.id)
        .fetch_optional(&state.db)
        .await
        .map_err(internal)?
        .ok_or_else(not_found)?;
    if pin.member_id != me.id && !me.is_admin() {
        return Err(forbidden());
    }
    Ok(pin)
}

#[derive(Deserialize)]
pub struct UpdatePinBody {
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    pub description: Option<String>,
}

pub async fn update_pin(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((public_id, pin_id)): Path<(String, i64)>,
    Json(body): Json<UpdatePinBody>,
) -> ApiResult<impl IntoResponse> {
    let (org, me) = require_member(&state, &jar, &public_id).await?;
    let pin = require_editable_pin(&state, &org, &me, pin_id).await?;

    let lat = body.lat.unwrap_or(pin.lat);
    let lng = body.lng.unwrap_or(pin.lng);
    if !valid_coords(lat, lng) {
        return Err(err(StatusCode::BAD_REQUEST, "invalid coordinates"));
    }
    // a description in the body replaces the old one; empty string clears it
    let description = match body.description {
        Some(d) => clean_description(Some(d))?,
        None => pin.description.clone(),
    };
    sqlx::query(
        "update pins set lat = $1, lng = $2, description = $3, modified = now() where id = $4",
    )
    .bind(lat)
    .bind(lng)
    .bind(&description)
    .bind(pin.id)
    .execute(&state.db)
    .await
    .map_err(internal)?;
    Ok(Json(json!({ "ok": "ok" })))
}

pub async fn delete_pin(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((public_id, pin_id)): Path<(String, i64)>,
) -> ApiResult<impl IntoResponse> {
    let (org, me) = require_member(&state, &jar, &public_id).await?;
    let pin = require_editable_pin(&state, &org, &me, pin_id).await?;

    let keys: Vec<String> = sqlx::query_scalar("select s3_key from photos where pin_id = $1")
        .bind(pin.id)
        .fetch_all(&state.db)
        .await
        .map_err(internal)?;
    storage::delete_objects_best_effort(&state.s3, &state.config.s3_bucket, &keys).await;

    sqlx::query("delete from pins where id = $1")
        .bind(pin.id)
        .execute(&state.db)
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "ok": "ok" })))
}

// ---------------------------------------------------------------------------
// Photos
// ---------------------------------------------------------------------------

const ALLOWED_PHOTO_TYPES: &[&str] = &["image/jpeg", "image/png", "image/webp", "image/gif"];

pub async fn upload_photo(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((public_id, pin_id)): Path<(String, i64)>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<impl IntoResponse> {
    let (org, me) = require_member(&state, &jar, &public_id).await?;
    let pin = require_editable_pin(&state, &org, &me, pin_id).await?;

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if !ALLOWED_PHOTO_TYPES.contains(&content_type.as_str()) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "photo must be a jpeg, png, webp, or gif",
        ));
    }
    if body.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "empty photo"));
    }
    if body.len() > state.config.max_photo_bytes {
        return Err(err(StatusCode::PAYLOAD_TOO_LARGE, "photo too large"));
    }

    // replace any existing photo on the pin
    let old_keys: Vec<String> =
        sqlx::query_scalar("delete from photos where pin_id = $1 returning s3_key")
            .bind(pin.id)
            .fetch_all(&state.db)
            .await
            .map_err(internal)?;
    storage::delete_objects_best_effort(&state.s3, &state.config.s3_bucket, &old_keys).await;

    let key = storage::new_photo_key(&org.public_id).map_err(internal)?;
    storage::put_object(&state.s3, &state.config.s3_bucket, &key, body.to_vec())
        .await
        .map_err(internal)?;

    // admin uploads don't need a second admin to sign off
    let approved = me.is_admin();
    let photo = sqlx::query_as::<_, Photo>(
        "insert into photos (pin_id, s3_key, content_type, approved, approved_by)
         values ($1, $2, $3, $4, $5)
         returning *",
    )
    .bind(pin.id)
    .bind(&key)
    .bind(&content_type)
    .bind(approved)
    .bind(if approved { Some(me.id) } else { None })
    .fetch_one(&state.db)
    .await
    .map_err(internal)?;
    Ok(Json(
        json!({ "photo": { "id": photo.id, "approved": photo.approved } }),
    ))
}

/// Fetch a photo (with its pin) in the org, 404 if absent.
async fn org_photo(state: &AppState, org: &Org, photo_id: i64) -> ApiResult<(Photo, Pin)> {
    let photo = sqlx::query_as::<_, Photo>(
        "select p.* from photos p
         inner join pins on pins.id = p.pin_id
         where p.id = $1 and pins.org_id = $2",
    )
    .bind(photo_id)
    .bind(org.id)
    .fetch_optional(&state.db)
    .await
    .map_err(internal)?
    .ok_or_else(not_found)?;
    let pin = sqlx::query_as::<_, Pin>("select * from pins where id = $1")
        .bind(photo.pin_id)
        .fetch_one(&state.db)
        .await
        .map_err(internal)?;
    Ok((photo, pin))
}

pub async fn get_photo(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((public_id, photo_id)): Path<(String, i64)>,
) -> ApiResult<impl IntoResponse> {
    let (org, me) = require_member(&state, &jar, &public_id).await?;
    let (photo, pin) = org_photo(&state, &org, photo_id).await?;
    // pending photos are only visible to their submitter and admins
    if !photo.approved && pin.member_id != me.id && !me.is_admin() {
        return Err(not_found());
    }
    let bytes = storage::get_object(&state.s3, &state.config.s3_bucket, &photo.s3_key)
        .await
        .map_err(internal)?;
    Ok((
        [
            (header::CONTENT_TYPE, photo.content_type),
            (header::CACHE_CONTROL, "private, max-age=300".to_string()),
        ],
        bytes,
    ))
}

pub async fn approve_photo(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((public_id, photo_id)): Path<(String, i64)>,
) -> ApiResult<impl IntoResponse> {
    let (org, me) = require_admin(&state, &jar, &public_id).await?;
    let (photo, _pin) = org_photo(&state, &org, photo_id).await?;
    sqlx::query("update photos set approved = true, approved_by = $1 where id = $2")
        .bind(me.id)
        .bind(photo.id)
        .execute(&state.db)
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "ok": "ok" })))
}

pub async fn delete_photo(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((public_id, photo_id)): Path<(String, i64)>,
) -> ApiResult<impl IntoResponse> {
    let (org, me) = require_member(&state, &jar, &public_id).await?;
    let (photo, pin) = org_photo(&state, &org, photo_id).await?;
    if pin.member_id != me.id && !me.is_admin() {
        return Err(forbidden());
    }
    storage::delete_objects_best_effort(
        &state.s3,
        &state.config.s3_bucket,
        std::slice::from_ref(&photo.s3_key),
    )
    .await;
    sqlx::query("delete from photos where id = $1")
        .bind(photo.id)
        .execute(&state.db)
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "ok": "ok" })))
}

pub async fn pending_photos(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(public_id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let (org, _me) = require_admin(&state, &jar, &public_id).await?;
    #[derive(sqlx::FromRow, serde::Serialize)]
    struct PendingRow {
        id: i64,
        pin_id: i64,
        member_name: String,
        category_name: String,
        description: Option<String>,
        created: chrono::DateTime<chrono::Utc>,
    }
    let photos = sqlx::query_as::<_, PendingRow>(
        "select ph.id, ph.pin_id, m.display_name as member_name,
                c.name as category_name, p.description, ph.created
         from photos ph
         inner join pins p on p.id = ph.pin_id
         inner join members m on m.id = p.member_id
         inner join categories c on c.id = p.category_id
         where p.org_id = $1 and not ph.approved
         order by ph.created",
    )
    .bind(org.id)
    .fetch_all(&state.db)
    .await
    .map_err(internal)?;
    Ok(Json(json!({ "photos": photos })))
}
