-- the sha-256 of an account's one recovery code (ACCT-18)
ALTER TABLE players ADD COLUMN recovery_hash TEXT;

-- anonymous players gone quiet, for the cleanup (ACCT-21)
CREATE INDEX players_anonymous_seen ON players (last_seen) WHERE username IS NULL AND NOT bot;

-- a player's rooms by player, for the account page, leaving every room, and
-- the cascades when a player is deleted
CREATE INDEX room_members_player ON room_members (player_id);
CREATE INDEX room_stats_player ON room_stats (player_id);
CREATE INDEX rooms_host ON rooms (host_player_id);
