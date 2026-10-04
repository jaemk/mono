#!/usr/bin/env bash
# bin/test-db.sh
#
# Spin up ephemeral test databases, run the workspace test suite against them,
# then unconditionally drop them on exit.
#
# No manual setup: when no postgres superuser is reachable at the configured
# host/port without a password, an ephemeral docker postgres is started for
# this run and removed on exit. A postgres that is reachable (a CI service, or
# a local one with trust auth) is used as is.
#
# Usage:
#   ./bin/test-db.sh                       # run all workspace tests
#   ./bin/test-db.sh cargo test -p komino  # run a specific crate
#
# Needs docker (or a reachable postgres) and cargo. psql is used when present;
# against the ephemeral container it falls back to the container's psql.
# Override the ephemeral image with TEST_PG_IMAGE (default postgres:16).
#
# The script:
#   1. Generates a short random TEST_ID so DB names are unique even if a
#      previous run crashed without cleaning up.
#   2. Sources .env (if present), then fills test fallbacks for any secret a
#      crate's config requires.
#   3. Ensures a postgres superuser is reachable, starting an ephemeral docker
#      instance if not.
#   4. Creates one database per service (creating the service role if
#      needed) and exports the *_DB_* and *_DATABASE_URL variables pointing at
#      them. These are exported after sourcing .env, so they win.
#   5. Runs all pending migrations via migrant (installing it if missing).
#   6. Runs `cargo test --workspace -- --test-threads=1` (or a custom command
#      passed as arguments).
#   7. Drops the databases, or removes the ephemeral container, on exit.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# every service with a database; each reads <NAME>_DB_{USER,PASS,HOST,PORT,NAME}
SERVICES=(spot paste mapour komino)

TEST_ID="$(openssl rand -hex 4)"

if [ -f "${ROOT}/.env" ]; then
    set -a
    # shellcheck disable=SC1090
    source "${ROOT}/.env"
    set +a
fi

# Test fallbacks for secrets a config hard-requires; a real .env wins.
# Production still requires real keys.
export SPOT_ENC_KEY="${SPOT_ENC_KEY:-01234567890123456789012345678901}"
export SPOT_SPOTIFY_CLIENT_ID="${SPOT_SPOTIFY_CLIENT_ID:-test-client-id}"
export SPOT_SPOTIFY_SECRET_ID="${SPOT_SPOTIFY_SECRET_ID:-test-secret-id}"

upper() { echo "$1" | tr '[:lower:]' '[:upper:]'; }

# Read or set <SERVICE>_DB_<FIELD>.
db_var() { local name; name="$(upper "$1")_DB_$2"; echo "${!name:-}"; }
set_db_var() { local name; name="$(upper "$1")_DB_$2"; printf -v "${name}" '%s' "$3"; export "${name?}"; }

# Exported, since migrant reads them through each migrations/*/Migrant.toml.
for svc in "${SERVICES[@]}"; do
    user="$(db_var "${svc}" USER)"; set_db_var "${svc}" USER "${user:-${svc}}"
    pass="$(db_var "${svc}" PASS)"; set_db_var "${svc}" PASS "${pass:-${svc}}"
    host="$(db_var "${svc}" HOST)"; set_db_var "${svc}" HOST "${host:-localhost}"
    port="$(db_var "${svc}" PORT)"; set_db_var "${svc}" PORT "${port:-5432}"
    set_db_var "${svc}" NAME "${svc}_test_${TEST_ID}"
done

# ---------------------------------------------------------------------------
# psql as the superuser, via the host's psql or the ephemeral container's
# ---------------------------------------------------------------------------
EPHEMERAL_PG=""

psql_super() {
    local host="$1"; local port="$2"; shift 2
    if command -v psql >/dev/null 2>&1; then
        psql -h "${host}" -p "${port}" -d postgres --no-password "$@"
    elif [ -n "${EPHEMERAL_PG}" ]; then
        docker exec -i "${EPHEMERAL_PG}" psql -U postgres -d postgres "$@"
    else
        return 127
    fi
}

pg_exec() {
    local host="$1"; local port="$2"; shift 2
    psql_super "${host}" "${port}" -c "$@"
}

pg_reachable() {
    psql_super "$1" "$2" -tAc 'select 1' >/dev/null 2>&1
}

