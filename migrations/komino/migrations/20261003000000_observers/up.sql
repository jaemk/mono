-- one row per connected observer socket, held by a refreshed lease
CREATE TABLE room_observers (
    id TEXT PRIMARY KEY,
    room_id BIGINT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    until TIMESTAMPTZ NOT NULL
);
CREATE INDEX room_observers_room ON room_observers (room_id);
