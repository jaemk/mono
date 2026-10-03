# Spec

Every feature is documented here before or as it lands, with its status.

## Feature status

Status values: `done` (implemented and covered by tests), `pending` (documented,
not yet built; the default), `research` (needs investigation or design before it
can be built). Keep each row's status current with `spec.py set`.

| Feature | Status | Spec |
|---------|--------|------|
| Komino Rooms | done | [komino-rooms.md](komino-rooms.md) |
| Komino Rules | done | [komino-rules.md](komino-rules.md) |
| Komino Interface | pending | [komino-interface.md](komino-interface.md) |
| Komino Realtime | done | [komino-realtime.md](komino-realtime.md) |
| Komino Stats | done | [komino-stats.md](komino-stats.md) |
| Komino Storage | done | [komino-storage.md](komino-storage.md) |

## Conventions

- Each normative statement carries a stable ID (e.g. `FEAT-1`, `API-3`). IDs are
  append-only: retire an ID by marking it removed, never reuse the number.
- Specs are document-first: a feature is documented (status `pending`, or
  `research` if it needs design work) before implementation begins. Flip to
  `done` only once implemented and verified.
- Spec files are named `<slug>.md` and linked from the table above.
