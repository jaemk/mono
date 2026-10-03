CREATE TABLE players (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    created TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE rooms (
    id BIGSERIAL PRIMARY KEY,
    code TEXT NOT NULL UNIQUE,
    host_player_id TEXT NOT NULL REFERENCES players(id),
    last_winner TEXT,
    created TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_active TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE room_members (
    room_id BIGINT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    player_id TEXT NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    joined TIMESTAMPTZ NOT NULL DEFAULT now(),
    left_at TIMESTAMPTZ,
    removed BOOLEAN NOT NULL DEFAULT false,
    present_until TIMESTAMPTZ,
    PRIMARY KEY (room_id, player_id)
);

CREATE TABLE games (
    id BIGSERIAL PRIMARY KEY,
    room_id BIGINT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    status TEXT NOT NULL,
    version BIGINT NOT NULL DEFAULT 1,
    state JSONB NOT NULL,
    created TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended TIMESTAMPTZ
);

-- at most one unfinished game per room
CREATE UNIQUE INDEX games_one_active ON games (room_id) WHERE status <> 'scored';
CREATE INDEX games_room ON games (room_id, id DESC);

CREATE TABLE room_stats (
    room_id BIGINT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    player_id TEXT NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    games_played INT NOT NULL DEFAULT 0,
    wins INT NOT NULL DEFAULT 0,
    komino_calls INT NOT NULL DEFAULT 0,
    komino_wins INT NOT NULL DEFAULT 0,
    matches INT NOT NULL DEFAULT 0,
    failed_matches INT NOT NULL DEFAULT 0,
    special_moves INT NOT NULL DEFAULT 0,
    cards_interacted INT NOT NULL DEFAULT 0,
    forfeits INT NOT NULL DEFAULT 0,
    PRIMARY KEY (room_id, player_id)
);

CREATE TABLE game_events (
    id BIGSERIAL PRIMARY KEY,
    game_id BIGINT NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    version BIGINT NOT NULL,
    player_id TEXT,
    kind TEXT NOT NULL,
    payload JSONB NOT NULL,
    created TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX game_events_game ON game_events (game_id, id DESC);
