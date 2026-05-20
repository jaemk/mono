/// Truncate all transfer tables in dependency order and best-effort delete any
/// S3 objects referenced by the `upload` table.  Called at the start of every
/// DB-touching test so prior-test debris is removed even after a panic.
pub async fn clean_transfer_db(
    pool: &common::db::DbPool,
    s3: &aws_sdk_s3::Client,
    config: &crate::Config,
) {
    let uris: Vec<String> =
        sqlx::query_scalar("select storage_uri from upload where deleted = false")
            .fetch_all(pool)
            .await
            .unwrap_or_else(|e| {
                eprintln!("test_utils: failed to list transfer storage_uris: {e}");
                vec![]
            });

    for uri in &uris {
        if let Err(e) = crate::storage::delete_object(s3, &config.s3_bucket, uri).await {
            eprintln!(
                "test_utils: S3 delete of '{}' failed (will be cleaned by bucket TTL): {e}",
                uri
            );
        }
    }

    // Truncate in dependency order: children first, then parents.
    for table in &["download", "init_download", "upload", "init_upload", "auth"] {
        sqlx::query(&format!("truncate {table} restart identity cascade"))
            .execute(pool)
            .await
            .unwrap_or_else(|e| panic!("test_utils: truncate {table} failed: {e}"));
    }
}
