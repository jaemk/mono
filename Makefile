# Load .env into the make environment and export to all recipe shells.
# The leading - means make won't error if .env doesn't exist.
-include .env
export

.PHONY: fmt test test-js acceptance lint run push \
	db-setup db-setup-spot db-setup-spot-create db-setup-paste db-setup-paste-create \
	db-setup-mapour db-setup-mapour-create db-setup-komino db-setup-komino-create \
	db-migrate db-migrate-spot db-migrate-paste db-migrate-mapour db-migrate-komino \
	migrant db-shell f

fmt:
	cargo fmt --all

test:
	./bin/test-db.sh

# komino client tests (jsdom); needs node and npm on PATH
test-js:
	cd crates/komino/web && npm ci && npm test

# post-deploy checks against the live site (node 22+ on PATH); see
# acceptance/live.test.mjs for ACCEPTANCE_BASE and ACCEPTANCE_HOSTS
acceptance: ACCEPTANCE_VERSION ?= $(shell git rev-parse --short=7 HEAD)
acceptance:
	ACCEPTANCE_VERSION=$(ACCEPTANCE_VERSION) node --test acceptance/live.test.mjs

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

db-migrate-mapour: migrant
	cd migrations/mapour && \
		migrant setup && \
		(migrant apply -a || echo "ok")

db-setup-mapour-create:
	DB_NAME=mapour DB_USER=mapour DB_PASS=mapour DB_HOST=localhost DB_PORT=5432 ./bin/setup-dev-db.sh

db-setup-mapour: db-setup-mapour-create db-migrate-mapour

db-migrate-komino: migrant
	cd migrations/komino && \
		migrant setup && \
		(migrant apply -a || echo "ok")

db-setup-komino-create:
	DB_NAME=komino DB_USER=komino DB_PASS=komino DB_HOST=localhost DB_PORT=5432 ./bin/setup-dev-db.sh

db-setup-komino: db-setup-komino-create db-migrate-komino

db-setup: db-setup-spot db-setup-paste db-setup-mapour db-setup-komino
db-migrate: db-migrate-spot db-migrate-paste db-migrate-mapour db-migrate-komino

db-shell:
	LOG_LEVEL=info fly pg connect -a kom-db
