DROP TABLE player_partners;
DROP TABLE player_match_times;
DROP TABLE player_stats;

ALTER TABLE room_stats
    DROP COLUMN draws,
    DROP COLUMN swaps,
    DROP COLUMN views,
    DROP COLUMN others_matched,
    DROP COLUMN match_ms,
    DROP COLUMN timed_matches,
    DROP COLUMN bot_games;
