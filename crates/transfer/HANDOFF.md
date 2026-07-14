# Transfer handoff

Status as of this branch (`transfer`): the transfer sub-site is implemented but
temporarily unwired from the live `mono` app so the branch can merge. All
transfer crate code, migrations, and tests are intact. Re-enabling is mechanical
(see below).

## Why it is disabled

`mono` mounts each sub-site and calls its `init()` at startup. `transfer::service::init`
opens a DB pool and an S3 client, so leaving it wired would require a provisioned
transfer DB and S3 endpoint to boot or deploy. To merge without that dependency,
the transfer wiring in `mono` is commented out; the transfer crate itself is
unchanged and still compiles.

## Re-enabling

Each removal is marked with a `transfer sub-site temporarily disabled` comment.
Restore these four spots:

- `crates/mono/src/lib.rs` - add back the `transfer_state` field on `AppState`,
  its `FromRef<AppState>` impl, the `app()` parameter, and
  `.nest("/transfer", transfer::service::router(state.clone()))`.
- `crates/mono/src/main.rs` - restore the `transfer::service::init(transfer::Config::load())`
  call and pass `transfer_state` to `app(...)`.
- `crates/mono/src/handlers.rs` - restore the `transfer.kominick.com` -> `/transfer`
  host redirect.
- `crates/mono/tests/integration_tests.rs` - restore transfer init and pass it to `app(...)`.

`transfer.workspace = true` is still listed in `crates/mono/Cargo.toml` (unused for
now, not a build error) so re-adding the wiring needs no dependency change.

## What is implemented

Backend (`crates/transfer/src`):

- Encrypted upload / download / delete flow (`handlers::api_upload_*`,
  `api_download_*`). Client-side encryption; server stores ciphertext in S3 and
  metadata in Postgres.
- Accounts: email registration with emailed verification codes, login, logout,
  session cookies, `me`, and Google OAuth sign-in (`api_auth_*`). Optional
  password on account (`api_settings_add_password`).
- Per-user transfer listing and delete (`api_my_transfers`, `api_my_delete`);
  uploads are associated to a `user_id` when authenticated.
- Background sweep of expired uploads/registrations (`sweep.rs`).
- SMTP send path lives in `crates/common/src/smtp.rs`.

Frontend (`crates/transfer/web`): pages split into per-page ES modules under
`web/static/*.page.js` with `node --test` coverage in the matching
`*.page.test.mjs` files, plus shared `page-test-utils.mjs`.

Schema: `migrations/transfer/migrations/` - `20260515000000_initial` and
`20260520000000_add_users` (adds `transfer_user`, `pending_registration`,
`transfer_session`, and `user_id` columns on `upload` / `init_upload`).

## Config (env)

Loaded in `crates/transfer/src/config.rs`. Notable optional integrations:

- `TRANSFER_DATABASE_URL`, `TRANSFER_S3_BUCKET`, `TRANSFER_S3_ENDPOINT`, `TRANSFER_S3_REGION`
- `TRANSFER_GOOGLE_CLIENT_ID` - enables Google sign-in when set.
- SMTP env (see `common::smtp::SmtpConfig::from_env`) - enables email code sending.
- `TRANSFER_BASE_URL`, `TRANSFER_REGISTRATION_CODE_SECS`, upload/download timeout
  and lifespan vars.

Google sign-in and email verification are each gated on their config being
present (`Config::google_enabled`, `Config::smtp_enabled`).

## Tests

- Rust: `make test` (via `bin/test-db.sh`) spins up ephemeral Postgres databases
  and runs the workspace tests, including `crates/transfer/tests/integration_tests.rs`.
  Requires a reachable local Postgres.
- Frontend: `make test-js` runs the `web/static/*.test.mjs` suites (201 passing).
  Node lives at the nvm path, not on `PATH`.
- `make fmt` and `make lint` (`cargo clippy --workspace --tests -- -D warnings`)
  both pass on this branch.

## Pick-up notes

- The last feature commit is `f601d4b wip`; this branch layers the sub-site
  disable plus a one-line test fix (the transfer integration-test helper now
  passes `None` for the new `user_id` argument of `models::insert_upload`).
- To resume: re-enable the four wiring spots above, provision the transfer DB and
  S3 config, run the migrations, then verify end-to-end (upload -> download,
  register -> verify -> login, my-transfers).
