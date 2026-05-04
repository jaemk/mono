#!/usr/bin/env bash
# bin/test-db.sh
#
# Spin up ephemeral test databases, run the full workspace test suite with
# those databases, then unconditionally drop the ephemeral databases on exit.
#
# Usage:
#   ./bin/test-db.sh              # run all workspace tests
#   ./bin/test-db.sh cargo test -p spot  # run a specific crate
#
# The script:
#   1. Generates a short random TEST_ID so DB names are unique even if a
#      previous run crashed without cleaning up.
#   2. Sources .env (if present) so base credentials are available; then
#      exports overriding SPOT_DATABASE_URL / PASTE_DATABASE_URL pointing at
#      the new ephemeral DBs — these take precedence over any .env values
#      because we export them *after* sourcing the file.
#   3. Creates the ephemeral databases (reusing the existing spot/paste roles).
#   4. Runs all pending migrations via migrant.
#   5. Runs `cargo test --workspace -- --test-threads=1` (or a custom command
#      passed as arguments).
#   6. Unconditionally drops both ephemeral databases via a trap on EXIT.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# ---------------------------------------------------------------------------
# Unique identifier for this test run
# ---------------------------------------------------------------------------
TEST_ID="$(openssl rand -hex 4)"
SPOT_TEST_DB="spot_test_${TEST_ID}"
PASTE_TEST_DB="paste_test_${TEST_ID}"

# ---------------------------------------------------------------------------
# Load base credentials from .env (if it exists) so DB_HOST / ports etc. are
# available, but do NOT let it override the test DB URLs we set below.
# ---------------------------------------------------------------------------
if [ -f "${ROOT}/.env" ]; then
    # Export all non-comment, non-empty lines from .env.
    set -a
    # shellcheck disable=SC1090
    source "${ROOT}/.env"
    set +a
fi

# Derive connection params from what .env (or defaults) provided.
SPOT_DB_USER="${SPOT_DB_USER:-spot}"
SPOT_DB_PASS="${SPOT_DB_PASS:-spot}"
SPOT_DB_HOST="${SPOT_DB_HOST:-localhost}"
SPOT_DB_PORT="${SPOT_DB_PORT:-5432}"

PASTE_DB_USER="${PASTE_DB_USER:-paste}"
PASTE_DB_PASS="${PASTE_DB_PASS:-paste}"
PASTE_DB_HOST="${PASTE_DB_HOST:-localhost}"
PASTE_DB_PORT="${PASTE_DB_PORT:-5432}"

# Override database URLs to point at ephemeral DBs.
export SPOT_DB_NAME="${SPOT_TEST_DB}"
export PASTE_DB_NAME="${PASTE_TEST_DB}"
export SPOT_DATABASE_URL="postgres://${SPOT_DB_USER}:${SPOT_DB_PASS}@${SPOT_DB_HOST}:${SPOT_DB_PORT}/${SPOT_TEST_DB}"
export PASTE_DATABASE_URL="postgres://${PASTE_DB_USER}:${PASTE_DB_PASS}@${PASTE_DB_HOST}:${PASTE_DB_PORT}/${PASTE_TEST_DB}"

echo "==> Test run ID: ${TEST_ID}"
echo "    SPOT_DATABASE_URL  = ${SPOT_DATABASE_URL}"
echo "    PASTE_DATABASE_URL = ${PASTE_DATABASE_URL}"

# ---------------------------------------------------------------------------
# Helper: run psql as the postgres superuser
# ---------------------------------------------------------------------------
pg_exec() {
    local host="$1"; local port="$2"; shift 2
    psql -h "${host}" -p "${port}" -d postgres --no-password -c "$@"
}

# ---------------------------------------------------------------------------
# Cleanup: drop both ephemeral databases unconditionally.
# ---------------------------------------------------------------------------
cleanup() {
    echo ""
    echo "==> Cleaning up ephemeral test databases..."

    pg_exec "${SPOT_DB_HOST}" "${SPOT_DB_PORT}" \
        "DROP DATABASE IF EXISTS ${SPOT_TEST_DB};" 2>/dev/null || true

    pg_exec "${PASTE_DB_HOST}" "${PASTE_DB_PORT}" \
        "DROP DATABASE IF EXISTS ${PASTE_TEST_DB};" 2>/dev/null || true

    echo "==> Done."
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Create ephemeral spot test database
# ---------------------------------------------------------------------------
echo ""
echo "==> Creating ephemeral spot database '${SPOT_TEST_DB}'..."
pg_exec "${SPOT_DB_HOST}" "${SPOT_DB_PORT}" \
    "DO \$\$ BEGIN
       IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = '${SPOT_DB_USER}') THEN
         CREATE ROLE ${SPOT_DB_USER} WITH LOGIN PASSWORD '${SPOT_DB_PASS}';
       END IF;
     END \$\$;"
pg_exec "${SPOT_DB_HOST}" "${SPOT_DB_PORT}" \
    "CREATE DATABASE ${SPOT_TEST_DB} OWNER ${SPOT_DB_USER};"
pg_exec "${SPOT_DB_HOST}" "${SPOT_DB_PORT}" \
    "GRANT ALL PRIVILEGES ON DATABASE ${SPOT_TEST_DB} TO ${SPOT_DB_USER};"

# ---------------------------------------------------------------------------
# Create ephemeral paste test database
# ---------------------------------------------------------------------------
echo "==> Creating ephemeral paste database '${PASTE_TEST_DB}'..."
pg_exec "${PASTE_DB_HOST}" "${PASTE_DB_PORT}" \
    "DO \$\$ BEGIN
       IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = '${PASTE_DB_USER}') THEN
         CREATE ROLE ${PASTE_DB_USER} WITH LOGIN PASSWORD '${PASTE_DB_PASS}';
       END IF;
     END \$\$;"
pg_exec "${PASTE_DB_HOST}" "${PASTE_DB_PORT}" \
    "CREATE DATABASE ${PASTE_TEST_DB} OWNER ${PASTE_DB_USER};"
pg_exec "${PASTE_DB_HOST}" "${PASTE_DB_PORT}" \
    "GRANT ALL PRIVILEGES ON DATABASE ${PASTE_TEST_DB} TO ${PASTE_DB_USER};"

# ---------------------------------------------------------------------------
# Ensure migrant is installed
# ---------------------------------------------------------------------------
if ! which migrant > /dev/null 2>&1; then
    echo "==> Installing migrant..."
    cargo install migrant --features postgres
fi

# ---------------------------------------------------------------------------
# Run migrations
# ---------------------------------------------------------------------------
echo ""
echo "==> Running spot migrations on '${SPOT_TEST_DB}'..."
(builtin cd "${ROOT}/migrations/spot" && migrant setup && (migrant apply -a || echo "ok"))

echo "==> Running paste migrations on '${PASTE_TEST_DB}'..."
(builtin cd "${ROOT}/migrations/paste" && migrant setup && (migrant apply -a || echo "ok"))

# ---------------------------------------------------------------------------
# Run the test suite
# ---------------------------------------------------------------------------
echo ""
echo "==> Running tests (--test-threads=1 for DB isolation)..."
builtin cd "${ROOT}"
if [ $# -gt 0 ]; then
    # Custom command passed as arguments (e.g. `cargo test -p spot`)
    "$@" -- --test-threads=1
else
    cargo test --workspace -- --test-threads=1
fi

