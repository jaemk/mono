-- wrong passwords lock an account per client address (ACCT-9), so a
-- stranger can't lock its owner out
CREATE TABLE login_locks (
    player_id TEXT NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    addr TEXT NOT NULL,
    failures INT NOT NULL DEFAULT 0,
    locked_until TIMESTAMPTZ,
    updated TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (player_id, addr)
);
CREATE INDEX login_locks_updated ON login_locks (updated);
ALTER TABLE players DROP COLUMN failed_logins, DROP COLUMN locked_until;

-- each session's device, for the account page's device list (ACCT-19)
ALTER TABLE sessions
    ADD COLUMN id BIGINT GENERATED ALWAYS AS IDENTITY UNIQUE,
    ADD COLUMN agent TEXT NOT NULL DEFAULT '',
    ADD COLUMN last_used TIMESTAMPTZ NOT NULL DEFAULT now();

-- games played with partners since forgotten or deleted (STAT-10)
CREATE TABLE player_partners_gone (
    player_id TEXT PRIMARY KEY REFERENCES players(id) ON DELETE CASCADE,
    games INT NOT NULL,
    last_played TIMESTAMPTZ NOT NULL
);
