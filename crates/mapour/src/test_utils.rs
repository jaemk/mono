/// Wipe all mapour tables, first attempting to delete any photo S3 objects.
/// S3 deletion is best-effort: failures are logged and do not prevent the
/// truncation — the test bucket's lifecycle policy is the backstop.
pub async fn clean_mapour_db(
    pool: &common::db::DbPool,
    s3: &aws_sdk_s3::Client,
    config: &crate::Config,
) {
    let keys: Vec<String> = sqlx::query_scalar("SELECT s3_key FROM photos")
        .fetch_all(pool)
        .await
        .unwrap_or_else(|e| {
            eprintln!("test_utils: failed to list photo s3 keys: {e}");
            vec![]
        });
    for key in &keys {
        if let Err(e) = crate::storage::delete_object(s3, &config.s3_bucket, key).await {
            eprintln!(
                "test_utils: S3 delete of '{key}' failed (will be cleaned by bucket TTL): {e}"
            );
        }
    }

    sqlx::query(
        "TRUNCATE users, auth_tokens, email_verifications, anons, orgs, members,
         invites, join_requests, categories, pins, photos
         RESTART IDENTITY CASCADE",
    )
    .execute(pool)
    .await
    .unwrap_or_else(|e| panic!("test_utils: truncate mapour tables failed: {e}"));
}
