DROP TABLE player_partners_gone;
ALTER TABLE sessions DROP COLUMN id, DROP COLUMN agent, DROP COLUMN last_used;
ALTER TABLE players
    ADD COLUMN failed_logins INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN locked_until TIMESTAMPTZ;
DROP TABLE login_locks;
