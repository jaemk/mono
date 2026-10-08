DELETE FROM players WHERE bot;
ALTER TABLE players DROP COLUMN bot, DROP COLUMN bot_level;

ALTER TABLE rooms
    DROP COLUMN target_score,
    DROP COLUMN caller_penalty,
    DROP COLUMN exact_reset,
    DROP COLUMN memory_marks,
    DROP COLUMN history;
