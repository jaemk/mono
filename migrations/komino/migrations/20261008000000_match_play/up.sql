-- match play over several games (SET-14 - SET-16) and memory marks (SET-17)
ALTER TABLE rooms
    ADD COLUMN target_score INTEGER,
    ADD COLUMN caller_penalty INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN exact_reset BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN memory_marks BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN history JSONB NOT NULL DEFAULT '[]';

-- computer players (BOT-1) and how well they play (BOT-5)
ALTER TABLE players
    ADD COLUMN bot BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN bot_level TEXT NOT NULL DEFAULT 'normal';
