use serde::Serialize;

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow, Debug, Clone)]
pub struct User {
    pub id: i64,
    pub email: String,
    pub name: String,
    pub pw_salt: String,
    pub pw_hash: String,
    pub email_verified: bool,
    pub created: chrono::DateTime<chrono::Utc>,
    pub modified: chrono::DateTime<chrono::Utc>,
}

impl User {
    /// Public json view — never expose password material.
    pub fn to_api(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "email": self.email,
            "name": self.name,
            "email_verified": self.email_verified,
        })
    }
}

#[derive(sqlx::FromRow, Debug, Clone)]
pub struct Anon {
    pub id: i64,
    pub token_hash: String,
    pub created: chrono::DateTime<chrono::Utc>,
}

pub const ORG_KIND_STANDARD: &str = "standard";
pub const ORG_KIND_ANON: &str = "anon";

#[derive(sqlx::FromRow, Debug, Clone, Serialize)]
pub struct Org {
    #[serde(skip)]
    pub id: i64,
    pub public_id: String,
    pub name: String,
    pub kind: String,
    pub allowed_email_domain: Option<String>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created: chrono::DateTime<chrono::Utc>,
    #[serde(skip)]
    pub modified: chrono::DateTime<chrono::Utc>,
}

pub const ROLE_ADMIN: &str = "admin";
pub const ROLE_MEMBER: &str = "member";

#[derive(sqlx::FromRow, Debug, Clone, Serialize)]
pub struct Member {
    pub id: i64,
    #[serde(skip)]
    pub org_id: i64,
    #[serde(skip)]
    pub user_id: Option<i64>,
    #[serde(skip)]
    pub anon_id: Option<i64>,
    pub role: String,
    pub display_name: String,
    #[serde(skip)]
    pub created: chrono::DateTime<chrono::Utc>,
}

impl Member {
    pub fn is_admin(&self) -> bool {
        self.role == ROLE_ADMIN
    }
}

#[derive(sqlx::FromRow, Debug, Clone, Serialize)]
pub struct Category {
    pub id: i64,
    #[serde(skip)]
    pub org_id: i64,
    pub key: String,
    pub name: String,
    pub grp: Option<String>,
    pub color: String,
    pub max_per_member: Option<i32>,
    pub sort: i32,
}

#[derive(sqlx::FromRow, Debug, Clone)]
pub struct Pin {
    pub id: i64,
    pub org_id: i64,
    pub member_id: i64,
    pub category_id: i64,
    pub lat: f64,
    pub lng: f64,
    pub description: Option<String>,
    pub created: chrono::DateTime<chrono::Utc>,
    pub modified: chrono::DateTime<chrono::Utc>,
}

#[derive(sqlx::FromRow, Debug, Clone)]
pub struct Photo {
    pub id: i64,
    pub pin_id: i64,
    pub s3_key: String,
    pub content_type: String,
    pub approved: bool,
    pub approved_by: Option<i64>,
    pub created: chrono::DateTime<chrono::Utc>,
}

#[derive(sqlx::FromRow, Debug, Clone)]
pub struct Invite {
    pub id: i64,
    pub org_id: i64,
    pub email: String,
    pub token_hash: String,
    pub created_by: Option<i64>,
    pub accepted_by: Option<i64>,
    pub expires: chrono::DateTime<chrono::Utc>,
    pub created: chrono::DateTime<chrono::Utc>,
}

#[derive(sqlx::FromRow, Debug, Clone)]
pub struct JoinRequest {
    pub id: i64,
    pub org_id: i64,
    pub user_id: i64,
    pub status: String,
    pub created: chrono::DateTime<chrono::Utc>,
    pub modified: chrono::DateTime<chrono::Utc>,
}

// ---------------------------------------------------------------------------
// Shared queries
// ---------------------------------------------------------------------------

pub async fn org_by_public_id(
    db: &common::db::DbPool,
    public_id: &str,
) -> anyhow::Result<Option<Org>> {
    Ok(sqlx::query_as::<_, Org>(
        "select * from orgs
         where public_id = $1 and (expires_at is null or expires_at > now())",
    )
    .bind(public_id)
    .fetch_optional(db)
    .await?)
}

/// The org membership (if any) of a user or anonymous identity.
pub async fn member_for(
    db: &common::db::DbPool,
    org_id: i64,
    user_id: Option<i64>,
    anon_id: Option<i64>,
) -> anyhow::Result<Option<Member>> {
    Ok(sqlx::query_as::<_, Member>(
        "select * from members
         where org_id = $1
           and ((user_id is not null and user_id = $2)
             or (anon_id is not null and anon_id = $3))",
    )
    .bind(org_id)
    .bind(user_id)
    .bind(anon_id)
    .fetch_optional(db)
    .await?)
}

pub async fn admin_count(db: &common::db::DbPool, org_id: i64) -> anyhow::Result<i64> {
    Ok(
        sqlx::query_scalar("select count(*) from members where org_id = $1 and role = 'admin'")
            .bind(org_id)
            .fetch_one(db)
            .await?,
    )
}

/// Blueprint for a category seeded into every new org.
pub struct CategorySpec {
    pub key: &'static str,
    pub name: &'static str,
    pub grp: Option<&'static str>,
    pub color: &'static str,
    pub max_per_member: Option<i32>,
    pub sort: i32,
}

/// Default categories seeded into every new org.
pub const DEFAULT_CATEGORIES: &[CategorySpec] = &[
    CategorySpec {
        key: "live",
        name: "Where I live",
        grp: None,
        color: "#3388ff",
        max_per_member: Some(1),
        sort: 0,
    },
    CategorySpec {
        key: "work",
        name: "Where I work",
        grp: None,
        color: "#e0333c",
        max_per_member: None,
        sort: 1,
    },
    CategorySpec {
        key: "fav_outside",
        name: "Places to spend time outside",
        grp: Some("My favorite places"),
        color: "#2f9e44",
        max_per_member: None,
        sort: 2,
    },
    CategorySpec {
        key: "fav_restaurant",
        name: "Restaurants",
        grp: Some("My favorite places"),
        color: "#9c36b5",
        max_per_member: None,
        sort: 3,
    },
    CategorySpec {
        key: "fav_activity",
        name: "Activities",
        grp: Some("My favorite places"),
        color: "#f5a623",
        max_per_member: None,
        sort: 4,
    },
];

pub async fn seed_categories(db: &common::db::DbPool, org_id: i64) -> anyhow::Result<()> {
    for spec in DEFAULT_CATEGORIES {
        sqlx::query(
            "insert into categories (org_id, key, name, grp, color, max_per_member, sort)
             values ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(org_id)
        .bind(spec.key)
        .bind(spec.name)
        .bind(spec.grp)
        .bind(spec.color)
        .bind(spec.max_per_member)
        .bind(spec.sort)
        .execute(db)
        .await?;
    }
    Ok(())
}

/// All photo S3 keys belonging to `org_id` — used for cleanup before deletes.
pub async fn org_photo_keys(db: &common::db::DbPool, org_id: i64) -> anyhow::Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "select p.s3_key from photos p
         inner join pins on pins.id = p.pin_id
         where pins.org_id = $1",
    )
    .bind(org_id)
    .fetch_all(db)
    .await?)
}
