ALTER TABLE room_stats
    ADD COLUMN draws INT NOT NULL DEFAULT 0,
    ADD COLUMN swaps INT NOT NULL DEFAULT 0,
    ADD COLUMN views INT NOT NULL DEFAULT 0,
    ADD COLUMN others_matched INT NOT NULL DEFAULT 0,
    ADD COLUMN match_ms BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN timed_matches INT NOT NULL DEFAULT 0,
    ADD COLUMN bot_games INT NOT NULL DEFAULT 0;

-- lifetime totals per player, kept when rooms are deleted (STAT-7)
CREATE TABLE player_stats (
    player_id TEXT PRIMARY KEY REFERENCES players(id) ON DELETE CASCADE,
    games_played INT NOT NULL DEFAULT 0,
    wins INT NOT NULL DEFAULT 0,
    komino_calls INT NOT NULL DEFAULT 0,
    komino_wins INT NOT NULL DEFAULT 0,
    matches INT NOT NULL DEFAULT 0,
    failed_matches INT NOT NULL DEFAULT 0,
    special_moves INT NOT NULL DEFAULT 0,
    cards_interacted INT NOT NULL DEFAULT 0,
    forfeits INT NOT NULL DEFAULT 0,
    draws INT NOT NULL DEFAULT 0,
    swaps INT NOT NULL DEFAULT 0,
    views INT NOT NULL DEFAULT 0,
    others_matched INT NOT NULL DEFAULT 0,
    match_ms BIGINT NOT NULL DEFAULT 0,
    timed_matches INT NOT NULL DEFAULT 0,
    bot_games INT NOT NULL DEFAULT 0
);

-- correct matches per 50ms band of reaction time, for the median (STAT-9)
CREATE TABLE player_match_times (
    player_id TEXT NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    band SMALLINT NOT NULL,
    n INT NOT NULL,
    PRIMARY KEY (player_id, band)
);

-- games played together, one row each way (STAT-10)
CREATE TABLE player_partners (
    player_id TEXT NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    other_id TEXT NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    games INT NOT NULL,
    last_played TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (player_id, other_id)
);
CREATE INDEX player_partners_other ON player_partners (other_id);
