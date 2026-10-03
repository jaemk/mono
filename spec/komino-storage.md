# Komino Storage

Crate layout, config gate, postgres schema, and migrations.

## Layout

### STORE-1
Komino is a new workspace crate `crates/komino` following the `mapour` layout: `config.rs`,
`handlers.rs`, `models.rs` (rooms, members, observers, actions, views), `service.rs` (router,
notify listener, timer sweeper, init), `test_utils.rs`, and `assets/` holding `index.html` and
`static/app.{js,css}`. There is no object storage, so no `storage.rs`. The rules engine lives
in its own module (`game.rs`) with no database or io dependency. `web/` holds the client test
suite (UI-21) and is not served.

### STORE-2
The mono binary mounts it at `/komino`, adds `Option<komino::State>` to `AppState`, and adds
the crate to `Cargo.toml`, the Dockerfile `COPY` list, and `AI.md`.

### STORE-3
Komino is gated by `KOMINO_ENABLED` (default `false`), mirroring `MAPOUR_ENABLED`: when off,
no pool is created and no routes are mounted.

## Config

### STORE-4
Environment variables:

| Var | Default |
|-----|---------|
| `KOMINO_ENABLED` | `false` |
| `KOMINO_DATABASE_URL` | `postgres://localhost/komino` |
| `KOMINO_SIGNING_KEY` | a dev-only placeholder; startup fails when enabled with it or a key under 32 chars |
| `KOMINO_REAL_HOSTNAME` | `http://localhost:3000` |
| `KOMINO_DB_USER/NAME/HOST/PORT/PASS` | used by migrant |

`fly.toml` gets the non-secret vars and lists the secrets in its required-secrets comment.

## Schema

### STORE-5
Migrations live in `migrations/komino/` with a `Migrant.toml` matching `migrations/mapour`.

### STORE-6
Tables:
- `players`: id, name, created, last_seen.
- `rooms`: id, code (unique), host_player_id, last_winner, created, last_active.
- `room_members`: room_id, player_id, joined, left_at (nullable), removed (bool),
  present_until (presence heartbeat, ROOM-12), primary key (room_id, player_id).
- `games`: id, room_id, status (`peeking`, `playing`, `final`, `scoring`, `scored`), version,
  state (jsonb: deck, discard pile, discard sequence number, seats with hands and scores, turn,
  caller, reveals, deadlines), created, ended.
- `room_stats`: room_id, player_id, one integer column per STAT-2 counter.
- `game_events`: game_id, version, player_id, kind, public payload (jsonb), created. Used for
  the event log (UI-8).
- `room_observers`: id, room_id, until (observer lease, OBS-9).

### STORE-7
At most one non-scored game exists per room (partial unique index on `games(room_id)` where
status != `scored`).

### STORE-8
Use `sqlx::query()` / `sqlx::query_as()` with `.bind()`, never the `query!` macros (AI.md).

## Testing

### STORE-9
The rules engine (`game.rs`) has unit tests for every RULE-* statement, driven by a seeded
deck.

### STORE-10
Integration tests in `crates/komino/tests/` run against the hermetic test database
(`make test`) and cover every http and websocket route, room flows, redaction (no hidden
values in any other member's or observer's view), the observer limit, and concurrent match
attempts where exactly one wins (RT-10).

### STORE-11
Client tests are described in UI-21 and run with `make test-js`, separately from `make test`.
