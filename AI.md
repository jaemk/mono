# AI Assistant Notes

## Shell
- `cd` is aliased in this shell — always use `builtin cd` instead

## Project
- Rust workspace with seven crates: `crates/common`, `crates/spot`, `crates/mono`, `crates/paste`, `crates/mapour`, `crates/komino`, `crates/tick`
- Feature specs live in `spec/`; see `spec/README.md` for status
- Main binary is `mono` in `crates/mono`

## Build & Check
```bash
# type-check a crate
cargo check -p spot

# build the release binary
cargo build --release --bin mono

# run tests
cargo test
```
- Do **not** use `sqlx::query!` / `sqlx::query_as!` macros; use the regular `sqlx::query()` / `sqlx::query_as::<_, T>()` functions with `.bind()` chains instead — this avoids needing `SQLX_OFFLINE` or `sqlx-data.json`

## After Every Change
Always run these three in order before considering a task complete:
```bash
make fmt
make lint
make test
```
`make test` needs no setup beyond docker and cargo: `bin/test-db.sh` uses a postgres
reachable without a password (as in CI) or starts a throwaway `postgres:16` container, and
`bin/test-js.sh` runs the komino and tick client tests (`crates/{komino,tick}/web`) with node
from PATH or nvm, or in a `node:24` container. `make test-rust` and `make test-js` run either half.

## CI
`.github/workflows/ci.yml` runs on every PR and push to main: fmt and clippy, `cargo audit
--deny warnings` (ignores in `.cargo/audit.toml`), the full `bin/test-db.sh` suite against
postgres plus a RustFS S3 store (so S3-backed tests run), the komino and tick client tests
with coverage thresholds, and a Docker image build. Deploys are still manual.

After a deploy, `make acceptance` (or the manual `acceptance` workflow) runs
`acceptance/live.test.mjs` against the live site: every mounted app, a paste round trip,
and a full komino round with sealed reveals and an observer. Writes are short-lived.
Every feature or behavior change extends this suite as part of its test coverage, alongside the
unit, integration, and client tests.

## Docker
- `bin/stub_workspace.sh` generates stub source files from the workspace manifest for dependency-caching Docker builds
- When adding a new crate, add a `COPY crates/<name>/Cargo.toml` line to the Dockerfile builder stage

