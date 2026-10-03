-- client ECDH keys, bound to the player who first used them (SEAL-5)
CREATE TABLE client_keys (
    key_hash TEXT PRIMARY KEY,
    player_id TEXT NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    first_seen TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- peek reveals already handed out; each can be revealed once (SEAL-6)
CREATE TABLE reveal_fetches (
    reveal_id TEXT PRIMARY KEY,
    game_id BIGINT NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    created TIMESTAMPTZ NOT NULL DEFAULT now()
);
