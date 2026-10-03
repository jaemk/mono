/// Wipe all komino tables.
pub async fn clean_komino_db(pool: &common::db::DbPool) {
    sqlx::query(
        "TRUNCATE players, rooms, room_members, games, room_stats, game_events, room_observers,
         client_keys, reveal_fetches RESTART IDENTITY CASCADE",
    )
    .execute(pool)
    .await
    .unwrap_or_else(|e| panic!("test_utils: truncate komino tables failed: {e}"));
}
