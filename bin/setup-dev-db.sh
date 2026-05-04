#!/usr/bin/env bash
# setup-dev-db.sh
#
# Creates a database role and database (if they don't already exist)
# then runs all pending migrations.
#
# All DB_* vars must be set by the caller — no defaults are applied here.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Require all vars to be explicitly set
DB_NAME="${DB_NAME:?DB_NAME must be set}"
DB_USER="${DB_USER:?DB_USER must be set}"
DB_PASS="${DB_PASS:?DB_PASS must be set}"
DB_HOST="${DB_HOST:?DB_HOST must be set}"
DB_PORT="${DB_PORT:?DB_PORT must be set}"

echo "==> Setting up dev database"
echo "    host=$DB_HOST  port=$DB_PORT  db=$DB_NAME  user=$DB_USER"

# Helper: run SQL as a postgres superuser
pg_exec() {
    psql -h "$DB_HOST" -p "$DB_PORT" -d postgres --no-password -c "$1"
}

echo "==> Creating role '$DB_USER' (if not exists)..."
pg_exec "DO \$\$ BEGIN
  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = '$DB_USER') THEN
    CREATE ROLE $DB_USER WITH LOGIN PASSWORD '$DB_PASS';
  END IF;
END \$\$;"

echo "==> Creating database '$DB_NAME' (if not exists)..."
pg_exec "SELECT 1 FROM pg_database WHERE datname = '$DB_NAME'" \
    | grep -q 1 \
    || pg_exec "CREATE DATABASE $DB_NAME OWNER $DB_USER;"

echo "==> Granting privileges..."
pg_exec "GRANT ALL PRIVILEGES ON DATABASE $DB_NAME TO $DB_USER;"

echo ""
echo "Done. Connection string:"
echo "  DATABASE_URL=postgres://$DB_USER:$DB_PASS@$DB_HOST:$DB_PORT/$DB_NAME"