cleanup() {
    echo ""
    echo "==> Cleaning up ephemeral test resources..."
    if [ -n "${EPHEMERAL_PG}" ]; then
        docker rm -f "${EPHEMERAL_PG}" >/dev/null 2>&1 || true
    else
        for svc in "${SERVICES[@]}"; do
            pg_exec "$(db_var "${svc}" HOST)" "$(db_var "${svc}" PORT)" \
                "DROP DATABASE IF EXISTS $(db_var "${svc}" NAME);" >/dev/null 2>&1 || true
        done
    fi
    echo "==> Done."
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Ensure a postgres superuser is reachable. Every service shares one host and
# port by default, so the first service's settings are the probe target.
# ---------------------------------------------------------------------------
ensure_postgres() {
    local host port
    host="$(db_var "${SERVICES[0]}" HOST)"
    port="$(db_var "${SERVICES[0]}" PORT)"
    if pg_reachable "${host}" "${port}"; then
        echo "==> Using postgres already running at ${host}:${port}"
        return 0
    fi
    if ! command -v docker >/dev/null 2>&1; then
        echo "ERROR: no postgres reachable at ${host}:${port} without a password," >&2
        echo "       and docker is not installed. Install docker, or point" >&2
        echo "       <SERVICE>_DB_HOST/PORT and PGUSER at a reachable postgres." >&2
        exit 1
    fi
    local image="${TEST_PG_IMAGE:-postgres:16}"
    local name="mono_testpg_${TEST_ID}"
    echo "==> No postgres reachable at ${host}:${port}; starting ephemeral '${image}'..."
    docker run -d --name "${name}" \
        -e POSTGRES_HOST_AUTH_METHOD=trust -e POSTGRES_USER=postgres \
        -p 127.0.0.1::5432 "${image}" >/dev/null
    EPHEMERAL_PG="${name}"

    # a random host port, so a local postgres on 5432 never collides
    local mapping hostport
    mapping="$(docker port "${name}" 5432/tcp | head -n1)"
    hostport="${mapping##*:}"
    if [ -z "${hostport}" ]; then
        echo "ERROR: could not determine the ephemeral postgres host port." >&2
        exit 1
    fi
    export PGUSER=postgres
    for svc in "${SERVICES[@]}"; do
        set_db_var "${svc}" HOST 127.0.0.1
        set_db_var "${svc}" PORT "${hostport}"
    done

    echo "==> Waiting for ephemeral postgres on 127.0.0.1:${hostport}..."
    local i
    for i in $(seq 1 60); do
        # the container's own check first: the host port answers before
        # postgres finishes its init restart
        if docker exec "${name}" pg_isready -U postgres -h 127.0.0.1 >/dev/null 2>&1 \
            && pg_reachable 127.0.0.1 "${hostport}"; then
            echo "==> Ephemeral postgres ready"
            return 0
        fi
        sleep 1
    done
    echo "ERROR: ephemeral postgres did not become ready in time." >&2
    exit 1
}
ensure_postgres

echo "==> Test run ID: ${TEST_ID}"
for svc in "${SERVICES[@]}"; do
    user="$(db_var "${svc}" USER)"
    pass="$(db_var "${svc}" PASS)"
    host="$(db_var "${svc}" HOST)"
    port="$(db_var "${svc}" PORT)"
    db="$(db_var "${svc}" NAME)"
    url_var="$(upper "${svc}")_DATABASE_URL"
    printf -v "${url_var}" '%s' "postgres://${user}:${pass}@${host}:${port}/${db}"
    export "${url_var?}"
    echo "    ${url_var} = ${!url_var}"

    pg_exec "${host}" "${port}" \
        "DO \$\$ BEGIN
           IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = '${user}') THEN
             CREATE ROLE ${user} WITH LOGIN PASSWORD '${pass}';
           END IF;
         END \$\$;" >/dev/null
    pg_exec "${host}" "${port}" "CREATE DATABASE ${db} OWNER ${user};" >/dev/null
    pg_exec "${host}" "${port}" "GRANT ALL PRIVILEGES ON DATABASE ${db} TO ${user};" >/dev/null
done

if ! command -v migrant >/dev/null 2>&1; then
    echo "==> Installing migrant..."
    cargo install migrant --features postgres
fi

echo ""
for svc in "${SERVICES[@]}"; do
    echo "==> Running ${svc} migrations on '$(db_var "${svc}" NAME)'..."
    (builtin cd "${ROOT}/migrations/${svc}" && migrant setup && (migrant apply -a || echo "ok"))
done

echo ""
echo "==> Running tests (--test-threads=1 for DB isolation)..."
builtin cd "${ROOT}"
if [ $# -gt 0 ]; then
    # Custom command passed as arguments (e.g. `cargo test -p spot`)
    "$@" -- --test-threads=1
else
    cargo test --workspace -- --test-threads=1
fi
