/// Wipe all rows from the `pastes` table, first attempting to delete each
/// associated S3 object.  S3 deletion is best-effort: failures are logged as
/// warnings and do not prevent the DB truncation from completing.  The test
/// bucket has a 1-day lifecycle policy as a backstop for anything missed here.
pub async fn clean_paste_db(
    pool: &common::db::DbPool,
    s3: &aws_sdk_s3::Client,
    config: &crate::Config,
) {
    // Collect all storage URIs currently in the table.
    let uris: Vec<String> = sqlx::query_scalar("SELECT storage_uri FROM pastes")
        .fetch_all(pool)
        .await
        .unwrap_or_else(|e| {
            eprintln!("test_utils: failed to list paste storage_uris: {e}");
            vec![]
        });

    // Best-effort S3 cleanup — log but never abort on failure.
    for uri in &uris {
        if let Err(e) = crate::storage::delete_object(s3, &config.s3_bucket, uri).await {
            eprintln!(
                "test_utils: S3 delete of '{}' failed (will be cleaned by bucket TTL): {e}",
                uri
            );
        }
    }

    // Truncate the DB table regardless of S3 outcome.
    sqlx::query("TRUNCATE pastes RESTART IDENTITY CASCADE")
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("test_utils: truncate pastes failed: {e}"));
}
