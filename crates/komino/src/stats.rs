//! A player's lifetime stats (STAT-7 - STAT-11): totals kept per player as
//! each action saves, a band count of match reaction times for the median,
//! and who they play with. Nothing here is read back from game events,
//! which go with their room.

use crate::game::Stat;
use crate::models::Result;
use common::db::DbPool;
use serde_json::{json, Map, Value};
use sqlx::Row;

/// Width of a match time band, in ms (STAT-9).
pub const BAND_MS: i32 = 50;
/// The last band holds every time from `TOP_BAND * BAND_MS` up.
pub const TOP_BAND: i16 = 100;
/// Partners listed on the account page (STAT-10).
pub const PARTNERS_SHOWN: i64 = 20;

/// The band a match reaction time falls in.
pub fn band(ms: i32) -> i16 {
    (ms.max(0) / BAND_MS).min(i32::from(TOP_BAND)) as i16
}

/// One row per (player, band) with its count, for the save statement. A
/// band is never sent twice for a player, which the upsert can't take.
pub fn band_rows(times: &[(String, i32)]) -> Value {
    let mut rows: Vec<(String, i16, i32)> = Vec::new();
    for (player, ms) in times {
        let b = band(*ms);
        match rows.iter_mut().find(|(p, rb, _)| p == player && *rb == b) {
            Some(row) => row.2 += 1,
            None => rows.push((player.clone(), b, 1)),
        }
    }
    Value::Array(
        rows.into_iter()
            .map(|(player, band, n)| json!({ "player": player, "band": band, "n": n }))
            .collect(),
    )
}

/// The median match time from band counts, as the middle of its band; the
/// top band reads as its floor. `None` without any timed match.
pub fn median_ms(bands: &[(i16, i64)]) -> Option<i64> {
    let total: i64 = bands.iter().map(|(_, n)| n).sum();
    if total == 0 {
        return None;
    }
    let mut sorted = bands.to_vec();
    sorted.sort_by_key(|(b, _)| *b);
    // the lower middle sample of an even count
    let middle = (total + 1) / 2;
    let mut seen = 0;
    for (b, n) in sorted {
        seen += n;
        if seen >= middle {
            let floor = i64::from(b) * i64::from(BAND_MS);
            return Some(if b >= TOP_BAND {
                floor
            } else {
                floor + i64::from(BAND_MS) / 2
            });
        }
    }
    None
}

/// Everything the account page shows for `player` (STAT-11).
pub async fn player_stats(db: &DbPool, player: &str) -> Result<Value> {
    let row = sqlx::query(
        "SELECT
             (SELECT to_jsonb(s) - 'player_id' FROM player_stats s WHERE s.player_id = $1)
                 AS totals,
             (SELECT coalesce(jsonb_agg(jsonb_build_array(t.band, t.n)), '[]')
              FROM player_match_times t WHERE t.player_id = $1) AS bands,
             (SELECT coalesce(jsonb_agg(jsonb_build_object(
                  'id', p.id, 'name', p.name, 'username', p.username,
                  'games', pp.games, 'last_played', pp.last_played
              ) ORDER BY pp.games DESC, pp.last_played DESC), '[]')
              FROM (SELECT * FROM player_partners WHERE player_id = $1
                    ORDER BY games DESC, last_played DESC LIMIT $2) pp
                  JOIN players p ON p.id = pp.other_id) AS partners,
             coalesce((SELECT games FROM player_partners_gone WHERE player_id = $1), 0)
                 AS partners_gone",
    )
    .bind(player)
    .bind(PARTNERS_SHOWN)
    .fetch_one(db)
    .await?;
    let stored: Option<Value> = row.get("totals");
    let mut totals: Map<String, Value> = match stored {
        Some(Value::Object(m)) => m,
        _ => Stat::ALL
            .iter()
            .map(|s| (s.column().to_string(), json!(0)))
            .collect(),
    };
    let sum = totals
        .remove("match_ms")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let timed = totals
        .remove("timed_matches")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let bands: Vec<(i16, i64)> = row
        .get::<Value, _>("bands")
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| Some((b[0].as_i64()? as i16, b[1].as_i64()?)))
        .collect();
    totals.insert(
        "match_time".into(),
        json!({
            "timed": timed,
            "average_ms": (timed > 0).then(|| sum / timed),
            "median_ms": median_ms(&bands),
        }),
    );
    totals.insert("partners".into(), row.get("partners"));
    // games with partners since forgotten or deleted (STAT-10)
    totals.insert(
        "partners_gone".into(),
        json!(row.get::<i32, _>("partners_gone")),
    );
    Ok(Value::Object(totals))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_fall_in_fifty_ms_bands_up_to_the_top() {
        assert_eq!(band(0), 0);
        assert_eq!(band(49), 0);
        assert_eq!(band(50), 1);
        assert_eq!(band(4_999), 99);
        assert_eq!(band(5_000), TOP_BAND);
        assert_eq!(band(60_000), TOP_BAND);
        assert_eq!(band(-5), 0);
    }

    #[test]
    fn band_rows_fold_one_row_per_player_and_band() {
        let times = vec![
            ("a".to_string(), 310),
            ("b".to_string(), 320),
            ("a".to_string(), 340),
            ("a".to_string(), 900),
        ];
        assert_eq!(
            band_rows(&times),
            json!([
                { "player": "a", "band": 6, "n": 2 },
                { "player": "b", "band": 6, "n": 1 },
                { "player": "a", "band": 18, "n": 1 },
            ])
        );
        assert_eq!(band_rows(&[]), json!([]));
    }

    #[test]
    fn the_median_is_the_middle_of_its_band() {
        assert_eq!(median_ms(&[]), None);
        assert_eq!(median_ms(&[(3, 0)]), None);
        assert_eq!(median_ms(&[(6, 1)]), Some(325));
        // 1 fast, 1 middling, 3 slow: the third sample is slow
        assert_eq!(median_ms(&[(20, 3), (2, 1), (8, 1)]), Some(1_025));
        // an even count takes the lower middle
        assert_eq!(median_ms(&[(2, 2), (8, 2)]), Some(125));
        assert_eq!(median_ms(&[(TOP_BAND, 5), (1, 1)]), Some(5_000));
    }
}
