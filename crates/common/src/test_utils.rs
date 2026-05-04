/// Truncate each named table (schema-qualified names are fine, e.g.
/// `"spot.one_time_tokens"`).  Rows are removed, sequences reset, and
/// cascading foreign-key deletions applied automatically.
///
/// Panics if any TRUNCATE statement fails so that a broken test setup is
/// immediately visible rather than silently producing wrong results.
pub async fn truncate_tables(pool: &crate::db::DbPool, tables: &[&str]) {
    for table in tables {
        let sql = format!("TRUNCATE {} RESTART IDENTITY CASCADE", table);
        sqlx::query(&sql)
            .execute(pool)
            .await
            .unwrap_or_else(|e| panic!("test_utils: truncate {} failed: {}", table, e));
    }
}

/// Convenience macro: truncate a fixed list of tables at the start of a test.
///
/// ```ignore
/// clean_db!(&pool, ["spot.one_time_tokens", "spot.users"]);
/// ```
#[macro_export]
macro_rules! clean_db {
    ($pool:expr, [$($table:literal),+ $(,)?]) => {
        common::test_utils::truncate_tables($pool, &[$($table),+]).await
    };
}
