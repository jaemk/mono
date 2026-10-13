-- accounts (ACCT-1): a player with a username and password hash signs in
-- through a session instead of the anonymous player cookie
ALTER TABLE players
    ADD COLUMN username TEXT UNIQUE,
    ADD COLUMN password_hash TEXT,
    ADD COLUMN failed_logins INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN locked_until TIMESTAMPTZ,
    ADD CONSTRAINT players_account CHECK ((username IS NULL) = (password_hash IS NULL));

-- sessions (ACCT-6): only the sha-256 of the cookie token is stored
CREATE TABLE sessions (
    token_hash TEXT PRIMARY KEY,
    player_id TEXT NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    created TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires TIMESTAMPTZ NOT NULL
);
CREATE INDEX sessions_player ON sessions (player_id);
CREATE INDEX sessions_expires ON sessions (expires);

-- a display name for one room, over the player's default (ACCT-12)
ALTER TABLE room_members ADD COLUMN name TEXT;
