# Load .env into the make environment and export to all recipe shells.
# The leading - means make won't error if .env doesn't exist.
-include .env
export

.PHONY: fmt test test-js lint run push \
	db-setup db-setup-spot db-setup-spot-create db-setup-paste db-setup-paste-create \
	db-setup-transfer db-setup-transfer-create \
	db-migrate db-migrate-spot db-migrate-paste db-migrate-transfer migrant db-shell f

fmt:
	cargo fmt --all

test:
	./bin/test-db.sh

test-js:
	node --test "crates/transfer/web/static/*.test.mjs"

lint:
	cargo clippy --workspace --tests -- -D warnings

f: fmt lint

run:
	./bin/docker.sh run

# Ensure the migrant CLI is installed
migrant:
	@which migrant > /dev/null 2>&1 || cargo install migrant --features postgres

db-migrate-spot: migrant
	cd migrations/spot && \
		migrant setup && \
		(migrant apply -a || echo "ok")

db-setup-spot-create:
	DB_NAME=spot DB_USER=spot DB_PASS=spot DB_HOST=localhost DB_PORT=5432 ./bin/setup-dev-db.sh

db-setup-spot: db-setup-spot-create db-migrate-spot

db-migrate-paste: migrant
	cd migrations/paste && \
		migrant setup && \
		(migrant apply -a || echo "ok")

db-setup-paste-create:
	DB_NAME=paste DB_USER=paste DB_PASS=paste DB_HOST=localhost DB_PORT=5432 ./bin/setup-dev-db.sh

db-setup-paste: db-setup-paste-create db-migrate-paste

db-migrate-transfer: migrant
	cd migrations/transfer && \
		migrant setup && \
		(migrant apply -a || echo "ok")

db-setup-transfer-create:
	DB_NAME=transfer DB_USER=transfer DB_PASS=transfer DB_HOST=localhost DB_PORT=5432 ./bin/setup-dev-db.sh

db-setup-transfer: db-setup-transfer-create db-migrate-transfer

db-setup: db-setup-spot db-setup-paste db-setup-transfer
db-migrate: db-migrate-spot db-migrate-paste db-migrate-transfer

db-shell:
	LOG_LEVEL=info fly pg connect -a kom-db
