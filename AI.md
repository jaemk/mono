# AI Assistant Notes

## Shell
- `cd` is aliased in this shell — always use `builtin cd` instead

## Project
- Rust workspace with four crates: `crates/common`, `crates/spot`, `crates/mono`, `crates/paste`
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

## Docker
- `bin/stub_workspace.sh` generates stub source files from the workspace manifest for dependency-caching Docker builds
- When adding a new crate, add a `COPY crates/<name>/Cargo.toml` line to the Dockerfile builder stage

