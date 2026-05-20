use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Auth — stored password hash
// ---------------------------------------------------------------------------

#[derive(Debug, FromRow)]
pub struct Auth {
    pub id: i32,
    pub salt: Vec<u8>,
    pub hash: Vec<u8>,
}

pub async fn insert_auth(
    pool: &common::db::DbPool,
    salt: Vec<u8>,
    hash: Vec<u8>,
) -> anyhow::Result<i32> {
    let row = sqlx::query("insert into auth (salt, hash) values ($1, $2) returning id")
        .bind(&salt)
        .bind(&hash)
        .fetch_one(pool)
        .await?;
    use sqlx::Row;
    Ok(row.get(0))
}

pub async fn get_auth(pool: &common::db::DbPool, id: i32) -> anyhow::Result<Option<Auth>> {
    let row = sqlx::query_as::<_, Auth>("select id, salt, hash from auth where id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

// ---------------------------------------------------------------------------
// InitUpload — temporary upload session before bytes arrive
// ---------------------------------------------------------------------------

#[derive(Debug, FromRow)]
pub struct InitUpload {
    pub id: i32,
    pub uuid_: Uuid,
    pub file_name_hash: Vec<u8>,
    pub content_hash: Vec<u8>,
    pub size_: i64,
    pub nonce: Vec<u8>,
    pub access_password: i32,
    pub deletion_password: Option<i32>,
    pub download_limit: Option<i32>,
    pub expire_date: DateTime<Utc>,
    pub date_created: DateTime<Utc>,
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_init_upload(
    pool: &common::db::DbPool,
    uuid_: Uuid,
    file_name_hash: Vec<u8>,
    content_hash: Vec<u8>,
    size_: i64,
    nonce: Vec<u8>,
    access_password: i32,
    deletion_password: Option<i32>,
    download_limit: Option<i32>,
    expire_date: DateTime<Utc>,
) -> anyhow::Result<InitUpload> {
    let row = sqlx::query_as::<_, InitUpload>(
        "insert into init_upload
         (uuid_, file_name_hash, content_hash, size_, nonce, access_password, deletion_password,
          download_limit, expire_date)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         returning *",
    )
    .bind(uuid_)
    .bind(&file_name_hash)
    .bind(&content_hash)
    .bind(size_)
    .bind(&nonce)
    .bind(access_password)
    .bind(deletion_password)
    .bind(download_limit)
    .bind(expire_date)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

pub async fn get_init_upload(
    pool: &common::db::DbPool,
    uuid_: Uuid,
) -> anyhow::Result<Option<InitUpload>> {
    let row = sqlx::query_as::<_, InitUpload>("select * from init_upload where uuid_ = $1")
        .bind(uuid_)
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

pub async fn delete_init_upload(pool: &common::db::DbPool, id: i32) -> anyhow::Result<()> {
    sqlx::query("delete from init_upload where id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Upload — committed upload record
// ---------------------------------------------------------------------------

#[derive(Debug, FromRow)]
pub struct Upload {
    pub id: i32,
    pub uuid_: Uuid,
    pub file_name_hash: Vec<u8>,
    pub content_hash: Vec<u8>,
    pub size_: i64,
    pub storage_uri: String,
    pub nonce: Vec<u8>,
    pub access_password: i32,
    pub deletion_password: Option<i32>,
    pub download_limit: Option<i32>,
    pub expire_date: DateTime<Utc>,
    pub deleted: bool,
    pub date_created: DateTime<Utc>,
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_upload(
    pool: &common::db::DbPool,
    uuid_: Uuid,
    file_name_hash: Vec<u8>,
    content_hash: Vec<u8>,
    size_: i64,
    storage_uri: String,
    nonce: Vec<u8>,
    access_password: i32,
    deletion_password: Option<i32>,
    download_limit: Option<i32>,
    expire_date: DateTime<Utc>,
) -> anyhow::Result<Upload> {
    let row = sqlx::query_as::<_, Upload>(
        "insert into upload
         (uuid_, file_name_hash, content_hash, size_, storage_uri, nonce, access_password,
          deletion_password, download_limit, expire_date)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         returning *",
    )
    .bind(uuid_)
    .bind(&file_name_hash)
    .bind(&content_hash)
    .bind(size_)
    .bind(&storage_uri)
    .bind(&nonce)
    .bind(access_password)
    .bind(deletion_password)
    .bind(download_limit)
    .bind(expire_date)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

pub async fn get_upload(pool: &common::db::DbPool, uuid_: Uuid) -> anyhow::Result<Option<Upload>> {
    let row =
        sqlx::query_as::<_, Upload>("select * from upload where uuid_ = $1 and deleted = false")
            .bind(uuid_)
            .fetch_optional(pool)
            .await?;
    Ok(row)
}

pub async fn soft_delete_upload(pool: &common::db::DbPool, id: i32) -> anyhow::Result<()> {
    sqlx::query("update upload set deleted = true where id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn record_download(pool: &common::db::DbPool, upload_id: i32) -> anyhow::Result<()> {
    sqlx::query("insert into download (upload_id) values ($1)")
        .bind(upload_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn count_downloads(pool: &common::db::DbPool, upload_id: i32) -> anyhow::Result<i64> {
    use sqlx::Row;
    let row = sqlx::query("select count(*) from download where upload_id = $1")
        .bind(upload_id)
        .fetch_one(pool)
        .await?;
    Ok(row.get(0))
}

// ---------------------------------------------------------------------------
// InitDownload — temporary download token pair
// ---------------------------------------------------------------------------

#[derive(Debug, FromRow)]
pub struct InitDownload {
    pub id: i32,
    pub uuid_: Uuid,
    pub usage: String,
    pub upload_id: i32,
    pub date_created: DateTime<Utc>,
}

pub async fn insert_init_download(
    pool: &common::db::DbPool,
    uuid_: Uuid,
    usage: &str,
    upload_id: i32,
) -> anyhow::Result<InitDownload> {
    let row = sqlx::query_as::<_, InitDownload>(
        "insert into init_download (uuid_, usage, upload_id) values ($1, $2, $3) returning *",
    )
    .bind(uuid_)
    .bind(usage)
    .bind(upload_id)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

pub async fn get_init_download(
    pool: &common::db::DbPool,
    uuid_: Uuid,
    usage: &str,
) -> anyhow::Result<Option<InitDownload>> {
    let row = sqlx::query_as::<_, InitDownload>(
        "select * from init_download where uuid_ = $1 and usage = $2",
    )
    .bind(uuid_)
    .bind(usage)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

pub async fn delete_init_download(pool: &common::db::DbPool, id: i32) -> anyhow::Result<()> {
    sqlx::query("delete from init_download where id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Status — running totals
// ---------------------------------------------------------------------------

pub async fn increment_status(pool: &common::db::DbPool, bytes: i64) -> anyhow::Result<()> {
    sqlx::query(
        "update status set upload_count = upload_count + 1,
         total_bytes = total_bytes + $1,
         date_modified = now()",
    )
    .bind(bytes)
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Sweep helpers
// ---------------------------------------------------------------------------

#[derive(Debug, FromRow)]
pub struct ExpiredUpload {
    pub id: i32,
    pub storage_uri: String,
}

pub async fn get_expired_uploads(
    pool: &common::db::DbPool,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<ExpiredUpload>> {
    let rows = sqlx::query_as::<_, ExpiredUpload>(
        "select id, storage_uri from upload where deleted = false and expire_date < $1",
    )
    .bind(now)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn delete_expired_init_uploads(
    pool: &common::db::DbPool,
    cutoff: DateTime<Utc>,
) -> anyhow::Result<u64> {
    let result = sqlx::query("delete from init_upload where date_created < $1")
        .bind(cutoff)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

pub async fn delete_expired_init_downloads(
    pool: &common::db::DbPool,
    cutoff: DateTime<Utc>,
) -> anyhow::Result<u64> {
    let result = sqlx::query("delete from init_download where date_created < $1")
        .bind(cutoff)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}
